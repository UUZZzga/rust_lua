//! 基础库 (lbaselib.cpp → Rust)
//!
//! 对应 C 源码: lbaselib.cpp
//!
//! ## 主要功能
//! - 注册基础全局函数: print, type, tonumber, tostring, error,
//!   pcall, xpcall, assert, select, setmetatable, getmetatable,
//!   rawequal, rawlen, rawget, rawset, next, ipairs, pairs, warn
//! - 提供函数标签派发机制 (LightUserData 标签)
//!
//! ## 标签分配
//! - 标签 1-6: 原有临时实现 (print, setmetatable, getmetatable, type, pcall, error)
//! - 标签 7+: 新增基础库函数

use crate::objects::{NilKind, TValue};
use crate::state::LuaState;
use crate::execute::VmError;
use std::io::Write;

// ============================================================================
// 函数标签 (LightUserData 占位符值)
// ============================================================================

// 原有标签 (保持兼容性)
pub const BASE_PRINT: usize = 1;
pub const BASE_SETMETATABLE: usize = 2;
pub const BASE_GETMETATABLE: usize = 3;
pub const BASE_TYPE: usize = 4;
pub const BASE_PCALL: usize = 5;
pub const BASE_ERROR: usize = 6;

// 新增标签
pub const BASE_TONUMBER: usize = 7;
pub const BASE_TOSTRING: usize = 8;
pub const BASE_ASSERT: usize = 9;
pub const BASE_SELECT: usize = 10;
pub const BASE_RAWEQUAL: usize = 11;
pub const BASE_RAWLEN: usize = 12;
pub const BASE_RAWGET: usize = 13;
pub const BASE_RAWSET: usize = 14;
pub const BASE_NEXT: usize = 15;
pub const BASE_IPAIRS: usize = 16;
pub const BASE_PAIRS: usize = 17;
pub const BASE_XPCALL: usize = 18;
pub const BASE_WARN: usize = 19;

// 迭代器辅助函数标签 (不在 is_base_tag 范围内, 只在 TFORCALL 中处理)
// 对应 C 的 ipairsaux 和 next 迭代器函数
pub const BASE_IPAIRS_AUX: usize = 20;
pub const BASE_NEXT_ITER: usize = 21;

/// 标签是否属于基础库
pub fn is_base_tag(tag: usize) -> bool {
    tag >= BASE_PRINT && tag <= BASE_WARN
}

/// 判断标签是否为已知的函数标签 (用于 type/tostring 显示)
///
/// 包括基础库函数标签 (1-19)、迭代器辅助函数标签 (20-21) 和字符串库标签 (100+)
pub fn is_function_tag(tag: usize) -> bool {
    is_base_tag(tag) || tag == BASE_IPAIRS_AUX || tag == BASE_NEXT_ITER || tag >= 100
}

// ============================================================================
// 辅助函数: TValue 转字符串 (对应 C 的 luaL_tolstring)
// ============================================================================

/// 将 TValue 转换为字符串表示 (对应 C 的 tostringbuff)
///
/// 用于 print 和 tostring 函数。
/// 注意: 此函数不调用 __tostring 元方法 (简化实现)。
pub fn lua_value_to_string(v: &TValue) -> String {
    match v {
        TValue::Nil(_) => "nil".to_string(),
        TValue::Boolean(b) => b.to_string(),
        TValue::Integer(n) => n.to_string(),
        TValue::Float(n) => format_float(*n),
        TValue::Str(s) => s.as_str().to_string(),
        TValue::Table(_) => "table: 0x0".to_string(),
        TValue::LClosure(_) | TValue::LCFn(_) | TValue::CClosure(_) => "function: 0x0".to_string(),
        TValue::LightUserData(p) => {
            // 内置函数标签显示为 function, 其他显示为 userdata
            let tag = *p as usize;
            if is_function_tag(tag) {
                "function: 0x0".to_string()
            } else {
                format!("userdata: {:?}", p)
            }
        }
        TValue::UserData(_) => "userdata: 0x0".to_string(),
        TValue::Thread(_) => "thread: 0x0".to_string(),
    }
}

/// 格式化浮点数 (对应 C 的 tostringbuffFloat)
///
/// 如果浮点数看起来像整数 (如 3.0), 则添加 ".0" 后缀。
fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    let s = format!("{}", f);
    // 如果结果看起来像整数 (只有数字和负号), 添加 ".0"
    let looks_like_int = s.chars().all(|c| c.is_ascii_digit() || c == '-');
    if looks_like_int && !s.is_empty() {
        format!("{}.0", s)
    } else {
        s
    }
}

// ============================================================================
// 字符串转整数 (对应 C 的 b_str2int)
// ============================================================================

const SPACECHARS: &[u8] = b" \x0c\n\r\t\x0b";

/// 将字符串按指定进制转换为整数 (对应 C 的 b_str2int)
///
/// 返回 Some(整数) 表示转换成功, None 表示失败。
/// 允许前导/尾随空格, 可选正负号。
pub fn b_str2int(s: &str, base: u32) -> Option<i64> {
    let bytes = s.as_bytes();
    let mut pos = 0;

    // 跳过前导空格
    while pos < bytes.len() && SPACECHARS.contains(&bytes[pos]) {
        pos += 1;
    }

    // 处理符号
    let neg = if pos < bytes.len() && bytes[pos] == b'-' {
        pos += 1;
        true
    } else if pos < bytes.len() && bytes[pos] == b'+' {
        pos += 1;
        false
    } else {
        false
    };

    // 必须至少有一个数字
    if pos >= bytes.len() || !bytes[pos].is_ascii_alphanumeric() {
        return None;
    }

    let mut n: u64 = 0;
    while pos < bytes.len() && bytes[pos].is_ascii_alphanumeric() {
        let c = bytes[pos];
        let digit = if c.is_ascii_digit() {
            (c - b'0') as u32
        } else {
            (c.to_ascii_uppercase() - b'A' + 10) as u32
        };
        if digit >= base {
            return None;
        }
        n = n.checked_mul(base as u64)?.checked_add(digit as u64)?;
        pos += 1;
    }

    // 跳过尾随空格
    while pos < bytes.len() && SPACECHARS.contains(&bytes[pos]) {
        pos += 1;
    }

    // 必须消费整个字符串
    if pos != bytes.len() {
        return None;
    }

    Some(if neg { -(n as i64) } else { n as i64 })
}

// ============================================================================
// 纯函数实现 (无状态, 可独立测试)
// ============================================================================

/// type(v) — 返回类型名字符串 (对应 C 的 luaB_type)
pub fn base_type_name(v: &TValue) -> &'static str {
    match v {
        TValue::Nil(_) => "nil",
        TValue::Boolean(_) => "boolean",
        TValue::LightUserData(p) => {
            // LightUserData 既用于实际 userdata, 也用于内置函数标签
            // 标签值在已知范围内 (1-19 基础库, 20-21 迭代器, 100+ 字符串库) 的是函数
            let tag = *p as usize;
            if is_function_tag(tag) {
                "function"
            } else {
                "userdata"
            }
        }
        TValue::Integer(_) | TValue::Float(_) => "number",
        TValue::Str(_) => "string",
        TValue::Table(_) => "table",
        TValue::LClosure(_) | TValue::CClosure(_) | TValue::LCFn(_) => "function",
        TValue::UserData(_) => "userdata",
        TValue::Thread(_) => "thread",
    }
}

/// tonumber(v [, base]) — 转换为数字 (对应 C 的 luaB_tonumber)
///
/// 无 base 参数时: 标准转换 (数字直接返回, 字符串解析为整数或浮点)
/// 有 base 参数时: 按进制解析字符串为整数
pub fn base_tonumber(v: &TValue, base: Option<i64>) -> Option<TValue> {
    match base {
        None => {
            // 标准转换
            match v {
                TValue::Integer(_) | TValue::Float(_) => Some(v.clone()),
                TValue::Str(s) => {
                    let s = s.as_str();
                    // 先尝试整数
                    if let Ok(i) = s.parse::<i64>() {
                        return Some(TValue::Integer(i));
                    }
                    // 再尝试浮点
                    if let Ok(f) = s.parse::<f64>() {
                        return Some(TValue::Float(f));
                    }
                    // 尝试十六进制 (0x 前缀)
                    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                        if let Ok(i) = i64::from_str_radix(rest, 16) {
                            return Some(TValue::Integer(i));
                        }
                    }
                    // 尝试十六进制浮点 (简化: 不支持)
                    None
                }
                _ => None,
            }
        }
        Some(b) => {
            // 按进制转换字符串
            if !(2..=36).contains(&b) {
                return None;
            }
            match v {
                TValue::Str(s) => {
                    b_str2int(s.as_str(), b as u32).map(TValue::Integer)
                }
                _ => None,
            }
        }
    }
}

/// tostring(v) — 转换为字符串 (对应 C 的 luaB_tostring)
pub fn base_tostring(v: &TValue) -> String {
    lua_value_to_string(v)
}

/// rawequal(v1, v2) — 原始相等比较 (对应 C 的 luaB_rawequal)
pub fn base_rawequal(v1: &TValue, v2: &TValue) -> bool {
    match (v1, v2) {
        (TValue::Nil(_), TValue::Nil(_)) => true,
        (TValue::Boolean(a), TValue::Boolean(b)) => a == b,
        (TValue::Integer(a), TValue::Integer(b)) => a == b,
        (TValue::Float(a), TValue::Float(b)) => a == b,
        (TValue::Integer(a), TValue::Float(b)) | (TValue::Float(b), TValue::Integer(a)) => {
            (*a as f64) == *b
        }
        (TValue::Str(a), TValue::Str(b)) => a == b,
        (TValue::LightUserData(a), TValue::LightUserData(b)) => std::ptr::eq(*a, *b),
        (TValue::Table(a), TValue::Table(b)) => std::ptr::eq(
            a as *const _ as *const u8,
            b as *const _ as *const u8,
        ),
        _ => false,
    }
}

/// rawlen(v) — 原始长度 (对应 C 的 luaB_rawlen)
pub fn base_rawlen(v: &TValue) -> Result<i64, String> {
    match v {
        TValue::Table(t) => Ok(t.len()),
        TValue::Str(s) => Ok(s.len() as i64),
        _ => Err(format!("table or string expected, got {}", base_type_name(v))),
    }
}

/// select(n, ...) — 选择参数 (对应 C 的 luaB_select)
///
/// n == "#": 返回参数总数
/// n > 0: 返回第 n 个及之后的参数
/// n < 0: 从末尾计数
pub fn base_select(n: i64, args: &[TValue]) -> Result<Vec<TValue>, String> {
    if n < 0 {
        let idx = (args.len() as i64 + n) as i64;
        if idx < 0 {
            return Err("bad argument #1 to 'select' (index out of range)".to_string());
        }
        Ok(args[idx as usize..].to_vec())
    } else if n == 0 {
        Err("bad argument #1 to 'select' (index out of range)".to_string())
    } else {
        let idx = (n - 1) as usize;
        if idx >= args.len() {
            Ok(vec![])
        } else {
            Ok(args[idx..].to_vec())
        }
    }
}

/// assert(v [, message]) — 断言 (对应 C 的 luaB_assert)
///
/// v 为真: 返回所有参数
/// v 为假: 抛出错误 (使用 message 或默认 "assertion failed!")
pub fn base_assert(args: &[TValue]) -> Result<Vec<TValue>, String> {
    if args.is_empty() {
        return Err("assertion failed!".to_string());
    }
    if args[0].is_false() {
        let msg = if args.len() >= 2 {
            lua_value_to_string(&args[1])
        } else {
            "assertion failed!".to_string()
        };
        Err(msg)
    } else {
        Ok(args.to_vec())
    }
}

// ============================================================================
// 栈操作辅助函数
// ============================================================================

/// 从栈中读取参数 (0-based 索引, 相对于函数位置 a)
fn get_arg(state: &LuaState, a: usize, idx: usize) -> TValue {
    let stack_idx = a + 1 + idx;
    if stack_idx < state.stack.len() {
        state.stack[stack_idx].clone()
    } else {
        TValue::Nil(NilKind::Strict)
    }
}

/// 将结果压入栈并调整栈顶
fn push_results(state: &mut LuaState, a: usize, nresults: i32, results: Vec<TValue>) {
    state.stack.truncate(a);
    let n = if nresults < 0 {
        results.len()
    } else {
        nresults as usize
    };
    for i in 0..n {
        if i < results.len() {
            state.stack.push(results[i].clone());
        } else {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
    }
}

/// 将单个结果压入栈
fn push_single_result(state: &mut LuaState, a: usize, nresults: i32, result: TValue) {
    push_results(state, a, nresults, vec![result]);
}

// ============================================================================
// 派发函数 — 从 execute.rs 的 op_call 和 state.rs 的 pcall 调用
// ============================================================================

/// 基础库函数派发
///
/// 从 execute.rs 的 op_call 或 state.rs 的 pcall 调用,
/// 当 LightUserData 标签在 [BASE_PRINT, BASE_WARN] 范围内时。
///
/// 参数:
/// - tag: 函数标签
/// - state: Lua 状态
/// - a: 函数在栈中的位置 (0-based)
/// - nargs: 参数数量
/// - nresults: 期望结果数 (-1 = MULTRET)
pub fn call_base_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    match tag {
        BASE_PRINT => call_print(state, a, nargs, nresults),
        BASE_SETMETATABLE => call_setmetatable(state, a, nargs, nresults),
        BASE_GETMETATABLE => call_getmetatable(state, a, nargs, nresults),
        BASE_TYPE => call_type(state, a, nargs, nresults),
        BASE_PCALL => call_pcall(state, a, nargs, nresults),
        BASE_ERROR => call_error(state, a, nargs, nresults),
        BASE_TONUMBER => call_tonumber(state, a, nargs, nresults),
        BASE_TOSTRING => call_tostring(state, a, nargs, nresults),
        BASE_ASSERT => call_assert(state, a, nargs, nresults),
        BASE_SELECT => call_select(state, a, nargs, nresults),
        BASE_RAWEQUAL => call_rawequal(state, a, nargs, nresults),
        BASE_RAWLEN => call_rawlen(state, a, nargs, nresults),
        BASE_RAWGET => call_rawget(state, a, nargs, nresults),
        BASE_RAWSET => call_rawset(state, a, nargs, nresults),
        BASE_NEXT => call_next(state, a, nargs, nresults),
        BASE_IPAIRS => call_ipairs(state, a, nargs, nresults),
        BASE_PAIRS => call_pairs(state, a, nargs, nresults),
        BASE_XPCALL => call_xpcall(state, a, nargs, nresults),
        BASE_WARN => call_warn(state, a, nargs, nresults),
        _ => Err(VmError::RuntimeError(format!("unknown base function tag: {}", tag))),
    }
}

// ============================================================================
// 各函数的派发实现
// ============================================================================

/// print(...) — 对应 C 的 luaB_print
fn call_print(state: &mut LuaState, a: usize, nargs: usize, _nresults: i32) -> Result<(), VmError> {
    let mut s = String::new();
    for i in 0..nargs {
        if i > 0 {
            s.push('\t');
        }
        let val = get_arg(state, a, i);
        s.push_str(&lua_value_to_string(&val));
    }
    let _ = writeln!(state.stdout, "{}", s);
    let _ = state.stdout.flush();
    // print 返回 0 个结果
    state.stack.truncate(a);
    Ok(())
}

/// setmetatable(t, mt) — 对应 C 的 luaB_setmetatable
fn call_setmetatable(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg2 = get_arg(state, a, 1);

    // 检查第二个参数是否为 nil 或表
    if !matches!(&arg2, TValue::Table(_) | TValue::Nil(_)) {
        return Err(VmError::RuntimeError(
            "bad argument #2 to 'setmetatable' (nil or table expected)".to_string(),
        ));
    }

    // 先 intern 字符串, 避免借用冲突
    let metatable_key = TValue::Str(state.intern_str("__metatable"));

    // 原地修改栈上的表 (对应 C 的直接操作栈)
    let result = {
        let arg1_ref = &mut state.stack[a + 1];
        match arg1_ref {
            TValue::Table(t) => {
                // 检查是否有 __metatable 元方法 (受保护的元表)
                if let Some(ref mt) = t.metatable {
                    if mt.get(&metatable_key).is_some() {
                        return Err(VmError::RuntimeError(
                            "cannot change a protected metatable".to_string(),
                        ));
                    }
                }
                // 设置元表
                match &arg2 {
                    TValue::Table(mt) => {
                        t.metatable = Some(Box::new(mt.clone()));
                    }
                    TValue::Nil(_) => {
                        t.metatable = None;
                    }
                    _ => unreachable!(),
                }
                state.stack[a + 1].clone()
            }
            _ => {
                return Err(VmError::RuntimeError(
                    "bad argument #1 to 'setmetatable' (table expected)".to_string(),
                ));
            }
        }
    };

    push_single_result(state, a, nresults, result);
    Ok(())
}

/// getmetatable(t) — 对应 C 的 luaB_getmetatable
fn call_getmetatable(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg = get_arg(state, a, 0);
    // 先 intern 字符串, 避免借用冲突
    let metatable_key = TValue::Str(state.intern_str("__metatable"));
    let result = match &arg {
        TValue::Table(t) => {
            if let Some(ref mt) = t.metatable {
                // 检查 __metatable 元方法
                match mt.get(&metatable_key) {
                    Some(val) => val.clone(),
                    None => TValue::Table(mt.as_ref().clone()),
                }
            } else {
                TValue::Nil(NilKind::Strict)
            }
        }
        _ => TValue::Nil(NilKind::Strict),
    };
    push_single_result(state, a, nresults, result);
    Ok(())
}

/// type(v) — 对应 C 的 luaB_type
fn call_type(state: &mut LuaState, a: usize, _nargs: usize, nresults: i32) -> Result<(), VmError> {
    let arg = get_arg(state, a, 0);
    if matches!(arg, TValue::Nil(NilKind::Empty)) {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'type' (value expected)".to_string(),
        ));
    }
    let name = base_type_name(&arg);
    let result = TValue::Str(state.intern_str(name));
    push_single_result(state, a, nresults, result);
    Ok(())
}

/// pcall(f, args...) — 对应 C 的 luaB_pcall
fn call_pcall(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let func = get_arg(state, a, 0);
    let pcall_nargs = nargs.saturating_sub(1);

    // 把 f 和 args 移到 a 位置 (覆盖 pcall 函数本身)
    // 栈布局: [pcall_func | f | arg1 | arg2 | ...]
    // 调整为: [f | arg1 | arg2 | ...]
    if a + 1 < state.stack.len() {
        state.stack[a] = func;
        if a + 1 < state.stack.len() {
            state.stack.remove(a + 1);
        }
    }

    let status = state.pcall(pcall_nargs, -1, 0);

    // pcall 后: 栈截断到 a, 结果在 a..
    let nret = state.stack.len().saturating_sub(a);

    // 收集结果
    let mut results: Vec<TValue> = Vec::new();
    if status == 0 {
        // 成功: true, 结果...
        results.push(TValue::Boolean(true));
        for i in 0..nret {
            results.push(state.stack[a + i].clone());
        }
    } else {
        // 失败: false, 错误消息
        results.push(TValue::Boolean(false));
        if nret > 0 {
            results.push(state.stack[a].clone());
        } else {
            results.push(TValue::Nil(NilKind::Strict));
        }
    }

    // 写回结果
    push_results(state, a, nresults, results);
    Ok(())
}

/// error(msg [, level]) — 对应 C 的 luaB_error
fn call_error(state: &mut LuaState, a: usize, _nargs: usize, _nresults: i32) -> Result<(), VmError> {
    let msg = get_arg(state, a, 0);
    let err_msg = match &msg {
        TValue::Str(s) => s.as_str().to_string(),
        _ => lua_value_to_string(&msg),
    };
    Err(VmError::RuntimeError(err_msg))
}

/// tonumber(v [, base]) — 对应 C 的 luaB_tonumber
fn call_tonumber(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg = get_arg(state, a, 0);
    let base_arg = if nargs >= 2 {
        get_arg(state, a, 1)
    } else {
        TValue::Nil(NilKind::Strict)
    };

    let result = if matches!(base_arg, TValue::Nil(_)) {
        // 标准转换
        base_tonumber(&arg, None)
    } else {
        // 按进制转换
        let base = match &base_arg {
            TValue::Integer(b) => Some(*b),
            TValue::Float(f) => Some(*f as i64),
            _ => None,
        };
        match base {
            Some(b) if (2..=36).contains(&b) => base_tonumber(&arg, Some(b)),
            _ => {
                return Err(VmError::RuntimeError(
                    "bad argument #2 to 'tonumber' (base out of range)".to_string(),
                ));
            }
        }
    };

    match result {
        Some(v) => push_single_result(state, a, nresults, v),
        None => push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict)),
    }
    Ok(())
}

/// tostring(v) — 对应 C 的 luaB_tostring
fn call_tostring(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg = get_arg(state, a, 0);
    let s = base_tostring(&arg);
    push_single_result(state, a, nresults, TValue::Str(state.intern_str(&s)));
    Ok(())
}

/// assert(v [, message]) — 对应 C 的 luaB_assert
fn call_assert(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let args: Vec<TValue> = (0..nargs).map(|i| get_arg(state, a, i)).collect();
    match base_assert(&args) {
        Ok(results) => {
            push_results(state, a, nresults, results);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// select(n, ...) — 对应 C 的 luaB_select
fn call_select(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'select' (value expected)".to_string(),
        ));
    }
    let first = get_arg(state, a, 0);

    // 特殊情况: "#"
    if let TValue::Str(s) = &first {
        if s.as_str() == "#" {
            let count = nargs.saturating_sub(1) as i64;
            push_single_result(state, a, nresults, TValue::Integer(count));
            return Ok(());
        }
    }

    // 数字索引
    let n = match &first {
        TValue::Integer(n) => *n,
        TValue::Float(f) => *f as i64,
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #1 to 'select' (number expected)".to_string(),
            ));
        }
    };

    let args: Vec<TValue> = (1..nargs).map(|i| get_arg(state, a, i)).collect();
    match base_select(n, &args) {
        Ok(results) => {
            push_results(state, a, nresults, results);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// rawequal(v1, v2) — 对应 C 的 luaB_rawequal
fn call_rawequal(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let v1 = get_arg(state, a, 0);
    let v2 = get_arg(state, a, 1);
    let result = base_rawequal(&v1, &v2);
    push_single_result(state, a, nresults, TValue::Boolean(result));
    Ok(())
}

/// rawlen(v) — 对应 C 的 luaB_rawlen
fn call_rawlen(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let v = get_arg(state, a, 0);
    match base_rawlen(&v) {
        Ok(len) => {
            push_single_result(state, a, nresults, TValue::Integer(len));
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// rawget(t, k) — 对应 C 的 luaB_rawget
fn call_rawget(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let t = get_arg(state, a, 0);
    let k = get_arg(state, a, 1);
    match &t {
        TValue::Table(table) => {
            let result = table.get(&k).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
            push_single_result(state, a, nresults, result);
            Ok(())
        }
        _ => Err(VmError::RuntimeError(
            "bad argument #1 to 'rawget' (table expected)".to_string(),
        )),
    }
}

/// rawset(t, k, v) — 对应 C 的 luaB_rawset
fn call_rawset(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let k = get_arg(state, a, 1);
    let v = get_arg(state, a, 2);

    // 原地修改栈上的表 (对应 C 的直接操作栈)
    let result = {
        let arg1_ref = &mut state.stack[a + 1];
        match arg1_ref {
            TValue::Table(t) => {
                t.set(k, v);
                state.stack[a + 1].clone()
            }
            _ => {
                return Err(VmError::RuntimeError(
                    "bad argument #1 to 'rawset' (table expected)".to_string(),
                ));
            }
        }
    };

    push_single_result(state, a, nresults, result);
    Ok(())
}

/// next(t [, key]) — 对应 C 的 luaB_next
fn call_next(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let t = get_arg(state, a, 0);
    let key = if nargs >= 2 {
        get_arg(state, a, 1)
    } else {
        TValue::Nil(NilKind::Strict)
    };

    match &t {
        TValue::Table(table) => {
            let (next_key, next_val) = table_next(table, &key);
            match next_key {
                Some(k) => {
                    push_results(state, a, nresults, vec![k, next_val]);
                }
                None => {
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                }
            }
            Ok(())
        }
        _ => Err(VmError::RuntimeError(
            "bad argument #1 to 'next' (table expected)".to_string(),
        )),
    }
}

/// ipairs(t) — 对应 C 的 luaB_ipairs
fn call_ipairs(state: &mut LuaState, a: usize, _nargs: usize, nresults: i32) -> Result<(), VmError> {
    let t = get_arg(state, a, 0);
    // 返回迭代器函数 (ipairsaux), 状态 t, 初始值 0
    // 使用 BASE_IPAIRS_AUX 标签表示 ipairsaux (与 BASE_IPAIRS 区分, 避免被 op_call 误派发)
    let iter = TValue::LightUserData(BASE_IPAIRS_AUX as *mut std::ffi::c_void);
    push_results(state, a, nresults, vec![iter, t, TValue::Integer(0)]);
    Ok(())
}

/// pairs(t) — 对应 C 的 luaB_pairs
fn call_pairs(state: &mut LuaState, a: usize, _nargs: usize, nresults: i32) -> Result<(), VmError> {
    let t = get_arg(state, a, 0);
    // 简化实现: 不检查 __pairs 元方法, 直接返回 next, t, nil
    // 使用 BASE_NEXT_ITER 标签表示 next 迭代器 (与 BASE_NEXT 区分, 避免被 op_call 误派发)
    let next_fn = TValue::LightUserData(BASE_NEXT_ITER as *mut std::ffi::c_void);
    push_results(state, a, nresults, vec![next_fn, t, TValue::Nil(NilKind::Strict)]);
    Ok(())
}

/// xpcall(f, err, args...) — 对应 C 的 luaB_xpcall
fn call_xpcall(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let func = get_arg(state, a, 0);
    let err_fn = get_arg(state, a, 1);
    let xpcall_nargs = nargs.saturating_sub(2);

    // 把 f 和 args 移到 a 位置
    // 栈布局: [xpcall_func | f | err_fn | arg1 | arg2 | ...]
    // 调整为: [f | arg1 | arg2 | ...]
    if a + 2 < state.stack.len() {
        state.stack[a] = func;
        // 移除 f (a+1) 和 err_fn (a+2)
        state.stack.remove(a + 1);
        state.stack.remove(a + 1);
    }

    let status = state.pcall(xpcall_nargs, -1, 0);

    let nret = state.stack.len().saturating_sub(a);
    let mut results: Vec<TValue> = Vec::new();
    if status == 0 {
        // 成功: true, 结果...
        results.push(TValue::Boolean(true));
        for i in 0..nret {
            results.push(state.stack[a + i].clone());
        }
    } else {
        // 失败: 调用错误处理函数
        let err_msg = if nret > 0 {
            state.stack[a].clone()
        } else {
            TValue::Nil(NilKind::Strict)
        };

        // 设置栈: [err_fn | err_msg]
        state.stack.truncate(a);
        state.stack.push(err_fn);
        state.stack.push(err_msg);

        // 调用错误处理函数 (1 个参数, MULTRET)
        let handler_status = state.pcall(1, -1, 0);
        let handler_nret = state.stack.len().saturating_sub(a);

        results.push(TValue::Boolean(false));
        if handler_status == 0 {
            // 错误处理函数成功: 返回其结果
            for i in 0..handler_nret {
                results.push(state.stack[a + i].clone());
            }
        } else {
            // 错误处理函数本身出错
            if handler_nret > 0 {
                results.push(state.stack[a].clone());
            } else {
                results.push(TValue::Nil(NilKind::Strict));
            }
        }
    }

    push_results(state, a, nresults, results);
    Ok(())
}

/// warn(...) — 对应 C 的 luaB_warn
fn call_warn(state: &mut LuaState, a: usize, nargs: usize, _nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'warn' (string expected)".to_string(),
        ));
    }
    let mut msg = String::new();
    for i in 0..nargs {
        let arg = get_arg(state, a, i);
        match &arg {
            TValue::Str(s) => msg.push_str(s.as_str()),
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #{} to 'warn' (string expected)",
                    i + 1
                )));
            }
        }
    }
    // 输出到 stderr (简化实现)
    eprintln!("Lua warning: {}", msg);
    state.stack.truncate(a);
    Ok(())
}

// ============================================================================
// 表遍历辅助函数 (对应 C 的 lua_next)
// ============================================================================

/// 表遍历: 给定当前 key, 返回下一个 key-value 对
///
/// 对应 C 的 lua_next 语义:
/// - key 为 nil 时返回第一个 key-value 对
/// - key 为最后一个 key 时返回 None
///
/// 遍历顺序: 先数组部分 (1, 2, ...), 再哈希部分
fn table_next(table: &crate::table::Table, key: &TValue) -> (Option<TValue>, TValue) {
    // 如果 key 是 nil, 从数组部分开始
    if matches!(key, TValue::Nil(_)) {
        return find_first(table);
    }

    // 如果 key 是整数且在数组范围内
    if let TValue::Integer(k) = key {
        if *k > 0 {
            let idx = (*k - 1) as usize;
            if idx < table.array.len() {
                // 尝试下一个数组元素
                let next_idx = idx + 1;
                if next_idx < table.array.len() {
                    let next_val = &table.array[next_idx];
                    if !matches!(next_val, TValue::Nil(NilKind::Empty)) {
                        return (
                            Some(TValue::Integer(next_idx as i64 + 1)),
                            next_val.clone(),
                        );
                    }
                }
                // 数组部分结束, 转到哈希部分
                return find_first_hash(table);
            }
        }
    }

    // key 在哈希部分, 找下一个哈希键
    find_next_hash(table, key)
}

/// 查找第一个非空元素 (数组部分)
fn find_first(table: &crate::table::Table) -> (Option<TValue>, TValue) {
    // 先查找数组部分
    for (i, v) in table.array.iter().enumerate() {
        if !matches!(v, TValue::Nil(NilKind::Empty)) {
            return (Some(TValue::Integer(i as i64 + 1)), v.clone());
        }
    }
    // 数组部分为空, 查找哈希部分
    find_first_hash(table)
}

/// 查找哈希部分的第一个元素
fn find_first_hash(table: &crate::table::Table) -> (Option<TValue>, TValue) {
    if let Some((k, v)) = table.hash.iter().next() {
        return (Some(k.clone()), v.clone());
    }
    (None, TValue::Nil(NilKind::Strict))
}

/// 在哈希部分中查找给定 key 之后的下一个 key
fn find_next_hash(
    table: &crate::table::Table,
    key: &TValue,
) -> (Option<TValue>, TValue) {
    let mut found = false;
    for (k, v) in table.hash.iter() {
        if found {
            return (Some(k.clone()), v.clone());
        }
        if k == key {
            found = true;
        }
    }
    (None, TValue::Nil(NilKind::Strict))
}

// ============================================================================
// ipairs 辅助函数 — 对应 C 的 ipairsaux
// ============================================================================

/// ipairs 迭代器函数 (对应 C 的 ipairsaux)
///
/// 参数: state=t, control=i
/// 返回: i+1, t[i+1] (如果 t[i+1] 不为 nil)
pub fn call_ipairs_aux(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let t = get_arg(state, a, 0);
    let i = get_arg(state, a, 1);
    let i = match &i {
        TValue::Integer(n) => *n,
        TValue::Float(f) => *f as i64,
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #2 to 'ipairs' iterator (number expected)".to_string(),
            ));
        }
    };
    let next_i = i + 1;

    match &t {
        TValue::Table(table) => {
            let val = table
                .get_int(next_i)
                .cloned()
                .unwrap_or(TValue::Nil(NilKind::Strict));
            if matches!(val, TValue::Nil(_)) {
                // 结束迭代
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            } else {
                push_results(state, a, nresults, vec![TValue::Integer(next_i), val]);
            }
            Ok(())
        }
        _ => Err(VmError::RuntimeError(
            "bad argument #1 to 'ipairs' iterator (table expected)".to_string(),
        )),
    }
}

/// pairs 迭代器函数 (对应 C 的 next, 在 TFORCALL 中调用)
///
/// 参数: state=t, control=key
/// 返回: next_key, next_value (如果到达末尾则返回 nil)
pub fn call_next_iter(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    call_next(state, a, nargs, nresults)
}

// ============================================================================
// 打开基础库 — 对应 C 的 luaopen_base
// ============================================================================

/// 打开基础库, 注册所有全局函数
///
/// 对应 C 源码 lbaselib.cpp 的 luaopen_base 函数:
/// 1. 注册所有基础函数到全局表
/// 2. 设置 _G 和 _VERSION
pub fn open_base_lib(state: &mut LuaState) {
    // 注册所有基础库函数 (使用 LightUserData 标签)
    let register = |state: &mut LuaState, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        state.globals.set(
            key,
            TValue::LightUserData(tag as *mut std::ffi::c_void),
        );
    };

    // 原有函数 (保持兼容性)
    register(state, "print", BASE_PRINT);
    register(state, "setmetatable", BASE_SETMETATABLE);
    register(state, "getmetatable", BASE_GETMETATABLE);
    register(state, "type", BASE_TYPE);
    register(state, "pcall", BASE_PCALL);
    register(state, "error", BASE_ERROR);

    // 新增函数
    register(state, "tonumber", BASE_TONUMBER);
    register(state, "tostring", BASE_TOSTRING);
    register(state, "assert", BASE_ASSERT);
    register(state, "select", BASE_SELECT);
    register(state, "rawequal", BASE_RAWEQUAL);
    register(state, "rawlen", BASE_RAWLEN);
    register(state, "rawget", BASE_RAWGET);
    register(state, "rawset", BASE_RAWSET);
    register(state, "next", BASE_NEXT);
    register(state, "ipairs", BASE_IPAIRS);
    register(state, "pairs", BASE_PAIRS);
    register(state, "xpcall", BASE_XPCALL);
    register(state, "warn", BASE_WARN);

    // 设置 _G 全局变量 (指向全局表自身)
    let globals_clone = state.globals.clone();
    let g_key = TValue::Str(state.intern_str("_G"));
    state.globals.set(g_key, TValue::Table(globals_clone));

    // 设置 _VERSION 全局变量
    let version_key = TValue::Str(state.intern_str("_VERSION"));
    state.globals.set(
        version_key,
        TValue::Str(state.intern_str("Lua 5.5")),
    );
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_str(s: &str) -> TValue {
        TValue::Str(crate::strings::LuaString::Short(Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: s.to_string(),
            },
        )))
    }

    // ========================================================================
    // b_str2int 测试
    // ========================================================================

    #[test]
    fn test_b_str2int_decimal() {
        assert_eq!(b_str2int("42", 10), Some(42));
        assert_eq!(b_str2int("0", 10), Some(0));
        assert_eq!(b_str2int("-42", 10), Some(-42));
        assert_eq!(b_str2int("+42", 10), Some(42));
    }

    #[test]
    fn test_b_str2int_hex() {
        assert_eq!(b_str2int("ff", 16), Some(255));
        assert_eq!(b_str2int("FF", 16), Some(255));
        assert_eq!(b_str2int("1A", 16), Some(26));
    }

    #[test]
    fn test_b_str2int_binary() {
        assert_eq!(b_str2int("1010", 2), Some(10));
        assert_eq!(b_str2int("0", 2), Some(0));
    }

    #[test]
    fn test_b_str2int_with_spaces() {
        assert_eq!(b_str2int("  42  ", 10), Some(42));
        assert_eq!(b_str2int("  -42  ", 10), Some(-42));
    }

    #[test]
    fn test_b_str2int_invalid() {
        assert_eq!(b_str2int("abc", 10), None);
        assert_eq!(b_str2int("", 10), None);
        assert_eq!(b_str2int("8", 8), None); // 8 不是八进制数字
        assert_eq!(b_str2int("2", 2), None); // 2 不是二进制数字
    }

    // ========================================================================
    // base_type_name 测试
    // ========================================================================

    #[test]
    fn test_base_type_name() {
        assert_eq!(base_type_name(&TValue::Nil(NilKind::Strict)), "nil");
        assert_eq!(base_type_name(&TValue::Boolean(true)), "boolean");
        assert_eq!(base_type_name(&TValue::Integer(42)), "number");
        assert_eq!(base_type_name(&TValue::Float(3.14)), "number");
        assert_eq!(base_type_name(&make_str("hello")), "string");
        assert_eq!(base_type_name(&TValue::Table(crate::table::Table::new())), "table");
    }

    // ========================================================================
    // base_tonumber 测试
    // ========================================================================

    #[test]
    fn test_base_tonumber_integer() {
        let v = TValue::Integer(42);
        assert_eq!(base_tonumber(&v, None), Some(TValue::Integer(42)));
    }

    #[test]
    fn test_base_tonumber_float() {
        let v = TValue::Float(3.14);
        assert_eq!(base_tonumber(&v, None), Some(TValue::Float(3.14)));
    }

    #[test]
    fn test_base_tonumber_string_integer() {
        let v = make_str("42");
        assert_eq!(base_tonumber(&v, None), Some(TValue::Integer(42)));
    }

    #[test]
    fn test_base_tonumber_string_float() {
        let v = make_str("3.14");
        let result = base_tonumber(&v, None);
        assert!(matches!(result, Some(TValue::Float(f)) if (f - 3.14).abs() < 1e-10));
    }

    #[test]
    fn test_base_tonumber_string_hex() {
        let v = make_str("0xff");
        assert_eq!(base_tonumber(&v, None), Some(TValue::Integer(255)));
    }

    #[test]
    fn test_base_tonumber_with_base() {
        let v = make_str("ff");
        assert_eq!(base_tonumber(&v, Some(16)), Some(TValue::Integer(255)));
    }

    #[test]
    fn test_base_tonumber_invalid_string() {
        let v = make_str("abc");
        assert_eq!(base_tonumber(&v, None), None);
    }

    #[test]
    fn test_base_tonumber_invalid_base() {
        let v = make_str("42");
        assert_eq!(base_tonumber(&v, Some(1)), None);
        assert_eq!(base_tonumber(&v, Some(37)), None);
    }

    // ========================================================================
    // base_tostring 测试
    // ========================================================================

    #[test]
    fn test_base_tostring() {
        assert_eq!(base_tostring(&TValue::Nil(NilKind::Strict)), "nil");
        assert_eq!(base_tostring(&TValue::Boolean(true)), "true");
        assert_eq!(base_tostring(&TValue::Boolean(false)), "false");
        assert_eq!(base_tostring(&TValue::Integer(42)), "42");
        assert_eq!(base_tostring(&make_str("hello")), "hello");
    }

    #[test]
    fn test_base_tostring_float() {
        assert_eq!(base_tostring(&TValue::Float(3.14)), "3.14");
        assert_eq!(base_tostring(&TValue::Float(3.0)), "3.0");
        assert_eq!(base_tostring(&TValue::Float(f64::NAN)), "nan");
        assert_eq!(base_tostring(&TValue::Float(f64::INFINITY)), "inf");
        assert_eq!(base_tostring(&TValue::Float(f64::NEG_INFINITY)), "-inf");
    }

    // ========================================================================
    // base_rawequal 测试
    // ========================================================================

    #[test]
    fn test_base_rawequal() {
        assert!(base_rawequal(&TValue::Nil(NilKind::Strict), &TValue::Nil(NilKind::Empty)));
        assert!(base_rawequal(&TValue::Boolean(true), &TValue::Boolean(true)));
        assert!(!base_rawequal(&TValue::Boolean(true), &TValue::Boolean(false)));
        assert!(base_rawequal(&TValue::Integer(42), &TValue::Integer(42)));
        assert!(base_rawequal(&TValue::Integer(42), &TValue::Float(42.0)));
        assert!(base_rawequal(&make_str("a"), &make_str("a")));
        assert!(!base_rawequal(&make_str("a"), &make_str("b")));
    }

    // ========================================================================
    // base_rawlen 测试
    // ========================================================================

    #[test]
    fn test_base_rawlen_string() {
        assert_eq!(base_rawlen(&make_str("hello")).unwrap(), 5);
        assert_eq!(base_rawlen(&make_str("")).unwrap(), 0);
    }

    #[test]
    fn test_base_rawlen_table() {
        let mut t = crate::table::Table::new();
        t.set(TValue::Integer(1), TValue::Integer(10));
        t.set(TValue::Integer(2), TValue::Integer(20));
        assert_eq!(base_rawlen(&TValue::Table(t)).unwrap(), 2);
    }

    #[test]
    fn test_base_rawlen_invalid() {
        assert!(base_rawlen(&TValue::Integer(42)).is_err());
        assert!(base_rawlen(&TValue::Boolean(true)).is_err());
    }

    // ========================================================================
    // base_select 测试
    // ========================================================================

    #[test]
    fn test_base_select_positive() {
        let args = vec![
            TValue::Integer(1),
            TValue::Integer(2),
            TValue::Integer(3),
        ];
        let result = base_select(2, &args).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], TValue::Integer(2));
        assert_eq!(result[1], TValue::Integer(3));
    }

    #[test]
    fn test_base_select_negative() {
        let args = vec![
            TValue::Integer(1),
            TValue::Integer(2),
            TValue::Integer(3),
        ];
        let result = base_select(-1, &args).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], TValue::Integer(3));
    }

    #[test]
    fn test_base_select_out_of_range() {
        let args = vec![TValue::Integer(1)];
        let result = base_select(5, &args).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_base_select_zero_error() {
        let args = vec![TValue::Integer(1)];
        assert!(base_select(0, &args).is_err());
    }

    // ========================================================================
    // base_assert 测试
    // ========================================================================

    #[test]
    fn test_base_assert_true() {
        let args = vec![TValue::Boolean(true), make_str("msg")];
        let result = base_assert(&args).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_base_assert_false() {
        let args = vec![TValue::Boolean(false), make_str("error msg")];
        let result = base_assert(&args);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "error msg");
    }

    #[test]
    fn test_base_assert_false_default_msg() {
        let args = vec![TValue::Boolean(false)];
        let result = base_assert(&args);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "assertion failed!");
    }

    #[test]
    fn test_base_assert_nil_is_false() {
        let args = vec![TValue::Nil(NilKind::Strict)];
        let result = base_assert(&args);
        assert!(result.is_err());
    }

    // ========================================================================
    // lua_value_to_string 测试
    // ========================================================================

    #[test]
    fn test_lua_value_to_string() {
        assert_eq!(lua_value_to_string(&TValue::Nil(NilKind::Strict)), "nil");
        assert_eq!(lua_value_to_string(&TValue::Boolean(true)), "true");
        assert_eq!(lua_value_to_string(&TValue::Integer(42)), "42");
        assert_eq!(lua_value_to_string(&make_str("hello")), "hello");
    }

    #[test]
    fn test_lua_value_to_string_float() {
        assert_eq!(lua_value_to_string(&TValue::Float(3.14)), "3.14");
        assert_eq!(lua_value_to_string(&TValue::Float(3.0)), "3.0");
        assert_eq!(lua_value_to_string(&TValue::Float(f64::NAN)), "nan");
    }

    // ========================================================================
    // format_float 测试
    // ========================================================================

    #[test]
    fn test_format_float() {
        assert_eq!(format_float(3.14), "3.14");
        assert_eq!(format_float(3.0), "3.0");
        assert_eq!(format_float(-3.0), "-3.0");
        assert_eq!(format_float(0.0), "0.0");
        assert_eq!(format_float(f64::NAN), "nan");
        assert_eq!(format_float(f64::INFINITY), "inf");
        assert_eq!(format_float(f64::NEG_INFINITY), "-inf");
    }

    // ========================================================================
    // is_base_tag 测试
    // ========================================================================

    #[test]
    fn test_is_base_tag() {
        assert!(is_base_tag(BASE_PRINT));
        assert!(is_base_tag(BASE_ERROR));
        assert!(is_base_tag(BASE_WARN));
        assert!(!is_base_tag(0));
        assert!(!is_base_tag(100)); // 字符串库标签
    }

    // ========================================================================
    // open_base_lib 测试
    // ========================================================================

    #[test]
    fn test_open_base_lib_registers_functions() {
        let mut state = LuaState::new();
        open_base_lib(&mut state);

        // 验证原有函数
        for name in &["print", "setmetatable", "getmetatable", "type", "pcall", "error"] {
            let key = TValue::Str(state.intern_str(name));
            assert!(state.globals.get(&key).is_some(), "{} must be registered", name);
        }

        // 验证新增函数
        for name in &[
            "tonumber", "tostring", "assert", "select", "rawequal", "rawlen",
            "rawget", "rawset", "next", "ipairs", "pairs", "xpcall", "warn",
        ] {
            let key = TValue::Str(state.intern_str(name));
            assert!(state.globals.get(&key).is_some(), "{} must be registered", name);
        }
    }

    #[test]
    fn test_open_base_lib_registers_version() {
        let mut state = LuaState::new();
        open_base_lib(&mut state);
        let key = TValue::Str(state.intern_str("_VERSION"));
        let val = state.globals.get(&key);
        assert!(val.is_some(), "_VERSION must be registered");
        if let Some(TValue::Str(s)) = val {
            assert!(s.as_str().contains("Lua"));
        }
    }

    #[test]
    fn test_open_base_lib_registers_g() {
        let mut state = LuaState::new();
        open_base_lib(&mut state);
        let key = TValue::Str(state.intern_str("_G"));
        let val = state.globals.get(&key);
        assert!(val.is_some(), "_G must be registered");
        assert!(matches!(val, Some(TValue::Table(_))));
    }

    // ========================================================================
    // call_base_function 测试
    // ========================================================================

    #[test]
    fn test_call_base_function_type() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_TYPE as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(42));
        call_base_function(BASE_TYPE, &mut state, 0, 1, 1).unwrap();
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "number"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_base_function_tonumber() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_TONUMBER as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("42")));
        call_base_function(BASE_TONUMBER, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 42),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_base_function_tostring() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_TOSTRING as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(42));
        call_base_function(BASE_TOSTRING, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "42"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_base_function_rawequal() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_RAWEQUAL as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(42));
        state.stack.push(TValue::Integer(42));
        call_base_function(BASE_RAWEQUAL, &mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Boolean(b) => assert!(*b),
            _ => panic!("expected boolean result"),
        }
    }

    #[test]
    fn test_call_base_function_rawlen() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_RAWLEN as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("hello")));
        call_base_function(BASE_RAWLEN, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 5),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_base_function_rawget() {
        let mut state = LuaState::new();
        state.stack.clear();
        let mut t = crate::table::Table::new();
        t.set(TValue::Integer(1), TValue::Integer(100));
        state.stack.push(TValue::LightUserData(BASE_RAWGET as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Integer(1));
        call_base_function(BASE_RAWGET, &mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 100),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_base_function_rawset() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        state.stack.push(TValue::LightUserData(BASE_RAWSET as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(999));
        call_base_function(BASE_RAWSET, &mut state, 0, 3, 1).unwrap();
        match &state.stack[0] {
            TValue::Table(t) => {
                let val = t.get(&TValue::Integer(1));
                assert!(matches!(val, Some(TValue::Integer(999))));
            }
            _ => panic!("expected table result"),
        }
    }

    #[test]
    fn test_call_base_function_select_hash() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_SELECT as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("#")));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(2));
        state.stack.push(TValue::Integer(3));
        call_base_function(BASE_SELECT, &mut state, 0, 4, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 3),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_base_function_select_index() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_SELECT as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(2));
        state.stack.push(TValue::Integer(10));
        state.stack.push(TValue::Integer(20));
        state.stack.push(TValue::Integer(30));
        call_base_function(BASE_SELECT, &mut state, 0, 4, -1).unwrap();
        assert_eq!(state.stack.len(), 2);
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 20),
            _ => panic!("expected integer 20"),
        }
        match &state.stack[1] {
            TValue::Integer(n) => assert_eq!(*n, 30),
            _ => panic!("expected integer 30"),
        }
    }

    #[test]
    fn test_call_base_function_assert_true() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_ASSERT as *mut std::ffi::c_void));
        state.stack.push(TValue::Boolean(true));
        call_base_function(BASE_ASSERT, &mut state, 0, 1, -1).unwrap();
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0] {
            TValue::Boolean(b) => assert!(*b),
            _ => panic!("expected boolean true"),
        }
    }

    #[test]
    fn test_call_base_function_assert_false() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_ASSERT as *mut std::ffi::c_void));
        state.stack.push(TValue::Boolean(false));
        let result = call_base_function(BASE_ASSERT, &mut state, 0, 1, -1);
        assert!(result.is_err());
    }

    #[test]
    fn test_call_base_function_error() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(BASE_ERROR as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("test error")));
        let result = call_base_function(BASE_ERROR, &mut state, 0, 1, 0);
        assert!(result.is_err());
        match result {
            Err(VmError::RuntimeError(msg)) => assert_eq!(msg, "test error"),
            _ => panic!("expected RuntimeError"),
        }
    }

    #[test]
    fn test_call_base_function_setmetatable() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        let mt = crate::table::Table::new();
        state.stack.push(TValue::LightUserData(BASE_SETMETATABLE as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Table(mt));
        call_base_function(BASE_SETMETATABLE, &mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Table(t) => assert!(t.metatable.is_some()),
            _ => panic!("expected table result"),
        }
    }

    #[test]
    fn test_call_base_function_getmetatable() {
        let mut state = LuaState::new();
        state.stack.clear();
        let mut t = crate::table::Table::new();
        t.metatable = Some(Box::new(crate::table::Table::new()));
        state.stack.push(TValue::LightUserData(BASE_GETMETATABLE as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        call_base_function(BASE_GETMETATABLE, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Table(_) => {}
            _ => panic!("expected table result"),
        }
    }

    #[test]
    fn test_call_base_function_getmetatable_no_mt() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        state.stack.push(TValue::LightUserData(BASE_GETMETATABLE as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        call_base_function(BASE_GETMETATABLE, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Nil(_) => {}
            _ => panic!("expected nil result"),
        }
    }

    #[test]
    fn test_call_base_function_ipairs() {
        let mut state = LuaState::new();
        state.stack.clear();
        let mut t = crate::table::Table::new();
        t.set(TValue::Integer(1), TValue::Integer(10));
        t.set(TValue::Integer(2), TValue::Integer(20));
        state.stack.push(TValue::LightUserData(BASE_IPAIRS as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        call_base_function(BASE_IPAIRS, &mut state, 0, 1, 3).unwrap();
        assert_eq!(state.stack.len(), 3);
        // 第一个返回值是迭代器函数 (使用 BASE_IPAIRS_AUX 标签)
        match &state.stack[0] {
            TValue::LightUserData(p) => {
                let tag = *p as usize;
                assert_eq!(tag, BASE_IPAIRS_AUX);
            }
            _ => panic!("expected LightUserData"),
        }
        // 第二个返回值是表
        assert!(matches!(state.stack[1], TValue::Table(_)));
        // 第三个返回值是 0
        match &state.stack[2] {
            TValue::Integer(n) => assert_eq!(*n, 0),
            _ => panic!("expected integer 0"),
        }
    }

    #[test]
    fn test_call_base_function_pairs() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        state.stack.push(TValue::LightUserData(BASE_PAIRS as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        call_base_function(BASE_PAIRS, &mut state, 0, 1, 3).unwrap();
        assert_eq!(state.stack.len(), 3);
        match &state.stack[0] {
            TValue::LightUserData(p) => {
                let tag = *p as usize;
                assert_eq!(tag, BASE_NEXT_ITER);
            }
            _ => panic!("expected LightUserData"),
        }
        assert!(matches!(state.stack[1], TValue::Table(_)));
        assert!(matches!(state.stack[2], TValue::Nil(_)));
    }

    #[test]
    fn test_call_ipairs_aux() {
        let mut state = LuaState::new();
        state.stack.clear();
        let mut t = crate::table::Table::new();
        t.set(TValue::Integer(1), TValue::Integer(10));
        t.set(TValue::Integer(2), TValue::Integer(20));
        state.stack.push(TValue::LightUserData(BASE_IPAIRS_AUX as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Integer(0));
        call_ipairs_aux(&mut state, 0, 2, -1).unwrap();
        assert_eq!(state.stack.len(), 2);
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 1),
            _ => panic!("expected integer 1"),
        }
        match &state.stack[1] {
            TValue::Integer(n) => assert_eq!(*n, 10),
            _ => panic!("expected integer 10"),
        }
    }

    #[test]
    fn test_call_ipairs_aux_end() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        state.stack.push(TValue::LightUserData(BASE_IPAIRS_AUX as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Integer(0));
        call_ipairs_aux(&mut state, 0, 2, 1).unwrap();
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_base_function_unknown_tag() {
        let mut state = LuaState::new();
        let result = call_base_function(999, &mut state, 0, 0, 0);
        assert!(result.is_err());
    }

    // ========================================================================
    // table_next 测试
    // ========================================================================

    #[test]
    fn test_table_next_array() {
        // 使用 with_capacity 预分配数组部分, 确保值存储在数组中 (顺序迭代)
        let mut t = crate::table::Table::with_capacity(2, 0);
        t.set(TValue::Integer(1), TValue::Integer(10));
        t.set(TValue::Integer(2), TValue::Integer(20));

        // 从 nil 开始
        let (key, val) = table_next(&t, &TValue::Nil(NilKind::Strict));
        assert!(matches!(key, Some(TValue::Integer(1))));
        assert_eq!(val, TValue::Integer(10));

        // 下一个
        let (key, val) = table_next(&t, &TValue::Integer(1));
        assert!(matches!(key, Some(TValue::Integer(2))));
        assert_eq!(val, TValue::Integer(20));

        // 结束
        let (key, _) = table_next(&t, &TValue::Integer(2));
        assert!(key.is_none());
    }
}
