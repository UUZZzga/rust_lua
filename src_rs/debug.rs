//! 调试工具 — 错误报告 (ldebug.cpp → Rust 惯用重写)
//!
//! 对应 C 源码: ldebug.cpp 中的 luaG_concaterror / luaG_ordererror / luaG_runerror / luaG_opinterror
//!
//! ## 设计原则
//! - 错误通过 VmError 返回（对应 C 的 longjmp，但使用 Rust 的 Result 机制）
//! - 类型名通过 tm::obj_type_name 获取，支持元表 __name 字段
//! - 错误消息格式与 C 实现保持一致

use crate::execute::VmError;
use crate::objects::TValue;
use crate::tm::obj_type_name;
use crate::state::LuaState;

/// 通用运行时错误 — 对应 C 的 luaG_runerror
///
/// C 实现:
/// ```c
/// l_noret luaG_runerror (lua_State *L, const char *fmt, ...) {
///   ...
///   luaD_throw(L, LUA_ERRRUN);
/// }
/// ```
///
/// Rust 版本: 由于使用 Result 错误处理，此函数仅记录错误消息。
/// 实际错误通过 Result 返回给调用者。
pub fn runerror(_state: &mut LuaState, msg: &str, _args: &[&TValue]) {
    eprintln!("lua runtime error: {}", msg);
}

/// 字符串拼接错误 — 对应 C 的 luaG_concaterror
///
/// C 实现:
/// ```c
/// l_noret luaG_concaterror (lua_State *L, const TValue *p1, const TValue *p2) {
///   if (ttisstring(p1) || cvt2str(p1)) p1 = p2;
///   luaG_typeerror(L, p1, "concatenate");
/// }
/// ```
pub fn concaterror(p1: &TValue, p2: &TValue) -> VmError {
    let err_obj = if is_string_or_cvt2str(p1) { p2 } else { p1 };
    let tname = obj_type_name(err_obj);
    VmError::RuntimeError(format!("attempt to concatenate a {} value", tname))
}

/// 顺序比较错误 — 对应 C 的 luaG_ordererror
pub fn ordererror(p1: &TValue, p2: &TValue) -> VmError {
    let t1 = obj_type_name(p1);
    let t2 = obj_type_name(p2);
    let msg = if t1 == t2 {
        format!("attempt to compare two {} values", t1)
    } else {
        format!("attempt to compare {} with {}", t1, t2)
    };
    VmError::RuntimeError(msg)
}

/// 算术/位运算错误 — 对应 C 的 luaG_opinterror
///
/// C 实现:
/// ```c
/// l_noret luaG_opinterror (lua_State *L, const TValue *p1, const TValue *p2,
///                          const char *msg) {
///   lua_Number temp;
///   if (!tonumberns(p1, temp))  /* first operand is wrong? */
///     p2 = p1;  /* now second is wrong too */
///   luaG_typeerror(L, p2, msg);
/// }
/// ```
pub fn opinterror(p1: &TValue, p2: &TValue, op: &str) -> VmError {
    // C: 如果 p1 不是数字，则错误对象是 p1；否则错误对象是 p2
    let err_obj = if !is_number(p1) { p1 } else { p2 };
    let tname = obj_type_name(err_obj);
    VmError::RuntimeError(format!("attempt to {} a {} value", op, tname))
}

/// 整数转换错误 — 对应 C 的 luaG_tointerror
pub fn tointerror(p1: &TValue, p2: &TValue) -> VmError {
    let t1 = obj_type_name(p1);
    let t2 = obj_type_name(p2);
    VmError::RuntimeError(format!("number{} has no integer representation", 
        if t1 == t2 { format!("s ({})", t1) } else { format!(" or {}", t2) }))
}

/// 判断值是否为数字 — 对应 C 的 ttisnumber
fn is_number(v: &TValue) -> bool {
    matches!(v, TValue::Integer(_) | TValue::Float(_))
}

/// 判断值是否为字符串或可转换为字符串 — 对应 C 的 ttisstring || cvt2str
fn is_string_or_cvt2str(v: &TValue) -> bool {
    matches!(v, TValue::Str(_) | TValue::Integer(_) | TValue::Float(_))
}
