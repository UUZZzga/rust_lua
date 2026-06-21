//! 标签方法 / 元方法 (ltm.h / ltm.cpp → Rust 惯用重写)
//!
//! 本模块被 vm.rs 和 execute.rs 通过 `use crate::tm::*` 集成使用。
//!
//! ## 设计原则
//! - `TagMethod` 使用 Rust enum（而非 C 整数枚举），编译器保证穷举匹配
//! - 元方法名称通过 `TagMethod::name()` 方法获取，无需全局字符串表
//! - 快速访问缓存使用 `bitflags` crate 的 `MetatableFlags`，类型安全
//! - 元方法查找返回 `Option<&TValue>`，用类型系统替代 NULL 检查
//! - 元方法调用通过 state.pcall 实际执行，结果写回栈（与 C API 一致）
//! - Vararg 处理用 Rust enum + Vec，消除 C 的手动栈操作
//! - 错误处理使用 `Result<(), VmError>` 替代 C 的 longjmp

use std::fmt;

use bitflags::bitflags;

use crate::debug::{concaterror, ordererror, opinterror, tointerror};
use crate::execute::VmError;
use crate::objects::{Instruction, NilKind, TValue, Table, LuaType};
use crate::strings::{LuaString, ShortString, rust_hash};
use crate::vm::LuaState;

// ============================================================================
// get_mmbin_tm — 从 MM 系列指令中提取元方法事件索引
// ============================================================================

/// 从 MMBIN/MMBINI/MMBINK 指令中提取元方法事件索引。
/// C 对应: GETARG_C(i) → 取指令的 C 字段 (bits 24-31)
#[inline]
pub fn get_mmbin_tm(inst: Instruction) -> u8 {
    ((inst >> 24) & 0xFF) as u8
}

// ============================================================================
// TagMethod — 元方法枚举
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum TagMethod {
    Index = 0,
    NewIndex = 1,
    Gc = 2,
    Mode = 3,
    Len = 4,
    Eq = 5,
    Add = 6,
    Sub = 7,
    Mul = 8,
    Mod = 9,
    Pow = 10,
    Div = 11,
    IDiv = 12,
    BAnd = 13,
    BOr = 14,
    BXor = 15,
    Shl = 16,
    Shr = 17,
    Unm = 18,
    BNot = 19,
    Lt = 20,
    Le = 21,
    Concat = 22,
    Call = 23,
    Close = 24,
}

pub const TM_N: usize = 25;

impl TagMethod {
    pub fn name(self) -> &'static str {
        match self {
            TagMethod::Index => "__index",
            TagMethod::NewIndex => "__newindex",
            TagMethod::Gc => "__gc",
            TagMethod::Mode => "__mode",
            TagMethod::Len => "__len",
            TagMethod::Eq => "__eq",
            TagMethod::Add => "__add",
            TagMethod::Sub => "__sub",
            TagMethod::Mul => "__mul",
            TagMethod::Mod => "__mod",
            TagMethod::Pow => "__pow",
            TagMethod::Div => "__div",
            TagMethod::IDiv => "__idiv",
            TagMethod::BAnd => "__band",
            TagMethod::BOr => "__bor",
            TagMethod::BXor => "__bxor",
            TagMethod::Shl => "__shl",
            TagMethod::Shr => "__shr",
            TagMethod::Unm => "__unm",
            TagMethod::BNot => "__bnot",
            TagMethod::Lt => "__lt",
            TagMethod::Le => "__le",
            TagMethod::Concat => "__concat",
            TagMethod::Call => "__call",
            TagMethod::Close => "__close",
        }
    }

    pub fn is_fast_access(self) -> bool {
        (self as u8) <= (TagMethod::Eq as u8)
    }

    pub fn from_u8(n: u8) -> Option<Self> {
        match n {
            0 => Some(TagMethod::Index), 1 => Some(TagMethod::NewIndex),
            2 => Some(TagMethod::Gc), 3 => Some(TagMethod::Mode),
            4 => Some(TagMethod::Len), 5 => Some(TagMethod::Eq),
            6 => Some(TagMethod::Add), 7 => Some(TagMethod::Sub),
            8 => Some(TagMethod::Mul), 9 => Some(TagMethod::Mod),
            10 => Some(TagMethod::Pow), 11 => Some(TagMethod::Div),
            12 => Some(TagMethod::IDiv), 13 => Some(TagMethod::BAnd),
            14 => Some(TagMethod::BOr), 15 => Some(TagMethod::BXor),
            16 => Some(TagMethod::Shl), 17 => Some(TagMethod::Shr),
            18 => Some(TagMethod::Unm), 19 => Some(TagMethod::BNot),
            20 => Some(TagMethod::Lt), 21 => Some(TagMethod::Le),
            22 => Some(TagMethod::Concat), 23 => Some(TagMethod::Call),
            24 => Some(TagMethod::Close),
            _ => None,
        }
    }
}

impl fmt::Display for TagMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

// ============================================================================
// MetatableFlags — 元表快速访问缓存
// ============================================================================

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct MetatableFlags: u8 {
        const NO_INDEX    = 1 << 0;
        const NO_NEWINDEX = 1 << 1;
        const NO_GC       = 1 << 2;
        const NO_MODE     = 1 << 3;
        const NO_LEN      = 1 << 4;
        const NO_EQ       = 1 << 5;
    }
}

impl MetatableFlags {
    pub fn from_tag_method(tm: TagMethod) -> Option<Self> {
        match tm {
            TagMethod::Index => Some(MetatableFlags::NO_INDEX),
            TagMethod::NewIndex => Some(MetatableFlags::NO_NEWINDEX),
            TagMethod::Gc => Some(MetatableFlags::NO_GC),
            TagMethod::Mode => Some(MetatableFlags::NO_MODE),
            TagMethod::Len => Some(MetatableFlags::NO_LEN),
            TagMethod::Eq => Some(MetatableFlags::NO_EQ),
            _ => None,
        }
    }
}

// ============================================================================
// Metatable — 元表（含 flags 缓存）
// ============================================================================

#[derive(Debug, Clone)]
pub struct Metatable {
    pub table: Table,
    pub flags: MetatableFlags,
}

impl Metatable {
    pub fn new(table: Table) -> Self {
        Metatable { table, flags: MetatableFlags::empty() }
    }

    pub fn empty() -> Self {
        Metatable { table: Table::new(), flags: MetatableFlags::empty() }
    }

    pub fn get_tm(&mut self, tm: TagMethod) -> Option<&TValue> {
        if let Some(flag) = MetatableFlags::from_tag_method(tm) {
            if self.flags.contains(flag) { return None; }
        }
        let key = make_tm_tvalue(tm);
        let result = self.table.get(&key);
        if result.is_none() {
            if let Some(flag) = MetatableFlags::from_tag_method(tm) {
                self.flags.insert(flag);
            }
        }
        result
    }
}

// ============================================================================
// DefaultMetatables — 类型默认元表注册表
// ============================================================================

#[derive(Debug, Clone)]
pub struct DefaultMetatables {
    tables: [Option<Metatable>; 9],
}

impl DefaultMetatables {
    pub fn new() -> Self {
        const NONE: Option<Metatable> = None;
        DefaultMetatables { tables: [NONE; 9] }
    }

    pub fn get(&self, ty: LuaType) -> Option<&Table> {
        let idx = ty as usize;
        self.tables.get(idx)?.as_ref().map(|m| &m.table)
    }

    pub fn set(&mut self, ty: LuaType, mt: Metatable) {
        let idx = ty as usize;
        if idx < self.tables.len() {
            self.tables[idx] = Some(mt);
        }
    }

    pub fn get_mut(&mut self, ty: LuaType) -> Option<&mut Metatable> {
        let idx = ty as usize;
        self.tables.get_mut(idx)?.as_mut()
    }
}

impl Default for DefaultMetatables {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// type_name / obj_type_name
// ============================================================================

const TYPE_NAMES: [&str; 11] = [
    "no value", "nil", "boolean", "userdata", "number",
    "string", "table", "function", "userdata", "thread", "upvalue",
];

pub fn type_name(ty: LuaType) -> &'static str {
    match ty {
        LuaType::Nil => TYPE_NAMES[1],
        LuaType::Boolean => TYPE_NAMES[2],
        LuaType::LightUserData => TYPE_NAMES[3],
        LuaType::Number => TYPE_NAMES[4],
        LuaType::String => TYPE_NAMES[5],
        LuaType::Table => TYPE_NAMES[6],
        LuaType::Function => TYPE_NAMES[7],
        LuaType::UserData => TYPE_NAMES[8],
        LuaType::Thread => TYPE_NAMES[9],
    }
}

pub fn obj_type_name(obj: &TValue) -> String {
    let meta: Option<&Table> = match obj {
        TValue::Table(t) => t.metatable.as_ref().map(|b| b.as_ref()),
        TValue::UserData(u) => u.metatable.as_ref().map(|b| b.as_ref()),
        _ => None,
    };
    if let Some(mt) = meta {
        let name_key = make_name_key();
        if let Some(name_val) = mt.get(&name_key) {
            if let TValue::Str(s) = name_val {
                return s.as_str().to_string();
            }
        }
    }
    type_name(obj.ty()).to_string()
}

pub fn get_tm_by_obj<'a>(
    obj: &'a TValue,
    tm: TagMethod,
    default_mts: &'a DefaultMetatables,
) -> Option<&'a TValue> {
    let mt: Option<&Table> = match obj {
        TValue::Table(t) => t.metatable.as_ref().map(|b| b.as_ref()),
        TValue::UserData(u) => u.metatable.as_ref().map(|b| b.as_ref()),
        _ => default_mts.get(obj.ty()),
    };
    let mt = mt?;
    let key = make_tm_tvalue(tm);
    mt.get(&key)
}

// ============================================================================
// TagMethodError
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagMethodError {
    NoMetamethod(TagMethod),
    TypeError { expected: String, got: String },
    OrderError { left: String, right: String },
    ConcatError { left: String, right: String },
    OpError { op: String, left: String, right: String },
}

impl fmt::Display for TagMethodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TagMethodError::NoMetamethod(tm) => write!(f, "no metamethod '{}' found", tm.name()),
            TagMethodError::TypeError { expected, got } => write!(f, "type error: expected {}, got {}", expected, got),
            TagMethodError::OrderError { left, right } => write!(f, "attempt to compare {} with {}", left, right),
            TagMethodError::ConcatError { left, right } => write!(f, "attempt to concatenate {} with {}", left, right),
            TagMethodError::OpError { op, left, right } => write!(f, "attempt to {} {} and {}", op, left, right),
        }
    }
}

impl std::error::Error for TagMethodError {}

// ============================================================================
// 元方法调用 (ltm.cpp → Rust) — 与 C API 完全一致
// ============================================================================

/// 调用元方法并将结果写入栈 — 对应 C 的 luaT_callTMres
///
/// C 实现:
/// ```c
/// lu_byte luaT_callTMres (lua_State *L, const TValue *f, const TValue *p1,
///                         const TValue *p2, StkId res) {
///   ptrdiff_t result = savestack(L, res);
///   StkId func = L->top.p;
///   setobj2s(L, func, f);     // push function
///   setobj2s(L, func + 1, p1); // 1st argument
///   setobj2s(L, func + 2, p2); // 2nd argument
///   L->top.p += 3;
///   luaD_callnoyield(L, func, 1);
///   res = restorestack(L, result);
///   setobjs2s(L, res, --L->top.p); // move result to its place
///   return ttypetag(s2v(res));
/// }
/// ```
///
/// Rust 版本: 在栈顶压入函数和参数，调用 pcall，将结果写入 res 槽位。
fn call_tm_res(
    state: &mut LuaState,
    f: &TValue,
    p1: &TValue,
    p2: &TValue,
    res: usize,
) -> Result<(), VmError> {
    let func_idx = state.stack.len();
    // 压入函数和两个参数 (对应 C 的 setobj2s)
    state.stack.push(f.clone());
    state.stack.push(p1.clone());
    state.stack.push(p2.clone());
    state.top = state.stack.len();

    // 调用: 2 个参数, 1 个返回值 (对应 C 的 luaD_callnoyield(L, func, 1))
    let status = state.pcall(2, 1, 0);

    // pcall 后: 栈截断到 func_idx，1 个结果(或错误消息)在 func_idx
    let result = if func_idx < state.stack.len() {
        state.stack[func_idx].clone()
    } else {
        TValue::Nil(NilKind::Strict)
    };

    // 截断栈，移除临时压入的函数/参数/结果
    state.stack.truncate(func_idx);

    if status != 0 {
        // 元方法调用失败 — 提取错误消息
        state.top = state.stack.len();
        let err_msg = match &result {
            TValue::Str(s) => s.as_str().to_string(),
            _ => format!("{}", result),
        };
        return Err(VmError::RuntimeError(err_msg));
    }

    // 将结果写入 res 槽位 (对应 C 的 setobjs2s(L, res, ...))
    while state.stack.len() <= res {
        state.stack.push(TValue::Nil(NilKind::Strict));
    }
    state.stack[res] = result;
    state.top = state.stack.len();
    Ok(())
}

/// 查找并调用二元元方法 — 对应 C 的 callbinTM
///
/// C 实现:
/// ```c
/// static int callbinTM (lua_State *L, const TValue *p1, const TValue *p2,
///                       StkId res, TMS event) {
///   const TValue *tm = luaT_gettmbyobj(L, p1, event);
///   if (notm(tm)) tm = luaT_gettmbyobj(L, p2, event);
///   if (notm(tm)) return -1;
///   else return luaT_callTMres(L, tm, p1, p2, res);
/// }
/// ```
///
/// 返回 true 表示找到并调用了元方法，false 表示未找到。
fn callbin_tm(
    state: &mut LuaState,
    p1: &TValue,
    p2: &TValue,
    res: usize,
    tm: TagMethod,
) -> Result<bool, VmError> {
    // 先从 p1 查找元方法，再从 p2 查找
    let tm_val = get_tm_by_obj(p1, tm, &state.dmt)
        .or_else(|| get_tm_by_obj(p2, tm, &state.dmt))
        .cloned();

    match tm_val {
        Some(f) => {
            call_tm_res(state, &f, p1, p2, res)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// 尝试二元元方法 — 对应 C 的 luaT_trybinTM
///
/// C 实现:
/// ```c
/// void luaT_trybinTM (lua_State *L, const TValue *p1, const TValue *p2,
///                     StkId res, TMS event) {
///   if (l_unlikely(callbinTM(L, p1, p2, res, event) < 0)) {
///     switch (event) {
///       case TM_BAND: case TM_BOR: case TM_BXOR:
///       case TM_SHL: case TM_SHR: case TM_BNOT: {
///         if (ttisnumber(p1) && ttisnumber(p2))
///           luaG_tointerror(L, p1, p2);
///         else
///           luaG_opinterror(L, p1, p2, "perform bitwise operation on");
///       }
///       default:
///         luaG_opinterror(L, p1, p2, "perform arithmetic on");
///     }
///   }
/// }
/// ```
///
/// 找到元方法时调用它并将结果写入 res 槽位；未找到时返回 VmError。
pub fn try_bin_tm(
    state: &mut LuaState,
    p1: &TValue,
    p2: &TValue,
    res: usize,
    tm: TagMethod,
) -> Result<(), VmError> {
    if !callbin_tm(state, p1, p2, res, tm)? {
        // 未找到元方法 — 根据事件类型报错
        return Err(match tm {
            TagMethod::BAnd | TagMethod::BOr | TagMethod::BXor
            | TagMethod::Shl | TagMethod::Shr | TagMethod::BNot => {
                if p1.is_number() && p2.is_number() {
                    tointerror(p1, p2)
                } else {
                    opinterror(p1, p2, "perform bitwise operation on")
                }
            }
            _ => opinterror(p1, p2, "perform arithmetic on"),
        });
    }
    Ok(())
}

/// 尝试关联二元元方法 — 对应 C 的 luaT_trybinassocTM
///
/// C 实现:
/// ```c
/// void luaT_trybinassocTM (lua_State *L, const TValue *p1, const TValue *p2,
///                          int flip, StkId res, TMS event) {
///   if (flip) luaT_trybinTM(L, p2, p1, res, event);
///   else      luaT_trybinTM(L, p1, p2, res, event);
/// }
/// ```
pub fn try_bin_assoc_tm(
    state: &mut LuaState,
    p1: &TValue,
    p2: &TValue,
    flip: bool,
    res: usize,
    tm: TagMethod,
) -> Result<(), VmError> {
    if flip {
        try_bin_tm(state, p2, p1, res, tm)
    } else {
        try_bin_tm(state, p1, p2, res, tm)
    }
}

/// 尝试整数二元元方法 — 对应 C 的 luaT_trybiniTM
///
/// C 实现:
/// ```c
/// void luaT_trybiniTM (lua_State *L, const TValue *p1, lua_Integer i2,
///                      int flip, StkId res, TMS event) {
///   TValue aux;
///   setivalue(&aux, i2);
///   luaT_trybinassocTM(L, p1, &aux, flip, res, event);
/// }
/// ```
pub fn try_bini_tm(
    state: &mut LuaState,
    p1: &TValue,
    i2: i64,
    flip: bool,
    res: usize,
    tm: TagMethod,
) -> Result<(), VmError> {
    let aux = TValue::Integer(i2);
    try_bin_assoc_tm(state, p1, &aux, flip, res, tm)
}

/// 尝试字符串拼接元方法 — 对应 C 的 luaT_tryconcatTM
///
/// C 实现:
/// ```c
/// void luaT_tryconcatTM (lua_State *L) {
///   StkId p1 = L->top.p - 2;
///   if (l_unlikely(callbinTM(L, s2v(p1), s2v(p1 + 1), p1, TM_CONCAT) < 0))
///     luaG_concaterror(L, s2v(p1), s2v(p1 + 1));
/// }
/// ```
///
/// Rust 版本: p1, p2 为操作数，res 为结果槽位。
pub fn try_concat_tm(
    state: &mut LuaState,
    p1: &TValue,
    p2: &TValue,
    res: usize,
) -> Result<(), VmError> {
    if !callbin_tm(state, p1, p2, res, TagMethod::Concat)? {
        return Err(concaterror(p1, p2));
    }
    Ok(())
}

/// 调用顺序比较元方法 — 对应 C 的 luaT_callorderTM
///
/// C 实现:
/// ```c
/// int luaT_callorderTM (lua_State *L, const TValue *p1, const TValue *p2,
///                       TMS event) {
///   int tag = callbinTM(L, p1, p2, L->top.p, event);
///   if (tag >= 0) return !tagisfalse(tag);
///   luaG_ordererror(L, p1, p2);
///   return 0;
/// }
/// ```
pub fn call_order_tm(
    state: &mut LuaState,
    p1: &TValue,
    p2: &TValue,
    tm: TagMethod,
) -> Result<bool, VmError> {
    debug_assert!(tm == TagMethod::Lt || tm == TagMethod::Le);
    // 使用栈顶作为临时结果槽位
    let res = state.stack.len();
    state.stack.push(TValue::Nil(NilKind::Strict));

    let found = callbin_tm(state, p1, p2, res, tm)?;

    if found {
        let result = state.stack[res].clone();
        state.stack.truncate(res);
        state.top = state.stack.len();
        Ok(!result.is_false())
    } else {
        state.stack.truncate(res);
        state.top = state.stack.len();
        Err(ordererror(p1, p2))
    }
}

/// 调用整数顺序比较元方法 — 对应 C 的 luaT_callorderiTM
pub fn call_orderi_tm(
    state: &mut LuaState,
    p1: &TValue,
    v2: i64,
    flip: bool,
    tm: TagMethod,
) -> Result<bool, VmError> {
    let aux = TValue::Integer(v2);
    let (a, b) = if flip { (&aux, p1) } else { (p1, &aux) };
    call_order_tm(state, a, b, tm)
}

// ============================================================================
// equal_obj — 对应 C 的 luaV_equalobj
// ============================================================================

/// 比较两个 TValue 是否相等（支持元方法回退）— 对应 C 的 luaV_equalobj
///
/// C 实现:
/// ```c
/// int luaV_equalobj (lua_State *L, const TValue *t1, const TValue *t2) {
///   if (ttype(t1) != ttype(t2)) return 0;
///   else if (ttypetag(t1) != ttypetag(t2)) { ... }
///   else {  /* equal variants */
///     switch (ttypetag(t1)) {
///       case LUA_VTABLE: case LUA_VUSERDATA:
///         if (same object) return 1;
///         tm = fasttm(..., TM_EQ);
///         if (tm == NULL) return 0;
///         break;
///       ...
///     }
///     if (tm == NULL) return 0;
///     luaT_callTMres(L, tm, t1, t2, L->top.p);
///     return !tagisfalse(tag);
///   }
/// }
/// ```
pub fn equal_obj(
    state: &mut LuaState,
    t1: &TValue,
    t2: &TValue,
) -> Result<bool, VmError> {
    // C: if (ttype(t1) != ttype(t2)) return 0;
    // ttype 检查基类型: Integer 和 Float 同属 LUA_TNUMBER
    // 先处理数字混合比较 (integer == float), 与 C 的 ttypetag 分支一致
    if t1.is_number() && t2.is_number() {
        // 两个数字: 用 raw_equal 处理 (包括 integer/float 混合比较)
        return Ok(crate::vm::raw_equal(t1, t2));
    }
    if std::mem::discriminant(t1) != std::mem::discriminant(t2) {
        return Ok(false);
    }
    // raw_equal 处理值类型和引用类型（同对象）的相等
    if crate::vm::raw_equal(t1, t2) {
        return Ok(true);
    }
    // C: 只有 table 和 userdata 才尝试 __eq 元方法
    match (t1, t2) {
        (TValue::Table(_), TValue::Table(_)) | (TValue::UserData(_), TValue::UserData(_)) => {}
        _ => return Ok(false),
    }
    // C: fasttm(L, hvalue(t1)->metatable, TM_EQ) ?? fasttm(L, hvalue(t2)->metatable, TM_EQ)
    let tm = get_tm_by_obj(t1, TagMethod::Eq, &state.dmt)
        .or_else(|| get_tm_by_obj(t2, TagMethod::Eq, &state.dmt))
        .cloned();
    match tm {
        Some(f) => {
            // C: luaT_callTMres(L, tm, t1, t2, L->top.p); return !tagisfalse(tag);
            let res = state.stack.len();
            state.stack.push(TValue::Nil(NilKind::Strict));
            call_tm_res(state, &f, t1, t2, res)?;
            let result = state.stack[res].clone();
            state.stack.truncate(res);
            state.top = state.stack.len();
            Ok(!result.is_false())
        }
        None => Ok(false),
    }
}

// ============================================================================
// obj_len — 对应 C 的 luaV_objlen
// ============================================================================

/// 计算值的长度（# 操作符）并写入栈 — 对应 C 的 luaV_objlen
///
/// C 实现:
/// ```c
/// void luaV_objlen (lua_State *L, StkId ra, const TValue *rb) {
///   const TValue *tm;
///   switch (ttypetag(rb)) {
///     case LUA_VTABLE: {
///       Table *h = hvalue(rb);
///       tm = fasttm(L, h->metatable, TM_LEN);
///       if (tm) break;
///       setivalue(s2v(ra), luaH_getn(L, h));
///       return;
///     }
///     case LUA_VSHRSTR: case LUA_VLNGSTR: {
///       setivalue(s2v(ra), tsvalue(rb)->len);
///       return;
///     }
///     default: {
///       tm = luaT_gettmbyobj(L, rb, TM_LEN);
///       if (notm(tm)) luaG_typeerror(L, rb, "get length of");
///       break;
///     }
///   }
///   luaT_callTMres(L, tm, rb, rb, ra);
/// }
/// ```
pub fn obj_len(
    state: &mut LuaState,
    ra: usize,
    rb: &TValue,
) -> Result<(), VmError> {
    let tm: Option<TValue> = match rb {
        TValue::Table(t) => {
            // 先查表自身元表的 __len
            let tm_val = t.metatable.as_ref().and_then(|mt| {
                let mut meta = Metatable::new(mt.as_ref().clone());
                meta.get_tm(TagMethod::Len).cloned()
            });
            if tm_val.is_some() {
                tm_val
            } else {
                // 无元方法: 返回表长度
                let len = t.len();
                while state.stack.len() <= ra {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
                state.stack[ra] = TValue::Integer(len as i64);
                state.top = state.stack.len();
                return Ok(());
            }
        }
        TValue::Str(s) => {
            // 字符串: 返回长度
            let len = s.len();
            while state.stack.len() <= ra {
                state.stack.push(TValue::Nil(NilKind::Strict));
            }
            state.stack[ra] = TValue::Integer(len as i64);
            state.top = state.stack.len();
            return Ok(());
        }
        _ => {
            // 其他类型: 查找 __len 元方法
            get_tm_by_obj(rb, TagMethod::Len, &state.dmt).cloned()
        }
    };

    match tm {
        Some(f) => {
            // C: luaT_callTMres(L, tm, rb, rb, ra);
            call_tm_res(state, &f, rb, rb, ra)
        }
        None => {
            Err(VmError::RuntimeError(format!(
                "attempt to get length of a {} value",
                obj_type_name(rb)
            )))
        }
    }
}

// ============================================================================
// VarargInfo / VarargTable (ltm.c 变参处理 → Rust 惯用)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarargInfo {
    pub total_args: usize,
    pub num_fixed_params: usize,
    pub has_vararg_table: bool,
    pub has_hidden_varargs: bool,
}

impl VarargInfo {
    pub fn is_vararg(&self) -> bool {
        self.has_vararg_table || self.has_hidden_varargs
    }

    pub fn num_extra(&self) -> usize {
        let extra = self.total_args.saturating_sub(self.num_fixed_params);
        if self.is_vararg() { extra } else { 0 }
    }
}

#[derive(Debug, Clone)]
pub enum VarargTable {
    Table { table: Table, count: usize },
    Hidden { args: Vec<TValue> },
}

impl VarargTable {
    pub fn from_hidden(args: Vec<TValue>) -> Self {
        VarargTable::Hidden { args }
    }

    pub fn from_args(args: &[TValue]) -> Self {
        let mut t = Table::new();
        let count = args.len();
        for (i, v) in args.iter().enumerate() {
            t.set_int(i as i64 + 1, v.clone());
        }
        t.set(
            TValue::Str(LuaString::Short(std::sync::Arc::new(ShortString {
                hash: rust_hash("n"), contents: "n".to_string(),
            }))),
            TValue::Integer(count as i64),
        );
        VarargTable::Table { table: t, count }
    }

    pub fn count(&self) -> usize {
        match self {
            VarargTable::Table { count, .. } => *count,
            VarargTable::Hidden { args } => args.len(),
        }
    }

    pub fn get(&self, idx: usize) -> Option<TValue> {
        if idx < 1 { return None; }
        let i = idx - 1;
        match self {
            VarargTable::Table { ref table, count } => {
                if i >= *count { return None; }
                table.get_int(i as i64 + 1).cloned()
            }
            VarargTable::Hidden { args } => args.get(i).cloned(),
        }
    }

    pub fn get_vararg(&self, key: &TValue) -> Option<TValue> {
        match key {
            TValue::Integer(i) => self.get(*i as usize),
            TValue::Str(s) if s.as_str() == "n" => Some(TValue::Integer(self.count() as i64)),
            _ => None,
        }
    }

    pub fn get_varargs(&self, wanted: isize) -> Vec<TValue> {
        let n = self.count();
        let take = if wanted < 0 { n } else { let w = wanted as usize; if w > n { n } else { w } };
        let total = if wanted < 0 { n } else { wanted as usize };
        let mut result = Vec::with_capacity(total);
        for i in 0..take {
            result.push(self.get(i + 1).unwrap_or_else(|| TValue::Nil(NilKind::Strict)));
        }
        for _ in take..total {
            result.push(TValue::Nil(NilKind::Strict));
        }
        result
    }
}

// ============================================================================
// 辅助构造 TValue / LuaString
// ============================================================================

pub fn make_tm_tvalue(tm: TagMethod) -> TValue {
    let name = tm.name();
    TValue::Str(LuaString::Short(std::sync::Arc::new(ShortString {
        hash: rust_hash(name),
        contents: name.to_string(),
    })))
}

fn make_ls(s: &str) -> TValue {
    TValue::Str(LuaString::Short(std::sync::Arc::new(ShortString {
        hash: rust_hash(s),
        contents: s.to_string(),
    })))
}

fn make_name_key() -> TValue {
    make_ls("__name")
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::StringTable;

    // ========================================================================
    // TagMethod 测试
    // ========================================================================

    #[test]
    fn test_tag_method_count() {
        assert_eq!(TagMethod::Close as u8 + 1, TM_N as u8);
        assert_eq!(TM_N, 25);
    }

    #[test]
    fn test_tag_method_all_names() {
        for i in 0..TM_N as u8 {
            let tm = TagMethod::from_u8(i).unwrap();
            assert!(!tm.name().is_empty());
            assert!(tm.name().starts_with("__"));
        }
    }

    #[test]
    fn test_tag_method_is_fast_access() {
        assert!(TagMethod::Index.is_fast_access());
        assert!(TagMethod::Eq.is_fast_access());
        assert!(!TagMethod::Add.is_fast_access());
        assert!(!TagMethod::Call.is_fast_access());
    }

    #[test]
    fn test_tag_method_from_u8() {
        assert_eq!(TagMethod::from_u8(0), Some(TagMethod::Index));
        assert_eq!(TagMethod::from_u8(24), Some(TagMethod::Close));
        assert_eq!(TagMethod::from_u8(25), None);
    }

    #[test]
    fn test_tag_method_display() {
        assert_eq!(format!("{}", TagMethod::Index), "__index");
        assert_eq!(format!("{}", TagMethod::Add), "__add");
    }

    // ========================================================================
    // MetatableFlags / Metatable 测试
    // ========================================================================

    #[test]
    fn test_metatable_flags_from_tag_method() {
        assert_eq!(MetatableFlags::from_tag_method(TagMethod::Index), Some(MetatableFlags::NO_INDEX));
        assert_eq!(MetatableFlags::from_tag_method(TagMethod::Eq), Some(MetatableFlags::NO_EQ));
        assert_eq!(MetatableFlags::from_tag_method(TagMethod::Add), None);
    }

    #[test]
    fn test_metatable_get_tm_and_cache() {
        let mut mt = Metatable::empty();
        mt.table.set(make_tm_tvalue(TagMethod::Index), TValue::Integer(42));
        assert!(mt.get_tm(TagMethod::Index).is_some());
        assert!(mt.get_tm(TagMethod::Len).is_none());
        assert!(mt.flags.contains(MetatableFlags::NO_LEN));
    }

    #[test]
    fn test_metatable_cache_hit() {
        let mut mt = Metatable::empty();
        mt.flags.insert(MetatableFlags::NO_INDEX);
        mt.table.set(make_tm_tvalue(TagMethod::Index), TValue::Integer(99));
        assert!(mt.get_tm(TagMethod::Index).is_none());
    }

    // ========================================================================
    // type_name / obj_type_name 测试
    // ========================================================================

    #[test]
    fn test_type_name_all() {
        assert_eq!(type_name(LuaType::Nil), "nil");
        assert_eq!(type_name(LuaType::Number), "number");
        assert_eq!(type_name(LuaType::String), "string");
        assert_eq!(type_name(LuaType::Table), "table");
    }

    #[test]
    fn test_obj_type_name_plain_table() {
        assert_eq!(obj_type_name(&TValue::Table(Table::new())), "table");
    }

    #[test]
    fn test_obj_type_name_integer() {
        assert_eq!(obj_type_name(&TValue::Integer(42)), "number");
    }

    // ========================================================================
    // DefaultMetatables 测试
    // ========================================================================

    #[test]
    fn test_default_metatables_set_and_get() {
        let mut dmt = DefaultMetatables::new();
        let mut mt_data = Table::new();
        mt_data.set(make_tm_tvalue(TagMethod::Add), TValue::Integer(99));
        let mt = Metatable::new(mt_data);
        dmt.set(LuaType::Number, mt);
        assert!(dmt.get(LuaType::Number).is_some());
    }

    // ========================================================================
    // TagMethodError 测试
    // ========================================================================

    #[test]
    fn test_tag_method_error_display() {
        let err = TagMethodError::NoMetamethod(TagMethod::Add);
        assert!(format!("{}", err).contains("__add"));
    }

    #[test]
    fn test_tag_method_error_is_trait() {
        fn assert_error<T: std::error::Error>() {}
        assert_error::<TagMethodError>();
    }

    // ========================================================================
    // try_bin_tm / try_concat_tm / call_order_tm 测试
    // ========================================================================

    #[test]
    fn test_try_bin_tm_not_found() {
        let mut state = LuaState::new();
        let p1 = TValue::Integer(1);
        let p2 = TValue::Integer(2);
        // 整数没有 __add 元方法，应返回 RuntimeError
        let result = try_bin_tm(&mut state, &p1, &p2, 0, TagMethod::Add);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, VmError::RuntimeError(_)));
    }

    #[test]
    fn test_try_bin_tm_nil_operand() {
        // 对应闭包 n=n+1 时 n 为 nil 的场景
        let mut state = LuaState::new();
        let p1 = TValue::Nil(NilKind::Strict);
        let p2 = TValue::Integer(1);
        let result = try_bin_tm(&mut state, &p1, &p2, 0, TagMethod::Add);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, VmError::RuntimeError(_)));
        // 错误消息应包含 "nil"
        assert!(format!("{}", err).contains("nil"));
    }

    #[test]
    fn test_try_concat_tm_no_metamethod() {
        let mut state = LuaState::new();
        // nil 不能拼接
        let result = try_concat_tm(&mut state, &TValue::Nil(NilKind::Strict), &TValue::Integer(2), 0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VmError::RuntimeError(_)));
    }

    #[test]
    fn test_call_order_tm_no_metamethod() {
        let mut state = LuaState::new();
        // nil 和 integer 无法比较
        let result = call_order_tm(&mut state, &TValue::Nil(NilKind::Strict), &TValue::Integer(2), TagMethod::Lt);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VmError::RuntimeError(_)));
    }

    #[test]
    fn test_call_orderi_tm_no_metamethod() {
        let mut state = LuaState::new();
        let p1 = TValue::Nil(NilKind::Strict);
        let result = call_orderi_tm(&mut state, &p1, 3, false, TagMethod::Lt);
        assert!(result.is_err());
    }

    // ========================================================================
    // VarargInfo / VarargTable 测试
    // ========================================================================

    #[test]
    fn test_vararg_info_fixed() {
        let info = VarargInfo { total_args: 3, num_fixed_params: 3, has_vararg_table: false, has_hidden_varargs: false };
        assert!(!info.is_vararg());
        assert_eq!(info.num_extra(), 0);
    }

    #[test]
    fn test_vararg_info_vararg() {
        let info = VarargInfo { total_args: 5, num_fixed_params: 2, has_vararg_table: false, has_hidden_varargs: true };
        assert!(info.is_vararg());
        assert_eq!(info.num_extra(), 3);
    }

    #[test]
    fn test_vararg_table_from_args() {
        let args = vec![TValue::Integer(10), TValue::Integer(20), TValue::Integer(30)];
        let vt = VarargTable::from_args(&args);
        assert_eq!(vt.count(), 3);
        assert_eq!(vt.get(1), Some(TValue::Integer(10)));
        assert_eq!(vt.get(3), Some(TValue::Integer(30)));
        assert!(vt.get(4).is_none());
    }

    #[test]
    fn test_vararg_table_hidden() {
        let args = vec![TValue::Integer(100), TValue::Integer(200)];
        let vt = VarargTable::from_hidden(args);
        assert_eq!(vt.count(), 2);
        assert_eq!(vt.get(1), Some(TValue::Integer(100)));
    }

    #[test]
    fn test_vararg_table_get_varargs_less() {
        let args = vec![TValue::Integer(1), TValue::Integer(2), TValue::Integer(3)];
        let vt = VarargTable::from_hidden(args);
        let result = vt.get_varargs(2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], TValue::Integer(1));
    }

    #[test]
    fn test_vararg_table_get_varargs_more() {
        let args = vec![TValue::Integer(1)];
        let vt = VarargTable::from_hidden(args);
        let result = vt.get_varargs(3);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], TValue::Integer(1));
        assert!(result[1].is_nil());
        assert!(result[2].is_nil());
    }

    #[test]
    fn test_vararg_table_get_varargs_all() {
        let args = vec![TValue::Integer(10), TValue::Integer(20), TValue::Integer(30)];
        let vt = VarargTable::from_hidden(args);
        let result = vt.get_varargs(-1);
        assert_eq!(result.len(), 3);
    }

    fn _make_tm_tvalue_local(tm: TagMethod) -> TValue {
        super::make_tm_tvalue(tm)
    }

    fn _make_ls(s: &str) -> LuaString {
        let tb = StringTable::new();
        tb.intern(s)
    }
}