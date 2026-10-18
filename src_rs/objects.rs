//! Lua 对象和值类型表示 (纯 Rust 重写)
//!
//! 对应 C 源码: lobject.h + lobject.cpp
//!
//! ## 设计原则
//! - 使用 Rust `enum` 替代 C 的 tagged union + bit-tag，用模式匹配替代宏/位测试
//! - 使用 `Option` / `Result` 替代 NULL / 哨兵值
//! - 使用 Rust 所有权系统管理对象生命周期
//! - 类型安全的数字转换（区分整数和浮点数）
//! - nil 的多种语义通过 enum 变体表达，而非位掩码
//!
//! ## Lua 类型系统
//! Lua 有 9 种基本类型：nil, boolean, lightuserdata, number, string,
//! table, function, userdata, thread。每种类型可能有子变体（如 number 分 integer/float）。
//!
//! ## 规约驱动开发 (spec-driven-tdd)
//! 以下每个类型/函数都包含规约注释（Scenario / Given / When / Then）。
//! 规约未获批准前，不得编写实现代码。
//!
//! ```text
//! 工作流程: 编写规约 → 人类评审 → 红灯(失败测试) → 绿灯(最小实现) → 重构
//! ```

use std::fmt;
use std::hash::{Hash, Hasher};

use crate::alloc::SimpleRc;
use crate::config::{LuaInteger, LuaNumber, LuaUnsigned};
use crate::mem::SlotAlloc;
use crate::strings::{
    lua_string_as_str, lua_string_eq, lua_string_hash, ArcRc, LongString, ShortString,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::alloc::{MyRc, WeakRc, RC_HEADER_SIZE};
use crate::execute::VmError;
use crate::gc::GCObjectHeader;
use crate::state::LuaState;

// ============================================================================
// FxHash — 用于 Table 哈希部分的快速非加密哈希
// ============================================================================
//
// Rust 默认 HashMap 使用 SipHash（抗碰撞攻击），但 Lua 表的 key 已包含足够熵
// （Integer/Float 直接哈希，String 有预计算哈希，Table 用 ptr_id），
// 不需要 SipHash 的加密强度。FxHash 速度快 5-10 倍，显著降低哈希开销。
// 对应 C Lua 的 luaH_hashstr 等使用简单哈希的策略。
//
// FxHash 体积代价 ~几百字节（全 inline 算术，无调用），不值得为它维护
// 一份 SipHash 回退分支；std HashMap 的 SipHash/RawTable 代码无论如何都因
// compiler/state 等模块被链接，切换 hasher 边际节省 ≈ 0。

pub(crate) mod fx_hash_impl {
    use std::hash::{BuildHasherDefault, Hasher};

    /// FxHash 常量种子（来自 rustc-hash crate）
    const FX_HASH_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

    /// FxHasher — 快速非加密哈希器，用于 Table 的 key_to_bucket HashMap。
    /// 替代默认 SipHash，减少哈希计算开销（perf 显示 SipHash 占 ~4.36%）。
    #[derive(Default)]
    pub struct FxHasher {
        hash: u64,
    }

    impl FxHasher {
        #[inline]
        fn add_to_hash(&mut self, i: u64) {
            self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(FX_HASH_SEED);
        }
    }

    impl Hasher for FxHasher {
        fn finish(&self) -> u64 {
            self.hash
        }

        fn write(&mut self, bytes: &[u8]) {
            for chunk in bytes.chunks_exact(8) {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(chunk);
                self.add_to_hash(u64::from_le_bytes(buf));
            }
            let rem = bytes.len() % 8;
            if rem > 0 {
                let mut buf = [0u8; 8];
                buf[..rem].copy_from_slice(&bytes[bytes.len() - rem..]);
                self.add_to_hash(u64::from_le_bytes(buf));
            }
        }

        #[inline]
        fn write_u8(&mut self, i: u8) {
            self.add_to_hash(i as u64);
        }
        #[inline]
        fn write_u16(&mut self, i: u16) {
            self.add_to_hash(i as u64);
        }
        #[inline]
        fn write_u32(&mut self, i: u32) {
            self.add_to_hash(i as u64);
        }
        #[inline]
        fn write_u64(&mut self, i: u64) {
            self.add_to_hash(i);
        }
        #[inline]
        fn write_i8(&mut self, i: i8) {
            self.add_to_hash(i as u64);
        }
        #[inline]
        fn write_i16(&mut self, i: i16) {
            self.add_to_hash(i as u64);
        }
        #[inline]
        fn write_i32(&mut self, i: i32) {
            self.add_to_hash(i as u64);
        }
        #[inline]
        fn write_i64(&mut self, i: i64) {
            self.add_to_hash(i as u64);
        }
        #[inline]
        fn write_usize(&mut self, i: usize) {
            self.add_to_hash(i as u64);
        }
        #[inline]
        fn write_isize(&mut self, i: isize) {
            self.add_to_hash(i as u64);
        }
    }

    /// FxBuildHasher — BuildHasher 实现，构造 FxHasher
    pub type FxBuildHasher = BuildHasherDefault<FxHasher>;
    pub use FxHasher as FxHasherPub;
}

pub use fx_hash_impl::FxBuildHasher;

// ============================================================================
// Table 哈希索引 — hashbrown::HashTable<(TValue, usize)>
// ============================================================================

/// Table 的 key→bucket 索引：开放寻址 HashTable，每条目 (key, bucket_index)。
///
/// perf: 替代原 HashMap<TValue, usize> — HashTable::find 用预计算哈希（存于
/// 控制字节 SIMD 组）+ 调用方 eq 闭包探测，eq 直接比较条目内的 key 克隆，
/// 单跳定位（对应 C Lua Node 的 main position 比较模式），省去 HashMap
/// 每次 get 的 BuildHasher 调用链与 Result/Eq 适配层。
/// key→bucket 索引的分配器由 [`crate::mem::SlotAlloc`] 提供（对应 C Lua Node
/// 数组经 global_State.allocf 分配）。
pub type TableHashIndex<'a> = hashbrown::HashTable<(TValue<'a>, usize), crate::mem::SlotAlloc>;

/// `key_to_bucket` 的堆内盒子，经 [`crate::mem::SlotAlloc`]
/// 分配（内含 HashTable 的缓冲区同样经该分配器）。
/// 标准库 `Box<T, A>` 受 unstable `allocator_api` 门控，故经 [`crate::mem::AllocBox`]
/// 在“标准库 / allocator_api2”间选择。
pub type TableKeyBox<'a> = crate::mem::AllocBox<TableHashIndex<'a>, crate::mem::SlotAlloc>;

/// 数组部分 / 哈希部分的分配器感知 Vec（对应 C 的 luaM_growvector 分配路径）。
/// 经 [`crate::mem::AllocVec`] 在标准库 `Vec<T, A>` / `allocator_api2::vec::Vec` 间选择。
pub type TableVec<T> = crate::mem::AllocVec<T, crate::mem::SlotAlloc>;

/// 计算 TValue 的 FxHash 哈希 — 与 impl Hash for TValue + FxBuildHasher 一致。
///
/// TableHashIndex 以 (hash, eq) 二元组驱动 find/insert，此函数提供 hash 侧。
/// perf: 直接展开 FxHasher 轮函数 (rotate_left(5) ^ x) * SEED, 热键类型
/// (Str/Integer) 免泛型 Hash trait 分发。Str 键首轮 5*SEED 为编译期常量。
pub fn tvalue_fx_hash(v: &TValue) -> u64 {
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
    match v {
        TValue::ShortStr(_) | TValue::LongStr(_) => {
            let h1 = 5u64.wrapping_mul(SEED);
            let sh = lua_string_hash(v);
            (h1.rotate_left(5) ^ sh).wrapping_mul(SEED)
        }
        TValue::Integer(i) => {
            let h1 = 3u64.wrapping_mul(SEED);
            // 整数按位哈希: 与 `impl Hash for TValue` 的 `LuaInteger::hash` 一致 ——
            // FxHasher 的 write_i32/write_i64 都是 `i as u64` (符号扩展), 故此处
            // 直接 `*i as u64` 在 i32/i64 两种宽度下都逐位等价 (hash 边界宽类型转换)。
            (h1.rotate_left(5) ^ (*i as u64)).wrapping_mul(SEED)
        }
        TValue::Boolean(b) => {
            // 对应 Hash: 1u8 + bool(u8 0/1) 两轮
            let h1 = 1u64.wrapping_mul(SEED);
            let h2 = (h1.rotate_left(5) ^ (*b as u64)).wrapping_mul(SEED);
            (h2.rotate_left(5) ^ 0u64).wrapping_mul(SEED)
        }
        TValue::Nil(kind) => {
            // 对应 Hash: 0u8 + NilKind(u8 枚举) 两轮
            let kind_u8 = match kind {
                NilKind::Strict => 0u64,
                NilKind::Empty => 1u64,
            };
            let h2 = (0u64.rotate_left(5) ^ kind_u8).wrapping_mul(SEED);
            (h2.rotate_left(5) ^ 0u64).wrapping_mul(SEED)
        }
        _ => {
            use std::hash::Hasher;
            let mut h = fx_hash_impl::FxHasherPub::default();
            std::hash::Hash::hash(v, &mut h);
            h.finish()
        }
    }
}

// ============================================================================
// 规约：Lua 基础类型标签
// ============================================================================

/// 共享上值引用 —— 多个闭包可以共享同一个 UpVal（对应 C 中 UpVal 是堆分配对象）。
/// 当 Open 上值被关闭时，所有持有该引用的闭包都能看到 Closed 状态。
pub type UpValRef<'a> = Rc<RefCell<UpVal<'a>>>;

/// Lua 类型标签 —— 使用 Rust enum 替代 C 的整数常量 + 位掩码
///
/// Lua 有 9 种基本值类型。在 C 实现中，类型标签使用整数 + 变体位 + 可回收位。
/// Rust 版本直接用 enum，编译器保证穷举匹配。
///
/// Scenario: 类型标签的基本分类
/// Given: 一个 Lua 值
/// When: 检查其类型标签
/// Then: 返回 9 种基本类型之一
/// And: 每种类型的整数值与 Lua C API 兼容
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LuaType {
    /// nil 类型 (LUA_TNIL = 0)
    Nil = 0,
    /// boolean 类型 (LUA_TBOOLEAN = 1)
    Boolean = 1,
    /// light userdata 类型 (LUA_TLIGHTUSERDATA = 2)
    LightUserData = 2,
    /// number 类型 (LUA_TNUMBER = 3)
    Number = 3,
    /// string 类型 (LUA_TSTRING = 4)
    String = 4,
    /// table 类型 (LUA_TTABLE = 5)
    Table = 5,
    /// function 类型 (LUA_TFUNCTION = 6)
    Function = 6,
    /// full userdata 类型 (LUA_TUSERDATA = 7)
    UserData = 7,
    /// thread 类型 (LUA_TTHREAD = 8)
    Thread = 8,
}

impl fmt::Display for LuaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LuaType::Nil => write!(f, "nil"),
            LuaType::Boolean => write!(f, "boolean"),
            LuaType::LightUserData => write!(f, "lightuserdata"),
            LuaType::Number => write!(f, "number"),
            LuaType::String => write!(f, "string"),
            LuaType::Table => write!(f, "table"),
            LuaType::Function => write!(f, "function"),
            LuaType::UserData => write!(f, "userdata"),
            LuaType::Thread => write!(f, "thread"),
        }
    }
}

// ============================================================================
// 规约：Rust 原生内置函数 (BuiltinFn)
// ============================================================================

/// Rust 原生内置函数指针类型
///
/// 与 C API 的 `lua_CFunction` (`unsafe extern "C" fn(*mut lua_State) -> c_int`) 不同，
/// 使用 Rust ABI 和 `Result` 错误处理，类型安全且无 `unsafe`：
///
/// - `state`: VM 状态（可变引用，非裸指针）
/// - `a`: 函数在栈中的位置（0-based，`state.exec.stack[a]` 是函数本身，参数从 `a+1` 开始）
/// - `nargs`: 参数数量
/// - `nresults`: 期望结果数（-1 = MULTRET，>=0 = 固定数量）
///
/// 返回 `Result<(), VmError>`：
/// - `Ok(())`: 成功，结果已压入栈并由 VM 调整
/// - `Err(VmError::Yield(_))`: 协程 yield（非真实错误）
/// - `Err(other)`: 运行时错误
///
/// 对应 C 的 `lua_CFunction`，但更适合 Rust 用户使用。
pub type BuiltinFnPtr =
    for<'a, 's> fn(&'a mut LuaState<'s>, usize, usize, i32) -> Result<(), VmError<'s>>;

/// Rust 原生内置函数
///
/// 包含函数指针和静态函数名（用于 traceback）。
///
/// ## 与 `TValue::LightUserData(tag)` 的区别
///
/// 历史上内置库函数通过 `TValue::LightUserData(tag as *mut c_void)` 注册，
/// 用 tag 值范围（1-32, 100-119, 200-299 等）派发。这有两个问题：
/// 1. 用户通过 `lua_pushlightuserdata(L, p)` 保存的指针若落入 tag 范围，
///    会被 `type()` 误判为 "function"
/// 2. 派发需经过 `if-else` 链匹配 tag 范围 + 内部 `match tag`，开销 ~15-30 cycles
///
/// `BuiltinFn` 直接存储函数指针，调用时一次间接跳转即可（~5-10 cycles），
/// 与 C Lua 持平。同时 `LightUserData` 不再承担派发职责，类型判断正确。
///
/// ## 内存布局
///
/// 为保持 `TValue` 24 字节大小不变，`name` 使用 `*const u8`（8 字节 thin pointer）
/// 而非 `&'static str`（16 字节 fat pointer）。函数名均为 ASCII，NUL 终止。
/// 用 `c"name".as_ptr()` 构造（Rust 1.77+ 的 `c"..."` 字面量产生 `&'static CStr`，
/// `.as_ptr()` 得到指向 NUL 终止字节数组的 thin pointer）。
///
/// Scenario: 注册并调用 Rust 原生内置函数
/// Given: 一个签名为 `fn(&mut GlobalState, usize, usize, i32) -> Result<(), VmError>` 的函数
/// When: 用 `TValue::BuiltinFn(BuiltinFn { func, name })` 注册到表中
/// Then: Lua 代码调用该函数时，VM 直接通过函数指针调用，无需 tag 派发
#[derive(Clone, Copy)]
pub struct BuiltinFn {
    /// 编码后的函数指针 — bit0 = pure 标志 (1: 纯函数), bit1.. = 真实函数地址。
    ///
    /// 函数指针按机器字长对齐 (≥2), bit0 恒 0 可安全借用。pure 标志内嵌
    /// 指针低位使 BuiltinFn 保持 8 字节 (TValue 16B 布局不变), 取代原
    /// GlobalState.pure_fns HashSet 查询 (每次调用 2 个非内联 call:
    /// tvalue_fx_hash + RawTable::find, ~25 cycles) — op_call 现为一条 test。
    ///
    /// 任何调用/比较该指针的位置必须经 call_target()/raw_func() 解码。
    /// name 登记表 (builtin_names) 用解码后的原始指针, 与 impure 注册一致。
    pub func: BuiltinFnPtr,
}

impl BuiltinFn {
    /// bit0 掩码 — pure 标志位
    /// (Miri 下不做低位编码, 该常量未被引用)
    #[cfg_attr(miri, allow(dead_code))]
    const PURE_BIT: usize = 1;

    /// 标准 BuiltinFn 构造（impure — 可能回调 Lua / yield）。
    ///
    /// name 不存储在结构体内（为压缩 TValue 到 16 字节）：注册时登记到
    /// 全局 BUILTIN_NAMES 表（func 指针 → NUL 终止名字），traceback 冷路径查表。
    pub fn impure(func: BuiltinFnPtr, name: *const u8) -> Self {
        builtin_names::register(func, name);
        Self { func }
    }

    /// 构造 BuiltinFn 并返回 TValue 包装
    pub fn impure_tvalue<'a>(func: BuiltinFnPtr, name: *const u8) -> TValue<'a> {
        TValue::BuiltinFn(Self::impure(func, name))
    }

    /// 纯函数 BuiltinFn 构造 — 不回调 Lua / 不 yield / 错误只返回 Err。
    /// 数学库等纯函数注册用此构造; op_call 见 pure 位 (func bit0) 直接走
    /// 零簿记快速路径 (跳过 CallInfoEntry push/pop + name_str strlen)。
    pub fn pure_fn(func: BuiltinFnPtr, name: *const u8) -> Self {
        builtin_names::register(func, name);
        Self {
            func: Self::encode_pure(func),
        }
    }

    /// 构造 BuiltinFn 并返回 TValue 包装
    pub fn pure_fn_tvalue<'a>(func: BuiltinFnPtr, name: *const u8) -> TValue<'a> {
        TValue::BuiltinFn(Self::pure_fn(func, name))
    }

    /// 编码: 原始函数指针 | PURE_BIT，保留 provenance
    fn encode_pure(func: BuiltinFnPtr) -> BuiltinFnPtr {
        // Miri 的函数指针地址是合成地址, 低位不保证为 0 — 置 PURE_BIT 后再剥离会
        // 得到无效指针 (UB)。故 Miri 下不做低位编码: is_pure() 恒 false, 走通用
        // 调用路径, 语义等价, 仅跳过 pure 快速路径优化。
        #[cfg(miri)]
        {
            func
        }
        #[cfg(not(miri))]
        {
            // SAFETY: fn ptr 与 *const () 均为 thin pointer, 大小相同;
            // transmute 在指针类型间保 provenance.
            let p: *const () = unsafe { std::mem::transmute(func) };
            // map_addr 保留 provenance, 只改地址数值
            let p = p.map_addr(|a| a | Self::PURE_BIT);
            // SAFETY: 同上, 反向 transmute
            unsafe { std::mem::transmute(p) }
        }
    }

    /// 解码: 剥离 pure 位得到可调用函数指针
    #[inline]
    pub fn call_target(&self) -> BuiltinFnPtr {
        // Miri 下未编码低位 (见 encode_pure), 直接返回原指针
        #[cfg(miri)]
        {
            self.func
        }
        #[cfg(not(miri))]
        {
            let p: *const () = unsafe { std::mem::transmute(self.func) };
            let p = p.map_addr(|a| a & !Self::PURE_BIT);
            unsafe { std::mem::transmute(p) }
        }
    }

    /// 解码后的原始函数指针 (比较/查表用, 与注册时的指针一致)
    #[inline]
    pub fn raw_func(&self) -> BuiltinFnPtr {
        self.call_target()
    }

    /// 是否为纯函数
    #[inline]
    pub fn is_pure(&self) -> bool {
        // Miri 下不做低位编码 (见 encode_pure), 恒为非纯函数
        #[cfg(miri)]
        {
            false
        }
        #[cfg(not(miri))]
        {
            let p: *const () = unsafe { std::mem::transmute(self.func) };
            p.addr() & Self::PURE_BIT != 0
        }
    }

    /// 获取函数名的 &str（冷路径：traceback / Debug 输出）
    ///
    /// 从全局 BUILTIN_NAMES 表按原始 func 指针查找；未登记返回 ""。
    pub fn name_str(&self) -> &'static str {
        builtin_names::lookup(self.raw_func())
    }
}

impl std::fmt::Debug for BuiltinFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p: *const () = unsafe { std::mem::transmute(self.func) };
        f.debug_struct("BuiltinFn")
            .field("name", &self.name_str())
            .field("func", &p.addr())
            .finish()
    }
}

/// 全局内置函数名字表 — func 指针 → NUL 终止静态名字。
///
/// BuiltinFn 压缩到 8 字节后，name 无法随值存储；注册（impure 构造，冷路径）
/// 与查名（name_str，traceback 冷路径）都走此表。
///
/// 并发模型：std::sync::RwLock——注册只在库 open 时发生，查名只在错误路径；
/// 两者都远离热指令路径，锁开销可忽略。threaded feature 下多 GlobalState 并发
/// open 库时写锁保证安全。
mod builtin_names {
    use std::collections::HashMap;
    use std::sync::RwLock;

    // HashMap 按需扩容 — 注册总量 ~200 (9 个库)
    static NAMES: RwLock<Option<HashMap<super::BuiltinFnPtr, &'static str>>> = RwLock::new(None);

    /// 登记 func → name（重复登记以先注册者为准，语义与 C 静态注册一致）
    pub fn register(func: super::BuiltinFnPtr, name: *const u8) {
        if name.is_null() {
            return;
        }
        // SAFETY: name 来自注册点的 c"..." 静态字面量, 指向 NUL 终止字节数组
        let name_str: &'static str = unsafe {
            match std::ffi::CStr::from_ptr(name as *const std::ffi::c_char).to_str() {
                Ok(s) => std::mem::transmute::<&str, &'static str>(s),
                Err(_) => return,
            }
        };
        // 锁毒化不可能发生 (闭包内无 panic 路径); Err 时直接放弃登记,
        // name_str 返回 "" 不影响正确性
        if let Ok(mut guard) = NAMES.write() {
            guard
                .get_or_insert_with(HashMap::new)
                .entry(func)
                .or_insert(name_str);
        }
    }

    /// 按 func 指针查名 — 未登记返回 ""
    pub fn lookup(func: super::BuiltinFnPtr) -> &'static str {
        // 读锁失败(毒化) → 返回 "", traceback 降级为无名帧, 可接受。
        // 返回的 &'static str 不借用守卫: 表值本身是 'static 引用, copied() 拷出。
        let guard = match NAMES.read() {
            Ok(g) => g,
            Err(_) => return "",
        };
        match guard.as_ref() {
            Some(m) => m.get(&func).copied().unwrap_or(""),
            None => "",
        }
    }
}

// ============================================================================
// 规约：Rust 闭包 (RustClosure) — 携带 upvalues 的 Rust 原生函数
// ============================================================================

/// Rust 闭包 — 携带 upvalues 的 Rust 原生函数
///
/// 用于 coroutine.wrap 和 io.lines 等需要携带状态的内置函数。
/// 对应 C Lua 的 CClosure（lua_CFunction + upvalue），但使用 Rust ABI。
///
/// ## 与 `BuiltinFn` 的区别
///
/// `BuiltinFn` 是无状态的（仅函数指针 + 名字），适用于 `math.sin`、`string.len` 等
/// 纯函数。`RustClosure` 携带 `upvalues`，适用于需要绑定状态的场景：
/// - `coroutine.wrap(f)` 返回 `RustClosure { upvalues: [Thread] }`
/// - `io.lines(file)` 返回 `RustClosure { upvalues: [file_ptr_id, formats, ...] }`
///
/// ## upvalues 可变性
///
/// upvalues 使用 `Rc<RefCell<Vec<TValue>>>`，允许 io.lines 更新 `finished` 标志。
/// clone 时共享同一份 upvalues（对应 C Lua 中闭包共享 upvalue 的语义）。
///
/// ## 调用路径
///
/// 与 `BuiltinFn` 相同：op_call/op_tailcall/TFORCALL/pcall 直接通过函数指针派发，
/// 无需 tag 匹配或 metatable 查找。被调用函数从 `state.exec.stack[a]` 取回 RustClosure，
/// 再从 upvalues 取状态。
///
/// ## GC
///
/// RustClosure 需注册到 GC 遍历：GC 标记时遍历 upvalues 中的可达对象
/// （如 Thread、Table）。用 `Rc::as_ptr` 做 visited 去重。
///
/// Scenario: 创建并调用 Rust 闭包
/// Given: 一个 BuiltinFnPtr 函数指针和若干 upvalues
/// When: 用 `TValue::RustClosure(MyRc::new_in(slot, RustClosure { func, name, upvalues }))` 注册
/// Then: Lua 代码调用时，VM 直接通过函数指针调用，从 upvalues 取状态
#[derive(Clone)]
pub struct RustClosure<'a> {
    /// 函数指针 — 签名与 BuiltinFnPtr 相同
    pub func: BuiltinFnPtr,
    /// 函数名（NUL 终止 C 字符串，用于 traceback）
    pub name: *const u8,
    /// 上值列表 — 可变，存储 coroutine.wrap 的 Thread 或 io.lines 的文件/格式
    pub upvalues: Rc<RefCell<Vec<TValue<'a>>>>,
}

impl<'a> std::fmt::Debug for RustClosure<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = unsafe {
            if self.name.is_null() {
                "<null>".to_string()
            } else {
                std::ffi::CStr::from_ptr(self.name as *const std::ffi::c_char)
                    .to_string_lossy()
                    .into_owned()
            }
        };
        f.debug_struct("RustClosure")
            .field("name", &name)
            .field("func", &(self.func as usize))
            .field("nupvalues", &self.upvalues.borrow().len())
            .finish()
    }
}

impl<'a> RustClosure<'a> {
    /// 获取函数名的 &str
    pub fn name_str(&self) -> &'static str {
        if self.name.is_null() {
            ""
        } else {
            unsafe {
                std::ffi::CStr::from_ptr(self.name as *const std::ffi::c_char)
                    .to_str()
                    .unwrap_or("")
            }
        }
    }

    /// 估算 RustClosure 真实堆占用（用于 GC 内存计费）。
    /// RustClosure 不调用 register_object（无 gc_header），由 gc_extra_estimate 跟踪。
    pub fn gc_mem_size(&self) -> usize {
        // MyRc<RustClosure> 块 = RcHeader(16) + SlotAlloc(8) + RustClosure 自身
        // upvalues Vec 堆分配 = capacity * size_of::<TValue>()
        let upvals_cap = self.upvalues.borrow().capacity();
        RC_HEADER_SIZE
            + std::mem::size_of::<crate::mem::SlotAlloc>()
            + std::mem::size_of::<RustClosure>()
            + upvals_cap * std::mem::size_of::<TValue>()
            + 16 // Rc<RefCell<Vec>> 分配头估算
    }
}

// ============================================================================
// 规约：TValue — Lua 核心值类型
// ============================================================================

/// Lua 标记联合值 (Tagged Value) —— Lua 中最基础的值表示
///
/// 在 C 实现中，TValue 是一个 union + 一个 tag byte，tag 的低 4 位是类型，
/// 第 4-5 位是变体，第 6 位标记是否可 GC 回收。
///
/// Rust 版本使用 enum + struct，类型安全且无位操作。
///
/// **变体顺序不变式（性能关键，改动前先读这段）**：持有引用计数的变体（`ShortStr`
/// 起的连续 8 个）必须排在 tag 尾部，无引用计数的变体（`Nil` / `Boolean` /
/// `LightUserData` / `Integer` / `Float` / `LCFn` / `BuiltinFn`）必须全部排在它们
/// 之前。tag 值按声明顺序分配（实测判别式位掩码 `0x73e0` = 旧顺序下的 5..9 + 12..14），
/// 因此本顺序把「旧值是否需要释放引用计数」变成单次 `cmp tag, 7; jae`：
/// `write_stack` 的 `matches!` 落在连续 tag 区间上，LLVM 只发一条比较；不连续时
/// LLVM 退化为「范围保护 `ja` + 位掩码 `bt`/`jae`」两条分支（每个寄存器写多
/// ~4 uops，见 execute.rs `write_stack` 文档）。`impl Hash for TValue` 用的是手工
/// 数值 tag（与本顺序无关），故重排不改变哈希值、表迭代顺序或任何可观察行为。
///
/// Scenario: TValue 的构造与类型查询
/// Given: 创建各种类型的 TValue
/// When: 调用 .ty() 方法
/// Then: 返回正确的 LuaType
#[derive(Debug, Clone)]
pub enum TValue<'a> {
    /// nil 值，带子变体（标准 nil / 空槽 / 缺键）
    Nil(NilKind),
    /// 布尔值
    Boolean(bool),
    /// 轻量用户数据（裸指针）
    ///
    /// 注意：此变体仅用于真正的 light userdata（用户通过 `lua_pushlightuserdata`
    /// 保存的指针）。内置库函数标签已迁移到 `BuiltinFn` 变体，不再使用此变体
    /// 进行派发，避免用户指针被误判为内置函数。
    LightUserData(*mut std::ffi::c_void),
    /// 数值 —— 整数子变体
    Integer(LuaInteger),
    /// 数值 —— 浮点子变体
    Float(LuaNumber),
    /// 轻量 C 函数
    LCFn(LCFunction),
    /// Rust 原生内置函数（函数指针 + 静态名）
    ///
    /// 用于注册 Rust 实现的内置函数。调用时直接通过函数指针派发，
    /// 无需 tag 范围匹配。详见 `BuiltinFn` 类型文档。
    BuiltinFn(BuiltinFn),
    // ---- 以下 8 个变体持有引用计数 (tag 7..=14), 顺序不变式: 必须连续且位于尾部 ----
    /// 字符串（短字符串和长字符串）
    ShortStr(ArcRc<ShortString>),
    LongStr(SimpleRc<LongString, SlotAlloc>),
    /// 表
    Table(Table<'a>),
    /// Lua 闭包
    /// 使用 MyRc 共享指针而非 Box：clone 时仅增加引用计数（O(1)），不触发 malloc/free。
    /// 共享指针与 Box 一样支持 niche 优化（NonNull），TValue 大小保持 16 字节不变。
    /// 块分配/释放路由到 MemState allocator（见各类型 `new_in`）。
    LClosure(MyRc<LClosure<'a>, SlotAlloc>),
    /// C 闭包（同理用 allocator 路由的 MyRc）
    CClosure(MyRc<CClosure<'a>, SlotAlloc>),
    /// Rust 闭包（函数指针 + 可变 upvalues）
    ///
    /// 用于 coroutine.wrap 和 io.lines 等需要携带状态的内置函数。
    /// 详见 `RustClosure` 类型文档。
    RustClosure(MyRc<RustClosure<'a>, SlotAlloc>),
    /// 用户数据（同理用 allocator 路由的 MyRc）
    UserData(MyRc<Udata<'a>, SlotAlloc>),
    /// 线程/协程（同理用 allocator 路由的 MyRc）
    Thread(MyRc<LuaThread<'a>, SlotAlloc>),
}

#[derive(Debug, Clone)]
pub enum WeakValue<'a> {
    /// nil 值，带子变体（标准 nil / 空槽 / 缺键）
    Nil(NilKind),
    /// 布尔值
    Boolean(bool),
    /// 轻量用户数据（裸指针）
    LightUserData(*mut std::ffi::c_void),
    /// 数值 —— 整数子变体
    Integer(LuaInteger),
    /// 数值 —— 浮点子变体
    Float(LuaNumber),
    /// 字符串（短字符串和长字符串）
    ShortStr(ArcRc<ShortString>),
    LongStr(SimpleRc<LongString, SlotAlloc>),
    /// 表
    Table(WeakTable<'a>),
    /// Lua 闭包
    LClosure(WeakRc<LClosure<'a>, SlotAlloc>),
    /// C 闭包（同理用 allocator 路由的 WeakRc）
    CClosure(WeakRc<CClosure<'a>, SlotAlloc>),
    /// 轻量 C 函数
    LCFn(LCFunction),
    /// Rust 原生内置函数（函数指针 + 静态名）
    ///
    /// 用于注册 Rust 实现的内置函数。调用时直接通过函数指针派发，
    /// 无需 tag 范围匹配。详见 `BuiltinFn` 类型文档。
    BuiltinFn(BuiltinFn),
    /// Rust 闭包（函数指针 + 可变 upvalues）
    ///
    /// 用于 coroutine.wrap 和 io.lines 等需要携带状态的内置函数。
    /// 详见 `RustClosure` 类型文档。
    RustClosure(WeakRc<RustClosure<'a>, SlotAlloc>),
    /// 用户数据（同理用 allocator 路由的 WeakRc）
    UserData(WeakRc<Udata<'a>, SlotAlloc>),
    /// 线程/协程（同理用 allocator 路由的 WeakRc）
    Thread(WeakRc<LuaThread<'a>, SlotAlloc>),
}

/// nil 的子变体 —— 用 enum 替代 C 的 variant bit
///
/// Scenario: nil 的语义区分
/// Given: 一个值为 nil
/// When: 检查其 NilKind
/// Then: 可区分为标准 nil、空槽
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NilKind {
    /// 标准 nil (LUA_VNIL)
    Strict,
    /// 空槽位 (LUA_VEMPTY)
    Empty,
}

impl<'a> TValue<'a> {
    /// 获取 TValue 的 LuaType
    ///
    /// Scenario: 查询值的类型
    /// Given: 任意 TValue
    /// When: 调用 .ty()
    /// Then: 返回对应的 LuaType
    pub fn ty(&self) -> LuaType {
        match self {
            TValue::Nil(_) => LuaType::Nil,
            TValue::Boolean(_) => LuaType::Boolean,
            TValue::LightUserData(_) => LuaType::LightUserData,
            TValue::Integer(_) | TValue::Float(_) => LuaType::Number,
            TValue::ShortStr(_) | TValue::LongStr(_) => LuaType::String,
            TValue::Table(_) => LuaType::Table,
            TValue::LClosure(_)
            | TValue::CClosure(_)
            | TValue::LCFn(_)
            | TValue::BuiltinFn(_)
            | TValue::RustClosure(_) => LuaType::Function,
            TValue::UserData(_) => LuaType::UserData,
            TValue::Thread(_) => LuaType::Thread,
        }
    }

    /// 是否为 nil（任何子变体）
    ///
    /// Scenario: 判断是否为 nil
    /// Given: 一个 TValue::Nil(Strict)
    /// When: 调用 .is_nil()
    /// Then: 返回 true
    /// Given: 一个 TValue::Nil(Empty)
    /// When: 调用 .is_nil()
    /// Then: 返回 true
    /// Given: 一个 TValue::Boolean(true)
    /// When: 调用 .is_nil()
    /// Then: 返回 false
    pub fn is_nil(&self) -> bool {
        matches!(self, TValue::Nil(_))
    }

    /// 是否为布尔 false 或 nil（Lua 中的 "假" 值）
    ///
    /// Scenario: Lua 假值判断
    /// Given: TValue::Boolean(false)
    /// When: 调用 .is_false()
    /// Then: 返回 true
    /// Given: TValue::Nil(_)
    /// When: 调用 .is_false()
    /// Then: 返回 true
    /// Given: TValue::Boolean(true)
    /// When: 调用 .is_false()
    /// Then: 返回 false
    pub fn is_false(&self) -> bool {
        matches!(self, TValue::Boolean(false)) || self.is_nil()
    }

    /// 是否为数字（整数或浮点数）
    ///
    /// Scenario: 判断数字类型
    /// Given: TValue::Integer(42)
    /// When: 调用 .is_number()
    /// Then: 返回 true
    /// Given: TValue::Float(3.14)
    /// When: 调用 .is_number()
    /// Then: 返回 true
    /// Given: TValue::Nil(_)
    /// When: 调用 .is_number()
    /// Then: 返回 false
    pub fn is_number(&self) -> bool {
        matches!(self, TValue::Integer(_) | TValue::Float(_))
    }

    /// 是否为函数（任何类型）
    ///
    /// Scenario: 判断函数类型
    /// Given: TValue::LClosure(_)
    /// When: 调用 .is_function()
    /// Then: 返回 true
    /// Given: TValue::LCFn(_)
    /// When: 调用 .is_function()
    /// Then: 返回 true
    /// Given: TValue::BuiltinFn(_)
    /// When: 调用 .is_function()
    /// Then: 返回 true
    /// Given: TValue::RustClosure(_)
    /// When: 调用 .is_function()
    /// Then: 返回 true
    /// Given: TValue::LightUserData(_)  // 真正的 light userdata，非内置函数
    /// When: 调用 .is_function()
    /// Then: 返回 false
    pub fn is_function(&self) -> bool {
        matches!(
            self,
            TValue::LClosure(_)
                | TValue::CClosure(_)
                | TValue::LCFn(_)
                | TValue::BuiltinFn(_)
                | TValue::RustClosure(_)
        )
    }

    /// 判断值是否可调用
    ///
    /// 所有内置库已迁移到 BuiltinFn/RustClosure，coroutine.wrap 和 io.lines
    /// 返回 RustClosure。LightUserData 不再承担派发职责，回归本职。
    pub fn is_callable(&self) -> bool {
        self.is_function()
    }

    /// 尝试获取整数值
    ///
    /// Scenario: 从 TValue 提取整数
    /// Given: TValue::Integer(42)
    /// When: 调用 .as_integer()
    /// Then: 返回 Some(42)
    /// Given: TValue::Float(1.0)
    /// When: 调用 .as_integer()
    /// Then: 返回 Some(1)，如果 1.0 可以无损转为整数
    /// Given: TValue::Float(1.5)
    /// When: 调用 .as_integer()
    /// Then: 返回 None
    pub fn as_integer(&self) -> Option<LuaInteger> {
        match self {
            TValue::Integer(i) => Some(*i),
            TValue::Float(f) => {
                let i = *f as LuaInteger;
                if (i as LuaNumber) == *f {
                    Some(i)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 尝试获取浮点数值
    ///
    /// Scenario: 从 TValue 提取浮点数
    /// Given: TValue::Float(3.14)
    /// When: 调用 .as_float()
    /// Then: 返回 Some(3.14)
    /// Given: TValue::Integer(42)
    /// When: 调用 .as_float()
    /// Then: 返回 Some(42.0)
    pub fn as_float(&self) -> Option<LuaNumber> {
        match self {
            TValue::Float(f) => Some(*f),
            TValue::Integer(i) => Some(*i as LuaNumber),
            _ => None,
        }
    }
}

impl<'a> Default for TValue<'a> {
    fn default() -> Self {
        TValue::Nil(NilKind::Strict)
    }
}

impl<'a> PartialEq for TValue<'a> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TValue::Nil(a), TValue::Nil(b)) => a == b,
            (TValue::Boolean(a), TValue::Boolean(b)) => a == b,
            (TValue::LightUserData(a), TValue::LightUserData(b)) => a == b,
            (TValue::Integer(a), TValue::Integer(b)) => a == b,
            (TValue::Float(a), TValue::Float(b)) => a == b,
            (TValue::Integer(a), TValue::Float(b)) => {
                if b.is_nan() {
                    false
                } else {
                    (*a as LuaNumber) == *b
                }
            }
            (TValue::Float(a), TValue::Integer(b)) => {
                if a.is_nan() {
                    false
                } else {
                    *a == (*b as LuaNumber)
                }
            }
            (
                a @ (TValue::LongStr(_) | TValue::ShortStr(_)),
                b @ (TValue::LongStr(_) | TValue::ShortStr(_)),
            ) => lua_string_eq(a, b),
            (TValue::Table(a), TValue::Table(b)) => {
                // 与 Hash impl 一致: Rc 地址身份比较, 免 RefCell borrow
                // (borrow_mut 作用域内比较键相等性会 double-borrow panic)
                std::ptr::addr_eq(a.data.as_ptr(), b.data.as_ptr())
            }
            (TValue::LClosure(a), TValue::LClosure(b)) => a.gc_header.ptr_id == b.gc_header.ptr_id,
            (TValue::CClosure(a), TValue::CClosure(b)) => MyRc::ptr_eq(a, b),
            (TValue::LCFn(a), TValue::LCFn(b)) => {
                std::ptr::eq(a.func as *const (), b.func as *const ())
            }
            (TValue::BuiltinFn(a), TValue::BuiltinFn(b)) => {
                std::ptr::eq(a.func as *const (), b.func as *const ())
            }
            (TValue::RustClosure(a), TValue::RustClosure(b)) => MyRc::ptr_eq(a, b),
            (TValue::UserData(a), TValue::UserData(b)) => a.gc_header.ptr_id == b.gc_header.ptr_id,
            (TValue::Thread(a), TValue::Thread(b)) => Rc::ptr_eq(&a.context, &b.context),
            _ => false,
        }
    }
}

impl<'a> Eq for TValue<'a> {}

impl<'a> Hash for TValue<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            TValue::Nil(kind) => {
                0u8.hash(state);
                kind.hash(state);
            }
            TValue::Boolean(b) => {
                1u8.hash(state);
                b.hash(state);
            }
            TValue::LightUserData(p) => {
                2u8.hash(state);
                (*p as usize).hash(state);
            }
            TValue::Integer(i) => {
                3u8.hash(state);
                i.hash(state);
            }
            TValue::Float(f) => {
                if f.is_nan() {
                    4u8.hash(state);
                    0u64.hash(state);
                } else {
                    let i = *f as LuaInteger;
                    if (i as LuaNumber).to_bits() == f.to_bits() && *f != -0.0 {
                        3u8.hash(state);
                        i.hash(state);
                    } else {
                        4u8.hash(state);
                        f.to_bits().hash(state);
                    }
                }
            }
            s @ (TValue::LongStr(_) | TValue::ShortStr(_)) => {
                state.write_u64(lua_string_hash(s));
            }
            TValue::Table(t) => {
                // 身份哈希用 Rc 地址而非 gc_header.ptr_id: gc_header 已移入
                // TableData (Rc 内), 读它需 borrow — 若哈希发生在宿主表自身的
                // borrow_mut 作用域内 (如 t[t]=x 触发 rehash 对键 t 求哈希) 会
                // double-borrow panic (CI #78 all.lua attrib 复现)。
                // Rc 地址与 ptr_id 同为唯一身份: 同一表的所有 clone 共享同一
                // Rc → 同地址; 活表键持有 Rc 强引用, 地址不会复用。
                6u8.hash(state);
                (t.data.as_raw_ptr() as usize).hash(state);
            }
            TValue::LClosure(c) => {
                7u8.hash(state);
                c.gc_header.ptr_id.hash(state);
            }
            TValue::CClosure(c) => {
                8u8.hash(state);
                (&**c as *const CClosure as usize).hash(state);
            }
            TValue::LCFn(c) => {
                9u8.hash(state);
                (c.func as usize).hash(state);
            }
            TValue::BuiltinFn(b) => {
                12u8.hash(state);
                (b.func as usize).hash(state);
            }
            TValue::RustClosure(rc) => {
                13u8.hash(state);
                (rc.as_raw_ptr() as usize).hash(state);
            }
            TValue::UserData(u) => {
                10u8.hash(state);
                u.gc_header.ptr_id.hash(state);
            }
            TValue::Thread(t) => {
                11u8.hash(state);
                (Rc::as_ptr(&t.context) as usize).hash(state);
            }
        }
    }
}

// TValue 的 Display 实现用于调试输出
impl<'a> fmt::Display for TValue<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TValue::Nil(NilKind::Strict) => write!(f, "nil"),
            TValue::Nil(NilKind::Empty) => write!(f, "(empty)"),
            TValue::Boolean(b) => {
                // 体积优先: 避免 write!(f,"{}",b) 引入 core::fmt::num
                #[cfg(size_optimized)]
                {
                    f.write_str(if *b { "true" } else { "false" })
                }
                #[cfg(not(size_optimized))]
                {
                    write!(f, "{}", b)
                }
            }
            TValue::Integer(i) => {
                // 体积优先: 用 i64_to_string 避免 write!(f,"{}",i) 引入 core::fmt::num
                #[cfg(size_optimized)]
                {
                    f.write_str(&crate::float_utils::i64_to_string(*i))
                }
                #[cfg(not(size_optimized))]
                {
                    write!(f, "{}", i)
                }
            }
            TValue::Float(n) => write!(f, "{}", crate::float_utils::f64_to_string(*n)),
            s @ (TValue::LongStr(_) | TValue::ShortStr(_)) => write!(f, "{}", lua_string_as_str(s)),
            TValue::LightUserData(p) => write!(f, "lightuserdata({:p})", p),
            TValue::Table(_) => write!(f, "table"),
            TValue::LClosure(_) => write!(f, "function"),
            TValue::CClosure(_) => write!(f, "function"),
            TValue::LCFn(_) => write!(f, "function"),
            TValue::BuiltinFn(b) => write!(f, "function: {}", b.name_str()),
            TValue::RustClosure(rc) => write!(f, "function: {}", rc.name_str()),
            TValue::UserData(_) => write!(f, "userdata"),
            TValue::Thread(_) => write!(f, "thread"),
        }
    }
}

// size_optimized 模式: 手动实现 Debug 避免 f64::fmt 引入 flt2dec 代码 (~2.8KB)
// 同时避免 debug_tuple+field 格式化字符串 (会引入 core::unicode 转义表 ~4KB)
#[cfg(size_optimized)]
impl<'a> fmt::Debug for TValue<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TValue::Nil(n) => write!(f, "Nil({:?})", n),
            TValue::Boolean(b) => f.write_str(if *b {
                "Boolean(true)"
            } else {
                "Boolean(false)"
            }),
            TValue::LightUserData(p) => write!(f, "LightUserData({:p})", p),
            // 用 i64_to_string 避免 write!(f,"{}",i) 引入 core::fmt::num
            TValue::Integer(i) => write!(f, "Integer({})", crate::float_utils::i64_to_string(*i)),
            // 用 f64_to_string 避免直接格式化 f64 引入 flt2dec
            TValue::Float(n) => write!(f, "Float({})", crate::float_utils::f64_to_string(*n)),
            // 用 Display 而非 Debug 格式化字符串, 避免 str::Debug 引入 unicode 转义表
            s @ (TValue::LongStr(_) | TValue::ShortStr(_)) => write!(f, "Str({})", s.as_str()),
            TValue::Table(t) => write!(f, "Table({:p})", t),
            TValue::LClosure(lc) => write!(f, "LClosure({:p})", lc.as_raw_ptr()),
            TValue::CClosure(cc) => write!(f, "CClosure({:p})", cc.as_raw_ptr()),
            TValue::LCFn(lcf) => write!(f, "LCFn({:x})", lcf.func as usize),
            TValue::BuiltinFn(b) => write!(f, "BuiltinFn({})", b.name_str()),
            TValue::RustClosure(rc) => write!(f, "RustClosure({})", rc.name_str()),
            TValue::UserData(ud) => write!(f, "UserData({:p})", ud.as_raw_ptr()),
            TValue::Thread(t) => write!(f, "Thread({:p})", t.as_raw_ptr()),
        }
    }
}

// ============================================================================
// 规约：轻量 C 函数
// ============================================================================

/// 轻量 C 函数指针
///
/// Scenario: 创建轻量 C 函数
/// Given: 一个 extern "C" fn 指针
/// When: 构造 LCFunction
/// Then: 可通过 .call() 调用该函数
#[derive(Debug, Clone, Copy)]
pub struct LCFunction {
    pub func: unsafe extern "C" fn(*mut std::ffi::c_void) -> i32,
}

// ============================================================================
// 规约：表类型 (key→bucket 索引用 hashbrown::HashTable + SlotAlloc 重写)
// ============================================================================

/// Lua 表的数据部分 —— 被 [`TableDataRc`]（Rc 语义 + MemState allocator）包装以实现共享语义。
///
/// 将数据分离到 `TableData` 中，使得 `Table` 的克隆（仅克隆 Rc）共享同一份数据。
/// 这解决了 `_ENV` upvalue 与 `state.globals` 不同步的问题：
/// 克隆后的 Table 仍然指向同一份数据，修改对两者都可见。
pub struct TableData<'a> {
    pub gc_header: GCObjectHeader,
    /// 数组部分（1-based，索引 0 对应键 1）—— 缓冲区经 MemState allocator 分配
    pub array: TableVec<TValue<'a>>,
    /// 哈希部分：(key, value) 对 — 保持插入顺序用于 next() 遍历与 GC 标记
    /// 缓冲区经 MemState allocator 分配
    pub hash_buckets: TableVec<(TValue<'a>, TValue<'a>)>,
    /// `key → hash_buckets index` 索引 — 让 get / set / next 能 O(1) 定位。
    /// TableHashIndex = HashTable<(TValue, usize), SlotAlloc>: 开放寻址 + 预计算哈希 +
    /// eq 直接比较条目内 key 克隆，单跳定位（同 C Lua Node main position）。
    /// Option<Box<…>> 使空表不浪费结构体内存；Box 与表内缓冲区均经 MemState allocator。
    pub key_to_bucket: Option<TableKeyBox<'a>>,
    /// 元表
    pub metatable: Option<Box<Table<'a>>>,
}

impl<'a> TableData<'a> {
    /// 索引查找 — O(1) 返回 key 所在的 hash_buckets 下标
    #[inline]
    pub fn idx_get(&self, key: &TValue<'a>) -> Option<usize> {
        let ktb = self.key_to_bucket.as_ref()?;
        let hash = tvalue_fx_hash(key);
        ktb.find(hash, |(k, _)| k == key).map(|(_, idx)| *idx)
    }

    /// 索引写入前的容量预留 —— 保证 `key_to_bucket` 存在且还能容纳 1 个条目
    /// （对应 C 的 `luaH_newkey` 里先 `rehash` 再写 Node 的纪律）。
    ///
    /// 分配失败返回 [`crate::mem::AllocFailed`]，表内容不变。
    pub fn try_reserve_index(&mut self, _key: &TValue<'a>) -> Result<(), crate::mem::AllocFailed> {
        match self.key_to_bucket.as_mut() {
            Some(ktb) => ktb
                .try_reserve(1, |(k, _)| tvalue_fx_hash(k))
                .map_err(|_| crate::mem::AllocFailed),
            None => {
                // 索引盒子与 HashTable 缓冲区都沿用数组部分的 MemState allocator 槽
                let alloc = *self.array.allocator();
                let mut ktb = hashbrown::HashTable::with_capacity_in(0, alloc);
                ktb.try_reserve(1, |(k, _)| tvalue_fx_hash(k))
                    .map_err(|_| crate::mem::AllocFailed)?;
                self.key_to_bucket = Some(
                    crate::mem::AllocBox::try_new_in(ktb, alloc)
                        .map_err(|_| crate::mem::AllocFailed)?,
                );
                Ok(())
            }
        }
    }

    /// 索引插入 — key 必须不存在（insert_unique 不检查重复）。
    ///
    /// 前置条件：`key_to_bucket` 已存在且能容纳新条目（[`TableData::try_reserve_index`]
    /// 已成功）—— 即调用点不得在此处分配（否则内存耗尽时会 abort）。
    #[inline]
    pub fn idx_insert(&mut self, key: &TValue<'a>, idx: usize) {
        let hash = tvalue_fx_hash(key);
        let Some(ktb) = self.key_to_bucket.as_mut() else {
            debug_assert!(
                false,
                "idx_insert: key_to_bucket 未就绪（缺少 try_reserve_index）"
            );
            return;
        };
        ktb.insert_unique(hash, (key.clone(), idx), |(k, _)| tvalue_fx_hash(k));
    }

    /// 索引删除 — 返回被删 key 的 bucket 下标
    #[inline]
    pub fn idx_remove(&mut self, key: &TValue<'a>) -> Option<usize> {
        let ktb = self.key_to_bucket.as_mut()?;
        let hash = tvalue_fx_hash(key);
        ktb.find_entry(hash, |(k, _)| k == key).ok().map(|entry| {
            let ((_, idx), _) = entry.remove();
            idx
        })
    }
}

impl<'a> Drop for TableData<'a> {
    /// 迭代式释放，避免长链表等递归结构导致栈溢出。
    ///
    /// 编译器自动生成的 Drop 会递归释放 array/hash_buckets 中的 TValue::Table，
    /// 当 Table 通过 `next` 等字段形成长链表时（如 400K+ 节点），递归深度超过
    /// 主线程 8MB 栈限制导致栈溢出。
    ///
    /// 此实现将所有子 Table 引用收集到 pending 列表，然后迭代处理：
    /// - 独占引用（Rc::strong_count == 1）：取出 TableData 并继续收集子 Table
    /// - 共享引用：减少 Rc 引用计数，由其他持有者负责释放
    fn drop(&mut self) {
        fn extract<'a>(v: TValue<'a>, pending: &mut Vec<Table<'a>>) {
            if let TValue::Table(t) = v {
                pending.push(t);
            }
        }

        // 取走所有字段，避免编译器自动生成的 Drop 递归处理
        let mut pending: Vec<Table<'a>> = Vec::new();

        for v in self.array.drain(..) {
            extract(v, &mut pending);
        }
        for (k, v) in self.hash_buckets.drain(..) {
            extract(k, &mut pending);
            extract(v, &mut pending);
        }
        if let Some(mut ktb) = self.key_to_bucket.take() {
            for (k, _) in ktb.drain() {
                extract(k, &mut pending);
            }
        }
        if let Some(mt) = self.metatable.take() {
            pending.push(*mt);
        }

        // 迭代释放独占引用的 Table，避免递归 Drop
        while let Some(t) = pending.pop() {
            if t.data.strong_count() == 1 {
                // 独占引用：就地清空字段后交给 t.data 的 Drop 释放（字段已空，无递归）
                let mut td = t.data.borrow_mut();
                for v in td.array.drain(..) {
                    extract(v, &mut pending);
                }
                for (k, v) in td.hash_buckets.drain(..) {
                    extract(k, &mut pending);
                    extract(v, &mut pending);
                }
                if let Some(mut ktb) = td.key_to_bucket.take() {
                    for (k, _) in ktb.drain() {
                        extract(k, &mut pending);
                    }
                }
                if let Some(mt) = td.metatable.take() {
                    pending.push(*mt);
                }
            }
            // 共享引用：上面未清空，strong 减计数后由其他持有者负责释放
        }
    }
}

/// Lua 表 —— 关联数组，包含数组部分和哈希部分。
///
/// 数据字段包装在 [`TableDataRc`]（Rc 语义）中，克隆 `Table` 时共享同一份数据，
/// 而非深拷贝。这保证了 `_ENV` upvalue 与 `state.globals` 始终同步。
///
/// `gc_header` 保留在 `Table` 上（不在 `TableData` 中），因为 `ptr_id` 需要在克隆时保持一致。
///
/// 方法实现见 [crate::table]。
pub struct Table<'a> {
    /// 共享数据 —— 克隆 Table 时仅增加 Rc 引用计数
    pub data: TableDataRc<'a>,
}

impl<'a> Clone for Table<'a> {
    /// 克隆 Table：仅克隆 `Rc`（共享数据），并克隆 `gc_header`（保持同一 `ptr_id`）。
    fn clone(&self) -> Self {
        Table {
            data: self.data.clone(),
        }
    }
}

impl<'a> fmt::Debug for Table<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Table")
            .field("ptr_id", &self.data.borrow().gc_header.ptr_id)
            .finish_non_exhaustive()
    }
}

pub struct WeakTable<'a> {
    /// 共享数据 —— 克隆 Table 时仅增加 Rc 引用计数
    pub data: TableDataWeak<'a>,
}

impl<'a> Clone for WeakTable<'a> {
    /// 克隆 Table：仅克隆 `Rc`（共享数据），并克隆 `gc_header`（保持同一 `ptr_id`）。
    fn clone(&self) -> Self {
        WeakTable {
            data: self.data.clone(),
        }
    }
}

impl<'a> fmt::Debug for WeakTable<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WeakTable").finish_non_exhaustive()
    }
}

impl<'a> WeakTable<'a> {
    pub fn upgrade(&self) -> Option<Table<'a>> {
        self.data.upgrade().map(|td| Table { data: td })
    }
}

// ============================================================================
// TableDataRc — Table 数据共享指针（allocator 路由块）
// ============================================================================

/// Table 数据的共享指针：[`MyRc`]（strong + weak 计数）包装 `RefCell<TableData>`。
/// weak 侧为 [`WeakRc`]（弱表 GC 用，见 `GlobalState::weak_tables`）。
///
/// 块布局 = RcHeader + 分配器(A) + payload；分配/释放由 `MyRc` 的 Drop 按块内
/// 槽统一路由（对应 C Lua 中 Table 头走 global_State 的 allocf）。
pub type TableDataRc<'a> = MyRc<RefCell<TableData<'a>>, SlotAlloc>;
pub type TableDataWeak<'a> = WeakRc<RefCell<TableData<'a>>, SlotAlloc>;

impl<'a> TableDataRc<'a> {
    /// 创建 TableData 共享块。`alloc` 为 MemState 分配器 [`SlotAlloc`]：
    /// 分配/释放均路由到该 allocator。
    pub fn try_new_table(
        alloc: SlotAlloc,
        td: TableData<'a>,
    ) -> Result<Self, crate::mem::AllocFailed> {
        MyRc::try_new_in(alloc, RefCell::new(td)).map_err(|_| crate::mem::AllocFailed)
    }

    /// 【测试专用】分配失败即 panic 的便捷构造。
    #[cfg(test)]
    pub fn new_table(alloc: SlotAlloc, td: TableData<'a>) -> Self {
        Self::try_new_table(alloc, td)
            .ok()
            .expect("TableDataRc::new_table: 测试分配失败")
    }
}

// ============================================================================
// 规约：函数类型
// ============================================================================

/// Lua 闭包（Lua 侧）
///
/// Scenario: Lua 闭包包含原型和上值
/// Given: 一个 Proto 和 3 个 UpValue
/// When: 构造 LClosure
/// Then: nupvalues = 3, p 指向该 Proto
#[derive(Debug, Clone)]
pub struct LClosure<'a> {
    pub gc_header: GCObjectHeader,
    /// 函数原型（Rc 共享，clone 为 O(1)，避免每次 OP_CALL 深拷贝整个 Proto）
    pub proto: Rc<Proto<'a>>,
    /// 上值列表（共享引用，多个闭包可共享同一个 UpVal）。
    ///
    /// 使用 Rc<RefCell<Vec>> 包装，使 LClosure clone 时所有副本共享同一个 upvals Vec。
    /// 这样 debug.upvaluejoin 修改栈上副本的 upvals 会影响所有共享同一 Rc 的闭包
    /// （对应 C 中 Closure 是堆分配对象、栈上存 Closure* 指针的语义）。
    pub upvals: Rc<RefCell<UpValVec<'a>>>,
}

/// C 闭包 —— C 函数 + 捕获的上值
///
/// Scenario: C 闭包
/// Given: 一个 C 函数指针和 2 个上值
/// When: 构造 CClosure
/// Then: f 指向该函数，upvalue 有 2 个元素
#[derive(Debug, Clone)]
pub struct CClosure<'a> {
    /// C 函数指针
    pub f: unsafe extern "C" fn(*mut std::ffi::c_void) -> i32,
    /// 上值列表
    pub upvalue: RefCell<Vec<TValue<'a>>>,
}

impl<'a> LClosure<'a> {
    /// 估算 LClosure 真实堆占用（用于 GC 内存计费）。
    /// LClosure 通过 register_object 注册，但 size_of::<LClosure>() 不含 upvals 容量。
    /// Proto 由 gc_extra_estimate 单独跟踪（共享，避免重复计费）。
    pub fn gc_mem_size(&self) -> usize {
        // MyRc<LClosure> 路由块 = RcHeader(16) + SlotAlloc(8) + LClosure 自身
        // upvals: Rc<RefCell<UpValVec>>，Vec 堆分配 = capacity * size_of::<UpValRef>()
        let upvals_cap = self.upvals.borrow().capacity();
        RC_HEADER_SIZE
            + std::mem::size_of::<crate::mem::SlotAlloc>()
            + std::mem::size_of::<LClosure>()
            + upvals_cap * std::mem::size_of::<UpValRef>()
            + 16 // Rc<RefCell<Vec>> 分配头估算
    }
}

impl<'a> CClosure<'a> {
    /// 估算 CClosure 真实堆占用（用于 GC 内存计费）。
    /// CClosure 不调用 register_object（无 gc_header），由 gc_extra_estimate 跟踪。
    pub fn gc_mem_size(&self) -> usize {
        // MyRc<CClosure> 路由块 = RcHeader(16) + SlotAlloc(8) + CClosure 自身
        // upvalue: Vec<TValue>，Vec 堆分配 = capacity * size_of::<TValue>()
        RC_HEADER_SIZE
            + std::mem::size_of::<crate::mem::SlotAlloc>()
            + std::mem::size_of::<CClosure>()
            + self.upvalue.borrow().capacity() * std::mem::size_of::<TValue>()
    }
}

// ============================================================================
// 规约：上值 (UpValue)
// ============================================================================

/// 上值 —— 闭包捕获的外部局部变量
///
/// 上值有两种状态：打开（指向栈上的变量）和关闭（持有值的副本）。
///
/// Scenario: 打开的上值
/// Given: 上值指向栈上索引 2 的位置
/// When: 读取上值
/// Then: 返回栈上对应位置的值
///
/// Scenario: 关闭的上值
/// Given: 上值已关闭，持有值 TValue::Integer(42)
/// When: 读取上值
/// Then: 返回 TValue::Integer(42)
#[derive(Debug, Clone)]
pub enum UpVal<'a> {
    /// 打开的上值（指向栈上的活跃变量）
    Open {
        /// 栈上位置索引
        stack_index: usize,
        /// 链表中的下一个上值索引
        next: Option<usize>,
        /// 链表中的上一个上值索引
        previous: Option<usize>,
        /// 是否为 to-be-closed 上值（对应 C 的 uv->tbc）
        /// close 时若为 true，需调用 __close 元方法
        tbc: bool,
    },
    /// 关闭的上值（持有值的副本）
    Closed {
        /// 存储的值
        value: TValue<'a>,
    },
}

impl<'a> UpVal<'a> {
    pub fn is_open(&self) -> bool {
        matches!(self, UpVal::Open { .. })
    }

    pub fn level(&self) -> Option<usize> {
        match self {
            UpVal::Open { stack_index, .. } => Some(*stack_index),
            UpVal::Closed { .. } => None,
        }
    }
}

/// 上值数组 — 小数组内联存储，避免小闭包的上值 Vec 缓冲分配。
///
/// 对应 C Lua 的 `LClosure.upvals`（内联柔性数组）。闭包上值数量在创建后固定，
/// 常见闭包 ≤ 4 个上值，内联存储使创建闭包时少一次堆分配
/// （`Rc<RefCell<Vec>>` 的 Vec 缓冲由内联数组替代）。
///
/// 通过 `Deref<Target=[UpValRef]>` 暴露切片 API，调用点（`.len()` / `.iter()` /
/// `[idx]` / `.is_empty()`）无需改动。
#[derive(Debug)]
pub struct UpValVec<'a> {
    len: u8,
    inline: [std::mem::MaybeUninit<UpValRef<'a>>; 4],
    heap: Option<Box<[UpValRef<'a>]>>,
}

impl<'a> Clone for UpValVec<'a> {
    fn clone(&self) -> Self {
        let mut v = UpValVec::new();
        for x in self.iter() {
            v.push(x.clone());
        }
        v
    }
}

impl<'a> UpValVec<'a> {
    pub fn new() -> Self {
        UpValVec {
            len: 0,
            inline: std::array::from_fn(|_| std::mem::MaybeUninit::uninit()),
            heap: None,
        }
    }

    /// 存储容量：内联为固定 4，堆为实际长度。
    /// （GC 内存计费用，见 `LClosure::gc_mem_size`。）
    pub fn capacity(&self) -> usize {
        match &self.heap {
            Some(h) => h.len(),
            None => 4,
        }
    }

    pub fn push(&mut self, val: UpValRef<'a>) {
        if self.heap.is_none() && self.len as usize >= 4 {
            // 内联已满: 迁移到堆
            let mut v = Vec::with_capacity(8);
            for i in 0..self.len as usize {
                // SAFETY: 迁移只发生一次, i < len 的元素均已初始化
                v.push(unsafe { self.inline[i].assume_init_read() });
            }
            v.push(val);
            self.heap = Some(v.into_boxed_slice());
            self.len += 1;
            return;
        }
        match &mut self.heap {
            Some(h) => {
                let mut v = h.to_vec();
                v.push(val);
                *h = v.into_boxed_slice();
            }
            None => {
                // SAFETY: len < 4, 槽位未初始化, ptr::write 语义
                self.inline[self.len as usize].write(val);
            }
        }
        self.len += 1;
    }

    fn as_slice(&self) -> &[UpValRef<'a>] {
        match &self.heap {
            Some(h) => &h[..self.len as usize],
            // SAFETY: 0..len 的槽位均已初始化
            None => unsafe {
                std::slice::from_raw_parts(
                    self.inline.as_ptr() as *const UpValRef,
                    self.len as usize,
                )
            },
        }
    }

    fn as_mut_slice(&mut self) -> &mut [UpValRef<'a>] {
        match &mut self.heap {
            Some(h) => &mut h[..self.len as usize],
            // SAFETY: 0..len 的槽位均已初始化
            None => unsafe {
                std::slice::from_raw_parts_mut(
                    self.inline.as_mut_ptr() as *mut UpValRef,
                    self.len as usize,
                )
            },
        }
    }
}

impl<'a> Default for UpValVec<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> From<Vec<UpValRef<'a>>> for UpValVec<'a> {
    fn from(v: Vec<UpValRef<'a>>) -> Self {
        if v.len() <= 4 {
            let mut u = UpValVec::new();
            for x in v {
                u.push(x);
            }
            u
        } else {
            UpValVec {
                len: v.len() as u8,
                inline: std::array::from_fn(|_| std::mem::MaybeUninit::uninit()),
                heap: Some(v.into_boxed_slice()),
            }
        }
    }
}

impl<'a> std::ops::Deref for UpValVec<'a> {
    type Target = [UpValRef<'a>];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<'a> std::ops::DerefMut for UpValVec<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<'a> Drop for UpValVec<'a> {
    fn drop(&mut self) {
        if self.heap.is_none() {
            // inline 部分：MaybeUninit 不自动 drop，需手动
            for i in 0..self.len as usize {
                // SAFETY: 0..len 的槽位由 push 初始化
                unsafe {
                    self.inline[i].assume_init_drop();
                }
            }
        }
    }
}

// ============================================================================
// 规约：函数原型 (Proto)
// ============================================================================

/// 指令 —— 32 位无符号整数
pub type Instruction = u32;

/// 上值描述符
///
/// Scenario: 上值描述符
/// Given: upvalue 名为 "x", 在栈上, 索引为 0
/// When: 构造 UpvalDesc
/// Then: instack = true, idx = 0
#[derive(Debug, Clone)]
pub struct UpvalDesc<'a> {
    /// 上值名称（调试信息）
    pub name: Option<TValue<'a>>,
    /// 是否在栈上
    pub in_stack: bool,
    /// 索引（栈索引或外层函数上值列表索引）
    pub idx: u8,
    /// For in_stack upvalues: index in parent's locals array (used by mark_block_upval)
    /// For non-in_stack upvalues: unused (0)
    pub parent_local_idx: usize,
    /// 变量 kind (VDKREG/RDKCONST/RDKCTC 等，对应 C 的 Upvaldesc.kind)
    pub kind: u8,
}

/// 局部变量描述符（调试信息）
///
/// Scenario: 局部变量
/// Given: 变量名为 "i", startpc = 0, endpc = 5
/// When: 构造 LocVar
/// Then: varname = "i", startpc = 0, endpc = 5
#[derive(Debug, Clone)]
pub struct LocVar<'a> {
    /// 变量名
    pub varname: Option<TValue<'a>>,
    /// 变量活跃的起始 PC
    pub start_pc: i32,
    /// 变量失效的起始 PC
    pub end_pc: i32,
}

/// 绝对行号信息
#[derive(Debug, Clone)]
pub struct AbsLineInfo {
    pub pc: i32,
    pub line: i32,
}

/// 函数原型 —— 编译后的函数体
///
/// Scenario: 函数原型的完整结构
/// Given: 一个 Lua 函数 "function add(a,b) return a+b end"
/// When: 编译为 Proto
/// Then: numparams = 2, code 为非空, maxstacksize ≥ 2
#[derive(Debug, Clone)]
pub struct Proto<'a> {
    /// 固定参数数量
    pub num_params: u8,
    /// 标志位 (PF_VAHID, PF_VATAB)
    pub flag: u8,
    /// 最大栈大小
    pub max_stack_size: u8,
    /// 上值数量
    pub size_upvalues: i32,
    /// 常量数量
    pub size_k: i32,
    /// 代码大小
    pub size_code: i32,
    /// 行信息大小
    pub size_line_info: i32,
    /// 子原型数量
    pub size_p: i32,
    /// 局部变量数量
    pub size_loc_vars: i32,
    /// 绝对行信息数量
    pub size_abs_line_info: i32,
    /// 定义起始行
    pub line_defined: i32,
    /// 定义结束行
    pub last_line_defined: i32,
    /// 常量表 — Rc<Vec> 避免 op_call 中深拷贝（perf: 省 ~5.3% malloc+memmove）
    pub constants: Rc<Vec<TValue<'a>>>,
    /// 指令序列 — Rc<Vec> 避免 op_call 中深拷贝
    pub code: Rc<Vec<Instruction>>,
    /// 子原型 — Rc<Vec> 避免 op_call 中 O(n) Vec 分配与 Rc 指针数组深拷贝
    /// （op_call 调用频度高，原 `proto.protos.clone()` 每次都 malloc 新 Vec；
    ///  改为 Rc 共享后变为 O(1) 引用计数；编译期通过 Rc::make_mut 独占修改）
    pub protos: Rc<Vec<Rc<Proto<'a>>>>,
    /// 上值描述 — Rc<Vec> 避免 op_call 中深拷贝
    pub upvalues: Rc<Vec<UpvalDesc<'a>>>,
    /// 行号差值数组
    pub line_info: Vec<i8>,
    /// 绝对行号信息
    pub abs_line_info: Vec<AbsLineInfo>,
    /// 局部变量
    pub loc_vars: Vec<LocVar<'a>>,
    /// 源文件名（调试信息）
    pub source: Option<TValue<'a>>,
}

impl<'a> Proto<'a> {
    /// 是否为变参函数
    ///
    /// Scenario: 判断变参函数
    /// Given: flag 设置了 PF_VAHID 或 PF_VATAB
    /// When: 调用 .is_vararg()
    /// Then: 返回 true
    pub fn is_vararg(&self) -> bool {
        (self.flag & (PF_VAHID | PF_VATAB)) != 0
    }

    /// 估算 Proto 真实堆占用（用于 GC 内存计费）。
    /// Proto 不调用 register_object（无 gc_header），由 gc_extra_estimate 跟踪。
    /// Proto 创建后字段不再扩容，故用 capacity 精确计费。
    pub fn gc_mem_size(&self) -> usize {
        // Rc<Proto> 堆分配 = Proto 自身（Rc 引用计数在 RcBox 内）
        // Proto 内部多个 Rc<Vec> 共享，此处按 capacity 计费（共享时由首个持有者承担）
        std::mem::size_of::<Proto>()
            + self.constants.capacity() * std::mem::size_of::<TValue>()
            + self.code.capacity() * std::mem::size_of::<Instruction>()
            + self.protos.capacity() * std::mem::size_of::<Rc<Proto>>()
            + self.upvalues.capacity() * std::mem::size_of::<UpvalDesc>()
            + self.line_info.capacity() * std::mem::size_of::<i8>()
            + self.abs_line_info.capacity() * std::mem::size_of::<AbsLineInfo>()
            + self.loc_vars.capacity() * std::mem::size_of::<LocVar>()
            // Rc<Vec> 分配头估算（每个 Rc<Vec> 约 16 字节分配头）
            + 16 * 5
    }
}

/// 原型标志位
pub const PF_VAHID: u8 = 1;
pub const PF_VATAB: u8 = 2;

// ============================================================================
// 规约：用户数据 (UserData)
// ============================================================================

/// 用户数据 —— **变长对象**（单块分配），对应 C 的 `Udata`（`lobject.h`）。
///
/// 布局：`固定头 | uv[nuvalue] | data[ceil(len/8) * 8]`，全部在同一分配块内，
/// 对应 C 的 `sizeudata(nuv, nb) = udatamemoffset(nuv) + nb`。
/// 用户值区与数据区都在固定头之后，均按 8 字节对齐（C 的 `LUAI_MAXALIGN`
/// 在默认配置下即 8；C 用 `UValue` 联合保证，这里用 `[u64]` 尾数组保证）。
///
/// 与 C 的差异：GC 头的宽度与字段构成不同（C 的 `CommonHeader` 是
/// `GCObject *next; lu_byte tt; lu_byte marked` = 16 字节，另加 `gclist`；
/// 这里是 8 字节的 `GCObjectHeader`），故固定头之后的偏移与 C 不同。
/// 但 C 侧只通过 `getudatamem`（= [`Udata::data_ptr`]）、`lua_rawlen`（`len`）
/// 与 `lua_get/setiuservalue`（用户值）观察 userdata，故不构成 ABI 差异。
#[repr(C)]
pub struct Udata<'a> {
    /// GC 对象头 — ptr_id 在克隆时保持一致，用于哈希和相等性比较
    pub gc_header: GCObjectHeader,
    /// 用户值数量（对应 C `Udata.nuvalue`）
    pub nuvalue: u16,
    /// 数据长度（对应 C `Udata.len`，`lua_rawlen` 返回值）
    pub len: usize,
    /// 元表
    pub metatable: RefCell<Option<Table<'a>>>,
    /// 变长区：前 `nuvalue * UV_WORDS` 个字是用户值（`TValue`），
    /// 随后 `ceil(len/8)` 个字是原始数据区。
    pub tail: [u64],
}

/// `Udata` 的固定头（Sized）—— 用于取字段偏移与固定部分大小。
///
/// 字段与 `Udata` 的固定前缀逐位一致（同为 `#[repr(C)]`），
/// 故 `offset_of!(UdataFixed, X) == offset_of!(Udata, X)` 且
/// `size_of::<UdataFixed>() == offset_of!(Udata, tail)`。
#[repr(C)]
pub struct UdataFixed<'a> {
    pub gc_header: GCObjectHeader,
    pub nuvalue: u16,
    pub len: usize,
    pub metatable: RefCell<Option<Table<'a>>>,
}

/// 每个用户值占的 u64 字数（`TValue` = 16 字节）。
const _: () = {
    assert!(std::mem::size_of::<TValue<'static>>() == 16);
    assert!(std::mem::align_of::<TValue<'static>>() == 8);
};

/// 最大分配大小（对应 C 的 `MAX_SIZE`：`sizeof(size_t) < sizeof(lua_Integer) ?
/// MAX_SIZET : cast_sizet(LUA_MAXINTEGER)`，与 `stdlib/string_lib.rs` 同式）。
pub const MAX_SIZE: usize =
    if std::mem::size_of::<usize>() < std::mem::size_of::<crate::config::LuaInteger>() {
        usize::MAX
    } else {
        crate::config::MAX_INTEGER as usize
    };

impl<'a> Udata<'a> {
    /// 固定头大小 = 变长区起始偏移（对应 C 的 `offsetof(Udata, uv)` 之下的部分）。
    pub const FIXED_SIZE: usize = std::mem::size_of::<UdataFixed<'a>>();
    /// 每个用户值占的 u64 字数。
    pub const UV_WORDS: usize = std::mem::size_of::<TValue<'a>>() / 8;
    /// 数据区 u64 字数 = `ceil(len / 8)`。
    const LEN_OFF: usize = std::mem::offset_of!(UdataFixed<'a>, len);
    const NV_OFF: usize = std::mem::offset_of!(UdataFixed<'a>, nuvalue);
    const MT_OFF: usize = std::mem::offset_of!(UdataFixed<'a>, metatable);

    /// 数据区 u64 字数。
    #[inline]
    pub fn data_words(len: usize) -> usize {
        len.div_ceil(8)
    }

    /// 变长区 u64 字数。
    #[inline]
    fn tail_words(nuvalue: usize, len: usize) -> usize {
        nuvalue * Self::UV_WORDS + Self::data_words(len)
    }

    /// payload（不含 `RcHeader` / 分配器槽）的字节数。
    #[inline]
    pub fn payload_size(nuvalue: usize, len: usize) -> usize {
        Self::FIXED_SIZE + Self::tail_words(nuvalue, len) * 8
    }

    /// 上述大小的带检查版本：超出 C 的 `MAX_SIZE` 或算术溢出时返回 `None`
    /// （对应 C `luaS_newudata` 的 `sizeudata` 溢出检查 → `luaM_toobig`）。
    /// 创建路径必须用它 —— 否则 `payload_size` 回绕会分配一块远小于声明大小的内存。
    #[inline]
    pub fn checked_payload_size(nuvalue: usize, len: usize) -> Option<usize> {
        let uv_bytes = nuvalue.checked_mul(std::mem::size_of::<TValue<'a>>())?;
        let offset = Self::FIXED_SIZE.checked_add(uv_bytes)?; // = udatamemoffset(nuv)
                                                              // C: `if (s > MAX_SIZE - udatamemoffset(nuvalue)) luaM_toobig(L);`
        if len > MAX_SIZE.saturating_sub(offset) {
            return None;
        }
        offset.checked_add(Self::data_words(len) * 8)
    }

    /// 用户值切片（长度恒为 `nuvalue`）。
    #[inline]
    pub fn user_values(&self) -> &[TValue<'a>] {
        // SAFETY: 变长区前 nuvalue 个 TValue 由 init_payload 全部写入（16B / 8 对齐）。
        unsafe {
            std::slice::from_raw_parts(
                self.tail.as_ptr() as *const TValue<'a>,
                self.nuvalue as usize,
            )
        }
    }

    /// 数据区起始指针（对应 C 的 `getudatamem(u)`，`lua_touserdata` 返回值）。
    #[inline]
    pub fn data_ptr(&self) -> *mut u8 {
        unsafe {
            self.tail
                .as_ptr()
                .add(self.nuvalue as usize * Self::UV_WORDS) as *mut u8
        }
    }

    // ---- 以下为基于 payload 薄指针（`MyRc::as_raw_ptr()`）的访问器 ----
    // 写操作**不能**经 `&self` 派生指针（Tree Borrows 下共享引用不可写），
    // 必须从块基址（payload 指针）重新派生。

    /// 由 payload 薄指针重建胖指针。
    ///
    /// # Safety
    /// `payload` 必须是本类型 payload 的起点（`MyRc::as_raw_ptr()`）。
    #[inline]
    pub unsafe fn from_payload(payload: *mut u8) -> *mut Udata<'a> {
        let nuvalue = *(payload.add(Self::NV_OFF) as *const u16) as usize;
        let len = *(payload.add(Self::LEN_OFF) as *const usize);
        // 胖指针 = (数据地址, DST 元数据)，与 `(ptr, len)` 同布局
        // （与 strings.rs 的 `LongString::as_ref` 同一手法；`ptr_metadata` 仍是 unstable）。
        std::mem::transmute((payload as *mut (), Self::tail_words(nuvalue, len)))
    }

    /// payload 中数据区起始指针。
    ///
    /// # Safety
    /// `payload` 必须是本类型 payload 的起点。
    #[inline]
    pub unsafe fn data_ptr_of(payload: *mut u8) -> *mut u8 {
        let nuvalue = *(payload.add(Self::NV_OFF) as *const u16) as usize;
        payload.add(Self::FIXED_SIZE + nuvalue * std::mem::size_of::<TValue<'a>>())
    }

    /// payload 中元表槽（`RefCell`，可经共享引用修改）。
    ///
    /// # Safety
    /// `payload` 必须是本类型 payload 的起点。
    #[inline]
    pub unsafe fn metatable_of<'b>(payload: *mut u8) -> &'b RefCell<Option<Table<'a>>> {
        &*(payload.add(Self::MT_OFF) as *const RefCell<Option<Table<'a>>>)
    }

    /// 就地写第 `n` 个用户值（1-based），对应 C 的 `setuservalue`。
    ///
    /// # Safety
    /// `payload` 必须是本类型 payload 的起点，且 `1 <= n <= nuvalue`。
    #[inline]
    pub unsafe fn set_user_value(payload: *mut u8, n: usize, val: TValue<'a>) {
        let slot = payload.add(Self::FIXED_SIZE + (n - 1) * std::mem::size_of::<TValue<'a>>())
            as *mut TValue<'a>;
        std::ptr::drop_in_place(slot);
        std::ptr::write(slot, val);
    }

    /// 初始化整块 payload（固定头 + 用户值 = nil + 数据区 = 0）。
    ///
    /// 元表由调用方在分配成功后经 [`Udata::metatable_of`] 写入（初始化成 `None`）。
    ///
    /// # Safety
    /// `payload` 必须是按 [`Udata::payload_size`] 分配、尚未初始化的 payload 起点。
    pub unsafe fn init_payload(payload: *mut u8, nuvalue: usize, len: usize) {
        debug_assert!(nuvalue <= u16::MAX as usize, "nuvalue 必须已夹紧到 u16");
        let nuv = nuvalue as u16;
        std::ptr::write(
            payload.add(std::mem::offset_of!(UdataFixed<'a>, gc_header)) as *mut GCObjectHeader,
            GCObjectHeader::new(),
        );
        std::ptr::write(payload.add(Self::NV_OFF) as *mut u16, nuv);
        std::ptr::write(payload.add(Self::LEN_OFF) as *mut usize, len);
        std::ptr::write(
            payload.add(Self::MT_OFF) as *mut RefCell<Option<Table<'a>>>,
            RefCell::new(None),
        );
        let uv = payload.add(Self::FIXED_SIZE) as *mut TValue<'a>;
        for i in 0..nuvalue {
            std::ptr::write(uv.add(i), TValue::Nil(crate::objects::NilKind::Strict));
        }
        let data = payload.add(Self::FIXED_SIZE + nuvalue * std::mem::size_of::<TValue<'a>>());
        std::ptr::write_bytes(data, 0, Self::data_words(len) * 8);
    }

    /// 估算 Udata 真实堆占用（用于 GC 内存计费）。
    ///
    /// 整块分配 = `RcHeader` + 分配器槽 + payload（固定头 + 用户值 + 数据区），
    /// 由 [`crate::alloc::MyRc::block_size`] 精确给出（含尾部对齐填充）。
    pub fn gc_mem_size(&self) -> usize {
        crate::alloc::MyRc::<Udata<'a>, SlotAlloc>::block_size(Self::payload_size(
            self.nuvalue as usize,
            self.len,
        ))
    }
}

unsafe impl<'a> crate::alloc::RcPayload for Udata<'a> {
    const ALIGN: usize = std::mem::align_of::<u64>();

    unsafe fn as_ref<'b>(payload: *const u8) -> &'b Udata<'a> {
        let nuvalue = *(payload.add(Self::NV_OFF) as *const u16) as usize;
        let len = *(payload.add(Self::LEN_OFF) as *const usize);
        // 胖指针 = (数据地址, DST 元数据)，与 `(ptr, len)` 同布局
        let p: *const Udata<'a> = std::mem::transmute((payload, Self::tail_words(nuvalue, len)));
        &*p
    }

    unsafe fn drop_payload(payload: *mut u8) {
        // 固定头（GCObjectHeader / nuvalue / len 无析构）+ 元表 + 各用户值。
        // 数据区（u64）无析构。
        std::ptr::drop_in_place(payload.add(Self::MT_OFF) as *mut RefCell<Option<Table<'a>>>);
        let nuvalue = *(payload.add(Self::NV_OFF) as *const u16) as usize;
        let uv = payload.add(Self::FIXED_SIZE) as *mut TValue<'a>;
        for i in 0..nuvalue {
            std::ptr::drop_in_place(uv.add(i));
        }
    }

    unsafe fn payload_size(payload: *const u8) -> usize {
        let nuvalue = *(payload.add(Self::NV_OFF) as *const u16) as usize;
        let len = *(payload.add(Self::LEN_OFF) as *const usize);
        Self::FIXED_SIZE + Self::tail_words(nuvalue, len) * 8
    }
}

// ============================================================================
// 规约：线程/协程
// ============================================================================

/// 调用帧 — 保存 caller 的 VM 执行上下文（对应 C 的 CallInfo 中 Lua 部分）
///
/// 当 op_call 调用一个 LClosure 时，把当前 code/base/pc 等保存到 CallFrame，
/// 切换到被调用函数的原型；op_return 时从 CallFrame 恢复。
#[derive(Debug, Clone)]
pub struct CallFrame<'a> {
    pub code: Rc<Vec<Instruction>>,
    pub constants: Rc<Vec<TValue<'a>>>,
    /// 子原型列表 — Rc 共享，op_call 保存调用现场时 O(1) 引用计数（perf: 消除 Vec 分配）
    pub protos: Rc<Vec<Rc<Proto<'a>>>>,
    pub base: usize,
    pub return_pc: usize,
    pub return_base: usize,
    pub num_results: i32,
    pub num_params: u8,
    pub is_vararg: bool,
    pub proto_flag: u8,
    pub nextraargs: i32,
    /// perf: Rc 共享避免 op_call 中 Vec 深拷贝 (N 个 UpValRef 各 1 次 atomic inc)
    /// 对应 C Lua 的 ci->u.l.upvals = cl->upvals (指针共享, O(1))
    pub closure_upvals: Rc<RefCell<UpValVec<'a>>>,
    pub tbc_list: Option<usize>,
}

/// 协程挂起时保存的 VM 执行上下文
///
/// 协程的执行态就是 `ThreadContext.exec` 本身（Box 地址稳定）：resume/yield 直接在
/// 协程自己的 exec 上跑 `execute_loop`，调用者全程不动（无 swap）。协程 exec 通过
/// `bind_g` 指向共享全局态。
#[derive(Debug)]
pub struct ThreadContext<'a> {
    /// 是否已开始执行（首次 resume 后置 true）
    pub started: bool,
    /// 协程当前状态（共享可变，对应 LuaThread.status 的真实来源）
    pub status: ThreadStatus,
    /// 协程的 VM 执行上下文 — 协程自己的 lua_State（运行态驻留于此）
    pub exec: Box<crate::state::LuaState<'a>>,
    /// 协程错误时保存的错误信息（status=Error 时有效）
    /// coroutine.close 时若协程已 dead 且有错误，应返回该错误
    pub error_msg: Option<TValue<'a>>,
    /// 首次 resume 时收集的跨协程 upvalue 信息（uv_ref + original_stack_index）
    /// 用于 close_suspended_coroutine 时把 Closed upvalue 值同步回父栈
    pub upval_origins: Vec<(UpValRef<'a>, usize)>,
    /// wrap 协程创建时的 current_thread 指针值（用于检测首次 resume 是否跨栈）
    /// 0 表示在主线程创建；非 0 表示在某个协程内创建（Rc::as_ptr 的 usize 值）
    pub wrap_creator_thread_ptr: usize,
    /// wrap 协程创建时保存的开 upvalue 信息（uv_ref, original_stack_index, saved_value）
    /// 首次 resume 时若同栈则从 state.exec.stack 读最新值关闭；若跨栈则用 saved_value 关闭
    pub pending_wrap_upvals: Vec<(UpValRef<'a>, usize, TValue<'a>)>,
    /// yield 时关闭的 Open upvalue 信息（协程内部创建的闭包的 upvalue）
    /// resume 时把 Closed 值同步回协程栈并恢复 Open
    pub yield_upval_origins: Vec<(UpValRef<'a>, usize)>,
    /// 原始 LuaThread 的弱引用 — 用于 coroutine.running() 返回同一对象
    /// 避免 sleep_session/wakeup_queue 等 table 查找因对象不一致而失败。
    /// None 表示尚未绑定（对应原 `Weak::new()`）。
    pub thread_ref: Option<WeakRc<LuaThread<'a>, SlotAlloc>>,
}

impl<'a> Default for ThreadContext<'a> {
    fn default() -> Self {
        ThreadContext {
            started: false,
            status: ThreadStatus::Suspended,
            exec: Box::new(crate::state::LuaState::empty_thread()),
            error_msg: None,
            upval_origins: Vec::new(),
            wrap_creator_thread_ptr: 0,
            pending_wrap_upvals: Vec::new(),
            yield_upval_origins: Vec::new(),
            thread_ref: None,
        }
    }
}

/// Lua 线程（协程）
///
/// Scenario: 线程的生命周期
/// Given: 一个新创建的 Lua 线程
/// When: 检查其状态
/// Then: context.status = ThreadStatus::Suspended
///
/// 注意：与 C 的 lua_State 一致，线程的「栈」与「状态」只有一份权威来源——
/// 栈在 `context.exec.stack`（协程 exec 是协程自己的 lua_State，运行/挂起同一份），
/// 状态在 `context.status`。此结构不再持有影子副本 `stack`/`status`。
#[derive(Debug, Clone)]
pub struct LuaThread<'a> {
    /// 协程体函数 (coroutine.create 的参数)
    pub function: Option<Box<TValue<'a>>>,
    /// 是否为主线程
    pub is_main: bool,
    /// 持久化执行上下文（Rc 共享，clone 后仍指向同一份状态）
    pub context: Rc<RefCell<ThreadContext<'a>>>,
    /// C API 关联的 lua_State 指针（lua_newthread 创建时设置）
    /// 用于 lua_tothread 从 TValue::Thread 反查关联的 lua_State。
    /// 主线程和 Lua 层 coroutine.create 创建的协程此字段为 null。
    pub c_state: Cell<*mut LuaState<'a>>,
}

impl<'a> LuaThread<'a> {
    /// 估算 LuaThread 真实堆占用（用于 GC 内存计费）。
    /// LuaThread 不调用 register_object（无 gc_header），由 gc_extra_estimate 跟踪。
    pub fn gc_mem_size(&self) -> usize {
        // MyRc<LuaThread> 路由块 = RcHeader(16) + SlotAlloc(8) + LuaThread 自身
        // context: Rc<RefCell<ThreadContext>>，含 exec.stack 等 Vec（粗略估算 64）
        RC_HEADER_SIZE
            + std::mem::size_of::<crate::mem::SlotAlloc>()
            + std::mem::size_of::<LuaThread>()
            + 64
        // ThreadContext + RefCell 分配头估算
    }
}

/// 线程状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    OK,
    Suspended,
    Normal,
    Error,
}

impl Default for ThreadStatus {
    fn default() -> Self {
        ThreadStatus::Suspended
    }
}

// ============================================================================
// 规约：ceil_log2 — 计算 ceil(log2(x))
// ============================================================================

/// 计算 ceil(log2(x))，即满足 x ≤ 2^n 的最小整数 n。
///
/// Scenario: 计算 ceil(log2)
/// Given: x = 1
/// When: 调用 ceil_log2(1)
/// Then: 返回 0 (因为 1 ≤ 2^0)
///
/// Given: x = 2
/// When: 调用 ceil_log2(2)
/// Then: 返回 1 (因为 2 ≤ 2^1)
///
/// Given: x = 3
/// When: 调用 ceil_log2(3)
/// Then: 返回 2 (因为 3 ≤ 2^2 = 4)
///
/// Given: x = 256
/// When: 调用 ceil_log2(256)
/// Then: 返回 8 (因为 256 = 2^8)
///
/// Given: x = 257
/// When: 调用 ceil_log2(257)
/// Then: 返回 9 (因为 257 ≤ 2^9 = 512)
///
/// Given: x = 0
/// When: 调用 ceil_log2(0)
/// Then: 触发 panic (debug) 或未定义行为，因输入无效
pub fn ceil_log2(x: u32) -> u8 {
    if x == 0 {
        panic!("ceil_log2: zero is not a valid input");
    }
    // 规约: 待实现
    let x = x - 1;
    let mut l: u8 = 0;
    let mut v = x;
    while v >= 256 {
        l += 8;
        v >>= 8;
    }
    let log_2: [u8; 256] = [
        0, 1, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
        5, 5, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
        6, 6, 6, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
        7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
        7, 7, 7, 7, 7, 7, 7, 7, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
    ];
    l + log_2[v as usize]
}

// ============================================================================
// 规约：hexavalue — 十六进制字符转数值
// ============================================================================

/// 将十六进制字符转换为对应的整数值。
///
/// Scenario: 数字字符
/// Given: c = '5'
/// When: 调用 hexavalue('5')
/// Then: 返回 5
///
/// Scenario: 大写字母
/// Given: c = 'A'
/// When: 调用 hexavalue('A')
/// Then: 返回 10
///
/// Scenario: 小写字母
/// Given: c = 'f'
/// When: 调用 hexavalue('f')
/// Then: 返回 15
///
/// Scenario: 非十六进制字符
/// Given: c = 'G'
/// When: 调用 hexavalue('G')
/// Then: 返回 0 (或 panic，仅对有效输入)
pub fn hexavalue(c: u8) -> u8 {
    // 规约: 待实现
    if c.is_ascii_digit() {
        c - b'0'
    } else {
        (c.to_ascii_lowercase() - b'a') + 10
    }
}

// ============================================================================
// 规约：str2num — 字符串转数字
// ============================================================================

/// 将字符串解析为 Lua 数字（优先整数，否则浮点）。
///
/// Scenario: 解析十进制整数
/// Given: s = "42"
/// When: 调用 str2num("42")
/// Then: 返回 TValue::Integer(42)
///
/// Scenario: 解析负整数
/// Given: s = "-100"
/// When: 调用 str2num("-100")
/// Then: 返回 TValue::Integer(-100)
///
/// Scenario: 解析浮点数
/// Given: s = "3.14"
/// When: 调用 str2num("3.14")
/// Then: 返回 TValue::Float(3.14)
///
/// Scenario: 解析十六进制整数
/// Given: s = "0xFF"
/// When: 调用 str2num("0xFF")
/// Then: 返回 TValue::Integer(255)
///
/// Scenario: 解析无效字符串
/// Given: s = "abc"
/// When: 调用 str2num("abc")
/// Then: 返回 None
///
/// Scenario: 解析空字符串
/// Given: s = ""
/// When: 调用 str2num("")
/// Then: 返回 None
pub fn str2num<'a>(s: &str) -> Option<TValue<'a>> {
    // 性能优先: 用 s.trim() (Unicode whitespace, 但性能优化的 std 实现)
    #[cfg(not(size_optimized))]
    let s = s.trim();
    // 体积优先: 用 ASCII trim 避免 Unicode whitespace 表 (~4KB)
    #[cfg(size_optimized)]
    let s = s.trim_matches(|c: char| c.is_ascii_whitespace());
    if s.is_empty() {
        return None;
    }
    // 提取可选符号 — 对应 C 的 isneg(&s)
    let (neg, s) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        (false, rest)
    } else {
        (false, s)
    };
    // 符号后不能紧跟空格 (如 "+ 0.01" 无效).
    // strtod 会跳过前导空格导致 "+ 0.01" 被误解析为 0.01, 需显式拒绝.
    #[cfg(not(size_optimized))]
    if s.starts_with(char::is_whitespace) {
        return None;
    }
    #[cfg(size_optimized)]
    if s.starts_with(|c: char| c.is_ascii_whitespace()) {
        return None;
    }
    // 尝试十六进制 — 对应 C: if (s[0]=='0' && (s[1]=='x'||s[1]=='X'))
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        // 检查是否是浮点数（包含 '.', locale 小数点, 或 'p'/'P'）
        let dec_point = unsafe { crate::float_utils::get_locale_decpoint() };
        let is_float = rest.contains('.')
            || (dec_point != '.' && rest.contains(dec_point))
            || rest.contains('p')
            || rest.contains('P');
        if is_float {
            // hex float 的小数点总是 '.', 若 locale 用其他字符则替换
            let hex_rest = if dec_point != '.' && rest.contains(dec_point) {
                rest.replace(dec_point, ".")
            } else {
                rest.to_string()
            };
            return parse_hex_float(&hex_rest).map(|f| TValue::Float(if neg { -f } else { f }));
        }
        return parse_hex_int(rest).map(|v| {
            TValue::Integer(if neg {
                // C: l_castU2S(0u - a) —— 在 LuaUnsigned 域取负后按位重解释 (两补数环绕)
                (v as LuaUnsigned).wrapping_neg() as LuaInteger
            } else {
                v
            })
        });
    }
    // 尝试十进制整数（含符号）
    // 体积优先: 手动拼接避免 format! 引入 fmt 代码
    let signed_s = if neg {
        let mut tmp = String::with_capacity(s.len() + 1);
        tmp.push('-');
        tmp.push_str(s);
        tmp
    } else {
        s.to_string()
    };
    if let Some(i) = parse_dec_int(&signed_s) {
        return Some(TValue::Integer(i));
    }
    // 尝试浮点数（含符号）— 拒绝 "inf"/"nan"（对应 C 的 l_str2d 检查 .xXnN）
    if !signed_s.contains('n') && !signed_s.contains('N') {
        if let Some(f) = crate::float_utils::f64_from_str(&signed_s) {
            return Some(TValue::Float(f));
        }
    }
    None
}

fn parse_dec_int(s: &str) -> Option<LuaInteger> {
    s.parse::<LuaInteger>().ok()
}

fn parse_hex_int(s: &str) -> Option<LuaInteger> {
    // C 的 l_str2int: 只读取十六进制数字，然后检查剩余是否全为空格
    let mut result: LuaUnsigned = 0;
    let mut empty = true;
    let bytes = s.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() && bytes[pos].is_ascii_hexdigit() {
        let digit = hexavalue(bytes[pos]) as LuaUnsigned;
        result = result.wrapping_mul(16).wrapping_add(digit);
        empty = false;
        pos += 1;
    }
    if empty {
        return None;
    }
    // 剩余必须全为空格（对应 C 的 skip trailing spaces + *s == '\0' 检查）
    let remaining = &s[pos..];
    if !remaining
        .bytes()
        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'\x0b'))
    {
        return None;
    }
    // C: l_castU2S(a) —— 无符号位模式重解释为 LuaInteger (两补数)
    Some(result as LuaInteger)
}

/// `LuaNumber` 的位模式类型 — 对应 `f64::to_bits`/`from_bits`(u64) 或 `f32::`(u32)。
/// 用于按 `LuaNumber` 宽度构造 2^e (ldexp)。
#[cfg(not(feature = "num-f32"))]
type FloatBits = u64;
#[cfg(feature = "num-f32")]
type FloatBits = u32;

/// 解析十六进制浮点数 (对应 C 的 lua_strx2number)
///
/// 输入格式: <int_part>[.<frac_part>][p<exp>]
/// 例如: "1.999999999999ap-4" → 0.1
///
/// 按照 C 的实现，用 `LuaNumber` 直接累积所有数字（最多 MAXSIGDIG 位），
/// 让 IEEE 754 自动处理舍入，最后用 ldexp(r, e) 调整指数。
pub fn parse_hex_float(s: &str) -> Option<LuaNumber> {
    const MAXSIGDIG: usize = 30;

    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut r: LuaNumber = 0.0;
    let mut sigdig: usize = 0;
    let mut nosigdig: usize = 0;
    let mut e: i32 = 0;
    let mut hasdot = false;

    // 读取尾数部分（数字和小数点）
    while i < chars.len() {
        let c = chars[i];
        if c == '.' {
            if hasdot {
                break;
            }
            hasdot = true;
        } else if c.is_ascii_hexdigit() {
            let d = c.to_digit(16).unwrap();
            if sigdig == 0 && d == 0 {
                nosigdig += 1;
            } else {
                sigdig += 1;
                if sigdig <= MAXSIGDIG {
                    r = r * 16.0 + d as LuaNumber;
                } else {
                    e += 1;
                }
            }
            if hasdot {
                e -= 1;
            }
        } else {
            break;
        }
        i += 1;
    }

    if nosigdig + sigdig == 0 {
        return None;
    }

    e *= 4;

    // 处理 p 指数部分
    if i < chars.len() && (chars[i] == 'p' || chars[i] == 'P') {
        i += 1;
        let neg_exp = if i < chars.len() && chars[i] == '-' {
            i += 1;
            true
        } else if i < chars.len() && chars[i] == '+' {
            i += 1;
            false
        } else {
            false
        };
        if i >= chars.len() || !chars[i].is_ascii_digit() {
            return None;
        }
        let mut exp_val: i32 = 0;
        while i < chars.len() && chars[i].is_ascii_digit() {
            exp_val = exp_val * 10 + (chars[i] as u8 - b'0') as i32;
            i += 1;
        }
        if neg_exp {
            e -= exp_val;
        } else {
            e += exp_val;
        }
    }

    // 必须处理完所有字符（对应 C 的 endptr 检查）
    if i != chars.len() {
        return None;
    }

    // ldexp(r, e) = r * 2^e
    // 用位操作构造精确的 2^e, 避免 powi 的精度问题。
    // 各常数按 `LuaNumber` 的指数/尾数宽度参数化 (对应 C 的 l_mathop(ldexp),
    // libm 侧按 LUA_NUMBER 宽度选择 ldexpf/ldexp)。
    const BIAS: i32 = LuaNumber::MAX_EXP - 1; // f64: 1023, f32: 127
    const SIG_BITS: u32 = LuaNumber::MANTISSA_DIGITS - 1; // f64: 52, f32: 23
                                                          // 最小次正规数 2^(-(BIAS+SIG_BITS-1)) 的位模式偏移量
    const SUB_OFFSET: i32 = BIAS + SIG_BITS as i32 - 1; // f64: 1074, f32: 149
    let result = if r == 0.0 || !r.is_finite() {
        r
    } else if e >= BIAS {
        LuaNumber::INFINITY * r.signum()
    } else if e < -SUB_OFFSET {
        LuaNumber::copysign(0.0, r)
    } else {
        let bits: FloatBits = if e >= -(BIAS - 1) {
            ((e + BIAS) as FloatBits) << SIG_BITS
        } else {
            (1 as FloatBits) << (e + SUB_OFFSET)
        };
        r * LuaNumber::from_bits(bits)
    };

    Some(result)
}

// ============================================================================
// 测试模块 (红灯 — 失败测试先行)
// ============================================================================

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        alloc::{SimpleArc, SimpleRc},
        strings::{lua_string_len, LongString, ShortString},
    };

    // ========================================================================
    // TValue 大小检查
    // ========================================================================

    #[test]
    fn test_tvalue_size() {
        println!(
            "SimpleRc<ShortString> size: {}",
            std::mem::size_of::<SimpleRc<ShortString>>()
        );
        println!(
            "SimpleArc<ShortString> size: {}",
            std::mem::size_of::<SimpleArc<ShortString>>()
        );
        println!("TValue size: {}", std::mem::size_of::<TValue>());
        println!("TValue align: {}", std::mem::align_of::<TValue>());
        println!("BuiltinFn size: {}", std::mem::size_of::<BuiltinFn>());
        println!("BuiltinFnPtr size: {}", std::mem::size_of::<BuiltinFnPtr>());
        // BuiltinFn 压缩到 8 字节 (name 移入全局表) 后, rustc 把 TValue 的
        // tag 放入 LuaString 变体的 padding — TValue 16 字节 (tag 8 + payload 8)。
        // 这是 C Lua TValue (16 字节) 的同级布局。
        assert_eq!(std::mem::size_of::<BuiltinFn>(), 8);
        assert_eq!(std::mem::size_of::<SimpleRc<ShortString>>(), 8);
        assert_eq!(std::mem::size_of::<SimpleArc<ShortString>>(), 8);
        assert_eq!(std::mem::size_of::<TValue>(), 16);
    }

    // ========================================================================
    // LuaType 测试
    // ========================================================================

    #[test]
    fn test_lua_type_display() {
        assert_eq!(format!("{}", LuaType::Nil), "nil");
        assert_eq!(format!("{}", LuaType::Boolean), "boolean");
        assert_eq!(format!("{}", LuaType::Number), "number");
        assert_eq!(format!("{}", LuaType::String), "string");
    }

    // ========================================================================
    // TValue 测试
    // ========================================================================

    #[test]
    fn test_tvalue_nil() {
        let v = TValue::Nil(NilKind::Strict);
        assert!(v.is_nil());
        assert_eq!(v.ty(), LuaType::Nil);
    }

    #[test]
    fn test_tvalue_nil_empty() {
        let v = TValue::Nil(NilKind::Empty);
        assert!(v.is_nil());
    }

    #[test]
    fn test_tvalue_boolean() {
        let t = TValue::Boolean(true);
        let f = TValue::Boolean(false);
        assert_eq!(t.ty(), LuaType::Boolean);
        assert!(f.is_false());
        assert!(!t.is_false());
        assert!(!f.is_nil());
    }

    #[test]
    fn test_tvalue_integer() {
        let v = TValue::Integer(42);
        assert!(v.is_number());
        assert_eq!(v.as_integer(), Some(42));
        assert_eq!(v.as_float(), Some(42.0));
    }

    #[test]
    fn test_tvalue_float() {
        let v = TValue::Float(3.14);
        assert!(v.is_number());
        assert_eq!(v.as_float(), Some(3.14));
        assert_eq!(v.as_integer(), None);
    }

    #[test]
    fn test_tvalue_float_to_int_exact() {
        let v = TValue::Float(5.0);
        assert_eq!(v.as_integer(), Some(5));
    }

    #[test]
    fn test_tvalue_float_to_int_inexact() {
        let v = TValue::Float(5.5);
        assert_eq!(v.as_integer(), None);
    }

    #[test]
    fn test_tvalue_default() {
        let v = TValue::default();
        assert_eq!(v, TValue::Nil(NilKind::Strict));
    }

    #[test]
    fn test_tvalue_partial_eq() {
        assert_eq!(TValue::Nil(NilKind::Strict), TValue::Nil(NilKind::Strict));
        assert_ne!(TValue::Nil(NilKind::Strict), TValue::Nil(NilKind::Empty));
        assert_eq!(TValue::Boolean(true), TValue::Boolean(true));
        assert_ne!(TValue::Boolean(true), TValue::Boolean(false));
        assert_eq!(TValue::Integer(42), TValue::Integer(42));
        assert_eq!(TValue::Integer(1), TValue::Float(1.0));
        assert_eq!(TValue::Float(1.0), TValue::Integer(1));
        assert_ne!(TValue::Integer(1), TValue::Nil(NilKind::Strict));
    }

    #[test]
    fn test_tvalue_display() {
        assert_eq!(format!("{}", TValue::Nil(NilKind::Strict)), "nil");
        assert_eq!(format!("{}", TValue::Boolean(true)), "true");
        assert_eq!(format!("{}", TValue::Integer(42)), "42");
    }

    #[test]
    fn test_tvalue_is_false() {
        assert!(TValue::Boolean(false).is_false());
        assert!(TValue::Nil(NilKind::Strict).is_false());
        assert!(TValue::Nil(NilKind::Empty).is_false());
        assert!(!TValue::Boolean(true).is_false());
        assert!(!TValue::Integer(0).is_false());
    }

    // ========================================================================
    // LuaString 测试
    // ========================================================================

    #[test]
    fn test_luastring_short() {
        let short = crate::strings::ArcRc::<ShortString>::new_sstr(0, 0, "hello".as_bytes());
        let ts = TValue::ShortStr(short);
        assert_eq!(lua_string_as_str(&ts), "hello");
        assert_eq!(lua_string_len(&ts), 5);
        assert!(matches!(ts, TValue::ShortStr(_)));
    }

    #[test]
    fn test_luastring_long() {
        let alloc = crate::mem::TestAlloc::new();
        let long = crate::alloc::SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            None,
            "a".repeat(100).as_bytes(),
        );
        let ts = TValue::LongStr(long);
        assert_eq!(lua_string_len(&ts), 100);
        assert!(matches!(ts, TValue::LongStr(_)));
    }
    #[test]
    fn test_luastring_empty() {
        let short = crate::strings::ArcRc::<ShortString>::new_sstr(0, 0, "".as_bytes());
        let ts = TValue::ShortStr(short);
        assert_eq!(lua_string_len(&ts), 0);
        assert_eq!(lua_string_as_str(&ts), "");
    }

    #[test]
    fn test_luastring_eq() {
        let alloc = crate::mem::TestAlloc::new();
        let arc1 = crate::strings::ArcRc::<ShortString>::new_sstr(0, 0, "foo".as_bytes());
        let arc2 = crate::strings::ArcRc::clone(&arc1);
        let ts1 = TValue::ShortStr(arc1);
        let ts2 = TValue::ShortStr(arc2);
        assert_eq!(ts1, ts2);

        let arc3 = crate::strings::ArcRc::<ShortString>::new_sstr(0, 0, "bar".as_bytes());
        let ts3 = TValue::ShortStr(arc3);
        assert_ne!(ts1, ts3);

        let long1 = TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            None,
            "test".as_bytes(),
        ));
        let long2 = TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            None,
            "test".as_bytes(),
        ));
        assert_eq!(long1, long2);
    }
    #[test]
    fn test_luastring_as_str() {
        let short: TValue<'_> = TValue::ShortStr(crate::strings::ArcRc::<ShortString>::new_sstr(
            0,
            0,
            "abc".as_bytes(),
        ));
        assert_eq!(lua_string_as_str(&short), "abc");
    }

    // ========================================================================
    // Proto 测试
    // ========================================================================

    #[test]
    fn test_proto_vararg() {
        let p = Proto {
            num_params: 0,
            flag: 0,
            max_stack_size: 2,
            size_upvalues: 0,
            size_k: 0,
            size_code: 0,
            size_line_info: 0,
            size_p: 0,
            size_loc_vars: 0,
            size_abs_line_info: 0,
            line_defined: 0,
            last_line_defined: 0,
            constants: Rc::new(vec![]),
            code: Rc::new(vec![]),
            protos: Rc::new(vec![]),
            upvalues: Rc::new(vec![]),
            line_info: vec![],
            abs_line_info: vec![],
            loc_vars: vec![],
            source: None,
        };
        assert!(!p.is_vararg());
    }

    // ========================================================================
    // ceil_log2 测试
    // ========================================================================

    #[test]
    fn test_ceil_log2_basics() {
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(7), 3);
        assert_eq!(ceil_log2(8), 3);
        assert_eq!(ceil_log2(9), 4);
        assert_eq!(ceil_log2(255), 8);
        assert_eq!(ceil_log2(256), 8);
        assert_eq!(ceil_log2(257), 9);
        assert_eq!(ceil_log2(65535), 16);
        assert_eq!(ceil_log2(65536), 16);
        assert_eq!(ceil_log2(65537), 17);
    }

    #[test]
    #[should_panic]
    fn test_ceil_log2_zero_panics() {
        ceil_log2(0);
    }

    // ========================================================================
    // hexavalue 测试
    // ========================================================================

    #[test]
    fn test_hexavalue_digits() {
        assert_eq!(hexavalue(b'0'), 0);
        assert_eq!(hexavalue(b'5'), 5);
        assert_eq!(hexavalue(b'9'), 9);
    }

    #[test]
    fn test_hexavalue_uppercase() {
        assert_eq!(hexavalue(b'A'), 10);
        assert_eq!(hexavalue(b'C'), 12);
        assert_eq!(hexavalue(b'F'), 15);
    }

    #[test]
    fn test_hexavalue_lowercase() {
        assert_eq!(hexavalue(b'a'), 10);
        assert_eq!(hexavalue(b'f'), 15);
    }

    // ========================================================================
    // str2num 测试
    // ========================================================================

    #[test]
    fn test_str2num_integer() {
        assert_eq!(str2num("42"), Some(TValue::Integer(42)));
        assert_eq!(str2num("-100"), Some(TValue::Integer(-100)));
        assert_eq!(str2num("0"), Some(TValue::Integer(0)));
    }

    #[test]
    fn test_str2num_hex() {
        assert_eq!(str2num("0xFF"), Some(TValue::Integer(255)));
        assert_eq!(str2num("0x10"), Some(TValue::Integer(16)));
        assert_eq!(str2num("0X1A"), Some(TValue::Integer(26)));
    }

    #[test]
    fn test_str2num_float() {
        assert_eq!(str2num("3.14"), Some(TValue::Float(3.14)));
        assert_eq!(str2num("-0.5"), Some(TValue::Float(-0.5)));
    }

    #[test]
    fn test_str2num_invalid() {
        assert_eq!(str2num("abc"), None);
        assert_eq!(str2num(""), None);
        assert_eq!(str2num("   "), None);
    }

    #[test]
    fn test_str2num_with_spaces() {
        assert_eq!(str2num("  42  "), Some(TValue::Integer(42)));
    }

    // ========================================================================
    // UpVal 测试
    // ========================================================================

    #[test]
    fn test_upval_open() {
        let uv = UpVal::Open {
            stack_index: 3,
            next: None,
            previous: None,
            tbc: false,
        };
        match uv {
            UpVal::Open { stack_index, .. } => assert_eq!(stack_index, 3),
            _ => panic!("expected Open"),
        }
    }

    #[test]
    fn test_upval_open_is_open() {
        let uv = UpVal::Open {
            stack_index: 3,
            next: None,
            previous: None,
            tbc: false,
        };
        assert!(uv.is_open());
        assert_eq!(uv.level(), Some(3));
    }

    #[test]
    fn test_upval_closed() {
        let uv = UpVal::Closed {
            value: TValue::Integer(42),
        };
        match uv {
            UpVal::Closed { value } => assert_eq!(value, TValue::Integer(42)),
            _ => panic!("expected Closed"),
        }
    }

    #[test]
    fn test_upval_closed_not_open() {
        let uv = UpVal::Closed {
            value: TValue::Integer(42),
        };
        assert!(!uv.is_open());
        assert_eq!(uv.level(), None);
    }

    // ========================================================================
    // Instruction 测试
    // ========================================================================

    #[test]
    fn test_instruction() {
        let i: Instruction = 42;
        assert_eq!(i, 42);
    }

    // ========================================================================
    // LocVar 测试
    // ========================================================================

    #[test]
    fn test_loc_var() {
        let lv = LocVar {
            varname: None,
            start_pc: 0,
            end_pc: 5,
        };
        assert_eq!(lv.start_pc, 0);
        assert_eq!(lv.end_pc, 5);
    }

    // ========================================================================
    // NilKind 测试
    // ========================================================================

    #[test]
    fn test_nil_kind_eq() {
        assert_eq!(NilKind::Strict, NilKind::Strict);
        assert_ne!(NilKind::Strict, NilKind::Empty);
    }

    #[test]
    fn test_type_sizes() {
        use crate::gc::GCObjectId;
        use std::cell::Cell;
        use std::mem::size_of;
        println!(
            "TValue: {} (align {})",
            size_of::<TValue>(),
            std::mem::align_of::<TValue>()
        );
        println!("Table: {}", size_of::<Table>());
        println!(
            "  GCObjectHeader: {}",
            size_of::<crate::gc::GCObjectHeader>()
        );
        println!("  GCObjectId: {}", size_of::<GCObjectId>());
        println!("  Option<GCObjectId>: {}", size_of::<Option<GCObjectId>>());
        println!(
            "  Cell<Option<GCObjectId>>: {}",
            size_of::<Cell<Option<GCObjectId>>>()
        );
        println!("TableData: {}", size_of::<TableData>());
        println!("LClosure: {}", size_of::<LClosure>());
        println!("CClosure: {}", size_of::<CClosure>());
        println!("LuaThread: {}", size_of::<LuaThread>());
        println!("Udata: 变长对象，固定头 {}", Udata::FIXED_SIZE);
        println!("LCFunction: {}", size_of::<LCFunction>());
        println!("NilKind: {}", size_of::<NilKind>());
        println!("Instruction: {}", size_of::<Instruction>());
        println!("Proto: {}", size_of::<Proto>());
        println!("CallFrame: {}", size_of::<CallFrame>());
        println!("UpvalDesc: {}", size_of::<UpvalDesc>());
        println!("LocVar: {}", size_of::<LocVar>());
        println!("AbsLineInfo: {}", size_of::<AbsLineInfo>());
        println!("UpVal: {}", size_of::<UpVal>());
    }

    /// Udata 是变长对象（单块分配）：固定头 + 用户值区 + 数据区。
    /// 验证布局常量、块内偏移、用户值读写与整块析构。
    #[test]
    fn test_udata_variable_length_layout() {
        use crate::alloc::{Global, MyRc};

        // 句柄仍是 8 字节 thin 指针（TValue 总宽 16 字节的前提）
        assert_eq!(size_of::<TValue>(), 16);
        assert_eq!(size_of::<MyRc<Udata>>(), 8);

        // 固定头 8 字节对齐（用户值 / 数据区都靠它对齐）
        assert_eq!(Udata::FIXED_SIZE % 8, 0);
        assert_eq!(
            Udata::FIXED_SIZE,
            std::mem::offset_of!(UdataFixed, metatable) + size_of::<RefCell<Option<Table>>>()
        );

        // payload 大小 = 固定头 + nuvalue * 16 + ceil(len/8) * 8
        assert_eq!(Udata::payload_size(0, 0), Udata::FIXED_SIZE);
        assert_eq!(Udata::payload_size(2, 0), Udata::FIXED_SIZE + 32);
        assert_eq!(Udata::payload_size(0, 9), Udata::FIXED_SIZE + 16);
        assert_eq!(Udata::payload_size(2, 8), Udata::FIXED_SIZE + 32 + 8);

        // 真正构造一个（nuvalue = 2, len = 8）：数据区在块内、紧跟用户值区
        let size = Udata::payload_size(2, 8);
        let ud = match MyRc::<Udata, Global>::try_new_unsized_in(Global, size, |p| unsafe {
            Udata::init_payload(p, 2, 8)
        }) {
            Ok(u) => u,
            Err(_) => panic!("分配失败"),
        };
        assert_eq!(ud.nuvalue, 2);
        assert_eq!(ud.len, 8);
        assert_eq!(ud.user_values().len(), 2);
        assert!(ud.user_values()[0].is_nil() && ud.user_values()[1].is_nil());

        let payload = ud.as_raw_ptr() as usize;
        assert_eq!(ud.data_ptr() as usize, payload + Udata::FIXED_SIZE + 32);
        // 数据区初始为 0，且可写（供 C 模块存放自己的结构体）
        assert_eq!(unsafe { *(ud.data_ptr() as *const u64) }, 0);
        unsafe { *(Udata::data_ptr_of(ud.as_raw_ptr()) as *mut u64) = 0xdead_beef };
        assert_eq!(unsafe { *(ud.data_ptr() as *const u64) }, 0xdead_beef);

        // 用户值就地写（对应 lua_setiuservalue）
        unsafe { Udata::set_user_value(ud.as_raw_ptr(), 2, TValue::Integer(7)) };
        assert_eq!(ud.user_values()[1].as_integer(), Some(7));
        assert!(ud.user_values()[0].is_nil());

        // 整块析构：用户值里持有引用的对象必须被释放（这里验证 clone/drop 平衡——
        // 2 份句柄 drop 后块回收，无双重释放/泄漏，由 Miri/ASan 检出）
        let ud2 = ud.clone();
        assert_eq!(ud.strong_count(), 2);
        drop(ud);
        assert_eq!(ud2.strong_count(), 1);
    }
}
