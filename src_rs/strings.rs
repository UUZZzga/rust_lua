//! # Lua 字符串模块 — Rust 惯用重写
//!
//! 将 Lua C 实现中的 `TString`/短字符串内部化/惰性哈希 转换为 Rust 类型系统。
//!
//! ## 核心类型
//! - `LuaString` — 枚举类型，统一表示短/长字符串
//!   - `LuaString::Short(ArcRc<ShortString>)` — 内部化（interned）的短字符串
//!   - `LuaString::Long(Box<LongString>)` — 非内部化的长字符串
//!
//! ## 设计原则
//! - 短字符串通过指针相等性比较（内部化保证同一内容只有一个 ArcRc 实例）
//! - 长字符串通过内容比较（hash → length → contents 三级短路）
//! - 长度直接从 `String` 获取（`contents.len()`），无冗余字段
//! - 哈希统一使用 Lua 风格快速 hash（对应 C `luaS_hash`），无随机种子（编译器场景无 DoS 风险）
//! - 短字符串创建时预计算 hash → O(1) Hash trait
//! - 长字符串惰性计算 hash，`Hash::hash` 首次计算后通过 Atomic 自动缓存，避免重复计算
//! - 字符串表使用 `hashbrown::HashTable<ArcRc<ShortString>>` 单级哈希表
//! - **多线程安全**：StringTable 使用 `RefCell/RwLock` 保护 HashTable，读可并发、写互斥
//!   LongString 使用 `AtomicU64`/`AtomicU8` 实现 Sync 内部可变性

// ============================================================================
// RwLock 抽象层 — 根据 `threaded` feature 切换实现
// ============================================================================
// 性能: 默认 RefCell 模式省去 atomic 操作开销。
// perf 数据显示 StringTable::intern 在编译热点路径上 (6.19%),
// 每次调用都要 read() 锁,RefCell 比 RwLock 快约 3-5ns (无 CAS)。
//
// 进一步优化: 非 threaded 模式下, RefCell 的运行时借用检查 (mov borrow
// counter + cmp + 写回) 仍然占 intern 时间的 3.14% (perf annotate 显示:
// `mov 0x10(%rcx),%rdi` 2.03% + `mov %rax,0x10(%rcx)` 1.11%).
// 由于 StringTable 在非 threaded 模式下是 !Sync, 单线程访问保证不会并发,
// intern/intern_bytes/count/remove 中用 unsafe 绕过借用检查 (as_ptr + 解引用)。
// threaded 模式下仍走 RwLock 路径保证线程安全。

#[cfg(not(feature = "threaded"))]
mod inner_lock {
    use std::cell::RefCell;
    use std::fmt;

    pub struct RwLock<T: ?Sized>(pub RefCell<T>);

    impl<T> RwLock<T> {
        #[inline(always)]
        pub const fn new(val: T) -> Self {
            RwLock(RefCell::new(val))
        }
        #[inline(always)]
        pub fn read(&self) -> std::cell::Ref<'_, T> {
            self.0.borrow()
        }
        #[inline(always)]
        pub fn write(&self) -> std::cell::RefMut<'_, T> {
            self.0.borrow_mut()
        }
        /// UNSAFE: 直接获取内部 RefCell 的裸指针 (供绕过借用检查使用)。
        /// 调用方需保证单线程独占访问 (非 threaded 模式下 StringTable 是 !Sync)。
        #[inline(always)]
        pub fn as_ptr(&self) -> *mut T {
            self.0.as_ptr()
        }
    }

    impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLock<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(&self.0, f)
        }
    }
}

#[cfg(feature = "threaded")]
mod inner_lock {
    pub use parking_lot::RwLock;
}

use inner_lock::RwLock;

// ============================================================================
// ArcRc 别名 — 根据 threaded feature 切换 Arc/Rc
// ============================================================================
// 性能: 非 threaded 模式下 LuaState 本身不是 Sync (含 Rc/RefCell/*mut),
// 短字符串的引用计数无需原子操作。perf 数据显示 intern 函数中
// `lock incq` (Arc::clone 的原子 CAS) 占 intern 时间的 14.61%。
// 改用 Rc 后 `incq` (非原子) 消除 cache line 上的 LOCK 前缀开销。
#[cfg(not(feature = "threaded"))]
pub type ArcRc<T> = std::rc::Rc<T>;
#[cfg(feature = "threaded")]
pub type ArcRc<T> = std::sync::Arc<T>;

use std::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::os::raw::c_char;

use hashbrown::HashTable;

// ============================================================================
// 规约：常量
// ============================================================================

/// 短字符串的最大长度（字节数）。
/// 长度 ≤ 40 的字符串会被内部化（interned），相同内容的字符串共享同一个 `ArcRc`。
pub const LUAI_MAXSHORTLEN: usize = 40;

const MEMERRMSG: &str = "not enough memory";

// ============================================================================
// 规约：字符串类型定义
// ============================================================================

/// 短字符串 — 长度 ≤ 40 字节，会被内部化。
///
/// `contents: String` 内含长度信息，无需独立的长度字段。
/// 内部化保证同一内容唯一实例，因此可仅通过 `contents` 比较判等。
#[derive(Clone, Debug)]
pub struct ShortString {
    pub hash: u64,
    pub contents: String,
}

/// 长字符串 — 长度 > 40 字节，不进行内部化，支持惰性哈希。
///
/// `contents: String` 内含长度信息，无需独立的 `lnglen` 字段。
/// `hash` / `extra` 使用 `Atomic` 实现线程安全内部可变性：
/// - `Hash::hash` 首次调用时自动计算并缓存 hash，后续 O(1) 复用
/// - 并发安全：Race 仅导致重复计算同一值（无安全隐患）
pub struct LongString {
    pub hash: AtomicU64,
    pub extra: AtomicU8,
    pub contents: String,
    /// 稳定的唯一标识符，用于 %p 格式输出。
    /// 克隆时保留同一值（表示同一个字符串实例）。
    pub ptr_id: u32,
}

impl Clone for LongString {
    fn clone(&self) -> Self {
        LongString {
            hash: AtomicU64::new(self.hash.load(Ordering::Relaxed)),
            extra: AtomicU8::new(self.extra.load(Ordering::Relaxed)),
            contents: self.contents.clone(),
            ptr_id: self.ptr_id,
        }
    }
}

impl Debug for LongString {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("LongString")
            .field("hash", &self.hash.load(Ordering::Relaxed))
            .field("extra", &self.extra.load(Ordering::Relaxed))
            .field("len", &self.contents.len())
            .field("contents", &self.contents)
            .finish()
    }
}

/// 统一字符串类型。
#[derive(Clone, Debug)]
pub enum LuaString {
    Short(ArcRc<ShortString>),
    Long(Box<LongString>),
}

// ============================================================================
// 规约：内容比较辅助函数（兼容 NUL 和非 NUL 末尾的字符串）
// ============================================================================

/// 比较两个字符串的内容是否相同，忽略任一侧末尾可能的 NUL 字节。
#[inline]
fn content_eq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let ab = if ab.last() == Some(&0) { &ab[..ab.len() - 1] } else { ab };
    let bb = if bb.last() == Some(&0) { &bb[..bb.len() - 1] } else { bb };
    ab == bb
}

/// 长字符串相等性：若双方均已哈希 → 先比 hash（快速淘汰），否则直接比内容。
impl PartialEq for LongString {
    fn eq(&self, other: &Self) -> bool {
        if self.extra.load(Ordering::Relaxed) == 1 && other.extra.load(Ordering::Relaxed) == 1 {
            if self.hash.load(Ordering::Relaxed) != other.hash.load(Ordering::Relaxed) {
                return false;
            }
        }
        content_eq(&self.contents, &other.contents)
    }
}

/// 短字符串：`ArcRc::ptr_eq` 快速路径，否则比较 `hash` 后再 `content_eq`。
/// 长字符串：委派给 `LongString::eq`。
/// 跨类型（Short vs Long）：比较内容 — 对应 C Lua 的 luaS_eqlngstr/luaS_hash 比较逻辑。
///
/// 性能：Short-Short 路径是编译器热点（const_index 查找）。
/// perf 数据显示 PartialEq::eq (LuaString) 占 4.18%，主要发生在
/// `const_index.get(&key)` → `ConstKey::PartialEq::eq` → `LuaString::PartialEq::eq`
/// 链路中。当不同常量字符串落在同一 hash 桶时，ptr_eq 失败后直接走 content_eq
/// 字节比较（涉及 NUL 处理与 slice 切片），开销较大。
///
/// 加 hash 预比较后：hash 不同 → 立即返回 false（O(1)，仅 1 条 `cmp` 指令），
/// 避免进入 content_eq。hash 相同时（hash 冲突，极少见）才走 content_eq 检查实际内容。
impl PartialEq for LuaString {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (LuaString::Short(a), LuaString::Short(b)) => {
                // ptr_eq 优先（内部化保证同一内容只有一个实例 → 极快）
                // hash 预比较（不同 hash → 内容必然不同 → 立即 false）
                // 最后才 content_eq（仅 hash 冲突时触发）
                ArcRc::ptr_eq(a, b)
                    || (a.hash == b.hash && content_eq(&a.contents, &b.contents))
            }
            (LuaString::Long(a), LuaString::Long(b)) => a == b,
            _ => self.as_str() == other.as_str(),
        }
    }
}

impl Eq for LuaString {}

// 与 &str / String 的内容比较 (用于编译器内部 `lv.name == "x"` 等便捷比较)
// 通过 as_str() O(1) 取 slice 再做字节比较, 无额外分配。
impl PartialEq<str> for LuaString {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<&str> for LuaString {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl PartialEq<LuaString> for str {
    #[inline]
    fn eq(&self, other: &LuaString) -> bool {
        self == other.as_str()
    }
}

// ============================================================================
// 规约：eq_str 辅助函数
// ============================================================================

/// 比较两个 `LuaString` 的内容是否相同。
pub fn eq_str(a: &LuaString, b: &LuaString) -> bool {
    match (a, b) {
        (LuaString::Short(a), LuaString::Short(b)) => {
            ArcRc::ptr_eq(a, b)
                || (a.hash == b.hash && content_eq(&a.contents, &b.contents))
        }
        (LuaString::Long(a), LuaString::Long(b)) => content_eq(&a.contents, &b.contents),
        _ => false,
    }
}

// ============================================================================
// 规约：哈希实现
// ============================================================================

/// 统一使用 Lua 风格快速 hash，始终写入 u64 到 Hasher。
///
/// 短字符串：`state.write_u64(hash)` → O(1)。
/// 长字符串：extra == 1 时 `state.write_u64(hash)` → O(1)；
/// extra == 0 时计算 `rust_hash` 后写入并**自动缓存**，后续 O(1)。
impl Hash for LuaString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            LuaString::Short(s) => state.write_u64(s.hash),
            LuaString::Long(s) => {
                if s.extra.load(Ordering::Relaxed) == 1 {
                    state.write_u64(s.hash.load(Ordering::Relaxed));
                } else {
                    let content = if s.contents.as_bytes().last() == Some(&0) {
                        &s.contents[..s.contents.len() - 1]
                    } else {
                        &s.contents
                    };
                    let h = rust_hash(content);
                    s.hash.store(h, Ordering::Relaxed);
                    s.extra.store(1, Ordering::Relaxed);
                    state.write_u64(h);
                }
            }
        }
    }
}

// ============================================================================
// 规约：字符串表（内部化）
// ============================================================================

/// 字符串表 — 管理短字符串的内部化。
///
/// 使用 `HashTable<ArcRc<ShortString>>` 单级哈希表，每个条目仅 8 字节（指针）。
/// 相比之前 `HashMap<u64, Vec<ArcRc<ShortString>>>` 的两级结构（每条目 32 字节）：
/// - 4 倍缓存密度（每缓存行 8 条目 vs 2 条目），减少 cache miss
/// - 消除 Vec 迭代开销（len 检查、索引、边界检查）
/// - hashbrown SIMD 探测直接在字符串 hash 上进行，等效函数仅比较内容
/// perf annotate 显示原结构中 `shl $0x5`（×32 偏移）占 intern 时间显著比例，
/// 改为 8 字节条目后变为 `shl $0x3`（×8 偏移）。
///
/// 注：使用 `hashbrown::HashTable`（公开 API），它包装了内部的 `RawTable`，
/// 提供 `find(hash, eq)` / `insert_unique(hash, value)` / `find_entry(hash, eq)`
/// 等方法（无需传 hasher，由调用方传入预计算 hash）。
pub struct StringTable {
    ht: RwLock<HashTable<ArcRc<ShortString>>>,
    nuse: RwLock<usize>,
}

impl Debug for StringTable {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("StringTable")
            .field("nuse", &*self.nuse.read())
            .finish()
    }
}

impl StringTable {
    pub fn new() -> Self {
        // 初始容量 256: perf 显示 intern 是 finish_grow 的主要 caller (12 次),
        // HashTable 扩容开销大 (realloc + rehash)。256 * 0.875 = 224 个条目后才扩容,
        // 覆盖 Lua 关键字 (21) + 标准库符号 (~100) + 常用变量名,减少首次扩容。
        // 内存开销: 256 * 8 = 2KB, 可忽略。
        StringTable {
            ht: RwLock::new(HashTable::with_capacity(256)),
            nuse: RwLock::new(0),
        }
    }
    /// 内部化一个短字符串。
    #[inline]
    #[cfg(not(feature = "threaded"))]
    pub fn intern(&self, str: &str) -> LuaString {
        let h = rust_hash(str);
        debug_assert!(str.len() <= LUAI_MAXSHORTLEN, "intern 只用于短字符串");

        let str_bytes = str.as_bytes();
        let str_len = str_bytes.len();

        // UNSAFE fast path: 非 threaded 模式下 StringTable 是 !Sync, 单线程独占访问。
        // 直接通过 RefCell::as_ptr() 解引用 HashTable, 绕过 RefCell::borrow 的运行时
        // borrow counter 检查。
        // SAFETY: StringTable 在非 threaded 模式下是 !Sync (RefCell 是 !Sync),
        // Rust 类型系统保证单线程访问; intern 不会重入 (无递归调用其他 intern).
        let ht = unsafe { &mut *self.ht.as_ptr() };

        // 单级查找: HashTable 用预计算 hash 做 SIMD 探测,
        // 等效函数仅在 hash tag 匹配时调用, 比较字符串内容。
        // 相比之前 HashMap<u64, Vec<...>> 两级结构:
        // 1. 消除 Vec 迭代开销 (len 检查、索引、边界检查)
        // 2. 条目大小 8 字节 (vs 32 字节), 4 倍缓存密度
        if let Some(ts) = ht.find(h, |ts| {
            let content_bytes = ts.contents.as_bytes();
            // 字符串表中所有 ShortString 都通过 LuaString::with_nul 或
            // buf.push(0) 创建, contents 末尾必有 NUL 终止符。
            // 当 content_bytes.len() == str_len + 1 时, NUL 检查冗余 (已去除)。
            content_bytes.len() == str_len + 1 && content_bytes[..str_len] == *str_bytes
        }) {
            return LuaString::Short(ArcRc::clone(ts));
        }

        // 写路径: 需要插入新字符串
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: LuaString::with_nul(str),
        });
        // hasher 函数仅在 resize 时调用, 返回预计算 hash
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        // SAFETY: 同上, nuse 也是 RefCell 包装, 单线程下安全.
        *unsafe { &mut *self.nuse.as_ptr() } += 1;
        LuaString::Short(ts)
    }

    /// 内部化一个短字符串 (threaded 模式 — 走 RwLock 保证线程安全).
    #[inline]
    #[cfg(feature = "threaded")]
    pub fn intern(&self, str: &str) -> LuaString {
        let h = rust_hash(str);
        debug_assert!(str.len() <= LUAI_MAXSHORTLEN, "intern 只用于短字符串");

        let str_bytes = str.as_bytes();
        let str_len = str_bytes.len();

        // 单级查找 (见非 threaded 版本注释)
        let ht_reader = self.ht.read();
        if let Some(ts) = ht_reader.find(h, |ts| {
            let content_bytes = ts.contents.as_bytes();
            content_bytes.len() == str_len + 1 && content_bytes[..str_len] == *str_bytes
        }) {
            return LuaString::Short(ArcRc::clone(ts));
        }
        drop(ht_reader);

        // 写路径: 需要插入新字符串
        // TOCTOU 在单线程执行中安全; 多线程下最多导致重复桶条目(无害)
        let mut ht = self.ht.write();
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: LuaString::with_nul(str),
        });
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        *self.nuse.write() += 1;
        LuaString::Short(ts)
    }

    /// 内部化一个短字符串（从任意字节，8-bit clean，绕过 UTF-8 验证）。
    /// 用于 C API 的 lua_pushlstring/lua_pushstring 等需要保留原始字节的场景。
    #[inline]
    #[cfg(not(feature = "threaded"))]
    pub fn intern_bytes(&self, bytes: &[u8]) -> LuaString {
        debug_assert!(bytes.len() <= LUAI_MAXSHORTLEN, "intern_bytes 只用于短字符串");
        // 必须使用与 intern() 相同的哈希算法（rust_hash_bytes）。
        // intern_bytes 与 intern 在相同字节输入下必须产生相同 hash。
        let h = rust_hash_bytes(bytes);

        let bytes_len = bytes.len();

        // SAFETY: 同 intern, 非 threaded 模式下 StringTable 是 !Sync, 单线程独占.
        let ht = unsafe { &mut *self.ht.as_ptr() };

        // 单级查找 (见 intern 注释)
        if let Some(ts) = ht.find(h, |ts| {
            let content_bytes = ts.contents.as_bytes();
            content_bytes.len() == bytes_len + 1 && content_bytes[..bytes_len] == *bytes
        }) {
            return LuaString::Short(ArcRc::clone(ts));
        }

        // 写路径
        let mut buf = bytes.to_vec();
        buf.push(0); // NUL 终止符
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: unsafe { String::from_utf8_unchecked(buf) },
        });
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        // SAFETY: 同上
        *unsafe { &mut *self.nuse.as_ptr() } += 1;
        LuaString::Short(ts)
    }

    /// 内部化一个短字符串 (threaded 模式 — 走 RwLock).
    #[inline]
    #[cfg(feature = "threaded")]
    pub fn intern_bytes(&self, bytes: &[u8]) -> LuaString {
        debug_assert!(bytes.len() <= LUAI_MAXSHORTLEN, "intern_bytes 只用于短字符串");
        let h = rust_hash_bytes(bytes);
        let bytes_len = bytes.len();

        let ht_reader = self.ht.read();
        if let Some(ts) = ht_reader.find(h, |ts| {
            let content_bytes = ts.contents.as_bytes();
            content_bytes.len() == bytes_len + 1 && content_bytes[..bytes_len] == *bytes
        }) {
            return LuaString::Short(ArcRc::clone(ts));
        }
        drop(ht_reader);

        let mut buf = bytes.to_vec();
        buf.push(0);
        let mut ht = self.ht.write();
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: unsafe { String::from_utf8_unchecked(buf) },
        });
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        *self.nuse.write() += 1;
        LuaString::Short(ts)
    }

    pub fn count(&self) -> usize {
        *self.nuse.read()
    }

    pub fn remove(&self, ts: &ShortString) {
        let h = ts.hash;
        let ptr = ts as *const ShortString;
        let mut ht = self.ht.write();
        // 用指针相等性匹配要删除的条目
        // HashTable 没有 remove_entry 方法，改用 find_entry + remove
        if let Ok(entry) = ht.find_entry(h, |item: &ArcRc<ShortString>| std::ptr::eq(item.as_ref(), ptr)) {
            entry.remove();
        }
        let mut nuse = self.nuse.write();
        *nuse = nuse.saturating_sub(1);
    }

    pub fn for_each<F: FnMut(&ShortString)>(&self, mut f: F) {
        let ht = self.ht.read();
        for ts in ht.iter() {
            f(ts);
        }
    }

    /// 清理字符串表中的死字符串（只有字符串表持有的字符串）。
    /// 对应 C Lua 的 sweep 阶段清理 string table 的逻辑。
    /// 返回被清理的字符串数量。
    pub fn sweep(&self) -> usize {
        let mut ht = self.ht.write();
        // 收集待删除条目的 (hash, ptr), 避免在迭代中修改表
        let mut to_remove: Vec<(u64, *const ShortString)> = Vec::new();
        for ts in ht.iter() {
            // strong_count == 1 表示只有字符串表持有，无其他引用 → 可回收
            if ArcRc::strong_count(ts) <= 1 {
                to_remove.push((ts.hash, ArcRc::as_ptr(ts)));
            }
        }
        let freed = to_remove.len();
        for (hash, ptr) in to_remove {
            // HashTable 没有 remove_entry 方法，改用 find_entry + remove
            if let Ok(entry) = ht.find_entry(hash, |item: &ArcRc<ShortString>| ArcRc::as_ptr(item) == ptr) {
                entry.remove();
            }
        }

        let mut nuse = self.nuse.write();
        *nuse = nuse.saturating_sub(freed);

        freed
    }
}

// ============================================================================
// 规约：哈希计算
// ============================================================================

/// Lua 风格的快速 hash 函数 — 对应 C 的 `luaS_hash` (lstring.c)。
///
/// 每字符仅需 4 条指令（shift+add+add+xor），比 Rust `DefaultHasher`
/// (SipHash-1-3, ~30 条指令/8 字节) 快 5-10 倍。
///
/// perf 数据显示 StringTable::intern 在编译热点路径上占 14.63%，
/// 其中绝大部分时间花在 SipHash13::write 上。改用此 hash 后 intern
/// 开销大幅下降。
///
/// 注意：编译器场景不面临 hash 碰撞 DoS 攻击（源码可信），因此无需
/// SipHash 的密码学安全性。固定 seed 即可。
#[inline]
pub fn rust_hash_bytes(bytes: &[u8]) -> u64 {
    let l = bytes.len();
    // seed = length * 0x5bd1e995（MurmurHash2 常量，扩散性好）
    let mut h: u64 = (l as u64).wrapping_mul(0x5bd1e995);
    // 反向遍历对应 C 的 `for (; l > 0; l--) h ^= ((h<<5) + (h>>2) + str[l-1]);`
    // 改为 64 位以充分利用寄存器，移位常量也相应调整。
    for i in (0..l).rev() {
        let b = bytes[i] as u64;
        h ^= h.wrapping_shl(7).wrapping_add(h.wrapping_shr(2)).wrapping_add(b);
    }
    h
}

#[inline]
pub fn rust_hash(str: &str) -> u64 {
    rust_hash_bytes(str.as_bytes())
}

// ============================================================================
// 规约：字符串方法
// ============================================================================
impl LuaString {
    /// 新建时自动追加 NUL 字节，确保作为 *const c_char 返回时安全。
    pub(crate) fn with_nul(str: &str) -> String {
        let mut s = str.to_string();
        s.push('\0');
        s
    }

    /// 估算字符串真实堆占用（用于 GC 内存计费）。
    /// 短串: ArcRc 分配头 + ShortString 结构 + contents 堆分配
    /// 长串: Box 指针 + LongString 结构 + contents 堆分配
    /// 字符串不调用 register_object（无 gc_header），由 gc_extra_estimate 跟踪。
    pub fn gc_mem_size(&self) -> usize {
        match self {
            // ArcRc 分配 = ArcInner<ShortString>（含引用计数 usize）+ ShortString 自身
            // ShortString = { hash: u64, contents: String }，String 堆分配 = capacity
            LuaString::Short(s) => {
                std::mem::size_of::<ShortString>() + s.contents.capacity() + 16
            }
            // Box<LongString> 堆分配 = LongString 自身（Box 无额外头）
            // LongString = { hash: AtomicU64, extra: AtomicU8, contents: String, ptr_id: u32 }
            LuaString::Long(s) => {
                std::mem::size_of::<LongString>() + s.contents.capacity() + 8
            }
        }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        self.as_str_inner().0
    }

    /// 内部实现：返回 (str, has_nul)
    fn as_str_inner(&self) -> (&str, bool) {
        match self {
            LuaString::Short(s) => {
                if s.contents.as_bytes().last() == Some(&0) {
                    (&s.contents[..s.contents.len() - 1], true)
                } else {
                    (&s.contents, false)
                }
            }
            LuaString::Long(s) => {
                if s.contents.as_bytes().last() == Some(&0) {
                    (&s.contents[..s.contents.len() - 1], true)
                } else {
                    (&s.contents, false)
                }
            }
        }
    }

    /// 返回一个 NUL 结尾的 C 字符串指针（供 C API 使用）。
    /// 指针在 LuaString 自身存活期间有效。
    pub fn as_c_str_ptr(&self) -> *const c_char {
        match self {
            LuaString::Short(s) => s.contents.as_ptr() as *const c_char,
            LuaString::Long(s) => s.contents.as_ptr() as *const c_char,
        }
    }

    /// 返回字符串长度（O(1)，不含末尾 NUL）。
    pub fn len(&self) -> usize {
        self.as_str().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 返回预计算的哈希值（短字符串始终有效；长字符串 extra==0 时为 0）。
    pub fn hash(&self) -> u64 {
        match self {
            LuaString::Short(s) => s.hash,
            LuaString::Long(s) => s.hash.load(Ordering::Relaxed),
        }
    }
}
impl std::fmt::Display for LuaString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// 规约：创建字符串
// ============================================================================

/// 创建一个长字符串对象，不预先计算哈希（惰性）。
pub fn new_long_str(str: &str) -> LuaString {
    debug_assert!(
        str.len() > LUAI_MAXSHORTLEN,
        "长字符串长度必须大于 LUAI_MAXSHORTLEN"
    );
    LuaString::Long(Box::new(LongString {
        hash: AtomicU64::new(0),
        extra: AtomicU8::new(0),
        contents: LuaString::with_nul(str),
        ptr_id: crate::gc::new_ptr_id(),
    }))
}

/// 创建一个长字符串对象，直接 consume 传入的 String，避免 clone。
/// 用于 str_format 等已知结果为长字符串且不再需要原 String 的场景。
/// perf: 消除 new_long_str 中 with_nul 的 to_string() clone（constructs.lua 热点）。
pub fn new_long_str_from_string(mut s: String) -> LuaString {
    debug_assert!(
        s.len() > LUAI_MAXSHORTLEN,
        "长字符串长度必须大于 LUAI_MAXSHORTLEN"
    );
    s.push('\0');
    LuaString::Long(Box::new(LongString {
        hash: AtomicU64::new(0),
        extra: AtomicU8::new(0),
        contents: s,
        ptr_id: crate::gc::new_ptr_id(),
    }))
}

pub fn new_long_bytes(bytes: Vec<u8>) -> LuaString {
    let mut buf = bytes;
    buf.push(0);
    LuaString::Long(Box::new(LongString {
        hash: AtomicU64::new(0),
        extra: AtomicU8::new(0),
        contents: unsafe { String::from_utf8_unchecked(buf) },
        ptr_id: crate::gc::new_ptr_id(),
    }))
}

/// 从字节创建短字符串（自动追加 NUL 终止符，与 as_str_inner 的 NUL 剥离机制配合）
/// 用于模式匹配、pack/unpack 等需要直接构造 ShortString 的场景
///
/// hash 必须与 `StringTable::intern_bytes` 保持一致，否则违反 Hash/Eq 契约：
/// 两个内容相同的 ShortString（一个由 intern 创建，一个由此函数创建）
/// PartialEq 为 true 但 hash 不同，会导致 HashMap 查找失败。
pub fn new_short_bytes(bytes: Vec<u8>) -> LuaString {
    // 与 intern_bytes 相同的哈希算法：rust_hash_bytes
    let h = rust_hash_bytes(&bytes);
    let mut buf = bytes;
    buf.push(0);
    LuaString::Short(ArcRc::new(ShortString {
        hash: h,
        contents: unsafe { String::from_utf8_unchecked(buf) },
    }))
}

/// 从 &str 创建短字符串（自动追加 NUL 终止符）
pub fn new_short_str(s: &str) -> LuaString {
    new_short_bytes(s.as_bytes().to_vec())
}

/// 确保长字符串有哈希值（惰性计算）。
pub fn ensure_long_hash(ls: &mut LongString) -> u64 {
    if ls.extra.load(Ordering::Relaxed) == 0 {
        let content = if ls.contents.as_bytes().last() == Some(&0) {
            &ls.contents[..ls.contents.len() - 1]
        } else {
            &ls.contents
        };
        let h = rust_hash(content);
        ls.hash.store(h, Ordering::Relaxed);
        ls.extra.store(1, Ordering::Relaxed);
    }
    ls.hash.load(Ordering::Relaxed)
}
#[inline]
pub fn new_lstr(table: &StringTable, str: &str) -> LuaString {
    if str.len() <= LUAI_MAXSHORTLEN {
        table.intern(str)
    } else {
        new_long_str(str)
    }
}

/// 从 String 创建 LuaString，长字符串路径直接 consume 避免 clone。
/// 短字符串仍走 intern（需要查表去重，intern 未命中时内部会 clone，但短串开销小）。
#[inline]
pub fn new_lstr_from_string(table: &StringTable, s: String) -> LuaString {
    if s.len() <= LUAI_MAXSHORTLEN {
        table.intern(&s)
    } else {
        new_long_str_from_string(s)
    }
}

/// 从任意字节创建 LuaString（8-bit clean，绕过 UTF-8 验证）。
/// 用于 C API 的 lua_pushlstring/lua_pushstring 等需要保留原始字节的场景。
#[inline]
pub fn new_lstr_bytes(table: &StringTable, bytes: &[u8]) -> LuaString {
    if bytes.len() <= LUAI_MAXSHORTLEN {
        table.intern_bytes(bytes)
    } else {
        new_long_bytes(bytes.to_vec())
    }
}

// ============================================================================
// 规约：字符串缓存
// ============================================================================

#[derive(Debug)]
pub struct StringCache {
    cached: RwLock<LuaString>,
}

impl StringCache {
    pub fn new(memerrmsg: LuaString) -> Self {
        StringCache {
            cached: RwLock::new(memerrmsg),
        }
    }

    /// 通过指针快速创建字符串。命中缓存时直接返回，否则创建并更新缓存。
    ///
    /// # Safety
    /// `str_ptr` 必须指向长度为 `len` 的有效 UTF-8 字符串。
    pub fn cached_new(&self, str_ptr: *const u8, len: usize, table: &StringTable) -> LuaString {
        let cached = self.cached.read();
        let cached_ptr = cached.as_str().as_ptr();
        let cached_len = cached.len();

        if cached_ptr == str_ptr && cached_len == len {
            return cached.clone();
        }
        drop(cached);

        let slice =
            unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(str_ptr, len)) };
        let new_s = new_lstr(table, slice);

        let mut cached = self.cached.write();
        *cached = new_s.clone();
        new_s
    }

    pub fn clear(&self, new_base: LuaString) {
        let mut cached = self.cached.write();
        *cached = new_base;
    }
}

// ============================================================================
// 规约：字符串状态
// ============================================================================

#[derive(Debug)]
pub struct StringState {
    pub table: StringTable,
    pub cache: StringCache,
    pub memerrmsg: LuaString,
}

impl StringState {
    pub fn new() -> Self {
        let table = StringTable::new();
        let memerrmsg = table.intern(MEMERRMSG);
        let cache = StringCache::new(memerrmsg.clone());
        StringState {
            table,
            cache,
            memerrmsg,
        }
    }
}

impl Default for StringState {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ------------------------------------------------------------------------
    // rust_hash 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_hash_deterministic() {
        let h1 = rust_hash("hello");
        let h2 = rust_hash("hello");
        assert_eq!(h1, h2, "相同输入的哈希值必须一致");
    }

    #[test]
    fn test_hash_different_content_different_hash() {
        let h1 = rust_hash("hello");
        let h2 = rust_hash("world");
        assert_ne!(h1, h2, "不同内容的哈希值应该不同");
    }

    #[test]
    fn test_hash_empty_string() {
        let h = rust_hash("");
        let h2 = rust_hash("");
        assert_eq!(h, h2, "空字符串哈希值一致");
    }

    // ------------------------------------------------------------------------
    // StringTable::new 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_new_string_table() {
        let tb = StringTable::new();
        assert_eq!(tb.count(), 0, "新表应该为空");
    }

    // ------------------------------------------------------------------------
    // StringTable::intern 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_intern_new_short_string() {
        let tb = StringTable::new();
        let s = tb.intern("hello");
        assert_eq!(s.as_str(), "hello");
        assert_eq!(s.len(), 5);
        assert_eq!(tb.count(), 1, "nuse 应为 1");
    }

    #[test]
    fn test_intern_duplicate_returns_same() {
        let tb = StringTable::new();
        let s1 = tb.intern("hello");
        let s2 = tb.intern("hello");
        assert_eq!(s1, s2, "相同内容应返回同一个实例");
        assert!(eq_str(&s1, &s2), "通过 eq_str 比较也应相等");
        assert_eq!(tb.count(), 1, "内部化后 nuse 仍应为 1");
    }

    #[test]
    fn test_intern_multiple_strings() {
        let tb = StringTable::new();
        let s1 = tb.intern("hello");
        let s2 = tb.intern("world");
        let s3 = tb.intern("lua");
        assert_eq!(s1.as_str(), "hello");
        assert_eq!(s2.as_str(), "world");
        assert_eq!(s3.as_str(), "lua");
        assert_eq!(tb.count(), 3);
        let s1_dup = tb.intern("hello");
        assert_eq!(s1, s1_dup);
        assert_eq!(tb.count(), 3);
    }

    #[test]
    fn test_intern_empty_string() {
        let tb = StringTable::new();
        let s = tb.intern("");
        assert_eq!(s.as_str(), "");
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
        assert_eq!(tb.count(), 1);
    }

    #[test]
    fn test_intern_max_short_length() {
        let tb = StringTable::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN);
        let s = tb.intern(&content);
        assert_eq!(s.as_str(), content);
        assert_eq!(s.len(), LUAI_MAXSHORTLEN);
    }

    #[test]
    fn test_intern_triggers_grow() {
        let tb = StringTable::new();
        for i in 0..256 {
            let content = format!("key_{}", i);
            tb.intern(&content);
        }
        assert_eq!(tb.count(), 256, "所有字符串应该都被插入");
    }

    // ------------------------------------------------------------------------
    // StringTable::remove 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_remove_short_string() {
        let tb = StringTable::new();
        let s1 = tb.intern("hello");
        let s2 = tb.intern("world");
        assert_eq!(tb.count(), 2);

        if let LuaString::Short(ref ts) = s1 {
            tb.remove(ts);
        }
        assert_eq!(tb.count(), 1, "移除后 nuse 应为 1");

        let s2_again = tb.intern("world");
        assert_eq!(s2, s2_again);
        assert_eq!(tb.count(), 1, "重新查找 world 不应增加 nuse");
    }

    // ------------------------------------------------------------------------
    // eq_str 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_eq_str_short_same_pointer() {
        let tb = StringTable::new();
        let a = tb.intern("foo");
        let b = tb.intern("foo");
        assert!(eq_str(&a, &b), "相同短字符串必须相等");
    }

    #[test]
    fn test_eq_str_short_different() {
        let tb = StringTable::new();
        let a = tb.intern("foo");
        let b = tb.intern("bar");
        assert!(!eq_str(&a, &b), "不同短字符串必须不等");
    }

    #[test]
    fn test_eq_str_long_same_content() {
        let long_content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let a = new_long_str(&long_content);
        let b = new_long_str(&long_content);
        assert!(eq_str(&a, &b), "相同内容的长字符串必须相等");
    }

    #[test]
    fn test_eq_str_long_different() {
        let a = new_long_str(&"a".repeat(LUAI_MAXSHORTLEN + 1));
        let b = new_long_str(&"b".repeat(LUAI_MAXSHORTLEN + 1));
        assert!(!eq_str(&a, &b), "不同内容的长字符串必须不等");
    }

    #[test]
    fn test_eq_str_short_vs_long() {
        let tb = StringTable::new();
        let short = tb.intern("hello");
        let long = LuaString::Long(Box::new(LongString {
            hash: AtomicU64::new(0),
            extra: AtomicU8::new(0),
            contents: "hello".to_string(),
            ptr_id: 0,
        }));
        assert!(!eq_str(&short, &long), "不同类型（短 vs 长）必须不等");
    }

    // ------------------------------------------------------------------------
    // new_lstr 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_new_lstr_short() {
        let tb = StringTable::new();
        let s = new_lstr(&tb, "hello");
        assert!(matches!(s, LuaString::Short(_)));
        assert_eq!(s.as_str(), "hello");
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn test_new_lstr_long() {
        let tb = StringTable::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let s = new_lstr(&tb, &content);
        assert!(matches!(s, LuaString::Long(_)));
        assert_eq!(s.as_str(), content);
        assert_eq!(s.len(), LUAI_MAXSHORTLEN + 1);
    }

    // ------------------------------------------------------------------------
    // new_long_str 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_new_long_str_has_hashing_marker() {
        let ls = new_long_str(&"a".repeat(LUAI_MAXSHORTLEN + 1));
        assert_eq!(ls.len(), LUAI_MAXSHORTLEN + 1);
        match &ls {
            LuaString::Long(ls) => assert_eq!(
                ls.extra.load(Ordering::Relaxed),
                0,
                "新长字符串 extra 应为 0"
            ),
            _ => panic!("应为长字符串"),
        }
    }

    // ------------------------------------------------------------------------
    // ensure_long_hash 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_ensure_long_hash_computes_on_first_call() {
        let mut ls = LongString {
            hash: AtomicU64::new(123),
            extra: AtomicU8::new(0),
            contents: "a".repeat(50),
            ptr_id: 0,
        };
        let hash = ensure_long_hash(&mut ls);
        assert_eq!(
            ls.extra.load(Ordering::Relaxed),
            1,
            "extra 应为 1（标记已计算哈希）"
        );
        assert_eq!(
            hash,
            ls.hash.load(Ordering::Relaxed),
            "返回的哈希应与存储的一致"
        );
    }

    #[test]
    fn test_ensure_long_hash_idempotent() {
        let mut ls = LongString {
            hash: AtomicU64::new(0),
            extra: AtomicU8::new(1),
            contents: "a".repeat(50),
            ptr_id: 0,
        };
        let hash_before = ls.hash.load(Ordering::Relaxed);
        let hash = ensure_long_hash(&mut ls);
        assert_eq!(hash, hash_before, "已有哈希不应重新计算");
        assert_eq!(ls.extra.load(Ordering::Relaxed), 1, "extra 仍为 1");
    }

    // ------------------------------------------------------------------------
    // StringState 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_string_state_new() {
        let state = StringState::new();
        assert_eq!(state.table.count(), 1, "应包含 memerrmsg");
        assert_eq!(
            state.memerrmsg.as_str(),
            MEMERRMSG,
            "memerrmsg 应为 MEMERRMSG"
        );
    }

    // ------------------------------------------------------------------------
    // StringCache 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_cache_cached_new_hit() {
        let state = StringState::new();
        let str_ptr = "hello".as_ptr();
        let s1 = state.cache.cached_new(str_ptr, 5, &state.table);
        let s2 = state.cache.cached_new(str_ptr, 5, &state.table);
        assert_eq!(s1, s2, "缓存命中返回相同实例");
    }

    #[test]
    fn test_cache_clear() {
        let state = StringState::new();
        let str_ptr = "test".as_ptr();
        let s = state.cache.cached_new(str_ptr, 4, &state.table);
        assert_eq!(s.as_str(), "test");

        let memerr = state.memerrmsg.clone();
        state.cache.clear(memerr);
    }

    // ------------------------------------------------------------------------
    // LuaString 方法测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_lua_string_hash() {
        let tb = StringTable::new();
        let s = tb.intern("hello");
        let h = rust_hash("hello");
        assert_eq!(s.hash(), h);
    }

    #[test]
    fn test_lua_string_len_and_is_empty() {
        let tb = StringTable::new();
        let empty = tb.intern("");
        let non_empty = tb.intern("x");
        assert!(empty.is_empty());
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.len(), 1);
    }

    #[test]
    fn test_intern_arc_identity() {
        let tb = StringTable::new();
        let a = tb.intern("shared");
        let b = tb.intern("shared");
        if let (LuaString::Short(ra), LuaString::Short(rb)) = (&a, &b) {
            assert!(ArcRc::ptr_eq(ra, rb), "同一字符串的内部化应该返回相同的 ArcRc");
            assert_eq!(ArcRc::strong_count(ra), 3, "引用计数应为 3（表 + a + b）");
        } else {
            panic!("应为短字符串");
        }
    }

    // ========================================================================
    // 多线程并发测试 — 仅在 threaded feature 下编译（StringTable 需要 Send/Sync）
    // ========================================================================

    #[cfg(feature = "threaded")]
    #[test]
    fn test_string_table_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<StringTable>();
        assert_sync::<StringTable>();
        assert_send::<StringState>();
        assert_sync::<StringState>();
        assert_send::<LuaString>();
        assert_sync::<LuaString>();
    }

    #[cfg(feature = "threaded")]
    #[test]
    fn test_concurrent_intern_same_strings() {
        let table = ArcRc::new(StringTable::new());

        let strings: Vec<&str> = vec![
            "function", "return", "local", "while", "if", "else", "end", "then", "do", "for",
            "repeat", "until", "break", "nil", "true", "false", "and", "or", "not", "in",
        ];
        let count = strings.len();

        let mut handles = Vec::new();
        for _ in 0..4 {
            let table = ArcRc::clone(&table);
            let strings = strings.clone();
            handles.push(std::thread::spawn(move || {
                let mut results = Vec::new();
                for s in &strings {
                    let ls = table.intern(s);
                    results.push(ls);
                }
                results
            }));
        }

        let all_results: Vec<Vec<LuaString>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        for i in 0..count {
            let first = &all_results[0][i];
            for t in 1..4 {
                assert_eq!(
                    first, &all_results[t][i],
                    "线程间同一字符串应返回相同 Arc 实例: '{}'",
                    strings[i]
                );
            }
        }

        assert_eq!(table.count(), count, "count 应为去重后的字符串数");
    }

    #[cfg(feature = "threaded")]
    #[test]
    fn test_concurrent_intern_with_state() {
        let state = ArcRc::new(StringState::new());

        let mut handles = Vec::new();
        for _ in 0..4 {
            let state = ArcRc::clone(&state);
            handles.push(std::thread::spawn(move || {
                let mut results = Vec::new();
                for i in 0..50 {
                    let content = format!("var_{}", i % 25);
                    let ls = state.table.intern(&content);
                    results.push(ls);
                }
                results
            }));
        }

        let all_results: Vec<Vec<LuaString>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        for i in 0..25 {
            let first = &all_results[0][i];
            for t in 1..4 {
                assert_eq!(
                    first, &all_results[t][i],
                    "跨线程 intern 同一内容应返回相同实例"
                );
            }
        }

        assert_eq!(state.table.count(), 26);
    }

    #[cfg(feature = "threaded")]
    #[test]
    fn test_concurrent_intern_many_strings() {
        let table = ArcRc::new(StringTable::new());
        let num_threads = 8;
        let per_thread = 2000;

        let mut handles = Vec::new();
        for t in 0..num_threads {
            let table = ArcRc::clone(&table);
            handles.push(std::thread::spawn(move || {
                for i in 0..per_thread {
                    let content = format!("thread_{}_key_{}", t, i);
                    let _ = table.intern(&content);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(table.count(), num_threads * per_thread);
    }

    // ========================================================================
    // Hash 行为测试 — 验证 impl Hash for LuaString 的正确性
    // ========================================================================

    fn hash_one<T: Hash>(t: &T) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        t.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn test_hash_same_content_same_hash() {
        let tb = StringTable::new();
        let a = tb.intern("hello");
        let b = tb.intern("hello");
        assert_eq!(
            hash_one(&a),
            hash_one(&b),
            "相同内容应产生相同的 Rust Hash 值"
        );
    }

    #[test]
    fn test_rust_hash_different_content_different() {
        let tb = StringTable::new();
        let a = tb.intern("hello");
        let b = tb.intern("world");
        assert_ne!(
            hash_one(&a),
            hash_one(&b),
            "不同内容应产生不同的 Rust Hash 值"
        );
    }

    #[test]
    fn test_u64_hash_equals_write_u64() {
        let value: u64 = 0xDEAD_BEEF_CAFE_BABE;
        let mut hasher1 = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher1);
        let h1 = hasher1.finish();

        let mut hasher2 = std::collections::hash_map::DefaultHasher::new();
        hasher2.write_u64(value);
        let h2 = hasher2.finish();

        assert_eq!(
            h1, h2,
            "u64::hash() 等价于 hasher.write_u64()，不是\"hash 的 hash\""
        );
    }

    /// LongString: Hash::hash 首次调用自动缓存，后续 O(1) 复用
    #[test]
    fn test_hash_long_string_caches_on_first_call() {
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let ls = new_long_str(&content);
        match &ls {
            LuaString::Long(inner) => {
                assert_eq!(
                    inner.extra.load(Ordering::Relaxed),
                    0,
                    "new_long_str 创建的字符串 extra 应为 0"
                );
                assert_eq!(
                    inner.hash.load(Ordering::Relaxed),
                    0,
                    "惰性策略：extra == 0 时 hash == 0，未计算"
                );
            }
            _ => panic!("应为 Long"),
        }

        let h1 = hash_one(&ls);

        match &ls {
            LuaString::Long(inner) => {
                assert_eq!(
                    inner.extra.load(Ordering::Relaxed),
                    1,
                    "Hash::hash 调用后 extra 应自动变为 1（已缓存）"
                );
                assert_ne!(
                    inner.hash.load(Ordering::Relaxed),
                    0,
                    "Hash::hash 调用后 hash 被缓存为非零值"
                );
            }
            _ => panic!("应为 Long"),
        }

        let h2 = hash_one(&ls);
        assert_eq!(h1, h2, "再次 hash 应返回相同值（缓存命中，不重复计算）");
    }

    /// 同内容长字符串：extra=0 和 extra=1 产生相同 Hash
    #[test]
    fn test_hash_mixed_extra_same_content() {
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let unhashed = LuaString::Long(Box::new(LongString {
            hash: AtomicU64::new(0),
            extra: AtomicU8::new(0),
            contents: content.clone(),
            ptr_id: 0,
        }));
        let mut ls = LongString {
            hash: AtomicU64::new(0),
            extra: AtomicU8::new(0),
            contents: content.clone(),
            ptr_id: 0,
        };
        ensure_long_hash(&mut ls);
        let hashed = LuaString::Long(Box::new(ls));

        assert_eq!(unhashed, hashed, "同内容的不同 extra 状态应相等");

        assert_eq!(
            hash_one(&unhashed),
            hash_one(&hashed),
            "extra=0 和 extra=1 的同内容长字符串必须产生相同 Rust Hash"
        );

        let mut map: HashMap<LuaString, i32> = HashMap::new();
        map.insert(hashed.clone(), 42);
        assert_eq!(
            map.get(&unhashed),
            Some(&42),
            "extra=0 的 key 应能找到 extra=1 的同内容 key 插入的值"
        );
    }

    /// 相同内容：extra=0（实时计算）vs extra=1（缓存命中）→ Hash 相同
    #[test]
    fn test_hash_same_content_different_hash_field() {
        let h = rust_hash("hello");
        let ls1 = LuaString::Long(Box::new(LongString {
            hash: AtomicU64::new(0),
            extra: AtomicU8::new(0),
            contents: "hello".to_string(),
            ptr_id: 0,
        }));
        let ls2 = LuaString::Long(Box::new(LongString {
            hash: AtomicU64::new(h),
            extra: AtomicU8::new(1),
            contents: "hello".to_string(),
            ptr_id: 0,
        }));

        assert_eq!(
            hash_one(&ls1),
            hash_one(&ls2),
            "相同内容 → Hash 应相同（extra=0 实时计算 vs extra=1 缓存命中）"
        );
    }

    /// 大批量长字符串创建时不计算 hash
    #[test]
    fn test_large_long_string_no_eager_hash() {
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        for _ in 0..100 {
            let ls = new_long_str(&content);
            if let LuaString::Long(inner) = &ls {
                assert_eq!(
                    inner.hash.load(Ordering::Relaxed),
                    0,
                    "所有长字符串创建时不计算 hash"
                );
                assert_eq!(inner.extra.load(Ordering::Relaxed), 0);
            }
        }
    }

    /// HashMap 集成测试：短字符串和长字符串混用
    #[test]
    fn test_hashmap_with_mixed_strings() {
        let tb = StringTable::new();

        let short1 = tb.intern("key1");
        let short2 = tb.intern("key2");
        let long_content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let long1 = new_long_str(&long_content);

        let mut map: HashMap<LuaString, &str> = HashMap::new();
        map.insert(short1.clone(), "value1");
        map.insert(short2.clone(), "value2");
        map.insert(long1.clone(), "value3");

        let short1_lookup = tb.intern("key1");
        let long1_lookup = new_long_str(&long_content);

        assert_eq!(map.get(&short1_lookup), Some(&"value1"));
        assert_eq!(map.get(&long1_lookup), Some(&"value3"));

        let nonexistent = tb.intern("nonexistent");
        assert_eq!(map.get(&nonexistent), None);
    }

    /// ensure_long_hash 同一内容多次调用，哈希值一致、不重复计算
    #[test]
    fn test_ensure_long_hash_same_content() {
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let mut a = LongString {
            hash: AtomicU64::new(0),
            extra: AtomicU8::new(0),
            contents: content.clone(),
            ptr_id: 0,
        };
        let mut b = LongString {
            hash: AtomicU64::new(0),
            extra: AtomicU8::new(0),
            contents: content.clone(),
            ptr_id: 0,
        };

        let h0 = ensure_long_hash(&mut a);
        let h1 = ensure_long_hash(&mut b);
        assert_eq!(h0, h1, "同一内容应产生相同的 Rust hash");

        assert_eq!(a.extra.load(Ordering::Relaxed), 1);
        assert_eq!(b.extra.load(Ordering::Relaxed), 1);

        let h0_again = ensure_long_hash(&mut a);
        assert_eq!(h0, h0_again, "再次调用不重复计算");
    }
}
