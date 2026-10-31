//! Lua 表实现 (基于 `crate::objects::Table`)
//!
//! `Table` 的定义在 [crate::objects] 中；此处包含所有方法实现及测试。
//!
//! ## 设计原则
//! - 数组部分：`Vec<TValue>`，1-based，空槽存储 `Nil(Empty)`
//! - 哈希部分：`Vec<(TValue, TValue)>` (hash_buckets) 按插入顺序存储，(key, value) 对
//! - 用 `key_to_bucket: HashMap<TValue, usize>` 做 O(1) 查找
//! - 数据通过 `TableDataRc`（Rc 语义，块经 MemState allocator 分配）共享，克隆 Table 共享同一份数据
//! - `LuaTable` 封装为未来元方法支持预留接口

use crate::config::{LuaInteger, LuaNumber, LuaUnsigned};
use crate::gc::GCObjectHeader;
use crate::mem::{AllocFailed, SlotAlloc};
use crate::objects::{
    tvalue_fx_hash, NilKind, TValue, TableData, TableDataRc, TableVec, WeakTable,
};
use crate::strings::lua_string_eq;

pub use crate::objects::Table;

/// ceil(log2(x)) — 对应 C 的 luaO_ceillog2
///
/// x=0→0, x=1→0, x=2→1, x=3→2, x=4→2, x=5→3 ...
fn ceillog2(x: LuaUnsigned) -> u32 {
    if x <= 1 {
        return 0;
    }
    (x - 1).ilog2() + 1
}
/// 表哈希查找 — TableData 上直接单跳探测 (key_to_bucket 索引)。
/// VM opcode 热路径 (op_getfield 等) 专用; 返回值 clone。
#[cfg_attr(not(size_optimized), inline)]
pub(crate) fn hash_get<'a>(td: &TableData<'a>, key: &TValue<'a>) -> Option<TValue<'a>> {
    if let Some(idx) = td.idx_get(key) {
        let v = &td.hash_buckets[idx].1;
        if !matches!(v, TValue::Nil(NilKind::Empty)) {
            return Some(v.clone());
        }
    }
    None
}
// ============================================================================
// 写入路径的容量预留（对应 C 的 luaH_growarrays / rehash 先行纪律）
// ============================================================================

/// 数组部分预留：下标落在数组内（或正好是末尾且写非 nil）时返回 `true`；
/// 需要走哈希路径时返回 `false`。
fn reserve_array_slot<'a>(
    data: &mut TableData<'a>,
    idx: usize,
    is_nil: bool,
) -> Result<bool, AllocFailed> {
    let len = data.array.len();
    if idx < len {
        return Ok(true);
    }
    if idx == len && !is_nil {
        // 顺序插入：数组末尾追加一个槽位
        if let Err(_e) = data.array.try_reserve(1) {
            return Err(AllocFailed);
        }
        return Ok(true);
    }
    Ok(false)
}

/// 哈希部分预留：key 已存在或写入 nil（C 语义：不创建 node）时无需扩容。
fn reserve_hash_slot<'a>(
    data: &mut TableData<'a>,
    key: &TValue<'a>,
    is_nil: bool,
) -> Result<(), AllocFailed> {
    if is_nil || data.idx_get(key).is_some() {
        return Ok(());
    }
    if data.hash_buckets.try_reserve(1).is_err() {
        return Err(AllocFailed);
    }
    if data.try_reserve_index(key).is_err() {
        return Err(AllocFailed);
    }
    Ok(())
}

// ============================================================================
// Table 方法实现
// ============================================================================

impl<'a> Table<'a> {
    /// 创建空表 — TableData 块（header + 数据）由 `alloc_slot` 指向的
    /// MemState allocator 分配/释放。数组/哈希缓冲区与 key_to_bucket 索引
    /// 同样经该分配器。
    ///
    /// 分配失败返回 [`AllocFailed`]：生产路径用 `LuaState::new_table`
    /// （失败 → 紧急 GC → 重试 → 内存错误），无状态上下文（引导/测试）自行决定。
    pub fn try_new_in(alloc: SlotAlloc) -> Result<Self, AllocFailed> {
        let data = TableDataRc::try_new_table(
            alloc,
            TableData {
                gc_header: GCObjectHeader::new(),
                array: TableVec::new_in(alloc),
                hash_buckets: TableVec::new_in(alloc),
                key_to_bucket: None,
                metatable: None,
            },
        )?;
        Ok(Table { data })
    }

    /// 【测试专用】分配失败即 panic 的便捷构造（生产路径走 `LuaState::new_table`）。
    #[cfg(test)]
    pub fn new_in(alloc: SlotAlloc) -> Self {
        Self::try_new_in(alloc)
            .ok()
            .expect("Table::new_in: 测试分配失败")
    }

    /// 创建带容量的表 — TableData 块由 `alloc_slot` 指向的 MemState allocator
    /// 分配/释放。对应 C 的 `lua_createtable`（预分配数组/哈希容量）。
    pub fn try_with_capacity_in(
        alloc: SlotAlloc,
        narray: usize,
        nhash: usize,
    ) -> Result<Self, AllocFailed> {
        // 对照 C 的 `luaH_resize`：先扩容（可失败，表仍为空表），再初始化槽位。
        let mut array = TableVec::new_in(alloc);
        array.try_reserve_exact(narray).map_err(|_| AllocFailed)?;
        array.resize(narray, TValue::Nil(NilKind::Empty));

        let mut hash_buckets = TableVec::new_in(alloc);
        hash_buckets
            .try_reserve_exact(nhash)
            .map_err(|_| AllocFailed)?;

        let key_to_bucket = if nhash > 0 {
            let mut ht = hashbrown::HashTable::with_capacity_in(0, alloc);
            ht.try_reserve(nhash, |(k, _)| tvalue_fx_hash(k))
                .map_err(|_| AllocFailed)?;
            Some(crate::mem::AllocBox::try_new_in(ht, alloc).map_err(|_| AllocFailed)?)
        } else {
            None
        };

        let data = TableDataRc::try_new_table(
            alloc,
            TableData {
                gc_header: GCObjectHeader::new(),
                array,
                hash_buckets,
                key_to_bucket,
                metatable: None,
            },
        )?;
        Ok(Table { data })
    }

    /// 【测试专用】分配失败即 panic 的便捷构造。
    #[cfg(test)]
    pub fn with_capacity_in(alloc: SlotAlloc, narray: usize, nhash: usize) -> Self {
        Self::try_with_capacity_in(alloc, narray, nhash)
            .ok()
            .expect("Table::with_capacity_in: 测试分配失败")
    }

    /// 转换为弱引用，用于 GC 收集。
    pub fn downgrade(&self) -> WeakTable<'a> {
        WeakTable {
            data: self.data.downgrade(),
        }
    }

    /// 打破引用计数循环：清空表的所有内容，让内部持有的 `Rc` 引用被释放。
    ///
    /// 用于 `LuaState::drop`，避免 `_G`（或 registry / metatable）自引用
    /// 导致 `Rc<RefCell<TableData>>` 计数永不归零，`TableData::drop`
    /// 从不被调用，进而里面的所有 `TValue`（含 `SimpleRc<ShortString>`）
    /// 永远不释放。
    ///
    /// 用 `try_borrow_mut` 而非 `borrow_mut`：drop 期间若已有活跃 borrow，
    /// 强行 borrow_mut 会 panic。宁可跳过清理，也不要 abort 进程。
    pub fn clear_for_drop(&self) {
        if let Ok(mut data) = self.data.try_borrow_mut() {
            data.array.clear();
            data.hash_buckets.clear();
            data.key_to_bucket = None; // 释放 hashbrown 表的堆内存
            data.metatable = None; // metatable 也可能形成循环
        }
    }

    // ========================================================================
    // 元表访问器
    // ========================================================================

    /// 获取元表的共享引用（克隆 Table，仅增加 Rc 引用计数，开销极小）。
    pub fn get_metatable(&self) -> Option<Table<'a>> {
        self.data.borrow().metatable.as_ref().map(|b| (**b).clone())
    }

    /// 按元方法名直查元表 (安全借用版, 零临时 clone) — 返回值引用。
    ///
    /// 用 Ref::filter_map 把 RefCell 借用收窄到命中的值槽上:
    /// guard 全程持有 (无 unsafe, 无 data_ro), 返回 Ref<TValue>。
    /// 查找本身用 LuaString 直查公式 (免构造 TValue 键的 Str clone)。
    /// 未命中 (含 nil tombstone / 无元表) 返回 None。
    /// 供 get_tm_by_obj 热路径使用。
    pub fn get_tm_ref(&self, key: &TValue<'a>) -> Option<std::cell::Ref<'_, TValue<'a>>> {
        let guard = self.data.borrow();
        let r = std::cell::Ref::filter_map(guard, |d| {
            use crate::objects::tvalue_fx_hash;

            let ktb = d.key_to_bucket.as_ref()?;
            let hash = tvalue_fx_hash(key);
            let (_, idx) = ktb.find(
                hash,
                |(k, _)| matches!(k, ks@(TValue::LongStr(_)|TValue::ShortStr(_)) if ks == key),
            )?;
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
    pub fn set_metatable(&self, mt: Option<Table<'a>>) {
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
    /// fast_unsafe 关闭时不存在调用点 (改用 borrow 守卫), 故整体不编译。
    #[cfg(feature = "fast_unsafe")]
    #[cfg_attr(not(size_optimized), inline(always))]
    pub(crate) fn data_ro(&self) -> &TableData<'a> {
        // 绕过 borrow 标志直接取 &TableData（仅用于无并发写场景）
        unsafe { &*(*self.data.as_ptr()).as_ptr() }
    }

    /// 一次 borrow 内同时查找值并返回元表 — 消除 metamethod 路径的第二次 borrow。
    /// 返回 (查找结果, 元表)。元表为 Some 时调用方无需再次 borrow 获取 __index。
    pub fn get_and_metatable(&self, key: &TValue<'a>) -> (Option<TValue<'a>>, bool) {
        #[cfg(feature = "fast_unsafe")]
        let data = self.data_ro();
        #[cfg(not(feature = "fast_unsafe"))]
        let data_guard = self.data.borrow();
        #[cfg(not(feature = "fast_unsafe"))]
        let data: &TableData<'a> = &data_guard;
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
    /// 语义 = 原始读 (rawget): 命中即返回, **与表是否有元表无关** —— `__index` 只在
    /// 原始读未命中时才参与 (`luaV_finishget` 首轮语义), 故有元表也不影响命中结果。
    /// 未命中时返回 None, 由调用方检查元表走 `__index` (可能拦截)。
    ///
    /// fast_unsafe: 读路径经 data_ro() 绕过 RefCell borrow, 返回命中槽引用
    /// (调用方决定 clone/位拷贝)。
    #[cfg(feature = "fast_unsafe")]
    #[cfg_attr(not(size_optimized), inline(always))]
    pub fn find_str_ref<'b>(&'b self, key: &TValue<'a>) -> Option<&'b TValue<'a>> {
        let data = self.data_ro();
        let ktb = data.key_to_bucket.as_ref()?;
        match key {
            TValue::ShortStr(_) => {}
            _ => return None, // 只特化 interned 短字符串键
        }
        let hash = crate::objects::tvalue_fx_hash(key);
        let entry = ktb.find(hash, |(k, _)| match (k, key) {
            (
                a @ (TValue::LongStr(_) | TValue::ShortStr(_)),
                b @ (TValue::LongStr(_) | TValue::ShortStr(_)),
            ) => lua_string_eq(a, b),
            _ => false,
        })?;
        let v = &data.hash_buckets[entry.1].1;
        if matches!(v, TValue::Nil(NilKind::Empty)) {
            return None; // tombstone 视为未命中
        }
        Some(v)
    }

    /// 安全版 (fast_unsafe 关闭) — 同 fast 版查找语义, 但持 borrow 守卫
    /// (guard 随函数返回释放), 命中值 clone 一份返回 owned。
    #[cfg(not(feature = "fast_unsafe"))]
    #[cfg_attr(not(size_optimized), inline)]
    pub fn find_str_ref(&self, key: &TValue<'a>) -> Option<TValue<'a>> {
        let data = self.data.borrow();
        let ktb = data.key_to_bucket.as_ref()?;
        match key {
            TValue::ShortStr(_) => {}
            _ => return None, // 只特化 interned 短字符串键
        }
        let hash = crate::objects::tvalue_fx_hash(key);
        let entry = ktb.find(hash, |(k, _)| match (k, key) {
            (
                a @ (TValue::LongStr(_) | TValue::ShortStr(_)),
                b @ (TValue::LongStr(_) | TValue::ShortStr(_)),
            ) => lua_string_eq(a, b),
            _ => false,
        })?;
        let v = &data.hash_buckets[entry.1].1;
        if matches!(v, TValue::Nil(NilKind::Empty)) {
            return None; // tombstone 视为未命中
        }
        Some(v.clone())
    }

    pub fn get(&self, key: &TValue<'a>) -> Option<TValue<'a>> {
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
    pub fn get_int(&self, key: LuaInteger) -> Option<TValue<'a>> {
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

    /// 写入前的容量预留 —— 对应 C 的 `luaH_growarrays` / `rehash` **先行** 纪律：
    /// 先扩容（可失败），再写入（不可失败）。
    ///
    /// 成功返回后，紧跟的 [`Table::set_unchecked`] / [`Table::set_int_unchecked`]
    /// 对**同一 key/value** 的写入不会再触发任何分配。失败时表内容不变（只做过容量
    /// 预留），因此调用方可以安全地「紧急 GC 后整体重试」（见 `LuaState::retry_alloc`）。
    pub fn try_reserve_for(&self, key: &TValue<'a>, is_nil: bool) -> Result<(), AllocFailed> {
        let mut data = self.data.borrow_mut();
        match key {
            TValue::Integer(i) if *i > 0 => {
                if reserve_array_slot(&mut data, (*i - 1) as usize, is_nil)? {
                    return Ok(());
                }
            }
            TValue::Float(f) => {
                // 对应 C: luaH_set — 浮点能精确转换为整数时按整数键插入
                if let Some(i) = float_key_to_int(*f) {
                    if i > 0 && reserve_array_slot(&mut data, (i - 1) as usize, is_nil)? {
                        return Ok(());
                    }
                    return reserve_hash_slot(&mut data, &TValue::Integer(i), is_nil);
                }
            }
            _ => {}
        }
        reserve_hash_slot(&mut data, key, is_nil)
    }

    /// 写入整型键前的容量预留（见 [`Table::try_reserve_for`]）。
    pub fn try_reserve_for_int(&self, key: LuaInteger, is_nil: bool) -> Result<(), AllocFailed> {
        self.try_reserve_for(&TValue::Integer(key), is_nil)
    }

    /// 【测试专用】无扩容失败的写入便捷入口（生产路径必须走
    /// `LuaState::table_set` / `LuaState::table_set_int`）。
    #[cfg(test)]
    pub fn set(&self, key: TValue<'a>, value: TValue<'a>) {
        let is_nil = matches!(&value, TValue::Nil(NilKind::Strict));
        self.try_reserve_for(&key, is_nil)
            .ok()
            .expect("Table::set: 测试分配失败");
        self.set_unchecked(key, value);
    }

    /// 【测试专用】见 [`Table::set`]。
    #[cfg(test)]
    pub fn set_int(&self, key: LuaInteger, value: TValue<'a>) {
        let is_nil = matches!(&value, TValue::Nil(NilKind::Strict));
        self.try_reserve_for_int(key, is_nil)
            .ok()
            .expect("Table::set_int: 测试分配失败");
        self.set_int_unchecked(key, value);
    }

    /// 无分配写入（前置条件：对同一 key/value 调用过 [`Table::try_reserve_for`] 且成功）。
    ///
    /// 与 C 的 `luaH_finishset` 等价：容量已就绪时写入不再可能失败。
    pub fn set_unchecked(&self, key: TValue<'a>, value: TValue<'a>) {
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
                    Self::hash_set_unchecked(&mut data, &TValue::Integer(i), value, is_nil);
                    return;
                }
            }
            _ => {}
        }
        Self::hash_set_unchecked(&mut data, &key, value, is_nil);
    }

    /// 无分配写入整型键（前置条件同 [`Table::set_unchecked`]）。
    pub fn set_int_unchecked(&self, key: LuaInteger, value: TValue<'a>) {
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
        Self::hash_set_unchecked(&mut data, &k, value, is_nil);
    }

    /// 哈希部分写入 — 维护 `hash_buckets`/`key_to_bucket` 并遵循 C 语义
    ///
    /// 对应 C 的 `luaH_psetshortstr`/`newcheckedkey` 的核心行为:
    /// - 若 key 不存在且写入 nil (Strict)，则不创建 node (C 直接返回 HOK)
    ///   避免无意义 tombstone 累积
    /// - 若 key 不存在且写入非 nil，追加到 hash_buckets + 记录 key_to_bucket
    /// - 若 key 存在 (含 tombstone)，覆盖值 (nil 写为 Nil(Empty) tombstone)
    fn hash_set_unchecked(
        data: &mut TableData<'a>,
        key: &TValue<'a>,
        value: TValue<'a>,
        is_nil: bool,
    ) {
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

    pub fn len(&self) -> LuaInteger {
        self.compute_len()
    }

    fn compute_len(&self) -> LuaInteger {
        let data = self.data.borrow();
        let asize = data.array.len();
        if asize == 0 {
            return match data.key_to_bucket.as_ref() {
                Some(_) => Self::hash_boundary_impl(&data, asize as LuaInteger, 1u32),
                None => asize as LuaInteger,
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
                    return limit as LuaInteger - 1;
                }
            }
            if !present_at(asize) {
                return Self::bin_search_array(&data.array, limit, asize) as LuaInteger;
            }
        } else {
            let mut limit = hint;
            for _ in 0..4 {
                if limit <= 1 {
                    break;
                }
                limit -= 1;
                if present_at(limit) {
                    return limit as LuaInteger;
                }
            }
            return Self::bin_search_array(&data.array, 0, limit) as LuaInteger;
        }

        match data.key_to_bucket.as_ref() {
            Some(_) => Self::hash_boundary_impl(&data, asize as LuaInteger, 1u32),
            None => asize as LuaInteger,
        }
    }

    fn bin_search_array(array: &[TValue<'a>], lo: usize, hi: usize) -> usize {
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
    fn hash_boundary_impl(data: &TableData<'a>, asize: LuaInteger, seed: u32) -> LuaInteger {
        use TValue::Integer;
        let contains = |k: &TValue<'a>| data.idx_get(k).is_some();
        if !contains(&Integer(asize + 1)) {
            return asize;
        }
        // 对应 C: l_castS2U(LUA_MAXINTEGER) —— 边界搜索全程在 LuaUnsigned 域
        let max_int = crate::config::MAX_INTEGER as LuaUnsigned;
        let mut i: LuaUnsigned = (asize + 1) as LuaUnsigned;
        let mut rnd: u32 = seed;
        let n = if asize > 0 {
            ceillog2(asize as LuaUnsigned)
        } else {
            0
        };
        let mask: u32 = if n >= 32 { u32::MAX } else { (1u32 << n) - 1 };
        let incr: LuaUnsigned = (rnd & mask) as LuaUnsigned + 1;
        let mut j: LuaUnsigned = if incr <= max_int - i { i + incr } else { i + 1 };
        rnd >>= n;
        while contains(&Integer(j as LuaInteger)) {
            i = j;
            if j <= max_int / 2 - 1 {
                j = j * 2 + (rnd & 1) as LuaUnsigned;
                rnd >>= 1;
            } else {
                j = max_int;
                if !contains(&Integer(j as LuaInteger)) {
                    break;
                } else {
                    return j as LuaInteger;
                }
            }
        }
        while j - i > 1 {
            let m = (i + j) / 2;
            if contains(&Integer(m as LuaInteger)) {
                i = m;
            } else {
                j = m;
            }
        }
        i as LuaInteger
    }

    // ========================================================================
    // next: 表遍历
    // ========================================================================

    pub fn next(&self, prev_key: Option<&TValue<'a>>) -> Option<(TValue<'a>, TValue<'a>)> {
        let data = self.data.borrow();
        match prev_key {
            None => {
                for (i, v) in data.array.iter().enumerate() {
                    if !matches!(v, TValue::Nil(NilKind::Empty)) {
                        return Some((TValue::Integer(i as LuaInteger + 1), v.clone()));
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
                            return Some((TValue::Integer(j as LuaInteger + 1), v.clone()));
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

/// 浮点数键转换为整数键 — 对应 C 的 `lua_numbertointeger` (F2Ieq 模式)
///
/// 当浮点数能精确表示为 `LuaInteger` 范围内的整数时返回该整数。
/// 包括 `-0.0 → 0`；排除 NaN、Inf 和超出范围的值。
///
/// 对应 C 实现:
/// ```c
/// #define lua_numbertointeger(n,p) \
///   ((*(p) = (lua_Integer)(n)), (lua_Number)(*(p)) == (n))
/// ```
pub(crate) fn float_key_to_int(f: LuaNumber) -> Option<LuaInteger> {
    // 范围检查走 config::number_to_integer (C 的 lua_numbertointeger:
    // 上界为 -(LuaNumber)MIN_INTEGER 开区间, 故 2^63 及以上被排除;
    // NaN 与任何数比较都是 false, 同样被排除)。
    // 随后用往返比较保证「精确整数值」(对应 C 的 luaV_flttointeger 的 F2Ieq)。
    let i = crate::config::number_to_integer(f)?;
    if (i as LuaNumber) == f {
        Some(i)
    } else {
        None
    }
}

// ============================================================================
// 测试 (TDD)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::{NilKind, TValue};
    use crate::strings::ShortString;

    // ------------------------------------------------------------------------
    // 构造 & 容量
    // ------------------------------------------------------------------------

    #[test]
    fn test_table_new() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        assert!(!t.has_metatable());
        assert_eq!(t.len(), 0);
    }
    #[test]
    fn test_table_with_capacity() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 10, 8);
        assert!(t.data.borrow().hash_buckets.capacity() >= 8);
        let data = t.data.borrow();
        for v in &data.array {
            assert!(matches!(v, TValue::Nil(NilKind::Empty)));
        }
    }
    /// 表的三个内部容器（array / hash_buckets / key_to_bucket）都携带并使用
    /// MemState allocator 槽（对应 C 表经 global_State.allocf 分配）。
    #[test]
    fn test_table_inner_containers_use_custom_allocator() {
        use std::cell::Cell;
        use std::rc::Rc;

        use crate::mem::{Allocator, DefaultAllocator};

        struct CountingAllocator {
            n: Rc<Cell<usize>>,
        }
        impl Allocator for CountingAllocator {
            fn alloc(&self, ptr: *mut u8, osize: usize, nsize: usize, align: usize) -> *mut u8 {
                if nsize > osize {
                    self.n.set(self.n.get() + 1);
                }
                DefaultAllocator.alloc(ptr, osize, nsize, align)
            }
        }

        let counter = Rc::new(Cell::new(0usize));
        let boxed: Box<dyn Allocator> = Box::new(CountingAllocator {
            n: Rc::clone(&counter),
        });
        let slot = unsafe { SlotAlloc::from_slot(&boxed) };

        let t = Table::new_in(slot);
        {
            let data = t.data.borrow();
            // 两个 Vec 直接携带该槽（空表不分配缓冲区）
            assert!(std::ptr::eq(data.array.allocator().slot(), slot.slot()));
            assert!(std::ptr::eq(
                data.hash_buckets.allocator().slot(),
                slot.slot()
            ));
        }

        // 写数组元素 → array 缓冲区经自定义分配器扩容
        let n0 = counter.get();
        for i in 1..=8 {
            t.set_int(i, TValue::Integer(i));
        }
        assert!(counter.get() > n0, "array 缓冲区应经自定义分配器");

        // 写哈希键 → hash_buckets 缓冲区与 key_to_bucket 索引经自定义分配器
        let n1 = counter.get();
        t.set(TValue::Boolean(true), TValue::Integer(42));
        {
            let data = t.data.borrow();
            let ktb = data.key_to_bucket.as_ref().unwrap();
            assert!(std::ptr::eq(ktb.allocator().slot(), slot.slot()));
        }
        assert!(
            counter.get() > n1,
            "hash_buckets/key_to_bucket 应经自定义分配器"
        );

        drop(t);
    }

    // ------------------------------------------------------------------------
    // get / get_int
    // ------------------------------------------------------------------------

    #[test]
    fn test_get_int_array_present() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 5, 0);
        t.set_int(1, TValue::Integer(10));
        assert_eq!(t.get_int(1), Some(TValue::Integer(10)));
    }
    #[test]
    fn test_get_int_array_empty_slot() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 5, 0);
        assert_eq!(t.get_int(1), None);
    }
    #[test]
    fn test_get_int_out_of_array() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        assert_eq!(t.get_int(100), None);
    }
    #[test]
    fn test_get_int_hash() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        t.set_int(100, TValue::Integer(42));
        assert_eq!(t.get_int(100), Some(TValue::Integer(42)));
    }
    #[test]
    fn test_get_int_zero_or_negative() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        t.set(TValue::Integer(0), TValue::Integer(99));
        assert_eq!(t.get_int(0), Some(TValue::Integer(99)));
        assert_eq!(t.get_int(-1), None);
    }
    #[test]
    fn test_get_string_key() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        let key = TValue::ShortStr(crate::strings::ArcRc::<ShortString>::new_sstr(
            0,
            0,
            "name".as_bytes(),
        ));
        t.set(key.clone(), TValue::Integer(42));
        let lookup = key;
        assert_eq!(t.get(&lookup), Some(TValue::Integer(42)));
    }
    #[test]
    fn test_get_float_integral() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 5, 0);
        t.set_int(1, TValue::Integer(10));
        assert_eq!(t.get(&TValue::Float(1.0)), Some(TValue::Integer(10)));
    }
    #[test]
    fn test_get_float_non_integral() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        t.set(TValue::Float(3.14), TValue::Integer(42));
        assert_eq!(t.get(&TValue::Float(3.14)), Some(TValue::Integer(42)));
    }
    #[test]
    fn test_get_nil_key() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        assert_eq!(t.get(&TValue::Nil(NilKind::Strict)), None);
    }
    #[test]
    fn test_get_with_duplicate_keys_float_int() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        t.set(TValue::Integer(42), TValue::Integer(100));
        // Float(42.0) 和 Integer(42) 在 Lua 中等价
        assert_eq!(t.get(&TValue::Float(42.0)), Some(TValue::Integer(100)));
    }
    // ------------------------------------------------------------------------
    // set / set_int
    // ------------------------------------------------------------------------

    #[test]
    fn test_set_int_array() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 5, 0);
        t.set_int(1, TValue::Integer(10));
        assert_eq!(t.data.borrow().array[0], TValue::Integer(10));
        assert_eq!(t.get_int(1), Some(TValue::Integer(10)));
    }
    #[test]
    fn test_set_int_hash() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        t.set_int(100, TValue::Integer(42));
        assert!(t.data.borrow().idx_get(&TValue::Integer(100)).is_some());
        assert_eq!(t.get_int(100), Some(TValue::Integer(42)));
    }
    #[test]
    fn test_set_overwrite() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 3, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(1, TValue::Integer(99));
        assert_eq!(t.get_int(1), Some(TValue::Integer(99)));
    }
    #[test]
    fn test_set_nil_removes() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 3, 0);
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
    fn test_set_string_key() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        let key = TValue::ShortStr(crate::strings::ArcRc::<ShortString>::new_sstr(
            0,
            0,
            "key".as_bytes(),
        ));
        t.set(key.clone(), TValue::Integer(7));
        let lookup = key;
        assert_eq!(t.get(&lookup), Some(TValue::Integer(7)));
    }
    #[test]
    fn test_set_bool_key() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        t.set(TValue::Boolean(true), TValue::Integer(1));
        t.set(TValue::Boolean(false), TValue::Integer(0));
        assert_eq!(t.get(&TValue::Boolean(true)), Some(TValue::Integer(1)));
        assert_eq!(t.get(&TValue::Boolean(false)), Some(TValue::Integer(0)));
    }
    #[test]
    fn test_set_chained_keys() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        for i in 1..=100 {
            t.set_int(i, TValue::Integer(i * 10));
        }
        // 顺序插入 1..100：set_int 的 array 扩展逻辑让连续正整数键进 array
        assert_eq!(t.len(), 100);
        assert_eq!(t.get_int(50), Some(TValue::Integer(500)));
    }
    // ------------------------------------------------------------------------
    // len (#t)
    // ------------------------------------------------------------------------

    #[test]
    fn test_len_dense_array() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));
        assert_eq!(t.len(), 3);
    }
    #[test]
    fn test_len_hole() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(3, TValue::Integer(30));
        assert_eq!(t.len(), 1);
    }
    #[test]
    fn test_len_empty_table() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        assert_eq!(t.len(), 0);
    }
    #[test]
    fn test_len_full_array() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 4, 0);
        for i in 1..=4 {
            t.set_int(i, TValue::Integer(i * 10));
        }
        assert_eq!(t.len(), 4);
    }
    #[test]
    fn test_len_hash_only() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        t.set(TValue::Boolean(true), TValue::Integer(1));
        assert_eq!(t.len(), 0);
    }
    #[test]
    fn test_len_array_then_hash_continuation() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 3, 10);
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
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 3, 0);
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
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 3, 0);
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
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 4, 0);
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
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        let key_a = TValue::Boolean(true);
        t.set(key_a.clone(), TValue::Integer(1));

        let r = t.next(None).unwrap();
        assert_eq!(r, (key_a.clone(), TValue::Integer(1)));
        assert_eq!(t.next(Some(&r.0)), None);
    }
    #[test]
    fn test_next_empty_table() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
        assert_eq!(t.next(None), None);
    }
    #[test]
    fn test_next_array_then_hash() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 2, 0);
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
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::new_in(alloc.slot());
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
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 5, 0);
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
    #[test]
    fn test_mem_size() {
        let alloc = crate::mem::TestAlloc::new();
        let t = Table::with_capacity_in(alloc.slot(), 10, 16);
        let size = t.mem_size();
        assert!(size > 0);
    }
}
