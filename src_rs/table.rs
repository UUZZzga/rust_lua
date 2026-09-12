//! Lua 表实现 (基于 `crate::objects::Table`)
//!
//! `Table` 的定义在 [crate::objects] 中；此处包含所有方法实现及测试。
//!
//! ## 设计原则
//! - 数组部分：`Vec<TValue>`，1-based，空槽存储 `Nil(Empty)`
//! - 哈希部分：`Vec<(TValue, TValue)>` (hash_buckets) 按插入顺序存储，(key, value) 对
//! - 用 `key_to_bucket: HashMap<TValue, usize>` 做 O(1) 查找
//! - 数据通过 `Rc<RefCell<TableData>>` 共享，克隆 Table 共享同一份数据
//! - `LuaTable` 封装为未来元方法支持预留接口

use crate::gc::GCObjectHeader;
use crate::objects::{NilKind, TValue, TableData};
use std::cell::RefCell;
use std::rc::Rc;

pub use crate::objects::Table;

/// ceil(log2(x)) — 对应 C 的 luaO_ceillog2
///
/// x=0→0, x=1→0, x=2→1, x=3→2, x=4→2, x=5→3 ...
fn ceillog2(x: u64) -> u32 {
    if x <= 1 {
        return 0;
    }
    (x - 1).ilog2() + 1
}
/// 表哈希查找 — TableData 上直接单跳探测 (key_to_bucket 索引)。
/// VM opcode 热路径 (op_getfield 等) 专用; 返回值 clone。
#[cfg_attr(not(size_optimized), inline)]
pub(crate) fn hash_get(td: &TableData, key: &TValue) -> Option<TValue> {
    if let Some(idx) = td.idx_get(key) {
        let v = &td.hash_buckets[idx].1;
        if !matches!(v, TValue::Nil(NilKind::Empty)) {
            return Some(v.clone());
        }
    }
    None
}
// ============================================================================
// Table 方法实现
// ============================================================================

impl Table {
    pub fn new() -> Self {
        Table {
            data: Rc::new(RefCell::new(TableData {
                gc_header: GCObjectHeader::new(),
                array: Vec::new(),
                hash_buckets: Vec::new(),
                key_to_bucket: None,
                metatable: None,
            })),
        }
    }

    pub fn with_capacity(narray: usize, nhash: usize) -> Self {
        Table {
            data: Rc::new(RefCell::new(TableData {
                gc_header: GCObjectHeader::new(),
                array: (0..narray).map(|_| TValue::Nil(NilKind::Empty)).collect(),
                hash_buckets: Vec::with_capacity(nhash),
                key_to_bucket: if nhash > 0 {
                    #[cfg(not(size_optimized))]
                    {
                        Some(Box::new(hashbrown::HashTable::with_capacity(nhash)))
                    }
                    #[cfg(size_optimized)]
                    {
                        Some(Box::new(crate::objects::TableHashMap::with_capacity_and_hasher(
                            nhash,
                            crate::objects::FxBuildHasher::default(),
                        )))
                    }
                } else {
                    None
                },
                metatable: None,
            })),
        }
    }

    pub fn array_size(&self) -> usize {
        self.data.borrow().array.len()
    }
    pub fn hash_size(&self) -> usize {
        self.data
            .borrow()
            .hash_buckets
            .iter()
            .filter(|(_, v)| !matches!(v, TValue::Nil(NilKind::Empty)))
            .count()
    }

    // ========================================================================
    // 元表访问器
    // ========================================================================

    /// 获取元表的共享引用（克隆 Table，仅增加 Rc 引用计数，开销极小）。
    pub fn get_metatable(&self) -> Option<Table> {
        self.data.borrow().metatable.as_ref().map(|b| (**b).clone())
    }

    /// 按元方法名直查元表 (安全借用版, 零临时 clone) — 返回值引用。
    ///
    /// 用 Ref::filter_map 把 RefCell 借用收窄到命中的值槽上:
    /// guard 全程持有 (无 unsafe, 无 data_ro), 返回 Ref<TValue>。
    /// 查找本身用 LuaString 直查公式 (免构造 TValue 键的 Str clone)。
    /// 未命中 (含 nil tombstone / 无元表) 返回 None。
    /// 供 get_tm_by_obj 热路径使用。
    #[cfg(not(size_optimized))]
    pub fn get_tm_ref(
        &self,
        key: &crate::strings::LuaString,
    ) -> Option<std::cell::Ref<'_, TValue>> {
        let guard = self.data.borrow();
        let r = std::cell::Ref::filter_map(guard, |d| {
            let ktb = d.key_to_bucket.as_ref()?;
            const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
            let sh = match key {
                crate::strings::LuaString::Short(ss) => ss.hash,
                _ => {
                    use std::hash::{Hash, Hasher};
                    let mut h = crate::objects::fx_hash_impl::FxHasherPub::default();
                    Hash::hash(key, &mut h);
                    h.finish()
                }
            };
            let hash = (5u64.wrapping_mul(SEED).rotate_left(5) ^ sh).wrapping_mul(SEED);
            let (_, idx) = ktb.find(hash, |(k, _)| matches!(k, TValue::Str(ks) if ks == key))?;
            let v = &d.hash_buckets[*idx].1;
            // C: notm — nil (含 Empty tombstone) 视为无元方法
            if v.is_nil() {
                None
            } else {
                Some(v)
            }
        });
        r.ok()
    }


    /// 设置元表。
    pub fn set_metatable(&self, mt: Option<Table>) {
        self.data.borrow_mut().metatable = mt.map(Box::new);
    }

    /// 是否有元表。
    pub fn has_metatable(&self) -> bool {
        self.data.borrow().metatable.is_some()
    }

    // ========================================================================
    // get / get_int —— 返回 owned TValue（RefCell 无法返回引用）
    // ========================================================================

    /// 获取 TableData 的裸引用 — 读路径专用 (绕过 RefCell borrow 计数)。
    ///
    /// 安全性: 仅限"同步读、不重入、不触发元方法/GC/写操作"的路径
    /// (get/hash_get/边界计算)。此类路径在函数返回前完成全部读取,
    /// 期间不可能发生 borrow_mut (VM 单线程, 无重入点)。
    /// Miri Tree Borrows 验证: as_ptr + 只读访问 + 立即结束生命周期,
    /// 与正常 borrow 等价但省去 borrow flag 读写 (各 1 次依赖加载 + 分支)。
    #[cfg_attr(not(size_optimized), inline(always))]
    pub(crate) fn data_ro(&self) -> &TableData {
        unsafe { &*self.data.as_ptr() }
    }

    /// 一次 borrow 内同时查找值并返回元表 — 消除 metamethod 路径的第二次 borrow。
    /// 返回 (查找结果, 元表)。元表为 Some 时调用方无需再次 borrow 获取 __index。
    pub fn get_and_metatable(&self, key: &TValue) -> (Option<TValue>, bool) {
        let data = self.data_ro();
        // perf: 只返回是否有元表的标志, 不 clone 元表 (Table 是 Rc, clone 需 incq)。
        // table_get 在 key 命中时直接返回, 无需元表; 仅 key 未命中时才调用 get_metatable()
        // 获取元表查 __index。命中路径省去一次冗余的 Rc incq/drop (all.lua table_get 占 10.65%)。
        let has_mt = data.metatable.is_some();
        let val = match key {
            TValue::Integer(i) if *i > 0 => {
                let idx = (*i - 1) as usize;
                if idx < data.array.len() {
                    let v = &data.array[idx];
                    if !matches!(v, TValue::Nil(NilKind::Empty)) {
                        Some(v.clone())
                    } else {
                        hash_get(&data, key)
                    }
                } else {
                    hash_get(&data, key)
                }
            }
            TValue::Float(f) => {
                if let Some(i) = float_key_to_int(*f) {
                    if i > 0 {
                        let idx = (i - 1) as usize;
                        if idx < data.array.len() {
                            let v = &data.array[idx];
                            if !matches!(v, TValue::Nil(NilKind::Empty)) {
                                return (Some(v.clone()), has_mt);
                            }
                        }
                    }
                    hash_get(&data, &TValue::Integer(i))
                } else {
                    hash_get(&data, key)
                }
            }
            _ => hash_get(&data, key),
        };
        (val, has_mt)
    }

    /// 单次探测特化查找 — GETTABUP/GETFIELD 热路径专用。
    ///
    /// 一次哈希探测返回命中槽的 &TValue 引用, 调用方按值类型分支处理:
    /// BuiltinFn → Copy 直取 (零 clone); Table → 结构 copy + Rc inc;
    /// 其他类型/未命中 → 调用方回退通用路径 (语义不变)。
    /// 取代此前 BuiltinFn/Table 各自探测的双查找
    /// (GETTABUP(math) 会 miss 一次 BuiltinFn 再 hit Table, 两次探测)。
    ///
    /// 元表存在时返回 None (__index 可能拦截, 不能绕过)。
    #[cfg_attr(not(size_optimized), inline(always))]
    pub fn find_str_ref<'a>(&'a self, key: &TValue) -> Option<&'a TValue> {
        let data = self.data_ro();
        if data.metatable.is_some() {
            return None;
        }
        let ktb = data.key_to_bucket.as_ref()?;
        match key {
            TValue::Str(crate::strings::LuaString::Short(_)) => {}
            _ => return None, // 只特化 interned 短字符串键
        }
        let hash = crate::objects::tvalue_fx_hash(key);
        let entry = ktb.find(hash, |(k, _)| match (k, key) {
            (TValue::Str(a), TValue::Str(b)) => a == b,
            _ => false,
        })?;
        let v = &data.hash_buckets[entry.1].1;
        if matches!(v, TValue::Nil(NilKind::Empty)) {
            return None; // tombstone 视为未命中
        }
        Some(v)
    }

    pub fn get(&self, key: &TValue) -> Option<TValue> {
        let data = self.data.borrow();
        match key {
            TValue::Integer(i) if *i > 0 => {
                let idx = (*i - 1) as usize;
                if idx < data.array.len() {
                    let v = &data.array[idx];
                    if !matches!(v, TValue::Nil(NilKind::Empty)) {
                        return Some(v.clone());
                    }
                }
                hash_get(&data, key)
            }
            TValue::Float(f) => {
                if let Some(i) = float_key_to_int(*f) {
                    if i > 0 {
                        let idx = (i - 1) as usize;
                        if idx < data.array.len() {
                            let v = &data.array[idx];
                            if !matches!(v, TValue::Nil(NilKind::Empty)) {
                                return Some(v.clone());
                            }
                        }
                    }
                    hash_get(&data, &TValue::Integer(i))
                } else {
                    hash_get(&data, key)
                }
            }
            _ => hash_get(&data, key),
        }
    }
    pub fn get_int(&self, key: i64) -> Option<TValue> {
        let data = self.data.borrow();
        if key > 0 {
            let idx = (key - 1) as usize;
            if idx < data.array.len() {
                let v = &data.array[idx];
                if !matches!(v, TValue::Nil(NilKind::Empty)) {
                    return Some(v.clone());
                }
            }
        }
        hash_get(&data, &TValue::Integer(key))
    }

    // ========================================================================
    // set / set_int —— 使用 borrow_mut 实现内部可变性
    // ========================================================================

    pub fn set(&self, key: TValue, value: TValue) {
        let is_nil = matches!(&value, TValue::Nil(NilKind::Strict));
        let mut data = self.data.borrow_mut();
        match &key {
            TValue::Integer(i) if *i > 0 => {
                let idx = (*i - 1) as usize;
                if idx < data.array.len() {
                    if is_nil {
                        data.array[idx] = TValue::Nil(NilKind::Empty);
                    } else {
                        data.array[idx] = value;
                    }
                    return;
                }
                // 顺序插入: key 正好是 array 末尾的下一个, 扩展 array
                if idx == data.array.len() && !is_nil {
                    data.array.push(value);
                    return;
                }
            }
            TValue::Float(f) => {
                // 对应 C: luaH_set — 浮点能精确转换为整数时，用整数键插入
                if let Some(i) = float_key_to_int(*f) {
                    if i > 0 {
                        let idx = (i - 1) as usize;
                        if idx < data.array.len() {
                            if is_nil {
                                data.array[idx] = TValue::Nil(NilKind::Empty);
                            } else {
                                data.array[idx] = value;
                            }
                            return;
                        }
                        if idx == data.array.len() && !is_nil {
                            data.array.push(value);
                            return;
                        }
                    }
                    Self::hash_set(&mut data, &TValue::Integer(i), value, is_nil);
                    return;
                }
            }
            _ => {}
        }
        Self::hash_set(&mut data, &key, value, is_nil);
    }

    pub fn set_int(&self, key: i64, value: TValue) {
        let is_nil = matches!(&value, TValue::Nil(NilKind::Strict));
        let mut data = self.data.borrow_mut();
        if key > 0 {
            let idx = (key - 1) as usize;
            if idx < data.array.len() {
                if is_nil {
                    data.array[idx] = TValue::Nil(NilKind::Empty);
                } else {
                    data.array[idx] = value;
                }
                return;
            }
            // 顺序插入: key 正好是 array 末尾的下一个, 扩展 array
            if idx == data.array.len() && !is_nil {
                data.array.push(value);
                return;
            }
        }
        let k = TValue::Integer(key);
        Self::hash_set(&mut data, &k, value, is_nil);
    }

    /// 哈希部分写入 — 维护 `hash_buckets`/`key_to_bucket` 并遵循 C 语义
    ///
    /// 对应 C 的 `luaH_psetshortstr`/`newcheckedkey` 的核心行为:
    /// - 若 key 不存在且写入 nil (Strict)，则不创建 node (C 直接返回 HOK)
    ///   避免无意义 tombstone 累积
    /// - 若 key 不存在且写入非 nil，追加到 hash_buckets + 记录 key_to_bucket
    /// - 若 key 存在 (含 tombstone)，覆盖值 (nil 写为 Nil(Empty) tombstone)
    fn hash_set(data: &mut TableData, key: &TValue, value: TValue, is_nil: bool) {
        let val = if is_nil {
            TValue::Nil(NilKind::Empty)
        } else {
            value
        };
        // 先检查 key 是否已存在，避免克隆 key
        if let Some(idx) = data.idx_get(key) {
            data.hash_buckets[idx].1 = val;
            return;
        }
        if is_nil {
            return; // C 语义: 对不存在的 key 设 nil 不创建 node
        }
        let idx = data.hash_buckets.len();
        data.hash_buckets.push((key.clone(), val));
        data.idx_insert(key, idx);
    }
    // ========================================================================

    pub fn len(&self) -> i64 {
        self.compute_len()
    }

    fn compute_len(&self) -> i64 {
        let data = self.data.borrow();
        let asize = data.array.len();
        if asize == 0 {
            return match data.key_to_bucket.as_ref() {
                Some(_) => Self::hash_boundary_impl(&data, asize as i64, 1u32),
                None => asize as i64,
            };
        }

        let hint = 1;

        let present_at = |i: usize| -> bool {
            i > 0 && i <= asize && !matches!(&data.array[i - 1], TValue::Nil(NilKind::Empty))
        };

        if present_at(hint) {
            let mut limit = hint;
            for _ in 0..4 {
                if limit >= asize {
                    break;
                }
                limit += 1;
                if !present_at(limit) {
                    return limit as i64 - 1;
                }
            }
            if !present_at(asize) {
                return Self::bin_search_array(&data.array, limit, asize) as i64;
            }
        } else {
            let mut limit = hint;
            for _ in 0..4 {
                if limit <= 1 {
                    break;
                }
                limit -= 1;
                if present_at(limit) {
                    return limit as i64;
                }
            }
            return Self::bin_search_array(&data.array, 0, limit) as i64;
        }

        match data.key_to_bucket.as_ref() {
            Some(_) => Self::hash_boundary_impl(&data, asize as i64, 1u32),
            None => asize as i64,
        }
    }

    fn bin_search_array(array: &[TValue], lo: usize, hi: usize) -> usize {
        let mut i = lo;
        let mut j = hi;
        while j - i > 1 {
            let m = (i + j) / 2;
            if matches!(&array[m - 1], TValue::Nil(NilKind::Empty)) {
                j = m;
            } else {
                i = m;
            }
        }
        i
    }

    /// 哈希边界搜索 — 对应 C 的 hash_search (ltable.cpp:1239)
    ///
    /// caller (compute_len) 在调用前已检查 t[asize] 非空（array 部分满）
    /// 或 asize==0。此处检查 t[asize+1] 是否存在：
    /// - 不存在：asize 即边界
    /// - 存在：进入指数增长 + 二分查找
    /// 使用 key_to_bucket 索引做 O(1) 存在性检查。
    fn hash_boundary_impl(data: &TableData, asize: i64, seed: u32) -> i64 {
        use TValue::Integer;
        let contains = |k: &TValue| data.idx_get(k).is_some();
        if !contains(&Integer(asize + 1)) {
            return asize;
        }
        let max_int = i64::MAX as u64;
        let mut i: u64 = (asize + 1) as u64;
        let mut rnd: u32 = seed;
        let n = if asize > 0 { ceillog2(asize as u64) } else { 0 };
        let mask: u32 = if n >= 32 { u32::MAX } else { (1u32 << n) - 1 };
        let incr: u64 = (rnd & mask) as u64 + 1;
        let mut j: u64 = if incr <= max_int - i { i + incr } else { i + 1 };
        rnd >>= n;
        while contains(&Integer(j as i64)) {
            i = j;
            if j <= max_int / 2 - 1 {
                j = j * 2 + (rnd & 1) as u64;
                rnd >>= 1;
            } else {
                j = max_int;
                if !contains(&Integer(j as i64)) {
                    break;
                } else {
                    return j as i64;
                }
            }
        }
        while j - i > 1 {
            let m = (i + j) / 2;
            if contains(&Integer(m as i64)) {
                i = m;
            } else {
                j = m;
            }
        }
        i as i64
    }

    // ========================================================================
    // next: 表遍历
    // ========================================================================

    pub fn next(&self, prev_key: Option<&TValue>) -> Option<(TValue, TValue)> {
        let data = self.data.borrow();
        match prev_key {
            None => {
                for (i, v) in data.array.iter().enumerate() {
                    if !matches!(v, TValue::Nil(NilKind::Empty)) {
                        return Some((TValue::Integer(i as i64 + 1), v.clone()));
                    }
                }
                for (k, v) in &data.hash_buckets {
                    if !matches!(v, TValue::Nil(NilKind::Empty)) {
                        return Some((k.clone(), v.clone()));
                    }
                }
                None
            }
            Some(prev) => match prev {
                TValue::Integer(i) if *i > 0 => {
                    let idx = *i as usize;
                    for j in idx..data.array.len() {
                        let v = &data.array[j];
                        if !matches!(v, TValue::Nil(NilKind::Empty)) {
                            return Some((TValue::Integer(j as i64 + 1), v.clone()));
                        }
                    }
                    for (k, v) in &data.hash_buckets {
                        if !matches!(v, TValue::Nil(NilKind::Empty)) {
                            return Some((k.clone(), v.clone()));
                        }
                    }
                    None
                }
                _ => {
                    let start = match data.idx_get(prev) {
                        Some(i) => i + 1,
                        None => return None,
                    };
                    for (k, v) in data.hash_buckets[start..].iter() {
                        if !matches!(v, TValue::Nil(NilKind::Empty)) {
                            return Some((k.clone(), v.clone()));
                        }
                    }
                    None
                }
            },
        }
    }

    // ========================================================================
    // rehash
    // ========================================================================

    pub fn rehash(&self, nasize: usize, nhsize: usize) {
        let mut data = self.data.borrow_mut();
        let mut all_entries: Vec<(TValue, TValue)> = Vec::new();

        for (i, v) in data.array.iter().enumerate() {
            if !matches!(v, TValue::Nil(NilKind::Empty)) {
                all_entries.push((TValue::Integer(i as i64 + 1), v.clone()));
            }
        }
        for (k, v) in data.hash_buckets.drain(..) {
            if matches!(v, TValue::Nil(NilKind::Empty)) {
                continue;
            }
            all_entries.push((k, v));
        }

        let old_asize = data.array.len();
        if nasize > old_asize {
            data.array.resize(nasize, TValue::Nil(NilKind::Empty));
        } else {
            data.array.truncate(nasize);
        }
        data.hash_buckets.clear();
        data.hash_buckets.reserve(nhsize);
        // 不再预设 key_to_bucket: 让下面的插入循环按需创建
        data.key_to_bucket = None;
        for (k, v) in all_entries {
            match &k {
                TValue::Integer(i) if *i > 0 => {
                    let idx = (*i - 1) as usize;
                    if idx < nasize {
                        data.array[idx] = v;
                    } else {
                        let bidx = data.hash_buckets.len();
                        data.hash_buckets.push((k.clone(), v));
                        data.idx_insert(&k, bidx);
                    }
                }
                _ => {
                    let bidx = data.hash_buckets.len();
                    data.hash_buckets.push((k.clone(), v));
                    data.idx_insert(&k, bidx);
                }
            }
        }
    }


    pub fn resize_array(&self, nasize: usize) {
        let nhsize = self.data.borrow().hash_buckets.len();
        self.rehash(nasize, nhsize);
    }

    pub fn mem_size(&self) -> usize {
        let data = self.data.borrow();
        let mut size = std::mem::size_of::<Table>()
            + data.array.capacity() * std::mem::size_of::<TValue>()
            + data.hash_buckets.capacity() * (std::mem::size_of::<TValue>() * 2);
        // key_to_bucket 索引堆占用：capacity * (size_of::<TValue>() + size_of::<usize>())
        // + HashTable 控制结构（约 24 字节）
        if let Some(ref ktb) = data.key_to_bucket {
            size += ktb.capacity() * (std::mem::size_of::<TValue>() + std::mem::size_of::<usize>())
                + 24;
        }
        size
    }
}

pub struct LuaTable {
    table: Table,
}

impl LuaTable {
    pub fn new() -> Self {
        LuaTable {
            table: Table::new(),
        }
    }

    pub fn with_capacity(narray: usize, nhash: usize) -> Self {
        LuaTable {
            table: Table::with_capacity(narray, nhash),
        }
    }

    pub fn inner(&self) -> &Table {
        &self.table
    }

    pub fn inner_mut(&mut self) -> &mut Table {
        &mut self.table
    }

    pub fn get(&self, key: &TValue) -> Option<TValue> {
        self.table.get(key)
    }

    pub fn get_int(&self, key: i64) -> Option<TValue> {
        self.table.get_int(key)
    }

    pub fn set(&mut self, key: TValue, value: TValue) {
        self.table.set(key, value);
    }

    pub fn set_int(&mut self, key: i64, value: TValue) {
        self.table.set_int(key, value);
    }

    pub fn len(&self) -> i64 {
        self.table.len()
    }

    pub fn next(&self, prev_key: Option<&TValue>) -> Option<(TValue, TValue)> {
        self.table.next(prev_key)
    }

    pub fn rehash(&mut self, nasize: usize, nhsize: usize) {
        self.table.rehash(nasize, nhsize);
    }

    pub fn resize_array(&mut self, nasize: usize) {
        self.table.resize_array(nasize);
    }
}

impl Default for LuaTable {
    fn default() -> Self {
        Self::new()
    }
}

pub fn new_table() -> Table {
    Table::new()
}

pub fn new_table_with_capacity(narray: usize, nhsize: usize) -> Table {
    Table::with_capacity(narray, nhsize)
}

/// 浮点数键转换为整数键 — 对应 C 的 `lua_numbertointeger`
///
/// 当浮点数能精确表示为 i64 范围内的整数时返回该整数。
/// 包括 `-0.0 → 0`；排除 NaN、Inf 和超出 i64 范围的值。
///
/// 对应 C 实现:
/// ```c
/// #define lua_numbertointeger(n,p) \
///   ((*(p) = (lua_Integer)(n)), (lua_Number)(*(p)) == (n))
/// ```
pub(crate) fn float_key_to_int(f: f64) -> Option<i64> {
    // 范围检查：i64::MAX as f64 实际是 2^63（向上舍入），所以 < i64::MAX as f64
    // 能正确排除 2^63 及以上的值；NaN 与任何数比较都是 false，会被排除
    if f >= i64::MIN as f64 && f < i64::MAX as f64 {
        let i = f as i64;
        if (i as f64) == f {
            return Some(i);
        }
    }
    None
}

// ============================================================================
// 测试 (TDD)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::{NilKind, TValue};
    use crate::strings::{LuaString, ShortString};

    // ------------------------------------------------------------------------
    // 构造 & 容量
    // ------------------------------------------------------------------------

    #[test]
    fn test_table_new() {
        let t = Table::new();
        assert_eq!(t.array_size(), 0);
        assert_eq!(t.hash_size(), 0);
        assert!(!t.has_metatable());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn test_table_with_capacity() {
        let t = Table::with_capacity(10, 8);
        assert!(t.data.borrow().hash_buckets.capacity() >= 8);
        let data = t.data.borrow();
        for v in &data.array {
            assert!(matches!(v, TValue::Nil(NilKind::Empty)));
        }
    }

    #[test]
    fn test_table_default() {
        let t = Table::default();
        assert_eq!(t.array_size(), 0);
        assert_eq!(t.hash_size(), 0);
    }

    #[test]
    fn test_lua_table_new() {
        let lt = LuaTable::new();
        assert_eq!(lt.len(), 0);
    }

    // ------------------------------------------------------------------------
    // get / get_int
    // ------------------------------------------------------------------------

    #[test]
    fn test_get_int_array_present() {
        let t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        assert_eq!(t.get_int(1), Some(TValue::Integer(10)));
    }

    #[test]
    fn test_get_int_array_empty_slot() {
        let t = Table::with_capacity(5, 0);
        assert_eq!(t.get_int(1), None);
    }

    #[test]
    fn test_get_int_out_of_array() {
        let t = Table::new();
        assert_eq!(t.get_int(100), None);
    }

    #[test]
    fn test_get_int_hash() {
        let t = Table::new();
        t.set_int(100, TValue::Integer(42));
        assert_eq!(t.get_int(100), Some(TValue::Integer(42)));
    }

    #[test]
    fn test_get_int_zero_or_negative() {
        let t = Table::new();
        t.set(TValue::Integer(0), TValue::Integer(99));
        assert_eq!(t.get_int(0), Some(TValue::Integer(99)));
        assert_eq!(t.get_int(-1), None);
    }

    #[test]
    fn test_get_string_key() {
        let t = Table::new();
        let key = LuaString::Short(crate::strings::ArcRc::new(ShortString {
            hash: 0,
            contents: crate::strings::LuaString::with_nul("name"),
        }));
        t.set(TValue::Str(key.clone()), TValue::Integer(42));
        let lookup = TValue::Str(key);
        assert_eq!(t.get(&lookup), Some(TValue::Integer(42)));
    }

    #[test]
    fn test_get_float_integral() {
        let t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        assert_eq!(t.get(&TValue::Float(1.0)), Some(TValue::Integer(10)));
    }

    #[test]
    fn test_get_float_non_integral() {
        let t = Table::new();
        t.set(TValue::Float(3.14), TValue::Integer(42));
        assert_eq!(t.get(&TValue::Float(3.14)), Some(TValue::Integer(42)));
    }

    #[test]
    fn test_get_nil_key() {
        let t = Table::new();
        assert_eq!(t.get(&TValue::Nil(NilKind::Strict)), None);
    }

    #[test]
    fn test_get_with_duplicate_keys_float_int() {
        let t = Table::new();
        t.set(TValue::Integer(42), TValue::Integer(100));
        // Float(42.0) 和 Integer(42) 在 Lua 中等价
        assert_eq!(t.get(&TValue::Float(42.0)), Some(TValue::Integer(100)));
    }

    // ------------------------------------------------------------------------
    // set / set_int
    // ------------------------------------------------------------------------

    #[test]
    fn test_set_int_array() {
        let t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        assert_eq!(t.data.borrow().array[0], TValue::Integer(10));
        assert_eq!(t.get_int(1), Some(TValue::Integer(10)));
    }

    #[test]
    fn test_set_int_hash() {
        let t = Table::new();
        t.set_int(100, TValue::Integer(42));
        assert!(t.data.borrow().idx_get(&TValue::Integer(100)).is_some());
        assert_eq!(t.get_int(100), Some(TValue::Integer(42)));
    }

    #[test]
    fn test_set_overwrite() {
        let t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(1, TValue::Integer(99));
        assert_eq!(t.get_int(1), Some(TValue::Integer(99)));
    }

    #[test]
    fn test_set_nil_removes() {
        let t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        assert!(t.get_int(1).is_some());
        t.set_int(1, TValue::Nil(NilKind::Strict));
        assert_eq!(t.get_int(1), None);
        assert!(matches!(
            &t.data.borrow().array[0],
            TValue::Nil(NilKind::Empty)
        ));
    }

    #[test]
    fn test_set_nil_removes_from_hash() {
        let t = Table::new();
        t.set_int(100, TValue::Integer(42));
        assert_eq!(t.hash_size(), 1);
        t.set_int(100, TValue::Nil(NilKind::Strict));
        assert_eq!(t.hash_size(), 0);
    }

    #[test]
    fn test_set_string_key() {
        let t = Table::new();
        let key = LuaString::Short(crate::strings::ArcRc::new(ShortString {
            hash: 0,
            contents: crate::strings::LuaString::with_nul("key"),
        }));
        t.set(TValue::Str(key.clone()), TValue::Integer(7));
        assert_eq!(t.hash_size(), 1);
        let lookup = TValue::Str(key);
        assert_eq!(t.get(&lookup), Some(TValue::Integer(7)));
    }

    #[test]
    fn test_set_bool_key() {
        let t = Table::new();
        t.set(TValue::Boolean(true), TValue::Integer(1));
        t.set(TValue::Boolean(false), TValue::Integer(0));
        assert_eq!(t.hash_size(), 2);
        assert_eq!(t.get(&TValue::Boolean(true)), Some(TValue::Integer(1)));
        assert_eq!(t.get(&TValue::Boolean(false)), Some(TValue::Integer(0)));
    }

    #[test]
    fn test_set_chained_keys() {
        let t = Table::new();
        for i in 1..=100 {
            t.set_int(i, TValue::Integer(i * 10));
        }
        // 顺序插入 1..100：set_int 的 array 扩展逻辑让连续正整数键进 array
        assert_eq!(t.len(), 100);
        assert_eq!(t.array_size(), 100);
        assert_eq!(t.hash_size(), 0);
        assert_eq!(t.get_int(50), Some(TValue::Integer(500)));
    }

    // ------------------------------------------------------------------------
    // len (#t)
    // ------------------------------------------------------------------------

    #[test]
    fn test_len_dense_array() {
        let t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn test_len_hole() {
        let t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(3, TValue::Integer(30));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn test_len_empty_table() {
        let t = Table::new();
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn test_len_full_array() {
        let t = Table::with_capacity(4, 0);
        for i in 1..=4 {
            t.set_int(i, TValue::Integer(i * 10));
        }
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn test_len_hash_only() {
        let t = Table::new();
        t.set(TValue::Boolean(true), TValue::Integer(1));
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn test_len_array_then_hash_continuation() {
        let t = Table::with_capacity(3, 10);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));
        t.set(TValue::Integer(4), TValue::Integer(40));
        t.set(TValue::Integer(5), TValue::Integer(50));
        assert_eq!(t.len(), 5);
    }

    // ------------------------------------------------------------------------
    // next
    // ------------------------------------------------------------------------

    #[test]
    fn test_next_from_nil() {
        let t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        let first = t.next(None);
        assert!(first.is_some());
        let (k, v) = first.unwrap();
        assert_eq!(k, TValue::Integer(1));
        assert_eq!(v, TValue::Integer(10));
    }

    #[test]
    fn test_next_array_full_traversal() {
        let t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));

        let r1 = t.next(None).unwrap();
        assert_eq!(r1, (TValue::Integer(1), TValue::Integer(10)));

        let r2 = t.next(Some(&r1.0)).unwrap();
        assert_eq!(r2, (TValue::Integer(2), TValue::Integer(20)));

        let r3 = t.next(Some(&r2.0)).unwrap();
        assert_eq!(r3, (TValue::Integer(3), TValue::Integer(30)));

        assert_eq!(t.next(Some(&r3.0)), None);
    }

    #[test]
    fn test_next_with_hole() {
        let t = Table::with_capacity(4, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(3, TValue::Integer(30));

        let r1 = t.next(None).unwrap();
        assert_eq!(r1.0, TValue::Integer(1));

        let r2 = t.next(Some(&r1.0)).unwrap();
        assert_eq!(r2.0, TValue::Integer(3));

        assert_eq!(t.next(Some(&r2.0)), None);
    }

    #[test]
    fn test_next_hash_part() {
        let t = Table::new();
        let key_a = TValue::Boolean(true);
        t.set(key_a.clone(), TValue::Integer(1));

        let r = t.next(None).unwrap();
        assert_eq!(r, (key_a.clone(), TValue::Integer(1)));
        assert_eq!(t.next(Some(&r.0)), None);
    }

    #[test]
    fn test_next_empty_table() {
        let t = Table::new();
        assert_eq!(t.next(None), None);
    }

    #[test]
    fn test_next_array_then_hash() {
        let t = Table::with_capacity(2, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set(TValue::Boolean(true), TValue::Integer(99));

        let mut keys_seen = Vec::new();
        let mut k_opt: Option<TValue> = None;
        while let Some((k, _v)) = t.next(k_opt.as_ref()) {
            keys_seen.push(k.clone());
            k_opt = Some(k);
        }
        assert_eq!(keys_seen.len(), 3);
        assert_eq!(keys_seen[0], TValue::Integer(1));
        assert_eq!(keys_seen[1], TValue::Integer(2));
    }

    #[test]
    fn test_next_multiple_hash_keys() {
        let t = Table::new();
        t.set(TValue::Boolean(true), TValue::Integer(1));
        t.set(TValue::Boolean(false), TValue::Integer(2));
        t.set(TValue::Integer(0), TValue::Integer(3));

        let mut keys_seen = Vec::new();
        let mut k_opt: Option<TValue> = None;
        while let Some((k, _)) = t.next(k_opt.as_ref()) {
            keys_seen.push(k);
            k_opt = Some(keys_seen.last().unwrap().clone());
        }
        assert_eq!(keys_seen.len(), 3);
    }

    #[test]
    fn test_next_after_nil_set() {
        let t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));
        t.set_int(2, TValue::Nil(NilKind::Strict)); // 删除键 2

        let all: Vec<_> = {
            let mut v = Vec::new();
            let mut k_opt: Option<TValue> = None;
            while let Some((k, _)) = t.next(k_opt.as_ref()) {
                v.push(k);
                k_opt = Some(v.last().unwrap().clone());
            }
            v
        };
        assert_eq!(all.len(), 2);
        assert!(all.contains(&TValue::Integer(1)));
        assert!(all.contains(&TValue::Integer(3)));
    }

    // ------------------------------------------------------------------------
    // rehash
    // ------------------------------------------------------------------------

    #[test]
    fn test_rehash_expand_array() {
        let t = Table::new();
        t.set_int(1, TValue::Integer(10));
        t.set_int(3, TValue::Integer(30));
        // set_int(1) 顺序扩展 array 到 1；set_int(3) 因 idx=2 != array.len()=1 进 hash
        assert_eq!(t.array_size(), 1);
        assert_eq!(t.hash_size(), 1);

        t.rehash(3, 0);
        assert_eq!(t.array_size(), 3);
        assert_eq!(t.get_int(1), Some(TValue::Integer(10)));
        assert_eq!(t.get_int(3), Some(TValue::Integer(30)));
        assert!(matches!(
            &t.data.borrow().array[1],
            TValue::Nil(NilKind::Empty)
        ));
    }

    #[test]
    fn test_rehash_shrink_array() {
        let t = Table::with_capacity(5, 0);
        for i in 1..=5 {
            t.set_int(i, TValue::Integer(i * 10));
        }
        assert_eq!(t.array_size(), 5);

        t.rehash(2, 0);
        assert_eq!(t.array_size(), 2);
        assert_eq!(t.get_int(1), Some(TValue::Integer(10)));
        assert_eq!(t.get_int(2), Some(TValue::Integer(20)));
        assert_eq!(t.hash_size(), 3);
        assert_eq!(t.get_int(3), Some(TValue::Integer(30)));
        assert_eq!(t.get_int(5), Some(TValue::Integer(50)));
    }

    #[test]
    fn test_resize_array() {
        let t = Table::with_capacity(2, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.resize_array(4);
        assert_eq!(t.array_size(), 4);
        assert_eq!(t.get_int(1), Some(TValue::Integer(10)));
        assert_eq!(t.get_int(2), Some(TValue::Integer(20)));
    }

    #[test]
    fn test_rehash_preserves_string_keys() {
        let t = Table::new();
        let key = LuaString::Short(crate::strings::ArcRc::new(ShortString {
            hash: 0,
            contents: crate::strings::LuaString::with_nul("mykey"),
        }));
        t.set(TValue::Str(key.clone()), TValue::Integer(77));
        t.set_int(1, TValue::Integer(10));

        t.rehash(5, 10);
        assert_eq!(t.get_int(1), Some(TValue::Integer(10)));
        let lookup = TValue::Str(key);
        assert_eq!(t.get(&lookup), Some(TValue::Integer(77)));
    }

    #[test]
    fn test_rehash_twice() {
        let t = Table::new();
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));

        t.rehash(5, 0);
        assert_eq!(t.array_size(), 5);
        assert_eq!(t.len(), 2);

        t.rehash(0, 10);
        assert_eq!(t.array_size(), 0);
        assert_eq!(t.hash_size(), 2);
        assert_eq!(t.get_int(1), Some(TValue::Integer(10)));
    }

    #[test]
    fn test_mem_size() {
        let t = Table::with_capacity(10, 16);
        let size = t.mem_size();
        assert!(size > 0);
    }

    // ------------------------------------------------------------------------
    // LuaTable 封装
    // ------------------------------------------------------------------------

    #[test]
    fn test_lua_table_get_set() {
        let mut lt = LuaTable::new();
        lt.set_int(1, TValue::Integer(42));
        assert_eq!(lt.get_int(1), Some(TValue::Integer(42)));
    }

    #[test]
    fn test_lua_table_len() {
        let mut lt = LuaTable::with_capacity(3, 0);
        lt.set_int(1, TValue::Integer(10));
        lt.set_int(2, TValue::Integer(20));
        lt.set_int(3, TValue::Integer(30));
        assert_eq!(lt.len(), 3);
    }

    #[test]
    fn test_lua_table_inner_access() {
        let mut lt = LuaTable::new();
        lt.set_int(1, TValue::Integer(99));
        assert_eq!(lt.inner().get_int(1), Some(TValue::Integer(99)));
        lt.inner_mut().set_int(2, TValue::Integer(88));
        assert_eq!(lt.get_int(2), Some(TValue::Integer(88)));
    }

    #[test]
    fn test_lua_table_next() {
        let mut lt = LuaTable::new();
        lt.set_int(1, TValue::Integer(10));
        let r = lt.next(None).unwrap();
        assert_eq!(r, (TValue::Integer(1), TValue::Integer(10)));
    }

    #[test]
    fn test_lua_table_rehash() {
        let mut lt = LuaTable::new();
        lt.set_int(1, TValue::Integer(10));
        lt.set_int(3, TValue::Integer(30));
        lt.rehash(3, 0);
        assert_eq!(lt.len(), 1);
    }

    #[test]
    fn test_new_table_functions() {
        let t = new_table();
        assert_eq!(t.array_size(), 0);
        let t2 = new_table_with_capacity(5, 8);
        assert_eq!(t2.array_size(), 5);
    }
}
