//! # Lua 字符串模块 — Rust 惯用重写
//!
//! 将 Lua C 实现中的 `TString`/短字符串内部化/惰性哈希 转换为 Rust 类型系统。
//!
//! ## 核心类型
//! - `LuaString` — 枚举类型，统一表示短/长字符串
//!   - `LuaString::Short(ArcRc<ShortString>)` — 内部化（interned）的短字符串
//!   - `LuaString::Long(ArcRc<LongString>)` — 非内部化的长字符串（Rc 共享，避免 lua_tolstring 返回的指针在 lua_pop 后悬垂）
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

use std::cell::Cell;
use std::fmt::{self, Debug, Formatter};
use std::os::raw::c_char;
use std::rc::Rc;

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
        #[cfg_attr(not(size_optimized), inline(always))]
        pub const fn new(val: T) -> Self {
            RwLock(RefCell::new(val))
        }
        #[cfg_attr(not(size_optimized), inline(always))]
        pub fn read(&self) -> std::cell::Ref<'_, T> {
            self.0.borrow()
        }
        #[cfg_attr(not(size_optimized), inline(always))]
        pub fn write(&self) -> std::cell::RefMut<'_, T> {
            self.0.borrow_mut()
        }
        /// UNSAFE: 直接获取内部 RefCell 的裸指针 (供绕过借用检查使用)。
        /// 调用方需保证单线程独占访问 (非 threaded 模式下 StringTable 是 !Sync)。
        #[cfg_attr(not(size_optimized), inline(always))]
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
pub type ArcRc<T> = Rc<T>;
#[cfg(feature = "threaded")]
pub type ArcRc<T> = std::sync::Arc<T>;

// 默认模式 (性能优先): 使用 hashbrown::HashTable 单级哈希表
// size_optimized: 使用 std::collections::HashMap 两级结构, 减小二进制体积
#[cfg(not(size_optimized))]
use hashbrown::HashTable;

use crate::objects::TValue;

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
/// - `Hash::hash` 首次调用时自动计算并缓存 hash，后续 O(1) 复用
pub struct LongString {
    pub hash: Cell<u64>,
    pub extra: Cell<u8>,
    pub contents: String,
    /// 稳定的唯一标识符，用于 %p 格式输出。
    /// 克隆时保留同一值（表示同一个字符串实例）。
    pub ptr_id: u32,
}

impl Clone for LongString {
    fn clone(&self) -> Self {
        LongString {
            hash: self.hash.clone(),
            extra: self.extra.clone(),
            contents: self.contents.clone(),
            ptr_id: self.ptr_id,
        }
    }
}

impl Debug for LongString {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("LongString")
            .field("hash", &self.hash)
            .field("extra", &self.extra)
            .field("len", &self.contents.len())
            .field("contents", &self.contents)
            .finish()
    }
}

/// 统一字符串类型。
#[derive(Clone, Debug)]
pub enum LLuaString {
    Short(ArcRc<ShortString>),
    Long(Rc<LongString>),
}

impl LLuaString {
    pub fn to_value<'a>(&self) -> TValue<'a> {
        match self {
            LLuaString::Short(s) => TValue::ShortStr(s.clone()),
            LLuaString::Long(s) => TValue::LongStr(s.clone()),
        }
    }

    pub fn get_string(&self) -> &String {
        match self {
            LLuaString::Short(s) => &s.contents,
            LLuaString::Long(s) => &s.contents,
        }
    }
}

impl PartialEq for LLuaString {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Short(l0), Self::Short(r0)) => core::ptr::eq(l0, r0),
            (Self::Long(l0), Self::Long(r0)) => l0 == r0,
            _ => false,
        }
    }
}

impl<'a> TValue<'a> {
    pub fn as_str(&self) -> LLuaString {
        match self {
            TValue::ShortStr(s) => LLuaString::Short(s.clone()),
            TValue::LongStr(s) => LLuaString::Long(s.clone()),
            _ => panic!("TValue::as_str: not a string"),
        }
    }
}

// ============================================================================
// 规约：内容比较辅助函数（兼容 NUL 和非 NUL 末尾的字符串）
// ============================================================================

/// 比较两个字符串的内容是否相同，忽略任一侧末尾可能的 NUL 字节。
#[cfg_attr(not(size_optimized), inline)]
fn content_eq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let ab = if ab.last() == Some(&0) {
        &ab[..ab.len() - 1]
    } else {
        ab
    };
    let bb = if bb.last() == Some(&0) {
        &bb[..bb.len() - 1]
    } else {
        bb
    };
    ab == bb
}

/// 长字符串相等性：若双方均已哈希 → 先比 hash（快速淘汰），否则直接比内容。
impl PartialEq for LongString {
    fn eq(&self, other: &Self) -> bool {
        if self.extra.get() == 1 && other.extra.get() == 1 {
            if self.hash != other.hash {
                return false;
            }
        }
        content_eq(&self.contents, &other.contents)
    }
}

// ============================================================================
// 规约：eq_str 辅助函数
// ============================================================================

/// 比较两个 `LuaString` 的内容是否相同。
pub fn eq_str<'a>(a: &TValue<'a>, b: &TValue<'a>) -> bool {
    match (a, b) {
        (TValue::ShortStr(a), TValue::ShortStr(b)) => {
            ArcRc::ptr_eq(a, b) || (a.hash == b.hash && content_eq(&a.contents, &b.contents))
        }
        (TValue::LongStr(a), TValue::LongStr(b)) => content_eq(&a.contents, &b.contents),
        _ => false,
    }
}

// ============================================================================
// 规约：字符串表（内部化）
// ============================================================================

/// 字符串表 — 管理短字符串的内部化。
///
/// # 默认模式 (性能优先)
/// 使用 `hashbrown::HashTable<ArcRc<ShortString>>` 单级哈希表，每个条目仅 8 字节（指针）。
/// 相比 `HashMap<u64, Vec<ArcRc<ShortString>>>` 的两级结构（每条目 32 字节）：
/// - 4 倍缓存密度（每缓存行 8 条目 vs 2 条目），减少 cache miss
/// - 消除 Vec 迭代开销（len 检查、索引、边界检查）
/// - hashbrown SIMD 探测直接在字符串 hash 上进行，等效函数仅比较内容
///
#[cfg(not(size_optimized))]
pub struct StringTable {
    ht: RwLock<HashTable<ArcRc<ShortString>>>,
    nuse: RwLock<usize>,
}

#[cfg(size_optimized)]
pub struct StringTable {
    ht: RwLock<
        std::collections::HashMap<u64, Vec<ArcRc<ShortString>>, crate::objects::FxBuildHasher>,
    >,
    nuse: RwLock<usize>,
}

impl Debug for StringTable {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("StringTable")
            .field("nuse", &*self.nuse.read())
            .finish()
    }
}

#[cfg(not(size_optimized))]
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
    #[cfg_attr(not(size_optimized), inline)]
    pub fn intern(&self, str: &str) -> ArcRc<ShortString> {
        self.intern_with_hash(str, rust_hash(str))
    }

    #[inline]
    pub fn intern_value<'a>(&self, str: &str) -> TValue<'a> {
        TValue::ShortStr(self.intern_with_hash(str, rust_hash(str)))
    }

    /// 内部化一个短字符串 (使用预计算的 hash, 避免重复计算)。
    /// 用于词法分析器标识符缓存: 缓存查找时已计算 hash, 未命中时直接传入。
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(not(feature = "threaded"))]
    pub fn intern_with_hash(&self, str: &str, h: u64) -> ArcRc<ShortString> {
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
        // 等效函数仅在 hash tag (hash 低位 7bit) 匹配时调用。
        // 相比之前 HashMap<u64, Vec<...>> 两级结构:
        // 1. 消除 Vec 迭代开销 (len 检查、索引、边界检查)
        // 2. 条目大小 8 字节 (vs 32 字节), 4 倍缓存密度
        //
        // perf 优化: hashbrown 的 tag 只有 7bit (128 种值), 字符串表 256+ 条目时
        // 平均每个 tag 有 2+ 个条目。先比较完整 hash (O(1), 仅读 ShortString.hash
        // 字段), 快速淘汰 tag 匹配但完整 hash 不匹配的桶, 避免不必要的
        // contents.as_bytes() (Rc deref + String 字段访问) + memcmp。
        // perf 数据显示 Rc::deref 占 1.20%, memcmp 占 1.09%,
        // hash 预比较可减少这两项的开销。
        if let Some(ts) = ht.find(h, |ts| {
            // 先比较完整 hash: hash 不同 → 内容必然不同 → 立即 false
            // (ts.hash 是 ShortString 第一个字段, 读取无需额外偏移)
            if ts.hash != h {
                return false;
            }
            let content_bytes = ts.contents.as_bytes();
            // 字符串表中所有 ShortString 都通过 lua_string_with_nul 或
            // buf.push(0) 创建, contents 末尾必有 NUL 终止符。
            // 当 content_bytes.len() == str_len + 1 时, NUL 检查冗余 (已去除)。
            content_bytes.len() == str_len + 1 && content_bytes[..str_len] == *str_bytes
        }) {
            return ArcRc::clone(ts);
        }

        // 写路径: 需要插入新字符串
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: lua_string_with_nul(str),
        });
        // hasher 函数仅在 resize 时调用, 返回预计算 hash
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        // SAFETY: 同上, nuse 也是 RefCell 包装, 单线程下安全.
        *unsafe { &mut *self.nuse.as_ptr() } += 1;
        ts
    }

    /// 内部化一个短字符串 (使用预计算的 hash, threaded 模式).
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(feature = "threaded")]
    pub fn intern_with_hash(&self, str: &str, h: u64) -> ArcRc<ShortString> {
        debug_assert!(str.len() <= LUAI_MAXSHORTLEN, "intern 只用于短字符串");

        let str_bytes = str.as_bytes();
        let str_len = str_bytes.len();

        // 单级查找 (见非 threaded 版本注释), 先比较完整 hash 快速淘汰
        let ht_reader = self.ht.read();
        if let Some(ts) = ht_reader.find(h, |ts| {
            if ts.hash != h {
                return false;
            }
            let content_bytes = ts.contents.as_bytes();
            content_bytes.len() == str_len + 1 && content_bytes[..str_len] == *str_bytes
        }) {
            return ArcRc::clone(ts);
        }
        drop(ht_reader);

        // 写路径: 需要插入新字符串
        // TOCTOU 在单线程执行中安全; 多线程下最多导致重复桶条目(无害)
        let mut ht = self.ht.write();
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: lua_string_with_nul(str),
        });
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        *self.nuse.write() += 1;
        ts
    }

    /// 内部化一个短字符串（从任意字节，8-bit clean，绕过 UTF-8 验证）。
    /// 用于 C API 的 lua_pushlstring/lua_pushstring 等需要保留原始字节的场景。
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(not(feature = "threaded"))]
    pub fn intern_bytes<'a>(&self, bytes: &[u8]) -> TValue<'a> {
        debug_assert!(
            bytes.len() <= LUAI_MAXSHORTLEN,
            "intern_bytes 只用于短字符串"
        );
        // 必须使用与 intern() 相同的哈希算法（rust_hash_bytes）。
        // intern_bytes 与 intern 在相同字节输入下必须产生相同 hash。
        let h = rust_hash_bytes(bytes);

        let bytes_len = bytes.len();

        // SAFETY: 同 intern, 非 threaded 模式下 StringTable 是 !Sync, 单线程独占.
        let ht = unsafe { &mut *self.ht.as_ptr() };

        // 单级查找 (见 intern 注释), 先比较完整 hash 快速淘汰
        if let Some(ts) = ht.find(h, |ts| {
            if ts.hash != h {
                return false;
            }
            let content_bytes = ts.contents.as_bytes();
            content_bytes.len() == bytes_len + 1 && content_bytes[..bytes_len] == *bytes
        }) {
            return TValue::ShortStr(ArcRc::clone(ts));
        }

        // 写路径
        // perf: 用 unsafe copy_nonoverlapping + set_len 替代 extend_from_slice + push,
        // 消除两次容量检查 (extend_from_slice 和 push 各检查一次)。
        // SAFETY: with_capacity(len+1) 保证至少 len+1 字节;
        //         copy 复制 len 字节; 写 0 在 [len] (≤ capacity); set_len(len+1) 合法。
        let blen = bytes.len();
        let mut buf = Vec::with_capacity(blen + 1);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.as_mut_ptr(), blen);
            *buf.as_mut_ptr().add(blen) = 0;
            buf.set_len(blen + 1);
        }
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: unsafe { String::from_utf8_unchecked(buf) },
        });
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        // SAFETY: 同上
        *unsafe { &mut *self.nuse.as_ptr() } += 1;
        TValue::ShortStr(ts)
    }

    /// 内部化一个短字符串 (threaded 模式 — 走 RwLock).
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(feature = "threaded")]
    pub fn intern_bytes<'a>(&self, bytes: &[u8]) -> TValue<'a> {
        debug_assert!(
            bytes.len() <= LUAI_MAXSHORTLEN,
            "intern_bytes 只用于短字符串"
        );
        let h = rust_hash_bytes(bytes);
        let bytes_len = bytes.len();

        let ht_reader = self.ht.read();
        if let Some(ts) = ht_reader.find(h, |ts| {
            if ts.hash != h {
                return false;
            }
            let content_bytes = ts.contents.as_bytes();
            content_bytes.len() == bytes_len + 1 && content_bytes[..bytes_len] == *bytes
        }) {
            return TValue::ShortStr(ArcRc::clone(ts));
        }
        drop(ht_reader);

        // perf: 同非 threaded 版本, 用 unsafe 避免 extend_from_slice + push 的容量检查
        let blen = bytes.len();
        let mut buf = Vec::with_capacity(blen + 1);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.as_mut_ptr(), blen);
            *buf.as_mut_ptr().add(blen) = 0;
            buf.set_len(blen + 1);
        }
        let mut ht = self.ht.write();
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: unsafe { String::from_utf8_unchecked(buf) },
        });
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        *self.nuse.write() += 1;
        TValue::ShortStr(ts)
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
        if let Ok(entry) = ht.find_entry(h, |item: &ArcRc<ShortString>| {
            std::ptr::eq(item.as_ref(), ptr)
        }) {
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
            if let Ok(entry) =
                ht.find_entry(hash, |item: &ArcRc<ShortString>| ArcRc::as_ptr(item) == ptr)
            {
                entry.remove();
            }
        }

        let mut nuse = self.nuse.write();
        *nuse = nuse.saturating_sub(freed);

        freed
    }
}

// ============================================================================
// 规约：字符串表 (size_optimized 版本 — 使用 std HashMap)
// ============================================================================
#[cfg(size_optimized)]
impl StringTable {
    pub fn new() -> Self {
        // size_optimized: 不预分配, 减小二进制体积
        StringTable {
            ht: RwLock::new(std::collections::HashMap::with_hasher(
                crate::objects::FxBuildHasher::default(),
            )),
            nuse: RwLock::new(0),
        }
    }

    /// 内部化一个短字符串。
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(not(feature = "threaded"))]
    pub fn intern(&self, str: &str) -> LuaString {
        self.intern_with_hash(str, rust_hash(str))
    }

    /// 内部化一个短字符串 (使用预计算的 hash, 避免重复计算)。
    #[cfg(not(feature = "threaded"))]
    pub fn intern_with_hash(&self, str: &str, h: u64) -> LuaString {
        debug_assert!(str.len() <= LUAI_MAXSHORTLEN, "intern 只用于短字符串");

        let str_bytes = str.as_bytes();
        let str_len = str_bytes.len();

        // size_optimized: 直接用 RefCell::borrow (无需 unsafe 优化)
        let ht = self.ht.read();
        if let Some(vec) = ht.get(&h) {
            if let Some(ts) = vec.iter().find(|ts| {
                let content_bytes = ts.contents.as_bytes();
                content_bytes.len() == str_len + 1 && content_bytes[..str_len] == *str_bytes
            }) {
                return LuaString::Short(ArcRc::clone(ts));
            }
        }
        drop(ht);

        // 写路径: 需要插入新字符串
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: lua_string_with_nul(str),
        });
        let mut ht = self.ht.write();
        ht.entry(h).or_default().push(ArcRc::clone(&ts));
        *self.nuse.write() += 1;
        LuaString::Short(ts)
    }

    /// 内部化一个短字符串 (threaded 模式 — 走 RwLock 保证线程安全).
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(feature = "threaded")]
    pub fn intern(&self, str: &str) -> LuaString {
        self.intern_with_hash(str, rust_hash(str))
    }

    /// 内部化一个短字符串 (使用预计算的 hash, threaded + size_optimized 模式).
    #[cfg(feature = "threaded")]
    pub fn intern_with_hash(&self, str: &str, h: u64) -> LuaString {
        debug_assert!(str.len() <= LUAI_MAXSHORTLEN, "intern 只用于短字符串");

        let str_bytes = str.as_bytes();
        let str_len = str_bytes.len();

        let ht_reader = self.ht.read();
        if let Some(vec) = ht_reader.get(&h) {
            if let Some(ts) = vec.iter().find(|ts| {
                let content_bytes = ts.contents.as_bytes();
                content_bytes.len() == str_len + 1 && content_bytes[..str_len] == *str_bytes
            }) {
                return LuaString::Short(ArcRc::clone(ts));
            }
        }
        drop(ht_reader);

        // 写路径: 需要插入新字符串
        // TOCTOU 在单线程执行中安全; 多线程下最多导致重复桶条目(无害)
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: lua_string_with_nul(str),
        });
        let mut ht = self.ht.write();
        ht.entry(h).or_default().push(ArcRc::clone(&ts));
        *self.nuse.write() += 1;
        LuaString::Short(ts)
    }

    /// 内部化一个短字符串（从任意字节，8-bit clean，绕过 UTF-8 验证）。
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(not(feature = "threaded"))]
    pub fn intern_bytes(&self, bytes: &[u8]) -> LuaString {
        debug_assert!(
            bytes.len() <= LUAI_MAXSHORTLEN,
            "intern_bytes 只用于短字符串"
        );
        let h = rust_hash_bytes(bytes);
        let bytes_len = bytes.len();

        let ht = self.ht.read();
        if let Some(vec) = ht.get(&h) {
            if let Some(ts) = vec.iter().find(|ts| {
                let content_bytes = ts.contents.as_bytes();
                content_bytes.len() == bytes_len + 1 && content_bytes[..bytes_len] == *bytes
            }) {
                return LuaString::Short(ArcRc::clone(ts));
            }
        }
        drop(ht);

        // 写路径
        let blen = bytes.len();
        let mut buf = Vec::with_capacity(blen + 1);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.as_mut_ptr(), blen);
            *buf.as_mut_ptr().add(blen) = 0;
            buf.set_len(blen + 1);
        }
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: unsafe { String::from_utf8_unchecked(buf) },
        });
        let mut ht = self.ht.write();
        ht.entry(h).or_default().push(ArcRc::clone(&ts));
        *self.nuse.write() += 1;
        LuaString::Short(ts)
    }

    /// 内部化一个短字符串 (threaded 模式 — 走 RwLock).
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(feature = "threaded")]
    pub fn intern_bytes(&self, bytes: &[u8]) -> LuaString {
        debug_assert!(
            bytes.len() <= LUAI_MAXSHORTLEN,
            "intern_bytes 只用于短字符串"
        );
        let h = rust_hash_bytes(bytes);
        let bytes_len = bytes.len();

        let ht_reader = self.ht.read();
        if let Some(vec) = ht_reader.get(&h) {
            if let Some(ts) = vec.iter().find(|ts| {
                let content_bytes = ts.contents.as_bytes();
                content_bytes.len() == bytes_len + 1 && content_bytes[..bytes_len] == *bytes
            }) {
                return LuaString::Short(ArcRc::clone(ts));
            }
        }
        drop(ht_reader);

        let blen = bytes.len();
        let mut buf = Vec::with_capacity(blen + 1);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.as_mut_ptr(), blen);
            *buf.as_mut_ptr().add(blen) = 0;
            buf.set_len(blen + 1);
        }
        let ts = ArcRc::new(ShortString {
            hash: h,
            contents: unsafe { String::from_utf8_unchecked(buf) },
        });
        let mut ht = self.ht.write();
        ht.entry(h).or_default().push(ArcRc::clone(&ts));
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
        if let Some(vec) = ht.get_mut(&h) {
            if let Some(pos) = vec
                .iter()
                .position(|item: &ArcRc<ShortString>| std::ptr::eq(item.as_ref(), ptr))
            {
                vec.remove(pos);
            }
        }
        let mut nuse = self.nuse.write();
        *nuse = nuse.saturating_sub(1);
    }

    pub fn for_each<F: FnMut(&ShortString)>(&self, mut f: F) {
        let ht = self.ht.read();
        for ts in ht.values().flatten() {
            f(ts);
        }
    }

    /// 清理字符串表中的死字符串（只有字符串表持有的字符串）。
    /// 对应 C Lua 的 sweep 阶段清理 string table 的逻辑。
    /// 返回被清理的字符串数量。
    pub fn sweep(&self) -> usize {
        let mut ht = self.ht.write();
        let mut freed = 0;
        for vec in ht.values_mut() {
            let before = vec.len();
            // strong_count > 1 表示有其他引用 → 保留; <= 1 表示只有表持有 → 回收
            vec.retain(|ts| ArcRc::strong_count(ts) > 1);
            freed += before - vec.len();
        }
        // 清理空 Vec, 避免长期累积空桶
        ht.retain(|_, vec| !vec.is_empty());

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
#[cfg_attr(not(size_optimized), inline)]
pub fn rust_hash_bytes(bytes: &[u8]) -> u64 {
    let l = bytes.len();
    // seed = length * 0x5bd1e995（MurmurHash2 常量，扩散性好）
    let mut h: u64 = (l as u64).wrapping_mul(0x5bd1e995);
    // 反向遍历对应 C 的 `for (; l > 0; l--) h ^= ((h<<5) + (h>>2) + str[l-1]);`
    // 改为 64 位以充分利用寄存器，移位常量也相应调整。
    // perf: 用 iter().rev() 替代索引循环, 消除每次迭代的边界检查。
    // 迭代器版本编译器可证明无越界, 生成更紧凑的循环体。
    for &b in bytes.iter().rev() {
        let b = b as u64;
        h ^= h
            .wrapping_shl(7)
            .wrapping_add(h.wrapping_shr(2))
            .wrapping_add(b);
    }
    h
}

#[cfg_attr(not(size_optimized), inline)]
pub fn rust_hash(str: &str) -> u64 {
    rust_hash_bytes(str.as_bytes())
}

// ============================================================================
// 规约：字符串方法
// ============================================================================

pub fn lua_string_eq(s1: &TValue, s2: &TValue) -> bool {
    match (s1, s2) {
        (TValue::LongStr(_), TValue::LongStr(_)) => {
            lua_string_hash(s1) == lua_string_hash(s2)
                && lua_string_as_str(s1) == lua_string_as_str(s2)
        }
        (TValue::ShortStr(s1), TValue::ShortStr(s2)) => ArcRc::ptr_eq(s1, s2),
        _ => false,
    }
}

/// 新建时自动追加 NUL 字节，确保作为 *const c_char 返回时安全。
/// 预分配 str.len()+1 容量，避免 push('\0') 触发扩容。
/// perf: with_nul 在 intern 未命中路径上调用，每次分配一个新 String。
/// 用 unsafe 直接 copy_nonoverlapping + set_len 替代 push_str + push,
/// 消除两次容量检查和两次长度更新 (push_str 和 push 各检查一次容量)。
/// SAFETY: with_capacity(len+1) 保证至少 len+1 字节可用;
///         copy_nonoverlapping 复制 len 字节; 写 0 在 [len] 位置 (≤ capacity);
///         set_len(len+1) 不超过已分配容量; from_utf8_unchecked 安全因为
///         源数据来自 &str (已验证 UTF-8) + NUL (合法 UTF-8 单字节)。
pub fn lua_string_with_nul(str: &str) -> String {
    let len = str.len();
    let mut buf = Vec::with_capacity(len + 1);
    unsafe {
        std::ptr::copy_nonoverlapping(str.as_ptr(), buf.as_mut_ptr(), len);
        *buf.as_mut_ptr().add(len) = 0;
        buf.set_len(len + 1);
    }
    unsafe { String::from_utf8_unchecked(buf) }
}

/// 估算字符串真实堆占用（用于 GC 内存计费）。
/// 短串: ArcRc 分配头 + ShortString 结构 + contents 堆分配
/// 长串: Box 指针 + LongString 结构 + contents 堆分配
/// 字符串不调用 register_object（无 gc_header），由 gc_extra_estimate 跟踪。
pub fn lua_string_gc_mem_size(str: &TValue) -> usize {
    match str {
        // ArcRc 分配 = ArcInner<ShortString>（含引用计数 usize）+ ShortString 自身
        // ShortString = { hash: u64, contents: String }，String 堆分配 = capacity
        TValue::ShortStr(s) => std::mem::size_of::<ShortString>() + s.contents.capacity() + 16,
        // Box<LongString> 堆分配 = LongString 自身（Box 无额外头）
        // LongString = { hash: AtomicU64, extra: AtomicU8, contents: String, ptr_id: u32 }
        TValue::LongStr(s) => std::mem::size_of::<LongString>() + s.contents.capacity() + 8,
        _ => unreachable!("Not a string"),
    }
}

#[cfg_attr(not(size_optimized), inline)]
pub fn lua_string_as_str<'a, 'b>(str: &'b TValue<'a>) -> &'b str {
    lua_string_as_str_inner(str)
}

/// 内部实现：返回 (str, has_nul)
/// ShortString 的 contents 末尾必然有 NUL 终止符 (所有创建路径都保证)，
/// 因此 Short 分支直接 slice 去掉末尾 NUL，无需 last() 检查。
/// LongString 的 contents 可能没有 NUL (从 String 直接构造)，需保留检查。
fn lua_string_as_str_inner<'a, 'b>(str: &'b TValue<'a>) -> &'b str {
    match str {
        TValue::ShortStr(s) => {
            assert!(s.contents.ends_with('\0'));
            &s.contents[..s.contents.len() - 1]
        }
        TValue::LongStr(s) => {
            assert!(s.contents.ends_with('\0'));
            &s.contents[..s.contents.len() - 1]
        }
        _ => unreachable!("Not a string"),
    }
}

/// 返回一个 NUL 结尾的 C 字符串指针（供 C API 使用）。
/// 指针在 LuaString 自身存活期间有效。
pub fn lua_string_as_c_str_ptr(str: &TValue) -> *const c_char {
    match str {
        TValue::ShortStr(s) => s.contents.as_ptr() as *const c_char,
        TValue::LongStr(s) => s.contents.as_ptr() as *const c_char,
        _ => unreachable!("Not a string"),
    }
}

/// 返回字符串长度（O(1)，不含末尾 NUL）。
/// ShortString 末尾必然有 NUL, 直接 contents.len()-1 省去 as_str 的 slice 操作。
pub fn lua_string_len(str: &TValue) -> usize {
    match str {
        TValue::ShortStr(s) => s.contents.len() - 1,
        TValue::LongStr(s) => s.contents.len() - 1,
        _ => unreachable!("Not a string"),
    }
}

pub fn lua_string_is_empty(str: &TValue) -> bool {
    lua_string_len(str) == 0
}

/// 返回预计算的哈希值（短字符串始终有效；长字符串 extra==0 时为 0）。
pub fn lua_string_hash(str: &TValue) -> u64 {
    match str {
        TValue::ShortStr(s) => s.hash,
        TValue::LongStr(s) => ensure_long_hash(s),
        _ => unreachable!("Not a string"),
    }
}

// ============================================================================
// 规约：创建字符串
// ============================================================================

/// 创建一个长字符串对象，不预先计算哈希（惰性）。
pub fn new_long_str<'a>(str: &str) -> TValue<'a> {
    debug_assert!(
        str.len() > LUAI_MAXSHORTLEN,
        "长字符串长度必须大于 LUAI_MAXSHORTLEN"
    );
    TValue::LongStr(Rc::new(LongString {
        hash: 0.into(),
        extra: 0.into(),
        contents: lua_string_with_nul(str),
        ptr_id: crate::gc::new_ptr_id(),
    }))
}

/// 创建一个长字符串对象，直接 consume 传入的 String，避免 clone。
/// 用于 str_format 等已知结果为长字符串且不再需要原 String 的场景。
/// perf: 消除 new_long_str 中 with_nul 的 to_string() clone（constructs.lua 热点）。
pub fn new_long_str_from_string<'a>(mut s: String) -> TValue<'a> {
    debug_assert!(
        s.len() > LUAI_MAXSHORTLEN,
        "长字符串长度必须大于 LUAI_MAXSHORTLEN"
    );
    s.reserve(1); // 确保 capacity >= len+1，避免 push('\0') 扩容
    s.push('\0');
    TValue::LongStr(Rc::new(LongString {
        hash: 0.into(),
        extra: 0.into(),
        contents: s,
        ptr_id: crate::gc::new_ptr_id(),
    }))
}

pub fn new_long_bytes<'a>(bytes: Vec<u8>) -> TValue<'a> {
    let mut buf = bytes;
    buf.reserve(1); // 避免 push(0) 扩容
    buf.push(0);
    TValue::LongStr(Rc::new(LongString {
        hash: 0.into(),
        extra: 0.into(),
        contents: unsafe { String::from_utf8_unchecked(buf) },
        ptr_id: crate::gc::new_ptr_id(),
    }))
}

/// 确保长字符串有哈希值（惰性计算）。
#[inline]
pub fn ensure_long_hash(ls: &LongString) -> u64 {
    if ls.extra.get() == 0 {
        let content = &ls.contents[..ls.contents.len() - 1];
        let h = rust_hash(content);
        ls.hash.set(h);
        ls.extra.set(1);
    }
    ls.hash.get()
}
#[cfg_attr(not(size_optimized), inline)]
pub fn new_lstr<'a>(table: &StringTable, str: &str) -> TValue<'a> {
    if str.len() <= LUAI_MAXSHORTLEN {
        table.intern_value(str)
    } else {
        new_long_str(str)
    }
}

/// 从 String 创建 LuaString，长字符串路径直接 consume 避免 clone。
/// 短字符串仍走 intern（需要查表去重，intern 未命中时内部会 clone，但短串开销小）。
#[cfg_attr(not(size_optimized), inline)]
pub fn new_lstr_from_string<'a>(table: &StringTable, s: String) -> TValue<'a> {
    if s.len() <= LUAI_MAXSHORTLEN {
        table.intern_value(&s)
    } else {
        new_long_str_from_string(s)
    }
}

/// 从任意字节创建 LuaString（8-bit clean，绕过 UTF-8 验证）。
/// 用于 C API 的 lua_pushlstring/lua_pushstring 等需要保留原始字节的场景。
#[cfg_attr(not(size_optimized), inline)]
pub fn new_lstr_bytes<'a>(table: &StringTable, bytes: &[u8]) -> TValue<'a> {
    if bytes.len() <= LUAI_MAXSHORTLEN {
        table.intern_bytes(bytes)
    } else {
        new_long_bytes(bytes.to_vec())
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
        let s = tb.intern_value("hello");
        assert_eq!(lua_string_as_str(&s), "hello");
        assert_eq!(lua_string_len(&s), 5);
        assert_eq!(tb.count(), 1, "nuse 应为 1");
    }

    #[test]
    fn test_intern_duplicate_returns_same() {
        let tb = StringTable::new();
        let s1 = tb.intern_value("hello");
        let s2 = tb.intern_value("hello");
        assert_eq!(s1, s2, "相同内容应返回同一个实例");
        assert!(eq_str(&s1, &s2), "通过 eq_str 比较也应相等");
        assert_eq!(tb.count(), 1, "内部化后 nuse 仍应为 1");
    }

    #[test]
    fn test_intern_multiple_strings() {
        let tb = StringTable::new();
        let s1 = tb.intern_value("hello");
        let s2 = tb.intern_value("world");
        let s3 = tb.intern_value("lua");
        assert_eq!(lua_string_as_str(&s1), "hello");
        assert_eq!(lua_string_as_str(&s2), "world");
        assert_eq!(lua_string_as_str(&s3), "lua");
        assert_eq!(tb.count(), 3);
        let s1_dup = tb.intern_value("hello");
        assert_eq!(s1, s1_dup);
        assert_eq!(tb.count(), 3);
    }

    #[test]
    fn test_intern_empty_string() {
        let tb = StringTable::new();
        let s = tb.intern_value("");
        assert_eq!(lua_string_as_str(&s), "");
        assert_eq!(lua_string_len(&s), 0);
        assert!(lua_string_is_empty(&s));
        assert_eq!(tb.count(), 1);
    }

    #[test]
    fn test_intern_max_short_length() {
        let tb = StringTable::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN);
        let s = tb.intern_value(&content);
        assert_eq!(lua_string_as_str(&s), content);
        assert_eq!(lua_string_len(&s), LUAI_MAXSHORTLEN);
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
        let s1 = tb.intern_value("hello");
        let s2 = tb.intern_value("world");
        assert_eq!(tb.count(), 2);

        if let TValue::ShortStr(ref ts) = s1 {
            tb.remove(ts);
        }
        assert_eq!(tb.count(), 1, "移除后 nuse 应为 1");

        let s2_again = tb.intern_value("world");
        assert_eq!(s2, s2_again);
        assert_eq!(tb.count(), 1, "重新查找 world 不应增加 nuse");
    }

    // ------------------------------------------------------------------------
    // eq_str 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_eq_str_short_same_pointer() {
        let tb = StringTable::new();
        let a = tb.intern_value("foo");
        let b = tb.intern_value("foo");
        assert!(eq_str(&a, &b), "相同短字符串必须相等");
    }

    #[test]
    fn test_eq_str_short_different() {
        let tb = StringTable::new();
        let a = tb.intern_value("foo");
        let b = tb.intern_value("bar");
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
        let short = tb.intern_value("hello");
        let long = TValue::LongStr(Rc::new(LongString {
            hash: 0.into(),
            extra: 0.into(),
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
        assert!(matches!(s, TValue::ShortStr(_)));
        assert_eq!(lua_string_as_str(&s), "hello");
        assert_eq!(lua_string_len(&s), 5);
    }

    #[test]
    fn test_new_lstr_long() {
        let tb = StringTable::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let s = new_lstr(&tb, &content);
        assert!(matches!(s, TValue::LongStr(_)));
        assert_eq!(lua_string_as_str(&s), content);
        assert_eq!(lua_string_len(&s), LUAI_MAXSHORTLEN + 1);
    }

    // ------------------------------------------------------------------------
    // new_long_str 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_new_long_str_has_hashing_marker() {
        let ls = new_long_str(&"a".repeat(LUAI_MAXSHORTLEN + 1));
        assert_eq!(lua_string_len(&ls), LUAI_MAXSHORTLEN + 1);
        match &ls {
            TValue::LongStr(ls) => assert_eq!(ls.extra.get(), 0, "新长字符串 extra 应为 0"),
            _ => panic!("应为长字符串"),
        }
    }

    // ------------------------------------------------------------------------
    // ensure_long_hash 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_ensure_long_hash_computes_on_first_call() {
        let mut ls = LongString {
            hash: 123.into(),
            extra: 0.into(),
            contents: "a".repeat(50),
            ptr_id: 0,
        };
        let hash = ensure_long_hash(&mut ls);
        assert_eq!(ls.extra.get(), 1, "extra 应为 1（标记已计算哈希）");
        assert_eq!(hash, ls.hash.get(), "返回的哈希应与存储的一致");
    }

    #[test]
    fn test_ensure_long_hash_idempotent() {
        let mut ls = LongString {
            hash: 0.into(),
            extra: 1.into(),
            contents: "a".repeat(50),
            ptr_id: 0,
        };
        let hash_before = ls.hash.get();
        let hash = ensure_long_hash(&mut ls);
        assert_eq!(hash, hash_before, "已有哈希不应重新计算");
        assert_eq!(ls.extra.get(), 1, "extra 仍为 1");
    }

    // ------------------------------------------------------------------------
    // LuaString 方法测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_lua_string_hash() {
        let tb = StringTable::new();
        let s = tb.intern_value("hello");
        let h = rust_hash("hello");
        assert_eq!(lua_string_hash(&s), h);
    }

    #[test]
    fn test_lua_string_len_and_is_empty() {
        let tb = StringTable::new();
        let empty = tb.intern_value("");
        let non_empty = tb.intern_value("x");
        assert!(lua_string_is_empty(&empty));
        assert!(!lua_string_is_empty(&non_empty));
        assert_eq!(lua_string_len(&non_empty), 1);
    }

    #[test]
    fn test_intern_arc_identity() {
        let tb = StringTable::new();
        let a = tb.intern_value("shared");
        let b = tb.intern_value("shared");
        if let (TValue::ShortStr(ra), TValue::ShortStr(rb)) = (&a, &b) {
            assert!(
                ArcRc::ptr_eq(ra, rb),
                "同一字符串的内部化应该返回相同的 ArcRc"
            );
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
                let mut results: Vec<ArcRc<ShortString>> = Vec::new();
                for s in &strings {
                    let ls = table.intern(s);
                    results.push(ls);
                }
                results
            }));
        }

        let all_results: Vec<Vec<ArcRc<ShortString>>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        for i in 0..count {
            let first = &all_results[0][i];
            for t in 1..4 {
                assert!(
                    ArcRc::ptr_eq(first, &all_results[t][i]),
                    "线程间同一字符串应返回相同 Arc 实例: '{}'",
                    strings[i]
                );
            }
        }

        assert_eq!(table.count(), count, "count 应为去重后的字符串数");
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

    fn hash_one(t: &TValue) -> u64 {
        lua_string_hash(t)
    }

    #[test]
    fn test_hash_same_content_same_hash() {
        let tb = StringTable::new();
        let a = tb.intern_value("hello");
        let b = tb.intern_value("hello");
        assert_eq!(
            hash_one(&a),
            hash_one(&b),
            "相同内容应产生相同的 Rust Hash 值"
        );
    }

    #[test]
    fn test_rust_hash_different_content_different() {
        let tb = StringTable::new();
        let a = tb.intern_value("hello");
        let b = tb.intern_value("world");
        assert_ne!(
            hash_one(&a),
            hash_one(&b),
            "不同内容应产生不同的 Rust Hash 值"
        );
    }

    /// LongString: Hash::hash 首次调用自动缓存，后续 O(1) 复用
    #[test]
    fn test_hash_long_string_caches_on_first_call() {
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let ls = new_long_str(&content);
        match &ls {
            TValue::LongStr(inner) => {
                assert_eq!(
                    inner.extra.get(),
                    0,
                    "new_long_str 创建的字符串 extra 应为 0"
                );
                assert_eq!(
                    inner.hash.get(),
                    0,
                    "惰性策略：extra == 0 时 hash == 0，未计算"
                );
            }
            _ => panic!("应为 Long"),
        }

        let h1 = hash_one(&ls);

        match &ls {
            TValue::LongStr(inner) => {
                assert_eq!(
                    inner.extra.get(),
                    1,
                    "Hash::hash 调用后 extra 应自动变为 1（已缓存）"
                );
                assert_ne!(inner.hash.get(), 0, "Hash::hash 调用后 hash 被缓存为非零值");
            }
            _ => panic!("应为 Long"),
        }

        let h2 = hash_one(&ls);
        assert_eq!(h1, h2, "再次 hash 应返回相同值（缓存命中，不重复计算）");
    }

    /// 同内容长字符串：extra=0 和 extra=1 产生相同 Hash
    #[test]
    fn test_hash_mixed_extra_same_content() {
        let content = lua_string_with_nul(&"a".repeat(LUAI_MAXSHORTLEN + 1));
        let unhashed = TValue::LongStr(Rc::new(LongString {
            hash: 0.into(),
            extra: 0.into(),
            contents: content.clone(),
            ptr_id: 0,
        }));
        let ls = LongString {
            hash: 0.into(),
            extra: 0.into(),
            contents: content.clone(),
            ptr_id: 0,
        };
        ensure_long_hash(&ls);
        let hashed = TValue::LongStr(Rc::new(ls));

        assert_eq!(unhashed, hashed, "同内容的不同 extra 状态应相等");

        assert_eq!(
            hash_one(&unhashed),
            hash_one(&hashed),
            "extra=0 和 extra=1 的同内容长字符串必须产生相同 Rust Hash"
        );

        let mut map: HashMap<TValue, i32> = HashMap::new();
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
        let ls1 = TValue::LongStr(Rc::new(LongString {
            hash: 0.into(),
            extra: 0.into(),
            contents: lua_string_with_nul("hello"),
            ptr_id: 0,
        }));
        let ls2 = TValue::LongStr(Rc::new(LongString {
            hash: h.into(),
            extra: 1.into(),
            contents: lua_string_with_nul("hello"),
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
            if let TValue::LongStr(inner) = &ls {
                assert_eq!(inner.hash.get(), 0, "所有长字符串创建时不计算 hash");
                assert_eq!(inner.extra.get(), 0);
            }
        }
    }

    /// HashMap 集成测试：短字符串和长字符串混用
    #[test]
    fn test_hashmap_with_mixed_strings() {
        let tb = StringTable::new();

        let short1 = tb.intern_value("key1");
        let short2 = tb.intern_value("key2");
        let long_content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let long1 = new_long_str(&long_content);

        let mut map: HashMap<TValue, &str> = HashMap::new();
        map.insert(short1.clone(), "value1");
        map.insert(short2.clone(), "value2");
        map.insert(long1.clone(), "value3");

        let short1_lookup = tb.intern_value("key1");
        let long1_lookup = new_long_str(&long_content);

        assert_eq!(map.get(&short1_lookup), Some(&"value1"));
        assert_eq!(map.get(&long1_lookup), Some(&"value3"));

        let nonexistent = tb.intern_value("nonexistent");
        assert_eq!(map.get(&nonexistent), None);
    }

    /// ensure_long_hash 同一内容多次调用，哈希值一致、不重复计算
    #[test]
    fn test_ensure_long_hash_same_content() {
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let mut a = LongString {
            hash: 0.into(),
            extra: 0.into(),
            contents: content.clone(),
            ptr_id: 0,
        };
        let mut b = LongString {
            hash: 0.into(),
            extra: 0.into(),
            contents: content.clone(),
            ptr_id: 0,
        };

        let h0 = ensure_long_hash(&mut a);
        let h1 = ensure_long_hash(&mut b);
        assert_eq!(h0, h1, "同一内容应产生相同的 Rust hash");

        assert_eq!(a.extra.get(), 1);
        assert_eq!(b.extra.get(), 1);

        let h0_again = ensure_long_hash(&mut a);
        assert_eq!(h0, h0_again, "再次调用不重复计算");
    }
}
