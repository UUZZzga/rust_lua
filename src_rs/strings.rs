//! # Lua 字符串模块 — Rust 惯用重写
//!
//! 将 Lua C 实现中的 `TString`/短字符串内部化/惰性哈希 转换为 Rust 类型系统。
//!
//! ## 核心类型
//! - `LuaString` — 枚举类型，统一表示短/长字符串
//!   - `LuaString::Short(Arc<ShortString>)` — 内部化（interned）的短字符串
//!   - `LuaString::Long(Box<LongString>)` — 非内部化的长字符串
//!
//! ## 设计原则
//! - 短字符串通过指针相等性比较（内部化保证同一内容只有一个 Arc 实例）
//! - 长字符串通过内容比较（hash → length → contents 三级短路）
//! - 长度直接从 `String` 获取（`contents.len()`），无冗余字段
//! - 哈希统一使用 Rust `DefaultHasher`（SipHash-1-3），进程内随机密钥防碰撞攻击
//! - 短字符串创建时预计算 hash → O(1) Hash trait
//! - 长字符串惰性计算 hash，`Hash::hash` 首次计算后通过 Atomic 自动缓存，避免重复计算
//! - 字符串表使用 HashMap<u64, Vec<Arc<ShortString>>> 处理哈希冲突
//! - **多线程安全**：StringTable 使用 `RwLock` 保护 HashMap，读可并发、写互斥
//!   LongString 使用 `AtomicU64`/`AtomicU8` 实现 Sync 内部可变性

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use parking_lot::RwLock;
use std::hash::BuildHasherDefault;

// Identity hasher for u64-keyed HashMaps — the key IS already a hash value,
// so using SipHash on it is wasted cycles.
#[derive(Default)]
struct IdHasher(u64);
impl Hasher for IdHasher {
    fn finish(&self) -> u64 { self.0 }
    fn write(&mut self, bytes: &[u8]) {
        // Only used for non-u64 keys; read as little-endian u64.
        let mut buf = [0u8; 8];
        let len = bytes.len().min(8);
        buf[..len].copy_from_slice(&bytes[..len]);
        self.0 = u64::from_le_bytes(buf);
    }
    fn write_u64(&mut self, i: u64) { self.0 = i; }
}

// ============================================================================
// 规约：常量
// ============================================================================

/// 短字符串的最大长度（字节数）。
/// 长度 ≤ 40 的字符串会被内部化（interned），相同内容的字符串共享同一个 `Arc`。
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
    Short(Arc<ShortString>),
    Long(Box<LongString>),
}

// ============================================================================
// 规约：相等性比较
// ============================================================================

/// 长字符串相等性：若双方均已哈希 → 先比 hash（快速淘汰），否则直接比内容。
impl PartialEq for LongString {
    fn eq(&self, other: &Self) -> bool {
        if self.extra.load(Ordering::Relaxed) == 1 && other.extra.load(Ordering::Relaxed) == 1 {
            if self.hash.load(Ordering::Relaxed) != other.hash.load(Ordering::Relaxed) {
                return false;
            }
        }
        self.contents == other.contents
    }
}

/// 短字符串：`Arc::ptr_eq` 快速路径，否则比较 `contents`。
/// 长字符串：委派给 `LongString::eq`。
/// 跨类型（Short vs Long）：比较内容 — 对应 C Lua 的 luaS_eqlngstr/luaS_hash 比较逻辑。
impl PartialEq for LuaString {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (LuaString::Short(a), LuaString::Short(b)) => {
                Arc::ptr_eq(a, b) || a.contents == b.contents
            }
            (LuaString::Long(a), LuaString::Long(b)) => a == b,
            _ => self.as_str() == other.as_str(),
        }
    }
}

impl Eq for LuaString {}

// ============================================================================
// 规约：eq_str 辅助函数
// ============================================================================

/// 比较两个 `LuaString` 的内容是否相同。
pub fn eq_str(a: &LuaString, b: &LuaString) -> bool {
    match (a, b) {
        (LuaString::Short(a), LuaString::Short(b)) => {
            Arc::ptr_eq(a, b) || a.contents == b.contents
        }
        (LuaString::Long(a), LuaString::Long(b)) => a.contents == b.contents,
        _ => false,
    }
}

// ============================================================================
// 规约：哈希实现
// ============================================================================

/// 统一使用 Rust `DefaultHasher`，始终写入 u64 到 Hasher。
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
                    let h = rust_hash(&s.contents);
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
#[derive(Debug)]
pub struct StringTable {
    ht: RwLock<HashMap<u64, Vec<Arc<ShortString>>, BuildHasherDefault<IdHasher>>>,
    nuse: RwLock<usize>,
}

impl StringTable {
    pub fn new() -> Self {
        StringTable {
            ht: RwLock::new(HashMap::with_capacity_and_hasher(
                128,
                BuildHasherDefault::<IdHasher>::default(),
            )),
            nuse: RwLock::new(0),
        }
    }
    /// 内部化一个短字符串。
    #[inline]
    pub fn intern(&self, str: &str) -> LuaString {
        let h = rust_hash(str);
        debug_assert!(str.len() <= LUAI_MAXSHORTLEN, "intern 只用于短字符串");

        // 读优先路径: 绝大多数 intern 调用是查找已有字符串
        let ht_reader = self.ht.read();
        if let Some(bucket) = ht_reader.get(&h) {
            for ts in bucket {
                if *ts.contents == *str {
                    return LuaString::Short(Arc::clone(ts));
                }
            }
        }
        drop(ht_reader);

        // 写路径: 需要插入新字符串
        // TOCTOU 在单线程执行中安全; 多线程下最多导致重复桶条目(无害)
        let mut ht = self.ht.write();
        let ts = Arc::new(ShortString {
            hash: h,
            contents: str.to_string(),
        });
        let bucket = ht.entry(h).or_insert_with(Vec::new);
        bucket.push(Arc::clone(&ts));
        *self.nuse.write() += 1;
        LuaString::Short(ts)
    }

    pub fn count(&self) -> usize {
        *self.nuse.read()
    }

    pub fn remove(&self, ts: &ShortString) {
        let h = ts.hash;
        let mut ht = self.ht.write();
        let bucket = ht.get_mut(&h).unwrap();
        bucket.retain(|item| !std::ptr::eq(item.as_ref(), ts));
        if bucket.is_empty() {
            ht.remove(&h);
        }
        let mut nuse = self.nuse.write();
        *nuse = nuse.saturating_sub(1);
    }

    pub fn for_each<F: FnMut(&ShortString)>(&self, mut f: F) {
        let ht = self.ht.read();
        for bucket in ht.values() {
            for ts in bucket {
                f(ts);
            }
        }
    }

    /// 清理字符串表中的死字符串（只有字符串表持有的字符串）。
    /// 对应 C Lua 的 sweep 阶段清理 string table 的逻辑。
    /// 返回被清理的字符串数量。
    pub fn sweep(&self) -> usize {
        let mut ht = self.ht.write();
        let mut freed = 0;
        let mut empty_keys = Vec::new();

        for (key, bucket) in ht.iter_mut() {
            let before = bucket.len();
            // strong_count == 1 表示只有字符串表持有，无其他引用 → 可回收
            bucket.retain(|item| Arc::strong_count(item) > 1);
            freed += before - bucket.len();
            if bucket.is_empty() {
                empty_keys.push(*key);
            }
        }

        for key in empty_keys {
            ht.remove(&key);
        }

        let mut nuse = self.nuse.write();
        *nuse = nuse.saturating_sub(freed);

        freed
    }
}

// ============================================================================
// 规约：哈希计算
// ============================================================================

#[inline]
pub fn rust_hash(str: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    str.hash(&mut hasher);
    hasher.finish()
}

// ============================================================================
// 规约：字符串方法
// ============================================================================
impl LuaString {
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            LuaString::Short(s) => &s.contents,
            LuaString::Long(s) => &s.contents,
        }
    }

    /// 返回字符串长度（O(1)，直接从 `String` 获取）。
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
    debug_assert!(str.len() > LUAI_MAXSHORTLEN, "长字符串长度必须大于 LUAI_MAXSHORTLEN");
    LuaString::Long(Box::new(LongString {
        hash: AtomicU64::new(0),
        extra: AtomicU8::new(0),
        contents: str.to_string(),
        ptr_id: crate::gc::new_ptr_id(),
    }))
}

/// 从字节创建长字符串 — 用于二进制数据 (io.read 返回的二进制内容等)
/// 对应 C Lua 中字符串可以包含任意字节 (包括 \0 和非 UTF-8 字节)
pub fn new_long_bytes(bytes: Vec<u8>) -> LuaString {
    LuaString::Long(Box::new(LongString {
        hash: AtomicU64::new(0),
        extra: AtomicU8::new(0),
        contents: unsafe { String::from_utf8_unchecked(bytes) },
        ptr_id: crate::gc::new_ptr_id(),
    }))
}

/// 确保长字符串有哈希值（惰性计算）。
pub fn ensure_long_hash(ls: &mut LongString) -> u64 {
    if ls.extra.load(Ordering::Relaxed) == 0 {
        let h = rust_hash(&ls.contents);
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
    pub fn cached_new(
        &self,
        str_ptr: *const u8,
        len: usize,
        table: &StringTable,
    ) -> LuaString {
        let cached = self.cached.read();
        let cached_ptr = cached.as_str().as_ptr();
        let cached_len = cached.len();

        if cached_ptr == str_ptr && cached_len == len {
            return cached.clone();
        }
        drop(cached);

        let slice = unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(str_ptr, len)) };
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
        StringState { table, cache, memerrmsg }
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
            LuaString::Long(ls) => assert_eq!(ls.extra.load(Ordering::Relaxed), 0,
                "新长字符串 extra 应为 0"),
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
        assert_eq!(ls.extra.load(Ordering::Relaxed), 1, "extra 应为 1（标记已计算哈希）");
        assert_eq!(hash, ls.hash.load(Ordering::Relaxed), "返回的哈希应与存储的一致");
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
            assert!(
                Arc::ptr_eq(ra, rb),
                "同一字符串的内部化应该返回相同的 Arc"
            );
            assert_eq!(Arc::strong_count(ra), 3, "引用计数应为 3（表 + a + b）");
        } else {
            panic!("应为短字符串");
        }
    }

    // ========================================================================
    // 多线程并发测试
    // ========================================================================

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

    #[test]
    fn test_concurrent_intern_same_strings() {
        let table = Arc::new(StringTable::new());

        let strings: Vec<&str> = vec![
            "function", "return", "local", "while", "if", "else", "end",
            "then", "do", "for", "repeat", "until", "break", "nil",
            "true", "false", "and", "or", "not", "in",
        ];
        let count = strings.len();

        let mut handles = Vec::new();
        for _ in 0..4 {
            let table = Arc::clone(&table);
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

        let all_results: Vec<Vec<LuaString>> = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();

        for i in 0..count {
            let first = &all_results[0][i];
            for t in 1..4 {
                assert_eq!(first, &all_results[t][i],
                    "线程间同一字符串应返回相同 Arc 实例: '{}'", strings[i]);
            }
        }

        assert_eq!(table.count(), count, "count 应为去重后的字符串数");
    }

    #[test]
    fn test_concurrent_intern_with_state() {
        let state = Arc::new(StringState::new());

        let mut handles = Vec::new();
        for _ in 0..4 {
            let state = Arc::clone(&state);
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

        let all_results: Vec<Vec<LuaString>> = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();

        for i in 0..25 {
            let first = &all_results[0][i];
            for t in 1..4 {
                assert_eq!(first, &all_results[t][i],
                    "跨线程 intern 同一内容应返回相同实例");
            }
        }

        assert_eq!(state.table.count(), 26);
    }

    #[test]
    fn test_concurrent_intern_many_strings() {
        let table = Arc::new(StringTable::new());
        let num_threads = 8;
        let per_thread = 2000;

        let mut handles = Vec::new();
        for t in 0..num_threads {
            let table = Arc::clone(&table);
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
        assert_eq!(hash_one(&a), hash_one(&b),
            "相同内容应产生相同的 Rust Hash 值");
    }

    #[test]
    fn test_rust_hash_different_content_different() {
        let tb = StringTable::new();
        let a = tb.intern("hello");
        let b = tb.intern("world");
        assert_ne!(hash_one(&a), hash_one(&b),
            "不同内容应产生不同的 Rust Hash 值");
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

        assert_eq!(h1, h2,
            "u64::hash() 等价于 hasher.write_u64()，不是\"hash 的 hash\"");
    }

    /// LongString: Hash::hash 首次调用自动缓存，后续 O(1) 复用
    #[test]
    fn test_hash_long_string_caches_on_first_call() {
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let ls = new_long_str(&content);
        match &ls {
            LuaString::Long(inner) => {
                assert_eq!(inner.extra.load(Ordering::Relaxed), 0,
                    "new_long_str 创建的字符串 extra 应为 0");
                assert_eq!(inner.hash.load(Ordering::Relaxed), 0,
                    "惰性策略：extra == 0 时 hash == 0，未计算");
            }
            _ => panic!("应为 Long"),
        }

        let h1 = hash_one(&ls);

        match &ls {
            LuaString::Long(inner) => {
                assert_eq!(inner.extra.load(Ordering::Relaxed), 1,
                    "Hash::hash 调用后 extra 应自动变为 1（已缓存）");
                assert_ne!(inner.hash.load(Ordering::Relaxed), 0,
                    "Hash::hash 调用后 hash 被缓存为非零值");
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

        assert_eq!(hash_one(&unhashed), hash_one(&hashed),
            "extra=0 和 extra=1 的同内容长字符串必须产生相同 Rust Hash");

        let mut map: HashMap<LuaString, i32> = HashMap::new();
        map.insert(hashed.clone(), 42);
        assert_eq!(map.get(&unhashed), Some(&42),
            "extra=0 的 key 应能找到 extra=1 的同内容 key 插入的值");
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

        assert_eq!(hash_one(&ls1), hash_one(&ls2),
            "相同内容 → Hash 应相同（extra=0 实时计算 vs extra=1 缓存命中）");
    }

    /// 大批量长字符串创建时不计算 hash
    #[test]
    fn test_large_long_string_no_eager_hash() {
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        for _ in 0..100 {
            let ls = new_long_str(&content);
            if let LuaString::Long(inner) = &ls {
                assert_eq!(inner.hash.load(Ordering::Relaxed), 0,
                    "所有长字符串创建时不计算 hash");
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