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

use crate::execute::VmError;
use crate::gc::GCObjectHeader;
use crate::objects::{LClosure, NilKind, Proto, TValue, UpVal, UpValRef};
use crate::state::LuaState;
use crate::strings::LuaString;
use std::io::Write;
use std::rc::Rc;

// ============================================================================
// 函数注册 (BuiltinFn 函数指针)
// ============================================================================
//
// 基础库所有函数（含迭代器 ipairsaux/next、searcher 占位函数、loadlib/searchpath）
// 已从 LightUserData(tag) 派发机制迁移到 BuiltinFn 函数指针方案。
// 迭代器函数 (call_ipairs_aux, call_next_iter) 作为 BuiltinFn 返回给 Lua，
// 由 op_call/op_tailcall/TFORCALL 的 BuiltinFn 分支统一派发。
//
// 注意：coroutine.wrap 返回 Table（带 WRAP_MARKER 元表），由 get_wrap_idx 检测，
// 不存在 LightUserData 形式的 wrap 函数。LightUserData 仅剩 io.lines 迭代器
// (tag >= 0x1000_0000_0000_0000)，由 io_lib::is_lines_iterator_tag 判定。

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
        TValue::LClosure(_)
        | TValue::LCFn(_)
        | TValue::CClosure(_)
        | TValue::BuiltinFn(_) => "function: 0x0".to_string(),
        // LightUserData 仅 io.lines 迭代器 (tag >= 0x1000_0000_0000_0000) 表现为 function
        TValue::LightUserData(p) => {
            let tag = *p as usize;
            if crate::stdlib::io_lib::is_lines_iterator_tag(tag) {
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
        return if f > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
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
        // LightUserData 仅 io.lines 迭代器 (tag >= 0x1000_0000_0000_0000) 表现为 function
        TValue::LightUserData(p) => {
            let tag = *p as usize;
            if crate::stdlib::io_lib::is_lines_iterator_tag(tag) {
                "function"
            } else {
                "userdata"
            }
        }
        TValue::Integer(_) | TValue::Float(_) => "number",
        TValue::Str(_) => "string",
        TValue::Table(_) => {
            // coroutine.wrap 返回的 Table 在 type() 中应表现为 "function"
            if crate::stdlib::coroutine_lib::get_wrap_idx(v).is_some() {
                "function"
            } else {
                "table"
            }
        }
        TValue::LClosure(_) | TValue::CClosure(_) | TValue::LCFn(_) | TValue::BuiltinFn(_) => {
            "function"
        }
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
                TValue::Str(s) => crate::objects::str2num(s.as_str()),
                _ => None,
            }
        }
        Some(b) => {
            // 按进制转换字符串
            if !(2..=36).contains(&b) {
                return None;
            }
            match v {
                TValue::Str(s) => b_str2int(s.as_str(), b as u32).map(TValue::Integer),
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
        (TValue::Table(a), TValue::Table(b)) => a.gc_header.ptr_id == b.gc_header.ptr_id,
        (TValue::UserData(a), TValue::UserData(b)) => a.gc_header.ptr_id == b.gc_header.ptr_id,
        _ => false,
    }
}

/// rawlen(v) — 原始长度 (对应 C 的 luaB_rawlen)
pub fn base_rawlen(v: &TValue) -> Result<i64, String> {
    match v {
        TValue::Table(t) => Ok(t.len()),
        TValue::Str(s) => Ok(s.len() as i64),
        _ => Err(format!(
            "table or string expected, got {}",
            base_type_name(v)
        )),
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
    state.adjust_results(a, nresults, results);
}

/// 将单个结果压入栈
fn push_single_result(state: &mut LuaState, a: usize, nresults: i32, result: TValue) {
    push_results(state, a, nresults, vec![result]);
}

// ============================================================================
// 各函数的实现（作为 BuiltinFnPtr 注册到全局表）
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
                if let Some(mt) = t.get_metatable() {
                    if mt.get(&metatable_key).is_some() {
                        return Err(VmError::RuntimeError(
                            "cannot change a protected metatable".to_string(),
                        ));
                    }
                }
                // 设置元表
                match &arg2 {
                    TValue::Table(mt) => {
                        t.set_metatable(Some(mt.clone()));
                    }
                    TValue::Nil(_) => {
                        t.set_metatable(None);
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

    // 检查是否设置了 __mode（弱引用表）或 __gc（finalizer），注册到相应列表
    if let TValue::Table(ref t) = result {
        let (has_mode, has_gc) = {
            let data = t.data.borrow();
            if let Some(ref mt) = data.metatable {
                let mode_key = TValue::Str(state.intern_str("__mode"));
                let gc_key = TValue::Str(state.intern_str("__gc"));
                (mt.get(&mode_key).is_some(), mt.get(&gc_key).is_some())
            } else {
                (false, false)
            }
        };
        if has_mode {
            state.register_weak_table(t);
        }
        if has_gc {
            state.register_finobj(t);
        }
    }

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
            if let Some(mt) = t.get_metatable() {
                // 检查 __metatable 元方法
                match mt.get(&metatable_key) {
                    Some(val) => val,
                    None => TValue::Table(mt),
                }
            } else {
                TValue::Nil(NilKind::Strict)
            }
        }
        // 基本类型: 从全局 G(L)->mt[type] 读取 (不检查 __metatable)
        _ => match state.dmt.get(arg.ty()) {
            Some(mt) => TValue::Table(mt.clone()),
            None => TValue::Nil(NilKind::Strict),
        },
    };
    push_single_result(state, a, nresults, result);
    Ok(())
}

/// type(v) — 对应 C 的 luaB_type
///
/// C 实现使用 luaL_argcheck(L, t != LUA_TNONE, 1, "value expected"):
/// 当参数缺失时 lua_type 返回 LUA_TNONE,从而报错;
/// 显式传入 nil 时返回 LUA_TNIL,正常返回 "nil"。
/// 这里用 nargs == 0 区分“参数缺失”与“显式 nil”。
fn call_type(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'type' (value expected)".to_string(),
        ));
    }
    let arg = get_arg(state, a, 0);
    let name = base_type_name(&arg);
    let result = TValue::Str(state.intern_str(name));
    push_single_result(state, a, nresults, result);
    Ok(())
}

/// pcall(f, args...) — 对应 C 的 luaB_pcall
pub(crate) fn call_pcall(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
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

    // 截断栈到 f + 其参数，确保 state.pcall 通过 stack.len() 计算的 func_idx 指向 f
    // (调用方帧可能有额外寄存器残留在参数之上)
    let new_top = a + pcall_nargs + 1;
    if state.stack.len() > new_top {
        state.stack.truncate(new_top);
    }

    // push pcall 保护状态 — 对应 C Lua 的 CIST_YPCALL
    // yield 穿过 pcall 后，pcall 的 C 函数栈帧被销毁，但保护状态保留。
    // 当 inner_func 后续执行 error/return 时，由 execute_loop 检查并处理。
    state
        .pcall_protection_stack
        .push(crate::state::PcallProtection {
            saved_code: state.code.clone(),
            saved_constants: state.constants.clone(),
            saved_upval_descs: state.upval_descs.clone(),
            saved_protos: state.protos.clone(),
            saved_base: state.base,
            saved_pc: state.pc,
            saved_num_params: state.num_params,
            saved_is_vararg: state.is_vararg,
            saved_proto_flag: state.proto_flag,
            saved_nextraargs: state.nextraargs,
            saved_closure_upvals: state.closure_upvals.clone(),
            saved_tbc_list: state.tbc_list,
            func_idx: a,
            // 保存 pcall 调用者期望的返回值数 (非 state.pcall 的 -1)，
            // 供 finish_pcall_return continuation 调整栈使用。
            nresults,
            pcall_kind: crate::state::PcallKind::Pcall,
            saved_filled: false,
            is_metamethod: false,
            metamethod_res: 0,
            saved_call_stack_len: 0,
            is_close_continuation: false,
            is_pairs_continuation: false,
        });
    let pcall_protection_idx = state.pcall_protection_stack.len() - 1;

    let status = state.pcall(pcall_nargs, -1, 0);

    // 非 yield 返回时 pop pcall 保护状态
    if status != crate::state::LUA_YIELD {
        state.pcall_protection_stack.pop();
    } else {
        // yield: 更新 pcall 的 PcallProtection 为 saved_filled=true
        // 当 inner_func 是 LClosure 时，state.pcall 的 LClosure 分支已经更新了
        // saved_filled=true 和 saved_pc+1，这里是冗余但无害的。
        // 当 inner_func 是 C 函数时，state.pcall 的 LightUserData 分支不更新 PcallProtection，
        // 所以这里需要手动更新。
        // saved_pc + 1: 跳过调用 pcall 的 CALL 指令（与 LClosure 分支的 saved_pc + 1 一致）。
        let protection = &mut state.pcall_protection_stack[pcall_protection_idx];
        if !protection.saved_filled {
            protection.saved_pc += 1;
            protection.saved_filled = true;
        }
    }

    // yield: 从 pending_yield 取出 yield 值并传播 (对应 C 中 yield 穿过 pcall)
    if status == crate::state::LUA_YIELD {
        let yield_values = state.pending_yield.take().unwrap_or_default();
        // yield 时不截断栈，保留 foo 的执行状态供第二次 resume 恢复
        // 对应 C Lua 中 yield 穿过 pcall，pcall 的状态被销毁
        return Err(VmError::Yield(yield_values));
    }

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

/// 对应 C 的 luaO_chunkid：将 source 格式化为短源标识
fn short_src(source: &LuaString) -> String {
    let bytes = source.as_str().as_bytes();
    if bytes.is_empty() {
        return "?".to_string();
    }
    match bytes[0] {
        b'=' => String::from_utf8_lossy(&bytes[1..]).into_owned(),
        b'@' => String::from_utf8_lossy(&bytes[1..]).into_owned(),
        _ => {
            let end = bytes
                .iter()
                .position(|&b| b == b'\n')
                .unwrap_or(bytes.len())
                .min(40);
            let head = String::from_utf8_lossy(&bytes[..end]);
            if bytes.len() > 40 || bytes.iter().any(|&b| b == b'\n') {
                format!("[string \"{}...\"]", head)
            } else {
                format!("[string \"{}\"]", head)
            }
        }
    }
}

/// 对应 C 的 luaG_getfuncline：从 Proto 的 line_info/abs_line_info 计算 pc 所在行号
fn get_func_line(proto: &Proto, pc: usize) -> i32 {
    if proto.line_info.is_empty() || pc >= proto.line_info.len() {
        return -1;
    }
    let mut base_pc = -1i32;
    let mut base_line = proto.line_defined;
    for abs in &proto.abs_line_info {
        let abs_pc = abs.pc;
        if abs_pc <= pc as i32 && abs_pc > base_pc {
            base_pc = abs_pc;
            base_line = abs.line;
        }
    }
    let mut line = base_line;
    let mut i = base_pc + 1;
    while i <= pc as i32 {
        let delta = proto.line_info[i as usize];
        if delta != i8::MIN {
            line += delta as i32;
        }
        i += 1;
    }
    line
}

/// 返回当前 Lua 函数的位置前缀 "source:line: "（对应 C 的 luaL_where 核心）
///
/// 当前 Lua 函数由 state.base/pc 代表。C 函数不改变 state.base,
/// 所以在 C 函数中调用时, state.base/pc 仍然是调用该 C 函数的 Lua 函数。
fn get_current_lua_func_position(state: &LuaState) -> String {
    if state.base == 0 || state.base > state.stack.len() {
        return String::new();
    }
    let closure = match &state.stack[state.base - 1] {
        TValue::LClosure(c) => c,
        _ => return String::new(),
    };
    let line = get_func_line(&closure.proto, state.pc);
    if line <= 0 {
        return String::new();
    }
    let source = closure
        .proto
        .source
        .as_ref()
        .map(short_src)
        .unwrap_or_else(|| "?".to_string());
    format!("{}:{}: ", source, line)
}

/// 对应 C 的 luaL_where：返回 "source:line: " 形式的位置前缀
///
/// level 语义对应 C 的 lua_getstack: level=0 是当前帧, level=1 是调用者帧。
/// C 版本中 C 函数（error/assert/pcall）创建 CallInfo, level=1 跳过当前 C 函数帧。
/// Rust 版本中 C 函数推入 call_info 但不改变 state.base, 需要检查 call_info
/// 来正确模拟 C 的 level 语义。
fn lua_l_where(state: &LuaState, level: usize) -> String {
    if level == 0 {
        return String::new();
    }
    // level=1: 调用当前函数的帧
    // level=k (k>=2): 回溯 call_stack 到更上层帧
    //   call_stack[last] = 直接调用者, call_stack[last-1] = 调用者的调用者, ...
    //   level=2 -> call_stack[cs_len-1], level=k -> call_stack[cs_len-(k-1)]
    if level == 1 {
        // 检查 call_info 最后一个元素是否是 C 函数帧
        // 对应 C 的 lua_getstack(L, 1): 跳过当前帧 (L->ci), 返回 L->ci->previous
        // Rust 版本中, call_info 最后一个元素是当前 C 函数帧 (如果在 C 函数中)
        if let Some(last_frame) = state.call_info.last() {
            if last_frame.is_c {
                // 当前在 C 函数中 (如 error/assert)
                // 调用该 C 函数的帧可能是:
                // 1. call_info[len-2] (如果是 C 函数帧) — 如 pcall -> assert
                //    C 函数帧 currentline=-1, 返回空字符串
                // 2. state.base/pc 代表的 Lua 函数 — 如直接调用 assert/error
                let ci_len = state.call_info.len();
                if ci_len >= 2 {
                    let prev_frame = &state.call_info[ci_len - 2];
                    if prev_frame.is_c {
                        // 调用者也是 C 函数 (如 pcall -> assert), 返回空字符串
                        return String::new();
                    }
                }
                // 调用者是 Lua 函数, 返回 state.base/pc 代表的 Lua 函数位置
                return get_current_lua_func_position(state);
            }
        }
        // 当前不在 C 函数中, 返回 state.base/pc 代表的 Lua 函数位置
        get_current_lua_func_position(state)
    } else {
        let cs_len = state.call_stack.len();
        // level=k 对应 call_stack[cs_len - (k-1)]
        let frame_idx = if cs_len >= level - 1 {
            cs_len - (level - 1)
        } else {
            return String::new();
        };
        let frame = &state.call_stack[frame_idx];
        if frame.base == 0 || frame.base > state.stack.len() {
            return String::new();
        }
        let closure = match &state.stack[frame.base - 1] {
            TValue::LClosure(c) => c,
            _ => return String::new(),
        };
        // return_pc - 1 是调用当前函数的 OP_CALL 指令的 PC
        let call_pc = if frame.return_pc > 0 {
            frame.return_pc - 1
        } else {
            0
        };
        let line = get_func_line(&closure.proto, call_pc);
        if line <= 0 {
            return String::new();
        }
        let source = closure
            .proto
            .source
            .as_ref()
            .map(short_src)
            .unwrap_or_else(|| "?".to_string());
        format!("{}:{}: ", source, line)
    }
}

/// error(msg [, level]) — 对应 C 的 luaB_error
fn call_error(state: &mut LuaState, a: usize, nargs: usize, _nresults: i32) -> Result<(), VmError> {
    let msg = get_arg(state, a, 0);
    let level = if nargs >= 2 {
        get_arg(state, a, 1).as_integer().unwrap_or(1) as i32
    } else {
        1
    };
    // 对应 C Lua 的 error(): 字符串且 level > 0 时添加位置前缀；其他情况原样返回
    // 对于非字符串或 level==0，错误消息不应被 build_traceback 再加前缀
    state.error_no_prefix = true; // 默认不要前缀（非字符串/level=0 路径）
    if let TValue::Str(s) = &msg {
        let mut err_msg = s.as_str().to_string();
        if level > 0 {
            let prefix = lua_l_where(state, level as usize);
            err_msg = format!("{}{}", prefix, err_msg);
            state.error_no_prefix = false; // 字符串 + level>0：已加前缀，build_traceback 不再处理
        }
        state.last_error_value = Some(TValue::Str(state.intern_str(&err_msg)));
        Err(VmError::RuntimeError(err_msg))
    } else {
        // 非字符串错误值: 原样返回（对应 C Lua 中 errfunc 为非字符串时的行为）
        // 特殊处理：error() 即 error(nil) 应返回 "<no error object>"（对应 C luaG_errormsg）
        if matches!(msg, TValue::Nil(_)) {
            let err_msg = "<no error object>".to_string();
            state.last_error_value = Some(TValue::Str(state.intern_str(&err_msg)));
            return Err(VmError::RuntimeError(err_msg));
        }
        // 保留原始 TValue 类型（coroutine.close 需要返回原始值）
        state.last_error_value = Some(msg.clone());
        Err(VmError::RuntimeErrorValue(msg))
    }
}

/// tonumber(v [, base]) — 对应 C 的 luaB_tonumber
fn call_tonumber(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 对应 C 的 luaL_checkany(L, 1)：必须有一个参数
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'tonumber' (value expected)".to_string(),
        ));
    }
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
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 对应 C 的 luaL_checkany(L, 1)：必须有一个参数
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'tostring' (value expected)".to_string(),
        ));
    }
    let arg = get_arg(state, a, 0);
    // 对应 C 的 luaL_tolstring: 先尝试调用 __tostring 元方法
    if let TValue::Table(t) = &arg {
        let tostring_key = TValue::Str(state.intern_str("__tostring"));
        let meta_fn = {
            let data = t.data.borrow();
            data.metatable.as_ref().and_then(|mt| mt.get(&tostring_key))
        };
        if let Some(f) = meta_fn {
            // 调用 __tostring(value)
            let base = state.stack.len();
            state.stack.push(f);
            state.stack.push(arg.clone());
            let status = state.pcall(1, 1, 0);
            if status != 0 {
                // pcall 失败: 传播错误
                let err = if base < state.stack.len() {
                    match &state.stack[base] {
                        TValue::Str(s) => s.as_str().to_string(),
                        other => format!("{:?}", other),
                    }
                } else {
                    String::new()
                };
                state.stack.truncate(base);
                return Err(VmError::RuntimeError(err));
            }
            // 检查返回值是否为字符串
            let result_str = if base < state.stack.len() {
                match &state.stack[base] {
                    TValue::Str(s) => Some(s.as_str().to_string()),
                    _ => None,
                }
            } else {
                None
            };
            state.stack.truncate(base);
            return match result_str {
                Some(s) => {
                    push_single_result(state, a, nresults, TValue::Str(state.intern_str(&s)));
                    Ok(())
                }
                None => Err(VmError::RuntimeError(
                    "'__tostring' must return a string".to_string(),
                )),
            };
        }
    }
    // UserData: 查找元表的 __tostring 元方法 (对应 C 的 luaL_tolstring)
    if let TValue::UserData(u) = &arg {
        let tostring_key = TValue::Str(state.intern_str("__tostring"));
        let meta_fn = u.metatable.as_ref().and_then(|mt| mt.get(&tostring_key));
        if let Some(f) = meta_fn {
            let base = state.stack.len();
            state.stack.push(f);
            state.stack.push(arg.clone());
            let status = state.pcall(1, 1, 0);
            if status != 0 {
                let err = if base < state.stack.len() {
                    match &state.stack[base] {
                        TValue::Str(s) => s.as_str().to_string(),
                        other => format!("{:?}", other),
                    }
                } else {
                    String::new()
                };
                state.stack.truncate(base);
                return Err(VmError::RuntimeError(err));
            }
            let result_str = if base < state.stack.len() {
                match &state.stack[base] {
                    TValue::Str(s) => Some(s.as_str().to_string()),
                    _ => None,
                }
            } else {
                None
            };
            state.stack.truncate(base);
            return match result_str {
                Some(s) => {
                    push_single_result(state, a, nresults, TValue::Str(state.intern_str(&s)));
                    Ok(())
                }
                None => Err(VmError::RuntimeError(
                    "'__tostring' must return a string".to_string(),
                )),
            };
        }
    }
    // 无 __tostring 元方法: 使用默认转换
    // 对应 C 的 luaL_tolstring default 分支: "%s: %p" 用 obj_type_name
    let s = match &arg {
        TValue::Integer(_)
        | TValue::Float(_)
        | TValue::Str(_)
        | TValue::Boolean(_)
        | TValue::Nil(_)
        | TValue::LightUserData(_) => lua_value_to_string(&arg),
        _ => format!("{}: 0x0", crate::tm::obj_type_name(&arg)),
    };
    push_single_result(state, a, nresults, TValue::Str(state.intern_str(&s)));
    Ok(())
}

/// assert(v [, message]) — 对应 C 的 luaB_assert
fn call_assert(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    // C 中 luaB_assert 先检查 lua_toboolean(L, 1)，无参数时返回 false 进入 else 分支，
    // 然后 luaL_checkany(L, 1) 触发 "bad argument #1 (value expected)" 错误
    if nargs == 0 {
        let prefix = lua_l_where(state, 1);
        return Err(VmError::RuntimeError(format!(
            "{}bad argument #1 to 'assert' (value expected)",
            prefix
        )));
    }
    let args: Vec<TValue> = (0..nargs).map(|i| get_arg(state, a, i)).collect();
    match base_assert(&args) {
        Ok(results) => {
            push_results(state, a, nresults, results);
            Ok(())
        }
        Err(msg) => {
            // C 中 luaB_assert 最终调用 luaB_error(level=1)，error 会拼接 where 信息
            // 非字符串错误值（如 assert(false, t)）保留原始 TValue（对应 C Lua 中 error(non-string, 1)）
            if args.len() >= 2 && !matches!(args[1], TValue::Str(_) | TValue::Nil(_)) {
                let err_val = args[1].clone();
                state.last_error_value = Some(err_val.clone());
                Err(VmError::RuntimeErrorValue(err_val))
            } else {
                let prefix = lua_l_where(state, 1);
                Err(VmError::RuntimeError(format!("{}{}", prefix, msg)))
            }
        }
    }
}

/// select(n, ...) — 对应 C 的 luaB_select
fn call_select(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
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
            let result = table.get(&k).unwrap_or(TValue::Nil(NilKind::Strict));
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

    // 对应 C 的 luaH_finishset: 插入新键时检查 NaN/nil 键
    // NaN 永远不等于自身, 故每次都是新键插入; nil 键同理
    match &k {
        TValue::Nil(_) => {
            return Err(VmError::RuntimeError("table index is nil".to_string()));
        }
        TValue::Float(f) if f.is_nan() => {
            return Err(VmError::RuntimeError("table index is NaN".to_string()));
        }
        _ => {}
    }

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
            let (next_key, next_val) =
                table_next(table, &key).map_err(|e| VmError::RuntimeError(e.to_string()))?;
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
fn call_ipairs(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'ipairs' (value expected)".to_string(),
        ));
    }
    let t = get_arg(state, a, 0);
    // 返回迭代器函数 (ipairsaux), 状态 t, 初始值 0
    // ipairsaux 作为 BuiltinFn 注册（名称 "for iterator" 对应 C 的 luaB_auxlib_getn 语义）
    let iter = TValue::BuiltinFn(crate::objects::BuiltinFn {
        func: call_ipairs_aux,
        name: c"for iterator".as_ptr() as *const u8,
    });
    push_results(state, a, nresults, vec![iter, t, TValue::Integer(0)]);
    Ok(())
}

/// pairs(t) — 对应 C 的 luaB_pairs
///
/// 无 __pairs 元方法时: 返回 next, t, nil, nil (第 4 个 nil 是 TBC 占位)
/// 有 __pairs 元方法时: 调用 __pairs(t) 获取迭代器/state/control/closing
///   __pairs 内部可能 yield (对应 C 的 lua_callk + pairscont continuation)
fn call_pairs(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'pairs' (value expected)".to_string(),
        ));
    }
    let t = get_arg(state, a, 0);
    // 对应 C luaB_pairs: 检查 __pairs 元方法
    let pairs_key = TValue::Str(state.intern_str("__pairs"));
    let meta_pairs = match &t {
        TValue::Table(tbl) => tbl.get_metatable().and_then(|mt| mt.get(&pairs_key)),
        _ => None,
    };

    if let Some(pairs_fn) = meta_pairs {
        // 有 __pairs: 调用 __pairs(t), 期望 4 个返回值
        // 栈布局: [pairs(LightUserData) | t] → [pairs_fn | t]
        state.stack[a] = pairs_fn;
        // 确保栈上有参数 t (a+1)
        while state.stack.len() <= a + 1 {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
        state.stack[a + 1] = t.clone();
        // 截断栈到 a+2 (移除多余参数)
        state.stack.truncate(a + 2);
        state.top = state.stack.len();

        // push pairs continuation 保护状态 — 对应 C 的 lua_callk + pairscont
        // __pairs 内部 yield 时，call_pairs 返回 Yield，保护状态保留。
        // resume 时 __pairs 返回，op_return 检查 pcall_protection_stack，
        // 调用 finish_pcall_return 执行 continuation（不 push true 前缀）。
        state
            .pcall_protection_stack
            .push(crate::state::PcallProtection {
                saved_code: state.code.clone(),
                saved_constants: state.constants.clone(),
                saved_upval_descs: state.upval_descs.clone(),
                saved_protos: state.protos.clone(),
                saved_base: state.base,
                saved_pc: state.pc + 1, // 跳过调用 pairs 的 CALL 指令
                saved_num_params: state.num_params,
                saved_is_vararg: state.is_vararg,
                saved_proto_flag: state.proto_flag,
                saved_nextraargs: state.nextraargs,
                saved_closure_upvals: state.closure_upvals.clone(),
                saved_tbc_list: state.tbc_list,
                func_idx: a,
                nresults,
                pcall_kind: crate::state::PcallKind::Pcall,
                saved_filled: false,
                is_metamethod: false,
                metamethod_res: 0,
                saved_call_stack_len: 0,
                is_close_continuation: false,
                is_pairs_continuation: true,
            });

        // state.pcall(1, 4): 1 arg (t), 4 results
        let status = state.pcall(1, 4, 0);

        if status == crate::state::LUA_YIELD {
            // __pairs 内部 yield: 传播 yield (保护状态保留，saved_filled 由 state.pcall 设为 true)
            let yield_values = state.pending_yield.take().unwrap_or_default();
            return Err(VmError::Yield(yield_values));
        }

        // 非 yield: pop 保护状态，取 4 个返回值
        state.pcall_protection_stack.pop();

        // 取 __pairs 的 4 个返回值 (state.pcall 已调整栈到 a..a+4)
        let results: Vec<TValue> = (0..4)
            .map(|i| {
                state
                    .stack
                    .get(a + i)
                    .cloned()
                    .unwrap_or(TValue::Nil(NilKind::Strict))
            })
            .collect();
        push_results(state, a, nresults, results);
    } else {
        // 无 __pairs: 返回 next, t, nil, nil (第 4 个 nil 是 TBC 占位)
        // next 作为 BuiltinFn 注册（名称 "next" 对应 C 的 luaB_next）
        let next_fn = TValue::BuiltinFn(crate::objects::BuiltinFn {
            func: call_next_iter,
            name: c"next".as_ptr() as *const u8,
        });
        push_results(
            state,
            a,
            nresults,
            vec![
                next_fn,
                t,
                TValue::Nil(NilKind::Strict),
                TValue::Nil(NilKind::Strict),
            ],
        );
    }
    Ok(())
}

/// xpcall(f, err, args...) — 对应 C 的 luaB_xpcall
pub(crate) fn call_xpcall(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
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

    // 截断栈到 f + 其参数，确保 state.pcall 通过 stack.len() 计算的 func_idx 指向 f
    // (调用方帧可能有额外寄存器残留在参数之上)
    let new_top = a + xpcall_nargs + 1;
    if state.stack.len() > new_top {
        state.stack.truncate(new_top);
    }

    // push pcall 保护状态 — 对应 C Lua 的 CIST_YPCALL
    // yield 穿过 xpcall 后，xpcall 的 C 函数栈帧被销毁，但保护状态保留。
    // 当 inner_func 后续执行 error/return 时，由 execute_loop 检查并处理。
    state
        .pcall_protection_stack
        .push(crate::state::PcallProtection {
            saved_code: state.code.clone(),
            saved_constants: state.constants.clone(),
            saved_upval_descs: state.upval_descs.clone(),
            saved_protos: state.protos.clone(),
            saved_base: state.base,
            saved_pc: state.pc,
            saved_num_params: state.num_params,
            saved_is_vararg: state.is_vararg,
            saved_proto_flag: state.proto_flag,
            saved_nextraargs: state.nextraargs,
            saved_closure_upvals: state.closure_upvals.clone(),
            saved_tbc_list: state.tbc_list,
            func_idx: a,
            // 保存 xpcall 调用者期望的返回值数 (非 state.pcall 的 -1)，
            // 供 finish_pcall_return continuation 调整栈使用。
            nresults,
            pcall_kind: crate::state::PcallKind::Xpcall {
                handler: err_fn.clone(),
            },
            saved_filled: false,
            is_metamethod: false,
            metamethod_res: 0,
            saved_call_stack_len: 0,
            is_close_continuation: false,
            is_pairs_continuation: false,
        });
    let xpcall_protection_idx = state.pcall_protection_stack.len() - 1;

    let status = state.pcall(xpcall_nargs, -1, 0);

    // 非 yield 返回时 pop pcall 保护状态
    if status != crate::state::LUA_YIELD {
        state.pcall_protection_stack.pop();
    } else {
        // yield: 更新 xpcall 的 PcallProtection 为 saved_filled=true
        // state.pcall 的 LightUserData 分支（C 函数路径）不更新 PcallProtection，
        // 所以这里需要手动更新。
        // saved_pc + 1: 跳过调用 xpcall 的 CALL 指令（与 LClosure 分支的 saved_pc + 1 一致）。
        let protection = &mut state.pcall_protection_stack[xpcall_protection_idx];
        if !protection.saved_filled {
            protection.saved_pc += 1;
            protection.saved_filled = true;
        }
    }

    // yield: 从 pending_yield 取出 yield 值并传播 (对应 C 中 yield 穿过 xpcall)
    // 对应 C 的 finishpcall: status == LUA_YIELD 时视为成功，不调用错误处理函数
    if status == crate::state::LUA_YIELD {
        let yield_values = state.pending_yield.take().unwrap_or_default();
        return Err(VmError::Yield(yield_values));
    }

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

        // (栈设置移到下面的 handler 调用循环中)

        // 恢复错误发生时的 call_info 快照 — 对应 C Lua 中 errfunc 在 luaG_errormsg
        // 中被调用，此时 CallInfo 链表仍完整（longjmp 跳过了 callclosemethod 的弹出代码）。
        // 这样 debug.traceback 能看到 __close 帧。
        let saved_call_info = std::mem::take(&mut state.last_error_call_info);
        let original_call_info = if let Some(ref err_ci) = saved_call_info {
            let orig = std::mem::take(&mut state.call_info);
            state.call_info = err_ci.clone();
            Some(orig)
        } else {
            None
        };

        // 调用错误处理函数 (1 个参数, MULTRET)
        // 对应 C Lua 的 luaG_errormsg 递归调用 errfunc:
        // C 版本中 errfunc 通过 luaD_callnoyield 调用，nCcalls 递增 1。
        // errfunc 中的 error() 通过 OP_CALL 调用，nCcalls 不递增（C 的 OP_CALL
        // 不递增 nCcalls）。所以每次递归 nCcalls 递增 1。
        // Rust 版本中 OP_CALL 递增 n_ccalls，与 C 版本不同。为了模拟 C 的递归
        // 行为，用 recursion_count 控制递归次数，设置 n_ccalls = LUAI_MAXCCALLS
        // 避免 state.pcall 触发栈溢出（对应 C 的 201-219 不触发错误）。
        // 当 recursion_count >= LUAI_MAXCCALLS 时，手动设置错误值为 "C stack overflow"，
        // 模拟 C 的 nCcalls = 200 时触发 "C stack overflow"。
        // 当 recursion_count >= LUAI_MAXCCALLS * 11 / 10 时，返回 "error in error handling"，
        // 模拟 C 的 nCcalls >= 220 时触发 "error in error handling"。
        let saved_handler_n_ccalls = state.n_ccalls;
        let mut current_err = err_msg;
        let mut handler_status = crate::state::ERR_RUN;
        let mut recursion_count: u32 = 0;
        let mut handler_nret = 0;

        loop {
            // 检查递归次数，模拟 C 的 nCcalls 检查
            if recursion_count >= crate::state::LUAI_MAXCCALLS * 11 / 10 {
                // 达到上限 — "error in error handling"
                break;
            }
            if recursion_count >= crate::state::LUAI_MAXCCALLS {
                // 递归次数达到 200，触发 "C stack overflow"
                current_err = TValue::Str(state.intern_str("C stack overflow"));
            }

            // 设置栈: [err_fn | current_err]
            state.stack.truncate(a);
            state.stack.push(err_fn.clone());
            state.stack.push(current_err.clone());

            // 设置 n_ccalls = LUAI_MAXCCALLS，避免 state.pcall 触发栈溢出
            // 对应 C 中 errfunc 在 luaG_errormsg 中被调用，nCcalls 在 201-219 之间不触发错误
            state.n_ccalls = crate::state::LUAI_MAXCCALLS;

            handler_status = state.pcall(1, -1, 0);
            handler_nret = state.stack.len().saturating_sub(a);

            if handler_status == 0 {
                // handler 成功返回
                break;
            }

            // handler 失败 — 获取错误值
            current_err = if state.stack.len() > a {
                state.stack[a].clone()
            } else {
                TValue::Nil(NilKind::Strict)
            };

            // 递增 recursion_count，模拟 C 的递归调用 errfunc
            recursion_count = recursion_count.saturating_add(1);
        }

        state.n_ccalls = saved_handler_n_ccalls;

        // 恢复 call_info 到清理后的状态
        if let Some(orig) = original_call_info {
            state.call_info = orig;
        }
        state.last_error_call_info = None;

        results.push(TValue::Boolean(false));
        if handler_status == 0 {
            // 错误处理函数成功: 返回其结果
            for i in 0..handler_nret {
                results.push(state.stack[a + i].clone());
            }
        } else {
            // 错误处理函数本身出错 — 对应 C 的 luaD_errerr:
            // 返回 "error in error handling" 作为最终错误消息。
            results.push(TValue::Str(state.intern_str("error in error handling")));
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
    // 对应 C: for (i = 1; i < n; i++) lua_warning(L, msg_i, 1);
    //         lua_warning(L, msg_n, 0);
    for i in 0..nargs {
        let arg = get_arg(state, a, i);
        match &arg {
            TValue::Str(s) => {
                let tocont = i + 1 < nargs;
                state.warning(s.as_str(), tocont);
            }
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #{} to 'warn' (string expected)",
                    i + 1
                )));
            }
        }
    }
    state.stack.truncate(a);
    Ok(())
}

/// require(modname) — 加载模块
///
/// 对应 C loadlib.cpp 的 ll_require，按顺序尝试 4 个 searcher：
/// 1. package.preload[modname] — 预加载函数
/// 2. package.path — Lua 文件搜索（modname 中 `.` 替换为 `/`）
/// 3. package.cpath — C 模块搜索（modname 中 `.` 替换为 `_` 构造 luaopen_xxx）
/// 4. 全局表 _G[modname] — 内置库兼容（Rust 扩展，非 C 标准行为）
///
/// 返回 (module_value, loader_data)，loader_data 通常是文件路径。
fn call_require(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'require' (string expected, got no value)".to_string(),
        ));
    }
    let modname_val = get_arg(state, a, 0);
    let modname = match &modname_val {
        TValue::Str(s) => s.as_str().to_string(),
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'require' (string expected, got {})",
                modname_val.ty()
            )));
        }
    };

    // 1. 检查 package.loaded 表中是否已缓存
    // 使用 registry 中的 package 表（对应 C 版 require 的 upvalue），不受
    // Lua 代码 `package = {}` 重置全局变量影响。
    if let Some(package_table) = get_package_table(state) {
        let loaded_key = TValue::Str(state.intern_str("loaded"));
        if let Some(TValue::Table(loaded_table)) = package_table.get(&loaded_key) {
            let mod_key = TValue::Str(state.intern_str(&modname));
            if let Some(val) = loaded_table.get(&mod_key) {
                // 对应 C 版 lua_toboolean：仅 truthy（非 nil 非 false）才视为已加载
                // false 被视为未加载，需要重新执行 loader
                if !matches!(val, TValue::Nil(_)) && !matches!(val, TValue::Boolean(false)) {
                    push_results(state, a, nresults, vec![val.clone()]);
                    return Ok(());
                }
            }
        }
    }

    // 检查 package.searchers 是否为 table — 对应 C findloader (loadlib.cpp:624)
    if let Some(package_table) = get_package_table(state) {
        let searchers_key = TValue::Str(state.intern_str("searchers"));
        match package_table.get(&searchers_key) {
            Some(TValue::Table(_)) => {}
            _ => {
                return Err(VmError::RuntimeError(
                    "'package.searchers' must be a table".to_string(),
                ));
            }
        }
    }

    // 收集错误消息
    let mut err_msgs: Vec<String> = Vec::new();

    // 2. 检查 package.preload[modname]
    if let Some(preload_func) = get_preload(state, &modname) {
        if !matches!(preload_func, TValue::Nil(_)) {
            // 调用 preload 函数: preload(modname)
            return run_loader(state, a, nresults, &modname, preload_func, ":preload:");
        }
    }
    err_msgs.push(format!("no field package.preload['{}']", modname));

    // 3. Lua 文件搜索 (package.path) — 对应 C searcher_Lua
    match findfile(state, &modname, "path", "/", ".") {
        Ok((Some(filepath), _)) => {
            return load_lua_module(state, a, nresults, &modname, &filepath);
        }
        Ok((None, errmsg)) => {
            err_msgs.push(errmsg);
        }
        Err(e) => {
            return Err(VmError::RuntimeError(e));
        }
    }

    // 4. C 模块搜索 (package.cpath) — 对应 C searcher_C
    match search_c_module(state, &modname) {
        Ok((loader_func, filepath)) => {
            return run_loader(state, a, nresults, &modname, loader_func, &filepath);
        }
        Err(msg) => {
            err_msgs.push(msg);
        }
    }

    // 5. C root 搜索 — 对应 C searcher_Croot
    // 如果 modname 含 '.'，提取 root 部分搜索 cpath
    if let Some(dot_pos) = modname.find('.') {
        let root = &modname[..dot_pos];
        match findfile(state, root, "cpath", "/", ".") {
            Ok((Some(filepath), _)) => {
                // root 文件找到，尝试加载 modname 对应的 openfunc
                match load_c_root(state, &filepath, &modname) {
                    Ok(loader_func) => {
                        return run_loader(state, a, nresults, &modname, loader_func, &filepath);
                    }
                    Err(msg) => {
                        err_msgs.push(msg);
                    }
                }
            }
            Ok((None, errmsg)) => {
                err_msgs.push(errmsg);
            }
            Err(e) => {
                return Err(VmError::RuntimeError(e));
            }
        }
    }

    // 6. 内置库兼容：检查全局表 _G[modname]
    let global_key = TValue::Str(state.intern_str(&modname));
    if let Some(val) = state.globals.get(&global_key) {
        if !matches!(val, TValue::Nil(_)) {
            cache_module_loaded(state, &modname, val.clone());
            push_results(state, a, nresults, vec![val]);
            return Ok(());
        }
    }

    Err(VmError::RuntimeError(format!(
        "module '{}' not found:\n\t{}",
        modname,
        err_msgs.join("\n\t")
    )))
}

/// 获取 package.preload[modname]
fn get_preload(state: &LuaState, modname: &str) -> Option<TValue> {
    if let Some(package_table) = get_package_table(state) {
        let preload_key = TValue::Str(state.intern_str("preload"));
        if let Some(TValue::Table(preload_table)) = package_table.get(&preload_key) {
            let mod_key = TValue::Str(state.intern_str(modname));
            return Some(
                preload_table
                    .get(&mod_key)
                    .unwrap_or(TValue::Nil(NilKind::Strict)),
            );
        }
    }
    None
}

/// 调用 loader 函数（preload 或 Lua 文件返回的函数）
fn run_loader(
    state: &mut LuaState,
    a: usize,
    nresults: i32,
    modname: &str,
    loader: TValue,
    loader_data: &str,
) -> Result<(), VmError> {
    let saved_len = state.stack.len();
    state.stack.push(loader);
    state.stack.push(TValue::Str(state.intern_str(modname)));
    state.stack.push(TValue::Str(state.intern_str(loader_data)));
    let status = state.pcall(2, 1, 0);
    if status != 0 {
        let err = state.to_string(-1).unwrap_or_default();
        state.settop(saved_len);
        return Err(VmError::RuntimeError(err));
    }
    let result = state
        .stack
        .get(saved_len)
        .cloned()
        .unwrap_or_else(|| TValue::Nil(NilKind::Strict));
    state.settop(saved_len);
    let result = if matches!(result, TValue::Nil(_)) {
        TValue::Boolean(true)
    } else {
        result
    };
    cache_module_loaded(state, modname, result.clone());
    push_results(
        state,
        a,
        nresults,
        vec![result, TValue::Str(state.intern_str(loader_data))],
    );
    Ok(())
}

/// 加载并执行 .lua 模块文件
fn load_lua_module(
    state: &mut LuaState,
    a: usize,
    nresults: i32,
    modname: &str,
    filepath: &str,
) -> Result<(), VmError> {
    let saved_len = state.stack.len();
    let load_status = state.load_file(Some(filepath));
    if load_status != 0 {
        let err = state.to_string(-1).unwrap_or_default();
        state.settop(saved_len);
        return Err(VmError::RuntimeError(format!(
            "error loading module '{}' from '{}': {}",
            modname, filepath, err
        )));
    }
    // 调用加载的函数：(modname, filepath)
    state.stack.push(TValue::Str(state.intern_str(modname)));
    state.stack.push(TValue::Str(state.intern_str(filepath)));
    let call_status = state.pcall(2, 1, 0);
    if call_status != 0 {
        let err = state.to_string(-1).unwrap_or_default();
        state.settop(saved_len);
        return Err(VmError::RuntimeError(format!(
            "error loading module '{}' from '{}': {}",
            modname, filepath, err
        )));
    }
    let result = state
        .stack
        .get(saved_len)
        .cloned()
        .unwrap_or_else(|| TValue::Nil(NilKind::Strict));
    state.settop(saved_len);
    // 对应 C ll_require: 如果 loader 返回非 nil，设 package.loaded[modname] = result；
    // 如果返回 nil，检查模块代码是否已设置 package.loaded[modname]；若仍为 nil，设为 true。
    let result = if matches!(result, TValue::Nil(_)) {
        // loader 返回 nil — 检查模块是否已设置 package.loaded[modname]
        if let Some(package_table) = get_package_table(state) {
            let loaded_key = TValue::Str(state.intern_str("loaded"));
            if let Some(TValue::Table(loaded_table)) = package_table.get(&loaded_key) {
                let mod_key = TValue::Str(state.intern_str(modname));
                if let Some(val) = loaded_table.get(&mod_key) {
                    if !matches!(val, TValue::Nil(_)) {
                        val
                    } else {
                        TValue::Boolean(true)
                    }
                } else {
                    TValue::Boolean(true)
                }
            } else {
                TValue::Boolean(true)
            }
        } else {
            TValue::Boolean(true)
        }
    } else {
        // loader 返回非 nil — 缓存到 package.loaded
        cache_module_loaded(state, modname, result.clone());
        result
    };
    // 确保最终结果也缓存
    cache_module_loaded(state, modname, result.clone());
    push_results(
        state,
        a,
        nresults,
        vec![result, TValue::Str(state.intern_str(filepath))],
    );
    Ok(())
}

/// 通用路径搜索 — 对应 C loadlib.cpp 的 searchpath
///
/// 在 package[fieldname] 中搜索 name，name 中的 sep 替换为 dirsep，
/// 模板中 ? 替换为 name。返回第一个存在的文件路径。
/// 通用路径搜索 — 对应 C loadlib.cpp 的 findfile + searchpath
///
/// 在 package[fieldname] 中搜索 name，name 中的 sep 替换为 dirsep，
/// 模板中 ? 替换为 name。返回 (找到的路径, 错误消息)。
/// 找到时错误消息为空；未找到时路径为 None，错误消息列出所有尝试的文件。
/// path 不是字符串时返回 Err（对应 C 的 luaL_error）。
fn findfile(
    state: &LuaState,
    name: &str,
    fieldname: &str,
    dirsep: &str,
    sep: &str,
) -> Result<(Option<String>, String), String> {
    let path = match get_package_table(state) {
        Some(t) => {
            let path_key = TValue::Str(state.intern_str(fieldname));
            match t.get(&path_key) {
                Some(TValue::Str(s)) => s.as_str().to_string(),
                _ => return Err(format!("'package.{}' must be a string", fieldname)),
            }
        }
        None => return Err(format!("'package.{}' must be a string", fieldname)),
    };
    let name_replaced = if !sep.is_empty() && name.contains(sep) {
        name.replace(sep, dirsep)
    } else {
        name.to_string()
    };
    let mut err_paths: Vec<String> = Vec::new();
    for template in path.split(';') {
        let filepath = template.replace('?', &name_replaced);
        if std::path::Path::new(&filepath).is_file() {
            return Ok((Some(filepath), String::new()));
        }
        err_paths.push(filepath);
    }
    let errmsg = format!("no file '{}'", err_paths.join("'\n\tno file '"));
    Ok((None, errmsg))
}

/// 旧版兼容：仅返回找到的路径，不返回错误消息
fn search_path(
    state: &LuaState,
    name: &str,
    fieldname: &str,
    dirsep: &str,
    sep: &str,
) -> Result<Option<String>, String> {
    findfile(state, name, fieldname, dirsep, sep).map(|(opt, _)| opt)
}

/// loadfunc — 对应 C loadlib.cpp 的 loadfunc
///
/// 在 filename 中查找 luaopen_<modname> 函数。
/// modname 中 `.` → `_`，处理 `-` ignore mark（先试前缀，再试后缀）。
fn loadfunc(state: &mut LuaState, filename: &str, modname: &str) -> Result<TValue, LoadlibError> {
    let modname_normalized = modname.replace('.', "_");
    let openfunc = if let Some(dash_pos) = modname_normalized.find('-') {
        let prefix = &modname_normalized[..dash_pos];
        let func_name = format!("luaopen_{}", prefix);
        match lookforfunc(state, filename, &func_name) {
            Ok(val) => return Ok(val),
            Err(LoadlibError::FuncNotFound) => {
                let suffix = &modname_normalized[dash_pos + 1..];
                format!("luaopen_{}", suffix)
            }
            Err(e) => return Err(e),
        }
    } else {
        format!("luaopen_{}", modname_normalized)
    };
    lookforfunc(state, filename, &openfunc)
}

/// 搜索并加载 C 模块 — 对应 C loadlib.cpp 的 searcher_C
///
/// 在 package.cpath 中搜索 .so 文件，调用 luaopen_xxx 函数。
/// 返回 Ok((loader_func, filepath)) 或 Err(error_message)。
fn search_c_module(state: &mut LuaState, modname: &str) -> Result<(TValue, String), String> {
    let (filename_opt, errmsg) = findfile(state, modname, "cpath", "/", ".")?;
    let filename = match filename_opt {
        Some(f) => f,
        None => return Err(errmsg),
    };
    let loader = loadfunc(state, &filename, modname).map_err(|e| e.to_error_msg(&filename))?;
    Ok((loader, filename))
}

/// C root 加载 — 对应 C loadlib.cpp 的 searcher_Croot
///
/// 在已找到的 root 文件中查找 modname 对应的 openfunc。
fn load_c_root(state: &mut LuaState, filepath: &str, modname: &str) -> Result<TValue, String> {
    match loadfunc(state, filepath, modname) {
        Ok(val) => Ok(val),
        Err(LoadlibError::FuncNotFound) => {
            Err(format!("no module '{}' in file '{}'", modname, filepath))
        }
        Err(e) => Err(e.to_error_msg(filepath)),
    }
}

/// loadlib 错误类型
enum LoadlibError {
    LibNotFound(String), // dlopen 失败，错误消息
    FuncNotFound,        // dlsym 失败
}

impl LoadlibError {
    fn to_error_msg(&self, filename: &str) -> String {
        match self {
            LoadlibError::LibNotFound(msg) => {
                format!("error loading module at file '{}': {}", filename, msg)
            }
            LoadlibError::FuncNotFound => format!("no symbol 'luaopen_' in file '{}'", filename),
        }
    }
}

// ============================================================================
// 动态库加载辅助函数 — 对应 C loadlib.cpp 的 lsys_load / lsys_sym / lsys_unload
// 直接调用 libc dlopen/dlsym/dlclose，不依赖 capi 模块（避免 ffi feature 冲突）
// ============================================================================

/// dlopen 加载动态库，返回库句柄。seeglb=true 时用 RTLD_GLOBAL。
unsafe fn sys_load(path: &str, seeglb: bool) -> *mut std::ffi::c_void {
    let cpath = match std::ffi::CString::new(path) {
        Ok(c) => c,
        Err(_) => return std::ptr::null_mut(),
    };
    let flags = if seeglb {
        libc::RTLD_NOW | libc::RTLD_GLOBAL
    } else {
        libc::RTLD_NOW | libc::RTLD_LOCAL
    };
    unsafe { libc::dlopen(cpath.as_ptr(), flags) }
}

/// dlsym 查找符号，返回函数指针
unsafe fn sys_sym(
    lib: *mut std::ffi::c_void,
    sym: &str,
) -> Option<unsafe extern "C" fn(*mut std::ffi::c_void) -> i32> {
    let csym = std::ffi::CString::new(sym).ok()?;
    let ptr = unsafe { libc::dlsym(lib, csym.as_ptr()) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe {
            std::mem::transmute::<
                *mut std::ffi::c_void,
                unsafe extern "C" fn(*mut std::ffi::c_void) -> i32,
            >(ptr)
        })
    }
}

/// dlerror 获取错误消息
unsafe fn sys_dlerror() -> String {
    let ptr = unsafe { libc::dlerror() };
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

/// lookforfunc — 对应 C loadlib.cpp 的 lookforfunc
///
/// 1. 检查 registry.CLIBS[path]，已加载则复用
/// 2. 未加载则 dlopen
/// 3. sym == "*" 返回 true（仅加载库）
/// 4. 否则 dlsym 找函数，返回 C 函数
fn lookforfunc(state: &mut LuaState, path: &str, sym: &str) -> Result<TValue, LoadlibError> {
    // 检查 CLIBS 缓存
    let clibs_key = TValue::Str(state.intern_str("CLIBS"));
    let clibs_table = match state.registry.get(&clibs_key) {
        Some(TValue::Table(t)) => Some(t),
        _ => None,
    };
    let path_key = TValue::Str(state.intern_str(path));
    let mut lib_handle: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Some(ref clibs) = clibs_table {
        if let Some(TValue::LightUserData(p)) = clibs.get(&path_key) {
            lib_handle = p;
        }
    }

    // 未加载则 dlopen
    if lib_handle.is_null() {
        let seeglb = sym == "*";
        lib_handle = unsafe { sys_load(path, seeglb) };
        if lib_handle.is_null() {
            let msg = unsafe { sys_dlerror() };
            return Err(LoadlibError::LibNotFound(msg));
        }
        // 缓存到 registry.CLIBS[path]
        let clibs = match state.registry.get(&clibs_key) {
            Some(TValue::Table(t)) => t,
            _ => {
                let new_clibs = crate::table::Table::new();
                state
                    .registry
                    .set(clibs_key.clone(), TValue::Table(new_clibs.clone()));
                new_clibs
            }
        };
        clibs.set(path_key, TValue::LightUserData(lib_handle));
    }

    // sym == "*" 仅加载库
    if sym == "*" {
        return Ok(TValue::Boolean(true));
    }

    // dlsym 查找函数
    let func = unsafe { sys_sym(lib_handle, sym) };
    match func {
        Some(f) => {
            // 创建轻量 C 函数并返回
            use crate::objects::LCFunction;
            Ok(TValue::LCFn(LCFunction { func: f }))
        }
        None => Err(LoadlibError::FuncNotFound),
    }
}

/// package.loadlib(path, init) — 对应 C loadlib.cpp 的 ll_loadlib
///
/// init == "*": 仅加载库，返回 true
/// 否则: dlopen + dlsym，返回 C 函数
/// 错误: 返回 (nil, errmsg, when) — when 是 "open"（dlopen 失败）或 "init"（dlsym 失败）
fn call_loadlib(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 2 {
        return Err(VmError::RuntimeError(
            "bad argument to 'loadlib' (needs 2 arguments)".to_string(),
        ));
    }
    let path_val = get_arg(state, a, 0);
    let init_val = get_arg(state, a, 1);
    let path = match &path_val {
        TValue::Str(s) => s.as_str().to_string(),
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #1 to 'loadlib' (string expected)".to_string(),
            ))
        }
    };
    let init = match &init_val {
        TValue::Str(s) => s.as_str().to_string(),
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #2 to 'loadlib' (string expected)".to_string(),
            ))
        }
    };

    match lookforfunc(state, &path, &init) {
        Ok(val) => {
            push_results(state, a, nresults, vec![val]);
            Ok(())
        }
        Err(LoadlibError::LibNotFound(msg)) => {
            // 返回 (nil, errmsg, "open")
            push_results(
                state,
                a,
                nresults,
                vec![
                    TValue::Nil(NilKind::Strict),
                    TValue::Str(state.intern_str(&msg)),
                    TValue::Str(state.intern_str("open")),
                ],
            );
            Ok(())
        }
        Err(LoadlibError::FuncNotFound) => {
            let msg = unsafe { sys_dlerror() };
            push_results(
                state,
                a,
                nresults,
                vec![
                    TValue::Nil(NilKind::Strict),
                    TValue::Str(state.intern_str(&msg)),
                    TValue::Str(state.intern_str("init")),
                ],
            );
            Ok(())
        }
    }
}

/// package.searchpath(name, path [, sep [, dirsep]]) — 对应 C ll_searchpath
///
/// 在 path 中搜索 name，返回找到的文件路径。
/// 失败返回 (nil, errmsg)。
fn call_searchpath(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 2 {
        return Err(VmError::RuntimeError(
            "bad argument to 'searchpath' (needs at least 2 arguments)".to_string(),
        ));
    }
    let name_val = get_arg(state, a, 0);
    let path_val = get_arg(state, a, 1);
    let name = match &name_val {
        TValue::Str(s) => s.as_str().to_string(),
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #1 to 'searchpath' (string expected)".to_string(),
            ))
        }
    };
    let path = match &path_val {
        TValue::Str(s) => s.as_str().to_string(),
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #2 to 'searchpath' (string expected)".to_string(),
            ))
        }
    };
    let sep = match nargs >= 3 {
        true => match &get_arg(state, a, 2) {
            TValue::Str(s) => s.as_str().to_string(),
            _ => ".".to_string(),
        },
        false => ".".to_string(),
    };
    let dirsep = match nargs >= 4 {
        true => match &get_arg(state, a, 3) {
            TValue::Str(s) => s.as_str().to_string(),
            _ => "/".to_string(),
        },
        false => "/".to_string(),
    };

    // name 中 sep 替换为 dirsep
    let name_replaced = if !sep.is_empty() && name.contains(&sep) {
        name.replace(&sep, &dirsep)
    } else {
        name.clone()
    };
    let mut err_paths: Vec<String> = Vec::new();
    for template in path.split(';') {
        let filepath = template.replace('?', &name_replaced);
        if std::path::Path::new(&filepath).is_file() {
            push_results(
                state,
                a,
                nresults,
                vec![TValue::Str(state.intern_str(&filepath))],
            );
            return Ok(());
        }
        err_paths.push(filepath);
    }
    let errmsg = format!("no file '{}'", err_paths.join("'\n\tno file '"));
    push_results(
        state,
        a,
        nresults,
        vec![
            TValue::Nil(NilKind::Strict),
            TValue::Str(state.intern_str(&errmsg)),
        ],
    );
    Ok(())
}

/// package.searchers 表中的占位函数 — 对应 C 的 searcher_preload/Lua/C/Croot
///
/// 当前 call_require 硬编码搜索逻辑,不遍历 package.searchers 表;
/// 这些 BuiltinFn 仅用于让 searchers 表元素显示为 "function" 类型。
/// 直接调用会报错（与原 tag 行为一致）。
fn call_searcher_placeholder(
    _state: &mut LuaState,
    _a: usize,
    _nargs: usize,
    _nresults: i32,
) -> Result<(), VmError> {
    Err(VmError::RuntimeError(
        "package.searchers functions are not directly callable".to_string(),
    ))
}

/// 读取 package.path 并搜索模块文件 (旧版兼容,保留供 loadfile 等使用)
///
/// package.path 是用 `;` 分隔的模板列表,`?` 替换为 modname。
/// 返回第一个存在的文件路径。
fn search_module_file(state: &LuaState, modname: &str) -> Option<String> {
    search_path(state, modname, "path", "/", ".").ok().flatten()
}

/// 加载并执行模块文件,缓存结果到 package.loaded (对应 C 的 requiref 语义)
fn load_and_run_module(
    state: &mut LuaState,
    a: usize,
    nresults: i32,
    modname: &str,
    filepath: &str,
) -> Result<(), VmError> {
    let saved_len = state.stack.len();
    let load_status = state.load_file(Some(filepath));
    if load_status != 0 {
        let err = state.to_string(-1).unwrap_or_default();
        state.settop(saved_len);
        return Err(VmError::RuntimeError(format!(
            "error loading module '{}' from '{}': {}",
            modname, filepath, err
        )));
    }
    let call_status = state.pcall(0, 1, 0);
    if call_status != 0 {
        let err = state.to_string(-1).unwrap_or_default();
        state.settop(saved_len);
        return Err(VmError::RuntimeError(format!(
            "error loading module '{}' from '{}': {}",
            modname, filepath, err
        )));
    }
    let result = state
        .stack
        .get(saved_len)
        .cloned()
        .unwrap_or_else(|| TValue::Nil(NilKind::Strict));
    state.settop(saved_len);
    cache_module_loaded(state, modname, result.clone());
    push_results(state, a, nresults, vec![result]);
    Ok(())
}

/// 缓存模块到 package.loaded[modname]
fn cache_module_loaded(state: &mut LuaState, modname: &str, val: TValue) {
    let loaded_key = TValue::Str(state.intern_str("loaded"));
    let mod_key = TValue::Str(state.intern_str(modname));
    if let Some(package_table) = get_package_table(state) {
        if let Some(TValue::Table(loaded_table)) = package_table.get(&loaded_key) {
            // Table 共享数据 (Rc<RefCell>),直接 set 即可同步到 package.loaded
            loaded_table.set(mod_key, val);
        }
    }
}

/// 读取路径环境变量 — 对应 C loadlib.cpp 的 setpath
///
/// 顺序:版本化变量 (envname + "_5_5") → 未版本化变量 → 默认值。
/// 若 registry 中 LUA_NOENV 为真 (命令行 -E),忽略环境变量直接用默认值。
/// 路径中的 ";;" 会被替换为默认路径。
fn setpath(state: &LuaState, envname: &str, dft: &str) -> String {
    let noenv_key = TValue::Str(state.intern_str("LUA_NOENV"));
    let noenv = matches!(state.registry.get(&noenv_key), Some(TValue::Boolean(true)));

    let nver = format!("{}_5_5", envname);
    let path = if noenv {
        None
    } else {
        std::env::var(&nver)
            .ok()
            .or_else(|| std::env::var(envname).ok())
    };

    let path = match path {
        Some(p) => p,
        None => return dft.to_string(),
    };

    if let Some(pos) = path.find(";;") {
        let mut result = String::new();
        if pos > 0 {
            result.push_str(&path[..pos]);
            result.push(';');
        }
        result.push_str(dft);
        if pos + 2 < path.len() {
            result.push(';');
            result.push_str(&path[pos + 2..]);
        }
        result
    } else {
        path
    }
}

/// 初始化 package 表:设置 path/cpath/loaded/preload/loadlib/searchpath/searchers/config
/// 对应 C loadlib.cpp 的 luaopen_package
fn init_package_table(state: &mut LuaState) {
    let package_key = TValue::Str(state.intern_str("package"));
    let pkg = crate::table::Table::new();
    let loaded = crate::table::Table::new();
    // registry._LOADED 和 package.loaded 共享同一个 Rc 引用（对应 C luaopen_package）
    let loaded_shared = loaded.clone();
    pkg.set(
        TValue::Str(state.intern_str("loaded")),
        TValue::Table(loaded),
    );
    // preload 表 — 对应 registry[LUA_PRELOAD_TABLE]
    let preload = crate::table::Table::new();
    pkg.set(
        TValue::Str(state.intern_str("preload")),
        TValue::Table(preload),
    );
    let path = setpath(state, "LUA_PATH", "./?.lua;./?/init.lua");
    pkg.set(
        TValue::Str(state.intern_str("path")),
        TValue::Str(state.intern_str(&path)),
    );
    let cpath = setpath(state, "LUA_CPATH", crate::config::CPATH_DEFAULT);
    pkg.set(
        TValue::Str(state.intern_str("cpath")),
        TValue::Str(state.intern_str(&cpath)),
    );
    // config 字段 — 对应 C 的 package.config: DIRSEP \n PATH_SEP \n PATH_MARK \n EXEC_DIR \n IGMARK \n
    let config = "/\n;\n?\n!\n-\n";
    pkg.set(
        TValue::Str(state.intern_str("config")),
        TValue::Str(state.intern_str(config)),
    );
    // loadlib 函数 — BuiltinFn 注册
    pkg.set(
        TValue::Str(state.intern_str("loadlib")),
        TValue::BuiltinFn(crate::objects::BuiltinFn {
            func: call_loadlib,
            name: c"loadlib".as_ptr() as *const u8,
        }),
    );
    // searchpath 函数 — BuiltinFn 注册
    pkg.set(
        TValue::Str(state.intern_str("searchpath")),
        TValue::BuiltinFn(crate::objects::BuiltinFn {
            func: call_searchpath,
            name: c"searchpath".as_ptr() as *const u8,
        }),
    );
    // searchers 表 — 对应 C createsearcherstable (loadlib.cpp:703)
    // 包含 4 个 searcher 占位函数 (preload/Lua/C/Croot)
    // 当前 call_require 硬编码搜索逻辑,不遍历 package.searchers 表;
    // 这些 BuiltinFn 仅用于让 searchers 表元素显示为 "function" 类型，
    // 直接调用会报错（与原 tag 行为一致）。
    let make_searcher = |name: &'static std::ffi::CStr| -> TValue {
        TValue::BuiltinFn(crate::objects::BuiltinFn {
            func: call_searcher_placeholder,
            name: name.as_ptr() as *const u8,
        })
    };
    let searchers = crate::table::Table::new();
    searchers.set(TValue::Integer(1), make_searcher(c"searcher_preload"));
    searchers.set(TValue::Integer(2), make_searcher(c"searcher_Lua"));
    searchers.set(TValue::Integer(3), make_searcher(c"searcher_C"));
    searchers.set(TValue::Integer(4), make_searcher(c"searcher_Croot"));
    pkg.set(
        TValue::Str(state.intern_str("searchers")),
        TValue::Table(searchers),
    );
    state
        .globals
        .set(package_key.clone(), TValue::Table(pkg.clone()));
    // 同时在 registry 中保存 package 表引用 — 对应 C 版 ll_require 通过
    // upvalue 访问 package 表。这样 Lua 代码 `package = {}` 重置全局变量后，
    // require 仍能访问注册时的 package 表（含 loaded/preload/searchers）。
    let registry_pkg_key = TValue::Str(state.intern_str("_PACKAGE"));
    state.registry.set(registry_pkg_key, TValue::Table(pkg));
    // registry._LOADED = package.loaded (同一个 Rc 引用，对应 C luaopen_package)
    // C 代码 (如 lauxlib.cpp pushglobalfuncname) 通过 lua_getfield(LUA_REGISTRYINDEX, "_LOADED")
    // 访问 loaded 表，必须在 registry 中建立 _LOADED -> loaded 的映射。
    let loaded_key = TValue::Str(state.intern_str("_LOADED"));
    state.registry.set(loaded_key, TValue::Table(loaded_shared));
}

/// 从 registry 读取注册时的 package 表 — 对应 C 版 require 函数的 upvalue(1)
///
/// Lua 代码可能重置全局 `package = {}`，但 require 内部必须使用注册时的
/// package 表（含 loaded/preload/searchers/path/cpath），否则会破坏模块加载。
fn get_package_table(state: &LuaState) -> Option<crate::table::Table> {
    let registry_pkg_key = TValue::Str(state.intern_str("_PACKAGE"));
    match state.registry.get(&registry_pkg_key) {
        Some(TValue::Table(t)) => Some(t),
        _ => None,
    }
}

/// load(chunk [, chunkname [, mode [, env]]]) — 对应 C 的 luaB_load + lua_load
///
/// 加载并编译 Lua 代码块, 返回编译后的函数。
/// 支持两种 chunk 形式:
/// 1. 字符串 chunk — 直接编译字符串内容
/// 2. reader 函数 — 反复调用 reader() 直到返回 nil, 累积返回的字符串
///
/// mode 参数控制允许的格式:
/// - "t": 仅文本
/// - "b": 仅二进制
/// - "bt" 或缺省: 两者皆可
///
/// env 参数 (第 4 个) 作为加载函数的 _ENV 上值; 缺省时使用当前全局表。
///
/// 错误处理: 编译失败或 reader 抛错时返回 (nil, error_msg), 不向上抛错
/// (对应 C 的 load_aux 中 status != LUA_OK 的分支)。
fn call_load(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'load' (string expected, got no value)".to_string(),
        ));
    }

    let chunk_val = get_arg(state, a, 0);
    let is_string_chunk = matches!(chunk_val, TValue::Str(_));
    // Lazy default_chunkname — only computed when needed (nargs < 2 or name is nil).
    // For constructs.lua (206K load() calls with explicit "" name), this avoids
    // copying the 106-char source string into a chunkname that is never used.
    let chunkname = if nargs >= 2 {
        let name_val = get_arg(state, a, 1);
        match &name_val {
            TValue::Str(s) => s.as_str().to_string(),
            TValue::Nil(_) => {
                if is_string_chunk {
                    match &chunk_val {
                        TValue::Str(s) => s.as_str().to_string(),
                        _ => unreachable!(),
                    }
                } else {
                    "=(load)".to_string()
                }
            }
            _ => {
                if is_string_chunk {
                    match &chunk_val {
                        TValue::Str(s) => s.as_str().to_string(),
                        _ => unreachable!(),
                    }
                } else {
                    "=(load)".to_string()
                }
            }
        }
    } else {
        if is_string_chunk {
            match &chunk_val {
                TValue::Str(s) => s.as_str().to_string(),
                _ => unreachable!(),
            }
        } else {
            "=(load)".to_string()
        }
    };

    // 获取 mode 参数 (第 3 个) — "t" / "b" / "bt" / nil
    // 对应 C 的 getMode: 默认 "bt"，'B' (固定缓冲区) 对 Lua 代码非法
    let mode: Option<String> = if nargs >= 3 {
        let mode_val = get_arg(state, a, 2);
        match &mode_val {
            TValue::Str(s) => {
                let m = s.as_str().to_string();
                // C 的 getMode: if (strchr(mode, 'B') != NULL) luaL_argerror(...)
                if m.contains('B') {
                    return Err(VmError::RuntimeError(
                        "bad argument #3 to 'load' (invalid mode)".to_string(),
                    ));
                }
                Some(m)
            }
            TValue::Nil(_) => None,
            _ => None,
        }
    } else {
        None
    };

    // 获取 env 参数 (第 4 个) — 用作加载函数的第 1 个上值
    //
    // 对应 C 的 luaB_load 中 `int env = (!lua_isnone(L, 4) ? 4 : 0);`
    // 以及 load_aux 中 `if (envidx) lua_setupvalue(L, -2, 1);`:
    //   - nargs < 4 (env 缺省): envidx = 0, 不调用 lua_setupvalue;
    //     但 lua_load 内部仍会把第 1 个上值设为全局表 (_ENV 行为)。
    //     → 用全局表
    //   - nargs >= 4 且 env == nil: envidx = 4, lua_setupvalue 设第 1 个上值为 nil
    //     → 用 nil (覆盖 lua_load 设置的全局表)
    //   - nargs >= 4 且 env 非 nil: 用 env
    let env_val = if nargs >= 4 {
        get_arg(state, a, 3) // 即使是 nil 也直接用 (不替换为全局表)
    } else {
        TValue::Table(state.globals.clone())
    };

    // 根据 chunk 类型获取源代码字符串
    // 对应 C 的 luaB_load 中 s = lua_tolstring(L, 1, &l) 的判断
    // 字符串 chunk: 直接借用 &str 避免克隆 (关键优化: constructs.lua 调用 206,780 次 load())
    // reader 函数: 循环调用 reader() 累积字符串
    let source: &str;
    let _source_owned: String;
    match &chunk_val {
        TValue::Str(s) => {
            source = s.as_str();
            _source_owned = String::new();
        }
        TValue::LClosure(_)
        | TValue::BuiltinFn(_)
        | TValue::LCFn(_)
        | TValue::CClosure(_)
        | TValue::LightUserData(_) => {
            // reader 函数模式: 循环调用 reader() 累积字符串
            // LightUserData 必须是可调用的 function tag (io.lines 迭代器)，
            // 对应 C 的 luaL_checktype(L, 1, LUA_TFUNCTION) — C 中 io.lines
            // 返回 C 闭包 (LUA_TFUNCTION)，Rust 用 LightUserData tag 代替
            if let TValue::LightUserData(tag) = &chunk_val {
                let tag_val = *tag as usize;
                if !crate::stdlib::io_lib::is_lines_iterator_tag(tag_val) {
                    return Err(VmError::RuntimeError(format!(
                        "bad argument #1 to 'load' (function expected, got {})",
                        chunk_val.ty()
                    )));
                }
            }
            // 对应 C 的 generic_reader + luaZ_fill 的循环
            // EOF 条件: reader 返回 nil (对应 C 的 lua_isnil → NULL)
            //         或返回空字符串 "" (对应 C 的 size==0 → EOZ)
            let mut buffer = String::new();
            loop {
                // 推入 reader function 到栈顶
                // 对应 C 的 lua_pushvalue(L, 1); lua_call(L, 0, 1);
                let saved_len = state.stack.len();
                state.stack.push(chunk_val.clone());
                let status = state.pcall(0, 1, 0);
                if status != 0 {
                    let err_msg = if saved_len < state.stack.len() {
                        match &state.stack[saved_len] {
                            TValue::Str(s) => s.as_str().to_string(),
                            _ => "reader function must return a string".to_string(),
                        }
                    } else {
                        "reader function must return a string".to_string()
                    };
                    state.stack.truncate(saved_len);
                    push_results(
                        state,
                        a,
                        nresults,
                        vec![
                            TValue::Nil(NilKind::Strict),
                            TValue::Str(state.intern_str(&err_msg)),
                        ],
                    );
                    return Ok(());
                }
                let result = if saved_len < state.stack.len() {
                    state.stack[saved_len].clone()
                } else {
                    TValue::Nil(NilKind::Strict)
                };
                state.stack.truncate(saved_len);
                match &result {
                    TValue::Nil(_) => break,
                    TValue::Str(s) => {
                        if s.as_str().is_empty() {
                            break;
                        }
                        buffer.push_str(s.as_str());
                    }
                    _ => {
                        push_results(
                            state,
                            a,
                            nresults,
                            vec![
                                TValue::Nil(NilKind::Strict),
                                TValue::Str(
                                    state.intern_str("reader function must return a string"),
                                ),
                            ],
                        );
                        return Ok(());
                    }
                }
            }
            _source_owned = buffer;
            source = &_source_owned;
        }
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'load' (string or function expected, got {})",
                chunk_val.ty()
            )));
        }
    }

    // 检测二进制格式 (仅检查首字节 \x1b, 对应 C 的 f_parser: c == LUA_SIGNATURE[0])
    // 完整签名校验由 parse_dump 的 checkHeader 负责
    let is_binary = source.as_bytes().first().copied() == Some(0x1b);

    // mode 检查 (对应 C 的 getMode + lua_load 的 mode 参数)
    let mode_str = mode.as_deref();
    let allows_text = match mode_str {
        None => true,
        Some(m) => m.contains('t') || (!m.contains('b') && !m.contains('t')),
    };
    let allows_binary = match mode_str {
        None => true,
        Some(m) => m.contains('b'),
    };
    if is_binary && !allows_binary {
        // 二进制但 mode 不允许 (mode = "t")
        push_results(
            state,
            a,
            nresults,
            vec![
                TValue::Nil(NilKind::Strict),
                TValue::Str(state.intern_str("attempt to load a binary chunk (mode is 'text')")),
            ],
        );
        return Ok(());
    }
    if !is_binary && !allows_text {
        // 文本但 mode 不允许 (mode = "b")
        push_results(
            state,
            a,
            nresults,
            vec![
                TValue::Nil(NilKind::Strict),
                TValue::Str(state.intern_str("attempt to load a text chunk (mode is 'binary')")),
            ],
        );
        return Ok(());
    }

    // Defer GC for small source code (<64KB): during compiler-heavy workloads
    // (constructs.lua: 206K+ load() calls), threshold-based GC fires ~100 times
    // for short-lived closures that are freed by Box::drop anyway. Letting metas
    // accumulate until a deferred collection is more efficient.
    // For large source (>=64KB), force full GC before compilation to avoid
    // OOM during the allocation-heavy compilation process.
    if source.len() >= 65536 {
        state.collect_gc();
    }
    let proto_result = if is_binary {
        // 二进制格式: 使用 undump
        crate::compiler::bytecode_dump::undump_to_proto(source.as_bytes())
            .map_err(|e| format!("bad binary chunk: {}", e))
    } else {
        // 文本格式: 编译源代码
        crate::compiler::compile(state, &source, &chunkname)
    };

    match proto_result {
        Ok(mut proto) => {
            // 创建闭包, 设置 _ENV 上值为 env 参数 (缺省为全局表)
            let nup = proto.size_upvalues as usize;
            // 二进制加载的 proto 中字符串常量是 LongString, 需要驻留化为 ShortString
            // 以便与全局表中的 ShortString 键匹配 (Short vs Long 的 Hash/PartialEq 不一致)
            if is_binary {
                intern_proto_strings(&mut proto, state);
            }
            let mut upvals: Vec<UpValRef> = Vec::with_capacity(nup.max(1));
            upvals.push(std::rc::Rc::new(std::cell::RefCell::new(UpVal::Closed {
                value: Box::new(env_val),
            })));
            for _ in 1..nup {
                upvals.push(std::rc::Rc::new(std::cell::RefCell::new(UpVal::Closed {
                    value: Box::new(TValue::Nil(NilKind::Strict)),
                })));
            }
            let closure = Rc::new(LClosure {
                gc_header: GCObjectHeader::new(),
                proto: std::rc::Rc::new(proto),
                upvals: std::rc::Rc::new(std::cell::RefCell::new(upvals)),
            });
            push_results(state, a, nresults, vec![TValue::LClosure(closure)]);
            Ok(())
        }
        Err(err_msg) => {
            // 编译失败: 返回 nil + 错误消息 (对应 C 的 load_aux 失败分支)
            push_results(
                state,
                a,
                nresults,
                vec![
                    TValue::Nil(NilKind::Strict),
                    TValue::Str(state.intern_str(&err_msg)),
                ],
            );
            Ok(())
        }
    }
}

/// dofile([filename]) — 加载并执行文件
///
/// 对应 C 的 luaB_dofile:
/// ```c
/// static int luaB_dofile (lua_State *L) {
///   const char *fname = luaL_optstring(L, 1, NULL);
///   int status = luaL_loadfile(L, fname);
///   if (status != LUA_OK)
///     return luaL_error(L, "%s", lua_tostring(L, -1));
///   lua_call(L, 0, LUA_MULTRET);
///   return lua_gettop(L);
/// }
/// ```
fn call_dofile(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let filename: Option<String> = if nargs > 0 {
        let arg = get_arg(state, a, 0);
        match &arg {
            TValue::Str(s) => Some(s.as_str().to_string()),
            TValue::Nil(_) => None,
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #1 to 'dofile' (string expected, got {})",
                    arg.ty()
                )));
            }
        }
    } else {
        None
    };

    state.stack.truncate(a);
    // 截断后栈顶即为 a；load_file 将 chunk 压在 a，pcall 后结果/错误也在 a。
    // 因此 saved_len 必须在 truncate 之后捕获（== a），否则旧位置无法读到结果。
    let saved_len = state.stack.len();

    let load_status = state.load_file(filename.as_deref());
    if load_status != 0 {
        let err = state.to_string(-1).unwrap_or_default();
        state.settop(saved_len);
        return Err(VmError::RuntimeError(format!("{}", err)));
    }

    let call_status = state.pcall(0, -1, 0);
    // yield 穿过 dofile: 传播 yield (对应 C Lua 中 yield 穿过 C 函数 dofile)
    if call_status == crate::state::LUA_YIELD {
        let yield_values = state.pending_yield.take().unwrap_or_default();
        // yield 时不截断栈，保留 chunk 的执行状态供第二次 resume 恢复
        return Err(VmError::Yield(yield_values));
    }
    if call_status != 0 {
        let err_val = state
            .stack
            .get(saved_len)
            .cloned()
            .unwrap_or_else(|| TValue::Str(state.intern_str("dofile error")));
        state.settop(saved_len);
        match err_val {
            TValue::Str(s) => return Err(VmError::RuntimeError(s.as_str().to_string())),
            other => return Err(VmError::RuntimeErrorValue(other)),
        }
    }

    let n_results = state.stack.len().saturating_sub(saved_len);
    let results: Vec<TValue> = if n_results > 0 {
        state.stack[saved_len..].to_vec()
    } else {
        Vec::new()
    };
    state.settop(saved_len);
    push_results(state, a, nresults, results);
    Ok(())
}

/// loadfile([filename [, mode]]) — 加载文件为 chunk，但不执行
///
/// 对应 C 的 luaB_loadfile:
/// ```c
/// static int luaB_loadfile (lua_State *L) {
///   const char *fname = luaL_optstring(L, 1, NULL);
///   const char *mode = luaL_optstring(L, 2, NULL);
///   int status = luaL_loadfilex(L, fname, mode);
///   if (status == LUA_OK)
///     return 1;  /* File loaded successfully */
///   else {  /* error (error message is on top of the stack) */
///     lua_pushnil(L);
///     lua_insert(L, -2);  /* put nil before error message */
///     return 2;  /* return nil plus error message */
///   }
/// }
/// ```
fn call_loadfile(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 获取可选的 filename 参数 (默认 nil → 从 stdin 读取)
    let filename: Option<String> = if nargs >= 1 {
        let arg = get_arg(state, a, 0);
        match &arg {
            TValue::Str(s) => Some(s.as_str().to_string()),
            TValue::Nil(_) => None,
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #1 to 'loadfile' (string expected, got {})",
                    arg.ty()
                )));
            }
        }
    } else {
        None
    };

    // 获取可选的 mode 参数 (默认 nil)
    // 对应 C 的 getMode: 'B' (固定缓冲区) 对 Lua 代码非法
    let mode: Option<String> = if nargs >= 2 {
        let m = get_arg(state, a, 1);
        match &m {
            TValue::Str(s) => {
                let mode_str = s.as_str().to_string();
                if mode_str.contains('B') {
                    return Err(VmError::RuntimeError(
                        "bad argument #2 to 'loadfile' (invalid mode)".to_string(),
                    ));
                }
                Some(mode_str)
            }
            TValue::Nil(_) => None,
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #2 to 'loadfile' (string expected, got {})",
                    m.ty()
                )));
            }
        }
    } else {
        None
    };

    // 获取可选的 env 参数 (第 3 个参数)
    // 对应 C 的 luaB_loadfile: int env = (!lua_isnone(L, 3) ? 3 : 0)
    // load_aux 中: lua_pushvalue(L, envidx); lua_setupvalue(L, -2, 1) 设置为第 1 个上值 (_ENV)
    let env: Option<TValue> = if nargs >= 3 {
        Some(get_arg(state, a, 2))
    } else {
        None
    };

    // 保存栈位置，调用 load_filex
    state.stack.truncate(a);
    let saved_len = state.stack.len();

    let status = state.load_filex(filename.as_deref(), mode.as_deref());
    if status == 0 {
        // 成功: 栈顶是加载的 chunk 函数
        let chunk = state
            .stack
            .get(saved_len)
            .cloned()
            .unwrap_or_else(|| TValue::Nil(NilKind::Strict));
        state.settop(saved_len);
        // 如果提供了 env 参数, 设置为闭包的第 1 个上值 (_ENV)
        // 对应 C 的 load_aux: lua_setupvalue(L, -2, 1)
        if let Some(env_val) = env {
            if let TValue::LClosure(closure) = &chunk {
                let upvals = closure.upvals.borrow();
                if !upvals.is_empty() {
                    *upvals[0].borrow_mut() = UpVal::Closed {
                        value: Box::new(env_val),
                    };
                }
            }
        }
        push_results(state, a, nresults, vec![chunk]);
    } else {
        // 失败: 栈顶是错误消息
        let err_msg = state
            .stack
            .get(saved_len)
            .cloned()
            .unwrap_or_else(|| TValue::Str(state.intern_str("loadfile error")));
        state.settop(saved_len);
        // 返回 nil + 错误消息
        push_results(
            state,
            a,
            nresults,
            vec![TValue::Nil(NilKind::Strict), err_msg],
        );
    }
    Ok(())
}

// ============================================================================
// 二进制 chunk 字符串驻留化 (修复 ShortString/LongString 不匹配问题)
// ============================================================================

/// 递归地将 proto 及其子 proto 中的字符串常量驻留化
///
/// 二进制 dump/undump 后, 所有字符串都是 LongString。但全局表的键是 ShortString
/// (通过 StringTable::intern 创建)。由于 LuaString 的 PartialEq/Hash 实现中
/// Short vs Long 返回 false, 导致 GETTABUP 无法在全局表中找到键。
/// 此函数将短字符串 (<= 40 字节) 转换为驻留的 ShortString。
pub fn intern_proto_strings(proto: &mut Proto, state: &LuaState) {
    // 驻留化常量池中的字符串
    for c in Rc::make_mut(&mut proto.constants).iter_mut() {
        if let TValue::Str(s) = c {
            let s_str = s.as_str().to_string();
            *c = TValue::Str(state.intern_str(&s_str));
        }
    }
    // 驻留化 upvalue 名称
    for uv in Rc::make_mut(&mut proto.upvalues).iter_mut() {
        if let Some(name) = uv.name.take() {
            let name_str = name.as_str().to_string();
            uv.name = Some(state.intern_str(&name_str));
        }
    }
    // 驻留化局部变量名
    for lv in &mut proto.loc_vars {
        if let Some(name) = lv.varname.take() {
            let name_str = name.as_str().to_string();
            lv.varname = Some(state.intern_str(&name_str));
        }
    }
    // 驻留化 source
    if let Some(src) = proto.source.take() {
        let src_str = src.as_str().to_string();
        proto.source = Some(state.intern_str(&src_str));
    }
    // 递归处理子 proto — Rc::make_mut 在 protos 独占时（refcount=1）直接返回 &mut Vec，
    // 否则 clone-on-write；此处 intern_proto_strings 在加载后调用，protos 通常独占
    for p in std::rc::Rc::make_mut(&mut proto.protos).iter_mut() {
        intern_proto_strings(std::rc::Rc::make_mut(p), state);
    }
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
pub fn table_next(
    table: &crate::table::Table,
    key: &TValue,
) -> Result<(Option<TValue>, TValue), &'static str> {
    // 如果 key 是 nil, 从数组部分开始
    if matches!(key, TValue::Nil(_)) {
        return Ok(find_first(table));
    }

    // 规范化 Float key 到 Integer（对应 C 的 lua_numbertointeger）
    let key = if let TValue::Float(f) = key {
        if let Some(i) = crate::table::float_key_to_int(*f) {
            TValue::Integer(i)
        } else {
            key.clone()
        }
    } else {
        key.clone()
    };

    // 对应 C 的 findindex: 检查 key 是否在表中（array 或 hash）
    // 若 key 不存在则抛 "invalid key to 'next'" 错误
    //
    // 注意: hash 中的 tombstone (Nil(Empty)) 也算"在表中" — 对应 C 的 dead key 语义,
    // 让 `for k,v in pairs(t) do t[k] = nil end` 能继续遍历。
    // `table.get()` 对 tombstone 返回 None, 因此不能用 get().is_none() 判断。
    let key_exists = {
        let data = table.data.borrow();
        let mut exists = false;
        if let TValue::Integer(i) = &key {
            if *i > 0 {
                let idx = (*i - 1) as usize;
                if idx < data.array.len() && !matches!(data.array[idx], TValue::Nil(NilKind::Empty))
                {
                    exists = true;
                }
            }
        }
        if !exists {
            exists = data
                .key_to_bucket
                .as_ref()
                .map_or(false, |m| m.contains_key(&key));
        }
        exists
    };
    if !key_exists {
        return Err("invalid key to 'next'");
    }

    // key 存在，查找下一个
    // 如果 key 是整数且在数组范围内
    if let TValue::Integer(k) = &key {
        if *k > 0 {
            let idx = (*k - 1) as usize;
            let data = table.data.borrow();
            if idx < data.array.len() {
                // 尝试下一个数组元素
                let next_idx = idx + 1;
                if next_idx < data.array.len() {
                    let next_val = &data.array[next_idx];
                    if !matches!(next_val, TValue::Nil(NilKind::Empty)) {
                        return Ok((Some(TValue::Integer(next_idx as i64 + 1)), next_val.clone()));
                    }
                }
                // 数组部分结束, 转到哈希部分
                drop(data); // 释放 borrow, 避免 find_first_hash 中重复 borrow
                return Ok(find_first_hash(table));
            }
        }
    }

    // key 在哈希部分, 找下一个哈希键
    Ok(find_next_hash(table, &key))
}

/// 查找第一个非空元素 (数组部分)
fn find_first(table: &crate::table::Table) -> (Option<TValue>, TValue) {
    // 先查找数组部分
    let data = table.data.borrow();
    for (i, v) in data.array.iter().enumerate() {
        if !matches!(v, TValue::Nil(NilKind::Empty)) {
            return (Some(TValue::Integer(i as i64 + 1)), v.clone());
        }
    }
    drop(data); // 释放 borrow
                // 数组部分为空, 查找哈希部分
    find_first_hash(table)
}

/// 查找哈希部分的第一个元素 (跳过 tombstone)
///
/// 用 `hash_buckets` 顺序遍历 + `hash.get(k)` 检查 live — 对应 C `luaH_next` 的
/// hash 部分扫描，但 Rust 用插入顺序而非 hash bucket 顺序。
fn find_first_hash(table: &crate::table::Table) -> (Option<TValue>, TValue) {
    let data = table.data.borrow();
    for (k, v) in &data.hash_buckets {
        if !matches!(v, TValue::Nil(NilKind::Empty)) {
            return (Some(k.clone()), v.clone());
        }
    }
    (None, TValue::Nil(NilKind::Strict))
}

/// 在哈希部分中查找给定 key 之后的下一个 key (跳过 tombstone)
///
/// 用 `key_to_bucket.get(key)` O(1) 定位 prev 的位置，然后线性扫描
/// `hash_buckets[idx+1..]` 找下一个 live entry — 对应 C 的 findindex O(1)
/// (C 用 hash→mainposition→chain 定位 node index)。
fn find_next_hash(table: &crate::table::Table, key: &TValue) -> (Option<TValue>, TValue) {
    let data = table.data.borrow();
    let start_idx = match data.key_to_bucket.as_ref().and_then(|m| m.get(key)) {
        Some(&i) => i + 1,
        None => return (None, TValue::Nil(NilKind::Strict)),
    };
    for (k, v) in data.hash_buckets[start_idx..].iter() {
        if !matches!(v, TValue::Nil(NilKind::Empty)) {
            return (Some(k.clone()), v.clone());
        }
    }
    (None, TValue::Nil(NilKind::Strict))
}
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
    // 对应 C 的 luaL_intop(+, i, 1): unsigned 算术 wrap-around
    // 在 i == math.maxinteger 时，next_i 会环绕到 math.mininteger
    let next_i = (i as u64).wrapping_add(1) as i64;

    // 对应 C 的 lua_geti → luaV_finishget: 支持 __index 元方法 (包括非 table 类型的元表)
    // ipairs 可用于非 table 值 (如带 __index 元方法的 userdata), 所以不限定 Table 类型
    let val = crate::execute::VmExecutor::table_get(
        state,
        &t,
        &TValue::Integer(next_i),
        crate::execute::VarSource::None,
    )?;
    if matches!(val, TValue::Nil(_)) {
        push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
    } else {
        push_results(state, a, nresults, vec![TValue::Integer(next_i), val]);
    }
    Ok(())
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

/// collectgarbage([opt [, arg]]) — 对应 C 的 luaB_collectgarbage
///
/// 简化实现: 由于当前 GC 是占位实现, 大部分选项返回合理默认值。
/// 支持的选项:
/// - "collect" (默认): 执行完整 GC, 返回 0
/// - "stop": 停止 GC, 返回 0
/// - "restart": 重启 GC, 返回 0
/// - "count": 返回内存使用量 (KB, 简化为 0)
/// - "step": 执行一步, 返回 boolean (是否完成)
/// - "isrunning": 返回 GC 是否运行
/// - "generational"/"incremental": 切换模式, 返回之前的模式字符串
/// - "param": 查询/设置 GC 参数 (简化, 返回 0)
fn call_collectgarbage(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let opt = if nargs >= 1 {
        match get_arg(state, a, 0) {
            TValue::Str(s) => s.as_str().to_string(),
            TValue::Nil(_) => "collect".to_string(),
            ref other => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #1 to 'collectgarbage' (string expected, got {})",
                    crate::tm::obj_type_name(other)
                )))
            }
        }
    } else {
        "collect".to_string()
    };

    let result = match opt.as_str() {
        "collect" => {
            // GC 正在进行中（finalizer 内重入）或状态关闭中（close_state 内）—
            // 不重入，对应 C 的 lua_gc 返回 -1，collectgarbage 不 push 返回值（返回 nil）
            if state.gc.is_gc_running() || state.gc_closing {
                state.stack.truncate(a);
                push_results(state, a, nresults, vec![]);
                return Ok(());
            } else {
                // 清理 wrap_coros 中不再被外部引用的协程
                // （ThreadContext 的 Rc 计数 == 1 表示只有 wrap_coros 持有）
                for entry in state.wrap_coros.iter_mut() {
                    if let Some(thread) = entry {
                        let rc_count = std::rc::Rc::strong_count(&thread.context);
                        if rc_count <= 1 {
                            *entry = None;
                        }
                    }
                }
                // collect_gc 内部会清理弱引用表中的死条目
                state.collect_gc();
                TValue::Integer(0)
            }
        }
        "stop" => {
            state.gc_stop();
            TValue::Integer(0)
        }
        "restart" => {
            state.gc_restart();
            TValue::Integer(0)
        }
        "count" => {
            // 返回内存使用量 (KB) — 基于 GC 估算
            TValue::Float(state.gc.gc_estimate.get() as f64 / 1024.0)
        }
        "countb" => {
            // 返回内存使用量的小数部分 (字节) — 简化为 0
            TValue::Integer(0)
        }
        "step" => {
            let siz: usize = if nargs >= 2 {
                match get_arg(state, a, 1) {
                    TValue::Integer(i) => (i as i64).max(0) as usize,
                    TValue::Float(f) => (f as i64).max(0) as usize,
                    _ => 0,
                }
            } else {
                0
            };
            let done = state.step_gc(siz);
            TValue::Boolean(done)
        }
        "isrunning" => TValue::Boolean(state.gc.is_running()),
        "generational" => {
            let old = state.gc.set_mode(crate::gc::GCMode::Generational);
            let prev = if old == crate::gc::GCMode::Generational {
                "generational"
            } else {
                "incremental"
            };
            TValue::Str(state.intern_str(prev))
        }
        "incremental" => {
            let old = state.gc.set_mode(crate::gc::GCMode::Incremental);
            let prev = if old == crate::gc::GCMode::Generational {
                "generational"
            } else {
                "incremental"
            };
            TValue::Str(state.intern_str(prev))
        }
        "param" => {
            use crate::gc::GCState;
            let pname = match get_arg(state, a, 1) {
                TValue::Str(s) => s.as_str().to_string(),
                ref other => {
                    return Err(VmError::RuntimeError(format!(
                        "bad argument #2 to 'collectgarbage' (string expected, got {})",
                        crate::tm::obj_type_name(other)
                    )))
                }
            };
            let pidx = match pname.as_str() {
                "minormul" => GCState::PARAM_MINORMUL,
                "majorminor" => GCState::PARAM_MAJORMINOR,
                "minormajor" => GCState::PARAM_MINORMAJOR,
                "pause" => GCState::PARAM_PAUSE,
                "stepmul" => GCState::PARAM_STEPMUL,
                "stepsize" => GCState::PARAM_STEPSIZE,
                _ => {
                    return Err(VmError::RuntimeError(format!(
                        "bad argument #2 to 'collectgarbage' (invalid parameter name '{}')",
                        pname
                    )))
                }
            };
            if nargs >= 3 {
                let val: i32 = match get_arg(state, a, 2) {
                    TValue::Integer(i) => i as i32,
                    TValue::Float(f) => f as i32,
                    ref other => {
                        return Err(VmError::RuntimeError(format!(
                            "bad argument #3 to 'collectgarbage' (number expected, got {})",
                            crate::tm::obj_type_name(other)
                        )))
                    }
                };
                let old = state.gc.swap_gc_param(pidx, val);
                TValue::Integer(old as i64)
            } else {
                let cur = state.gc.get_gc_param(pidx);
                TValue::Integer(cur as i64)
            }
        }
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'collectgarbage' (invalid option '{}')",
                opt
            )));
        }
    };

    push_single_result(state, a, nresults, result);
    Ok(())
}

// ============================================================================
// 打开基础库 — 对应 C 的 luaopen_base
// ============================================================================

/// 打开基础库, 注册所有全局函数
///
/// 对应 C 源码 lbaselib.cpp 的 luaopen_base 函数:
/// 1. 注册所有基础函数到全局表 (使用 BuiltinFn 函数指针)
/// 2. 设置 _G 和 _VERSION
pub fn open_base_lib(state: &mut LuaState) {
    // 注册所有基础库函数 (使用 BuiltinFn 函数指针)
    let register = |state: &mut LuaState, name: &'static std::ffi::CStr, func: crate::objects::BuiltinFnPtr| {
        let key = TValue::Str(state.intern_str(name.to_str().unwrap_or("")));
        let name_ptr = name.as_ptr() as *const u8;
        state
            .globals
            .set(key, TValue::BuiltinFn(crate::objects::BuiltinFn { func, name: name_ptr }));
    };

    // 基础库函数
    register(state, c"print", call_print);
    register(state, c"setmetatable", call_setmetatable);
    register(state, c"getmetatable", call_getmetatable);
    register(state, c"type", call_type);
    register(state, c"pcall", call_pcall);
    register(state, c"error", call_error);
    register(state, c"tonumber", call_tonumber);
    register(state, c"tostring", call_tostring);
    register(state, c"assert", call_assert);
    register(state, c"select", call_select);
    register(state, c"rawequal", call_rawequal);
    register(state, c"rawlen", call_rawlen);
    register(state, c"rawget", call_rawget);
    register(state, c"rawset", call_rawset);
    register(state, c"next", call_next);
    register(state, c"ipairs", call_ipairs);
    register(state, c"pairs", call_pairs);
    register(state, c"xpcall", call_xpcall);
    register(state, c"warn", call_warn);
    register(state, c"require", call_require);
    register(state, c"load", call_load);
    register(state, c"collectgarbage", call_collectgarbage);
    register(state, c"dofile", call_dofile);
    register(state, c"loadfile", call_loadfile);

    // 设置 _G 全局变量 (指向全局表自身)
    let globals_clone = state.globals.clone();
    let g_key = TValue::Str(state.intern_str("_G"));
    state.globals.set(g_key, TValue::Table(globals_clone));

    // 设置 _VERSION 全局变量
    let version_key = TValue::Str(state.intern_str("_VERSION"));
    state
        .globals
        .set(version_key, TValue::Str(state.intern_str("Lua 5.5")));

    // 初始化 package 表 (path + loaded),支持 require 从文件加载模块
    init_package_table(state);
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
        assert_eq!(
            base_type_name(&TValue::Table(crate::table::Table::new())),
            "table"
        );
    }

    /// 验证 LightUserData 在用户指针范围（超出内置 tag 范围）不被误判为 function
    ///
    /// 所有内置库已迁移到 BuiltinFn，coroutine.wrap 返回 Table，
    /// LightUserData 仅剩 io.lines iterator (极大值) 表现为 function。
    /// 其余所有 LightUserData 都应返回 "userdata"。
    #[test]
    fn test_lightuserdata_not_misjudged_as_function() {
        // 真实用户指针值（超出内置 tag 范围）不应被误判为 function
        let user_ptrs: [usize; 6] = [
            0,                    // NULL 指针
            1,                    // 原 BASE_PRINT 范围，基础库迁移后不再是内置 tag
            100,                  // 原字符串库范围，已迁移
            200,                  // 原数学库范围，已迁移
            1000,                 // 原 wrap_call 上限附近，coroutine.wrap 已改用 Table
            0x7fff_0000_0000,     // 真实用户指针高位 (Linux stack)
        ];
        for tag_val in user_ptrs {
            let v = TValue::LightUserData(tag_val as *mut std::ffi::c_void);
            assert_eq!(
                base_type_name(&v),
                "userdata",
                "LightUserData(0x{:x}) 应为 userdata, 不应被误判为 function",
                tag_val
            );
            assert!(
                !v.is_function(),
                "LightUserData(0x{:x}).is_function() 应为 false",
                tag_val
            );
        }
    }

    /// 验证 BuiltinFn 被正确识别为 function
    #[test]
    fn test_builtin_fn_type_recognition() {
        fn dummy_fn(
            _state: &mut crate::state::LuaState,
            _a: usize,
            _nargs: usize,
            _nresults: i32,
        ) -> Result<(), crate::execute::VmError> {
            Ok(())
        }

        let v = TValue::BuiltinFn(crate::objects::BuiltinFn {
            func: dummy_fn,
            name: c"dummy".as_ptr() as *const u8,
        });
        assert_eq!(base_type_name(&v), "function");
        assert!(v.is_function());
        assert_eq!(v.ty(), crate::objects::LuaType::Function);
        // Display 应包含函数名
        assert!(format!("{}", v).contains("dummy"));
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
        assert!(base_rawequal(
            &TValue::Nil(NilKind::Strict),
            &TValue::Nil(NilKind::Empty)
        ));
        assert!(base_rawequal(
            &TValue::Boolean(true),
            &TValue::Boolean(true)
        ));
        assert!(!base_rawequal(
            &TValue::Boolean(true),
            &TValue::Boolean(false)
        ));
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
        let args = vec![TValue::Integer(1), TValue::Integer(2), TValue::Integer(3)];
        let result = base_select(2, &args).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], TValue::Integer(2));
        assert_eq!(result[1], TValue::Integer(3));
    }

    #[test]
    fn test_base_select_negative() {
        let args = vec![TValue::Integer(1), TValue::Integer(2), TValue::Integer(3)];
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
    // is_base_tag 测试已删除（base 库已迁移到 BuiltinFn，不再使用 tag）
    // ========================================================================

    // ========================================================================
    // open_base_lib 测试
    // ========================================================================

    #[test]
    fn test_open_base_lib_registers_functions() {
        let mut state = LuaState::new();
        open_base_lib(&mut state);

        // 验证所有基础库函数注册为 BuiltinFn
        for name in &[
            "print",
            "setmetatable",
            "getmetatable",
            "type",
            "pcall",
            "error",
            "tonumber",
            "tostring",
            "assert",
            "select",
            "rawequal",
            "rawlen",
            "rawget",
            "rawset",
            "next",
            "ipairs",
            "pairs",
            "xpcall",
            "warn",
            "require",
            "load",
            "collectgarbage",
            "dofile",
            "loadfile",
        ] {
            let key = TValue::Str(state.intern_str(name));
            let val = state.globals.get(&key);
            assert!(val.is_some(), "{} must be registered", name);
            assert!(
                matches!(val, Some(TValue::BuiltinFn(_))),
                "{} must be registered as BuiltinFn",
                name
            );
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
    // 直接调用各 BuiltinFn 函数的测试（替代原 call_base_function 测试）
    // ========================================================================

    /// 辅助：构造一个占位 BuiltinFn 作为栈上的 "函数" 位置
    fn placeholder_builtin() -> TValue {
        TValue::BuiltinFn(crate::objects::BuiltinFn {
            func: call_searcher_placeholder,
            name: c"placeholder".as_ptr() as *const u8,
        })
    }

    #[test]
    fn test_call_type() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Integer(42));
        call_type(&mut state, 0, 1, 1).unwrap();
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "number"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_tonumber() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Str(state.intern_str("42")));
        call_tonumber(&mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 42),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_tostring() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Integer(42));
        call_tostring(&mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "42"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_rawequal() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Integer(42));
        state.stack.push(TValue::Integer(42));
        call_rawequal(&mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Boolean(b) => assert!(*b),
            _ => panic!("expected boolean result"),
        }
    }

    #[test]
    fn test_call_rawlen() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Str(state.intern_str("hello")));
        call_rawlen(&mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 5),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_rawget() {
        let mut state = LuaState::new();
        state.stack.clear();
        let mut t = crate::table::Table::new();
        t.set(TValue::Integer(1), TValue::Integer(100));
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Integer(1));
        call_rawget(&mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 100),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_rawset() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(999));
        call_rawset(&mut state, 0, 3, 1).unwrap();
        match &state.stack[0] {
            TValue::Table(t) => {
                let val = t.get(&TValue::Integer(1));
                assert!(matches!(val, Some(TValue::Integer(999))));
            }
            _ => panic!("expected table result"),
        }
    }

    #[test]
    fn test_call_select_hash() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Str(state.intern_str("#")));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(2));
        state.stack.push(TValue::Integer(3));
        call_select(&mut state, 0, 4, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 3),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_select_index() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Integer(2));
        state.stack.push(TValue::Integer(10));
        state.stack.push(TValue::Integer(20));
        state.stack.push(TValue::Integer(30));
        call_select(&mut state, 0, 4, -1).unwrap();
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
    fn test_call_assert_true() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Boolean(true));
        call_assert(&mut state, 0, 1, -1).unwrap();
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0] {
            TValue::Boolean(b) => assert!(*b),
            _ => panic!("expected boolean true"),
        }
    }

    #[test]
    fn test_call_assert_false() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Boolean(false));
        let result = call_assert(&mut state, 0, 1, -1);
        assert!(result.is_err());
    }

    #[test]
    fn test_call_error() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(placeholder_builtin());
        state
            .stack
            .push(TValue::Str(state.intern_str("test error")));
        let result = call_error(&mut state, 0, 1, 0);
        assert!(result.is_err());
        match result {
            Err(VmError::RuntimeError(msg)) => assert_eq!(msg, "test error"),
            _ => panic!("expected RuntimeError"),
        }
    }

    #[test]
    fn test_call_setmetatable() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        let mt = crate::table::Table::new();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Table(mt));
        call_setmetatable(&mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Table(t) => assert!(t.has_metatable()),
            _ => panic!("expected table result"),
        }
    }

    #[test]
    fn test_call_getmetatable() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        t.set_metatable(Some(crate::table::Table::new()));
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Table(t));
        call_getmetatable(&mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Table(_) => {}
            _ => panic!("expected table result"),
        }
    }

    #[test]
    fn test_call_getmetatable_no_mt() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Table(t));
        call_getmetatable(&mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Nil(_) => {}
            _ => panic!("expected nil result"),
        }
    }

    #[test]
    fn test_call_ipairs() {
        let mut state = LuaState::new();
        state.stack.clear();
        let mut t = crate::table::Table::new();
        t.set(TValue::Integer(1), TValue::Integer(10));
        t.set(TValue::Integer(2), TValue::Integer(20));
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Table(t));
        call_ipairs(&mut state, 0, 1, 3).unwrap();
        assert_eq!(state.stack.len(), 3);
        // 第一个返回值是迭代器函数 (BuiltinFn, func 指向 call_ipairs_aux)
        match &state.stack[0] {
            TValue::BuiltinFn(bf) => {
                assert_eq!(bf.func as usize, call_ipairs_aux as usize);
            }
            _ => panic!("expected BuiltinFn"),
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
    fn test_call_pairs() {
        let mut state = LuaState::new();
        state.stack.clear();
        let t = crate::table::Table::new();
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Table(t));
        call_pairs(&mut state, 0, 1, 3).unwrap();
        assert_eq!(state.stack.len(), 3);
        // 第一个返回值是 next 迭代器 (BuiltinFn, func 指向 call_next_iter)
        match &state.stack[0] {
            TValue::BuiltinFn(bf) => {
                assert_eq!(bf.func as usize, call_next_iter as usize);
            }
            _ => panic!("expected BuiltinFn"),
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
        state.stack.push(placeholder_builtin());
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
        state.stack.push(placeholder_builtin());
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Integer(0));
        call_ipairs_aux(&mut state, 0, 2, 1).unwrap();
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    // ========================================================================
    // call_base_function_unknown_tag 测试已删除（base 库已迁移到 BuiltinFn）
    // ========================================================================

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
        let (key, val) = table_next(&t, &TValue::Nil(NilKind::Strict)).unwrap();
        assert!(matches!(key, Some(TValue::Integer(1))));
        assert_eq!(val, TValue::Integer(10));

        // 下一个
        let (key, val) = table_next(&t, &TValue::Integer(1)).unwrap();
        assert!(matches!(key, Some(TValue::Integer(2))));
        assert_eq!(val, TValue::Integer(20));

        // 结束
        let (key, _) = table_next(&t, &TValue::Integer(2)).unwrap();
        assert!(key.is_none());
    }
}
