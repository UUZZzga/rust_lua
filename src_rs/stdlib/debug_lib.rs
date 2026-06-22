//! 调试库 (ldblib.cpp → Rust)
//!
//! 对应 C 源码: ldblib.cpp
//!
//! ## 主要功能
//! - 注册 debug 全局表，包含调试函数
//! - 提供 debug.getinfo, debug.getlocal, debug.setlocal 等栈操作
//! - 提供 debug.getupvalue, debug.setupvalue, debug.upvalueid, debug.upvaluejoin
//! - 提供 debug.sethook, debug.gethook 钩子管理
//! - 提供 debug.traceback 堆栈回溯
//! - 提供 debug.getregistry, debug.getmetatable, debug.setmetatable
//! - 提供 debug.getuservalue, debug.setuservalue
//! - 提供 debug.debug 交互式调试器
//!
//! ## 标签分配
//! - 标签 1-19: 基础库
//! - 标签 100+: 字符串库
//! - 标签 200+: 数学库
//! - 标签 300+: UTF-8 库
//! - 标签 400+: 表库
//! - 标签 500+: 调试库

use crate::objects::{NilKind, TValue, LClosure, Proto, UpVal};
use crate::state::LuaState;
use crate::table::Table;
use crate::execute::VmError;
use crate::strings::LuaString;
use std::cell::RefCell;
use std::rc::Rc;

// ============================================================================
// 函数标签 (LightUserData 占位符值)
// ============================================================================

pub const DEBUG_DEBUG: usize = 500;
pub const DEBUG_GETUSERVALUE: usize = 501;
pub const DEBUG_GETHOOK: usize = 502;
pub const DEBUG_GETINFO: usize = 503;
pub const DEBUG_GETLOCAL: usize = 504;
pub const DEBUG_GETREGISTRY: usize = 505;
pub const DEBUG_GETMETATABLE: usize = 506;
pub const DEBUG_GETUPVALUE: usize = 507;
pub const DEBUG_UPVALUEJOIN: usize = 508;
pub const DEBUG_UPVALUEID: usize = 509;
pub const DEBUG_SETUSERVALUE: usize = 510;
pub const DEBUG_SETHOOK: usize = 511;
pub const DEBUG_SETLOCAL: usize = 512;
pub const DEBUG_SETMETATABLE: usize = 513;
pub const DEBUG_SETUPVALUE: usize = 514;
pub const DEBUG_TRACEBACK: usize = 515;

/// 调试库标签范围: [500, 520)
pub fn is_debug_tag(tag: usize) -> bool {
    (500..520).contains(&tag)
}

/// 将 debug 库函数 tag 映射到函数名（用于 traceback）
pub fn debug_function_name(tag: usize) -> Option<&'static str> {
    match tag {
        DEBUG_DEBUG => Some("debug"),
        DEBUG_GETUSERVALUE => Some("getuservalue"),
        DEBUG_GETHOOK => Some("gethook"),
        DEBUG_GETINFO => Some("getinfo"),
        DEBUG_GETLOCAL => Some("getlocal"),
        DEBUG_GETREGISTRY => Some("getregistry"),
        DEBUG_GETMETATABLE => Some("getmetatable"),
        DEBUG_GETUPVALUE => Some("getupvalue"),
        DEBUG_UPVALUEJOIN => Some("upvaluejoin"),
        DEBUG_UPVALUEID => Some("upvalueid"),
        DEBUG_SETUSERVALUE => Some("setuservalue"),
        DEBUG_SETHOOK => Some("sethook"),
        DEBUG_SETLOCAL => Some("setlocal"),
        DEBUG_SETMETATABLE => Some("setmetatable"),
        DEBUG_SETUPVALUE => Some("setupvalue"),
        DEBUG_TRACEBACK => Some("traceback"),
        _ => None,
    }
}

// ============================================================================
// Hook 掩码常量 — 对应 C 的 LUA_MASK*
// ============================================================================

pub const LUA_MASKCALL: i32 = 1 << 0;  // 1
pub const LUA_MASKRET: i32 = 1 << 1;   // 2
pub const LUA_MASKLINE: i32 = 1 << 2;  // 4
pub const LUA_MASKCOUNT: i32 = 1 << 3; // 8

/// HOOKKEY — 注册表中存储 hook 表的键
const HOOKKEY: &str = "_HOOKKEY";

// ============================================================================
// 辅助函数
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

/// 检查参数是否为可选的线程 (对应 C 的 getthread)
///
/// 返回 (线程状态引用, arg_offset)
/// 由于当前实现不支持多线程, 始终返回当前状态
fn get_thread(state: &LuaState, a: usize) -> (usize, usize) {
    let arg0 = get_arg(state, a, 0);
    if matches!(arg0, TValue::Thread(_)) {
        (0, 1)
    } else {
        (0, 0)
    }
}

/// 对应 C 的 luaO_chunkid：将 source 格式化为短源标识
///
/// LUA_IDSIZE = 60 (luaconf.h)
/// RETS = "..." (lobject.cpp)
/// PRE = "[string \"" (lobject.cpp)
/// POS = "\"]" (lobject.cpp)
fn short_src(source: &LuaString) -> String {
    const LUA_IDSIZE: usize = 60;
    const RETS: &str = "...";
    const PRE: &str = "[string \"";
    const POS: &str = "\"]";

    let bytes = source.as_str().as_bytes();
    match bytes.first() {
        Some(&b'=') => {
            // 'literal' source: strip '=' prefix
            let content = &bytes[1..];
            if content.len() <= LUA_IDSIZE {
                String::from_utf8_lossy(content).into_owned()
            } else {
                // truncate to LUA_IDSIZE - 1
                String::from_utf8_lossy(&content[..LUA_IDSIZE - 1]).into_owned()
            }
        }
        Some(&b'@') => {
            // file name: strip '@' prefix
            let content = &bytes[1..];
            if content.len() <= LUA_IDSIZE {
                String::from_utf8_lossy(content).into_owned()
            } else {
                // prepend "...", take last (LUA_IDSIZE - 3) chars
                let keep = LUA_IDSIZE - RETS.len();
                let start = content.len() - keep;
                format!("{}{}", RETS, String::from_utf8_lossy(&content[start..]))
            }
        }
        _ => {
            // string: format as [string "source"]
            let nl_pos = bytes.iter().position(|&b| b == b'\n');
            let content_end = nl_pos.unwrap_or(bytes.len());
            // bufflen for content = LUA_IDSIZE - len(PRE) - len(RETS) - len(POS) - 1
            let bufflen = LUA_IDSIZE - PRE.len() - RETS.len() - POS.len() - 1;
            if content_end < bufflen && nl_pos.is_none() {
                // small one-line source
                format!("{}{}{}", PRE, String::from_utf8_lossy(&bytes[..content_end]), POS)
            } else {
                // truncate and add "..."
                let truncate_at = content_end.min(bufflen);
                format!("{}{}{}{}", PRE,
                    String::from_utf8_lossy(&bytes[..truncate_at]), RETS, POS)
            }
        }
    }
}

/// 对应 C 的 luaG_getfuncline：从 Proto 的 line_info/abs_line_info 计算 pc 所在行号
fn get_proto_line(proto: &Proto, pc: usize) -> i32 {
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

/// 获取当前栈帧的源和行号
fn get_current_source_line(state: &LuaState) -> (String, i32) {
    if state.base == 0 || state.base > state.stack.len() {
        return ("?".to_string(), -1);
    }
    if let TValue::LClosure(closure) = &state.stack[state.base - 1] {
        let src = closure
            .proto
            .source
            .as_ref()
            .map(short_src)
            .unwrap_or_else(|| "?".to_string());
        let ln = get_proto_line(&closure.proto, state.pc);
        (src, ln)
    } else {
        ("?".to_string(), -1)
    }
}

/// 从 Proto 的 loc_vars 获取指定 PC 处的局部变量名
///
/// 对应 C 的 lua_getlocalname
fn get_local_name(proto: &Proto, local_number: usize, pc: usize) -> Option<String> {
    for loc_var in &proto.loc_vars {
        if (loc_var.start_pc as usize) <= pc && pc < (loc_var.end_pc as usize) {
            if let Some(ref name) = loc_var.varname {
                let name = name.as_str().to_string();
                if local_number == 1 {
                    return Some(name);
                }
                // 继续查找下一个
            }
        }
    }
    None
}

/// 将字符串掩码转换为位掩码 — 对应 C 的 makemask
fn make_mask(smask: &str, count: i32) -> i32 {
    let mut mask = 0i32;
    if smask.contains('c') {
        mask |= LUA_MASKCALL;
    }
    if smask.contains('r') {
        mask |= LUA_MASKRET;
    }
    if smask.contains('l') {
        mask |= LUA_MASKLINE;
    }
    if count > 0 {
        mask |= LUA_MASKCOUNT;
    }
    mask
}

/// 将位掩码转换为字符串掩码 — 对应 C 的 unmakemask
fn unmake_mask(mask: i32) -> String {
    let mut s = String::new();
    if mask & LUA_MASKCALL != 0 {
        s.push('c');
    }
    if mask & LUA_MASKRET != 0 {
        s.push('r');
    }
    if mask & LUA_MASKLINE != 0 {
        s.push('l');
    }
    s
}

// ============================================================================
// 函数派发 — 从 execute.rs / state.rs 调用
// ============================================================================

/// 调试库函数派发
///
/// 从 execute.rs 的 op_call 或 state.rs 的 pcall 调用,
/// 当 LightUserData 标签在 [500, 520) 范围内时。
pub fn call_debug_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let prev_c_func = state.last_c_function.take();
    state.last_c_function = debug_function_name(tag).map(|s| s.to_string());

    let result = match tag {
        DEBUG_DEBUG => call_debug(state, a, nargs, nresults),
        DEBUG_GETUSERVALUE => call_getuservalue(state, a, nargs, nresults),
        DEBUG_GETHOOK => call_gethook(state, a, nargs, nresults),
        DEBUG_GETINFO => call_getinfo(state, a, nargs, nresults),
        DEBUG_GETLOCAL => call_getlocal(state, a, nargs, nresults),
        DEBUG_GETREGISTRY => call_getregistry(state, a, nargs, nresults),
        DEBUG_GETMETATABLE => call_getmetatable(state, a, nargs, nresults),
        DEBUG_GETUPVALUE => call_getupvalue(state, a, nargs, nresults),
        DEBUG_UPVALUEJOIN => call_upvaluejoin(state, a, nargs, nresults),
        DEBUG_UPVALUEID => call_upvalueid(state, a, nargs, nresults),
        DEBUG_SETUSERVALUE => call_setuservalue(state, a, nargs, nresults),
        DEBUG_SETHOOK => call_sethook(state, a, nargs, nresults),
        DEBUG_SETLOCAL => call_setlocal(state, a, nargs, nresults),
        DEBUG_SETMETATABLE => call_setmetatable(state, a, nargs, nresults),
        DEBUG_SETUPVALUE => call_setupvalue(state, a, nargs, nresults),
        DEBUG_TRACEBACK => call_traceback(state, a, nargs, nresults),
        _ => Err(VmError::RuntimeError(format!(
            "unknown debug function tag: {}",
            tag
        ))),
    };

    if result.is_ok() {
        state.last_c_function = prev_c_func;
    }
    result
}

// ============================================================================
// 各函数实现
// ============================================================================

/// debug.getregistry() — 对应 C 的 db_getregistry
///
/// 返回注册表
fn call_getregistry(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    push_single_result(state, a, nresults, TValue::Table(state.registry.clone()));
    Ok(())
}

/// debug.getmetatable(v) — 对应 C 的 db_getmetatable
///
/// 返回值的元表 (不调用 __metatable)
fn call_getmetatable(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg = get_arg(state, a, 0);
    let result = match &arg {
        TValue::Table(t) => {
            if let Some(ref mt) = t.metatable {
                TValue::Table(*mt.clone())
            } else {
                TValue::Nil(NilKind::Strict)
            }
        }
        TValue::UserData(u) => {
            if let Some(ref mt) = u.metatable {
                TValue::Table(mt.as_ref().clone())
            } else {
                TValue::Nil(NilKind::Strict)
            }
        }
        _ => TValue::Nil(NilKind::Strict),
    };
    push_single_result(state, a, nresults, result);
    Ok(())
}

/// debug.setmetatable(v, mt) — 对应 C 的 db_setmetatable
///
/// 设置值的元表, 返回原值
fn call_setmetatable(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg1 = get_arg(state, a, 0);
    let arg2 = get_arg(state, a, 1);

    // 检查第二个参数是否为 nil 或表
    if !matches!(&arg2, TValue::Table(_) | TValue::Nil(_)) {
        return Err(VmError::RuntimeError(
            "bad argument #2 to 'setmetatable' (nil or table expected)".to_string(),
        ));
    }

    let result = match (&arg1, &arg2) {
        (TValue::Table(_), TValue::Table(mt)) => {
            // 修改栈上的表
            if a + 1 < state.stack.len() {
                if let TValue::Table(ref mut t) = state.stack[a + 1] {
                    t.metatable = Some(Box::new(mt.clone()));
                }
            }
            arg1.clone()
        }
        (TValue::Table(_), TValue::Nil(_)) => {
            if a + 1 < state.stack.len() {
                if let TValue::Table(ref mut t) = state.stack[a + 1] {
                    t.metatable = None;
                }
            }
            arg1.clone()
        }
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #1 to 'setmetatable' (table expected)".to_string(),
            ));
        }
    };
    push_single_result(state, a, nresults, result);
    Ok(())
}

/// debug.getuservalue(u, [n]) — 对应 C 的 db_getuservalue
///
/// 返回用户数据的第 n 个用户值
fn call_getuservalue(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg = get_arg(state, a, 0);
    let n = if nargs >= 2 {
        get_arg(state, a, 1).as_integer().unwrap_or(1) as usize
    } else {
        1
    };

    let result = match &arg {
        TValue::UserData(u) => {
            if n > 0 && n <= u.user_values.len() {
                u.user_values[n - 1].clone()
            } else {
                TValue::Nil(NilKind::Strict)
            }
        }
        _ => TValue::Nil(NilKind::Strict),
    };
    push_single_result(state, a, nresults, result);
    Ok(())
}

/// debug.setuservalue(u, v, [n]) — 对应 C 的 db_setuservalue
///
/// 设置用户数据的第 n 个用户值
fn call_setuservalue(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg1 = get_arg(state, a, 0);
    let arg2 = get_arg(state, a, 1);
    let n = if nargs >= 3 {
        get_arg(state, a, 2).as_integer().unwrap_or(1) as usize
    } else {
        1
    };

    let result = match &arg1 {
        TValue::UserData(_) => {
            // 修改栈上的 userdata
            if a + 1 < state.stack.len() {
                if let TValue::UserData(ref mut u) = state.stack[a + 1] {
                    while u.user_values.len() < n {
                        u.user_values.push(TValue::Nil(NilKind::Strict));
                    }
                    u.user_values[n - 1] = arg2.clone();
                }
            }
            arg1.clone()
        }
        _ => {
            // 对于非 userdata, 返回 nil (对应 C 的 luaL_pushfail)
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            return Ok(());
        }
    };
    push_single_result(state, a, nresults, result);
    Ok(())
}

/// debug.getinfo([thread,] level_or_func [, what]) — 对应 C 的 db_getinfo
///
/// 返回包含调试信息的表
fn call_getinfo(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 解析参数: 可选 thread, level/func, 可选 what
    let mut arg_offset = 0;
    let arg0 = get_arg(state, a, 0);
    if matches!(arg0, TValue::Thread(_)) {
        arg_offset = 1;
    }

    let level_or_func = get_arg(state, a, arg_offset);
    let what = if nargs > arg_offset + 1 {
        match &get_arg(state, a, arg_offset + 1) {
            TValue::Str(s) => s.as_str().to_string(),
            _ => "flnSrtu".to_string(),
        }
    } else {
        "flnSrtu".to_string()
    };

    // 检查无效选项
    if what.starts_with('>') {
        return Err(VmError::RuntimeError(
            "bad argument to 'getinfo' (invalid option '>')".to_string(),
        ));
    }
    // 验证每个选项字符 (对应 C 的 lua_getinfo 返回 0 时报错)
    // 合法选项: S l u n r t L f
    for ch in what.chars() {
        if !matches!(ch, 'S' | 'l' | 'u' | 'n' | 'r' | 't' | 'L' | 'f') {
            return Err(VmError::RuntimeError(
                "bad argument to 'getinfo' (invalid option)".to_string(),
            ));
        }
    }

    let mut info = DebugInfo::default();

    if matches!(level_or_func, TValue::LClosure(_)) {
        // 函数参数模式
        if let TValue::LClosure(closure) = &level_or_func {
            fill_info_from_closure(&mut info, closure, &what);
        }
    } else if matches!(level_or_func, TValue::LightUserData(_)) {
        // C 函数
        info.what = "C".to_string();
        info.short_src = "[C]".to_string();
        info.source = "=[C]".to_string();
        info.currentline = -1;
        info.nups = 0;
        info.nparams = 0;
        info.isvararg = true;
        info.name = None;
        info.namewhat = String::new();
    } else {
        // 栈级别模式
        let level = level_or_func.as_integer().unwrap_or(0) as i32;
        if level < 0 {
            // 超出范围
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            return Ok(());
        }
        if !fill_info_from_level(state, &mut info, level, &what) {
            // 超出范围
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            return Ok(());
        }
    }

    // 构建结果表
    let mut result_table = Table::new();
    let what_bytes = what.as_bytes();

    if what.contains('S') {
        result_table.set(
            TValue::Str(state.intern_str("source")),
            TValue::Str(state.intern_str(&info.source)),
        );
        result_table.set(
            TValue::Str(state.intern_str("short_src")),
            TValue::Str(state.intern_str(&info.short_src)),
        );
        result_table.set(
            TValue::Str(state.intern_str("linedefined")),
            TValue::Integer(info.linedefined as i64),
        );
        result_table.set(
            TValue::Str(state.intern_str("lastlinedefined")),
            TValue::Integer(info.lastlinedefined as i64),
        );
        result_table.set(
            TValue::Str(state.intern_str("what")),
            TValue::Str(state.intern_str(&info.what)),
        );
    }
    if what.contains('l') {
        result_table.set(
            TValue::Str(state.intern_str("currentline")),
            TValue::Integer(info.currentline as i64),
        );
    }
    if what.contains('u') {
        result_table.set(
            TValue::Str(state.intern_str("nups")),
            TValue::Integer(info.nups as i64),
        );
        result_table.set(
            TValue::Str(state.intern_str("nparams")),
            TValue::Integer(info.nparams as i64),
        );
        result_table.set(
            TValue::Str(state.intern_str("isvararg")),
            TValue::Boolean(info.isvararg),
        );
    }
    if what.contains('n') {
        match &info.name {
            Some(name) => {
                result_table.set(
                    TValue::Str(state.intern_str("name")),
                    TValue::Str(state.intern_str(name)),
                );
            }
            None => {
                result_table.set(
                    TValue::Str(state.intern_str("name")),
                    TValue::Nil(NilKind::Strict),
                );
            }
        }
        result_table.set(
            TValue::Str(state.intern_str("namewhat")),
            TValue::Str(state.intern_str(&info.namewhat)),
        );
    }
    if what.contains('r') {
        result_table.set(
            TValue::Str(state.intern_str("ftransfer")),
            TValue::Integer(info.ftransfer as i64),
        );
        result_table.set(
            TValue::Str(state.intern_str("ntransfer")),
            TValue::Integer(info.ntransfer as i64),
        );
    }
    if what.contains('t') {
        result_table.set(
            TValue::Str(state.intern_str("istailcall")),
            TValue::Boolean(info.istailcall),
        );
        result_table.set(
            TValue::Str(state.intern_str("extraargs")),
            TValue::Integer(info.extraargs as i64),
        );
    }
    if what.contains('L') {
        // activelines 表 — 对应 C 的 collectvalidlines
        // 对于 C 函数, activelines 为 nil
        if let Some(ref closure) = info.closure {
            let mut actlines = Table::new();
            fill_active_lines(&mut actlines, &closure.proto);
            result_table.set(
                TValue::Str(state.intern_str("activelines")),
                TValue::Table(actlines),
            );
        } else {
            // C 函数: activelines = nil
            result_table.set(
                TValue::Str(state.intern_str("activelines")),
                TValue::Nil(NilKind::Strict),
            );
        }
    }
    if what.contains('f') {
        // 直接从栈上获取原始函数值 (避免克隆导致引用语义失效)
        // 对应 C 的 lua_pushvalue(L, arg + 1)
        let func_idx = a + 1 + arg_offset;
        if func_idx < state.stack.len() {
            // 使用 std::mem::replace 移动值, 避免克隆
            // (栈上的值会被替换为 nil, 但随后 push_results 会 truncate 栈)
            let func_val = std::mem::replace(
                &mut state.stack[func_idx],
                TValue::Nil(NilKind::Strict),
            );
            result_table.set(
                TValue::Str(state.intern_str("func")),
                func_val,
            );
        } else {
            result_table.set(
                TValue::Str(state.intern_str("func")),
                TValue::Nil(NilKind::Strict),
            );
        }
    }

    push_single_result(state, a, nresults, TValue::Table(result_table));
    Ok(())
}

/// 调试信息结构
#[derive(Default)]
struct DebugInfo {
    source: String,
    short_src: String,
    linedefined: i32,
    lastlinedefined: i32,
    what: String,
    currentline: i32,
    nups: usize,
    nparams: usize,
    isvararg: bool,
    name: Option<String>,
    namewhat: String,
    ftransfer: i32,
    ntransfer: i32,
    istailcall: bool,
    extraargs: i32,
    func: Option<TValue>,
    closure: Option<LClosure>,
}

/// 从闭包填充调试信息
fn fill_info_from_closure(info: &mut DebugInfo, closure: &LClosure, what: &str) {
    let proto = &closure.proto;
    // 注意: 不在此设置 info.func, 由 call_getinfo 直接从栈上获取原始值
    // 以确保 b.func == 原始函数 (引用语义)
    info.closure = Some(closure.clone());

    if what.contains('S') {
        info.source = proto
            .source
            .as_ref()
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| "=?".to_string());
        info.short_src = proto
            .source
            .as_ref()
            .map(short_src)
            .unwrap_or_else(|| "?".to_string());
        info.linedefined = proto.line_defined;
        info.lastlinedefined = proto.last_line_defined;
        info.what = "Lua".to_string();
    }
    if what.contains('l') {
        info.currentline = -1; // 函数参数模式没有当前行
    }
    if what.contains('u') {
        info.nups = closure.upvals.len();
        info.nparams = proto.num_params as usize;
        info.isvararg = proto.is_vararg();
    }
    if what.contains('n') {
        info.name = None;
        info.namewhat = String::new();
    }
    if what.contains('t') {
        info.istailcall = false;
        info.extraargs = 0;
    }
    if what.contains('r') {
        info.ftransfer = 0;
        info.ntransfer = 0;
    }
}

/// 从栈级别填充调试信息
///
/// 返回 false 表示级别超出范围
fn fill_info_from_level(
    state: &LuaState,
    info: &mut DebugInfo,
    level: i32,
    what: &str,
) -> bool {
    // level 0 = 当前函数 (debug.getinfo 自身, 通常是 C 函数)
    // level 1 = 调用 debug.getinfo 的函数 (当前 Lua 帧)
    // level 2 = 调用 level 1 的函数 (call_info 的最后一个元素)
    // level n = call_info[len - (n - 1)]

    if level == 0 {
        // C 函数 (debug.getinfo 自身)
        info.what = "C".to_string();
        info.short_src = "[C]".to_string();
        info.source = "=[C]".to_string();
        info.currentline = -1;
        info.nups = 0;
        info.nparams = 0;
        info.isvararg = true;
        info.name = None;
        info.namewhat = String::new();
        return true;
    }

    if level == 1 {
        // 当前 Lua 帧
        if state.base == 0 || state.base > state.stack.len() {
            return false;
        }
        if let TValue::LClosure(closure) = &state.stack[state.base - 1] {
            let proto = &closure.proto;
            info.func = Some(TValue::LClosure(closure.clone()));
            info.closure = Some(closure.clone());

            // 从 call_info 的最后一个元素获取 name 和 namewhat
            // call_info[last] 记录了当前函数是如何被调用的
            let (name, namewhat) = state
                .call_info
                .last()
                .map(|entry| {
                    (
                        if entry.name.is_empty() {
                            None
                        } else {
                            Some(entry.name.clone())
                        },
                        entry.namewhat.clone(),
                    )
                })
                .unwrap_or((None, String::new()));

            if what.contains('S') {
                info.source = proto
                    .source
                    .as_ref()
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_else(|| "=?".to_string());
                info.short_src = proto
                    .source
                    .as_ref()
                    .map(short_src)
                    .unwrap_or_else(|| "?".to_string());
                info.linedefined = proto.line_defined;
                info.lastlinedefined = proto.last_line_defined;
                info.what = "Lua".to_string();
            }
            if what.contains('l') {
                info.currentline = get_proto_line(proto, state.pc);
            }
            if what.contains('u') {
                info.nups = closure.upvals.len();
                info.nparams = proto.num_params as usize;
                info.isvararg = proto.is_vararg();
            }
            if what.contains('n') {
                info.name = name;
                info.namewhat = namewhat;
            }
            if what.contains('t') {
                info.istailcall = false;
                info.extraargs = state.nextraargs;
            }
            if what.contains('r') {
                info.ftransfer = 0;
                info.ntransfer = 0;
            }
            return true;
        }
        // C 函数帧
        info.what = "C".to_string();
        info.short_src = "[C]".to_string();
        info.source = "=[C]".to_string();
        info.currentline = -1;
        info.nups = 0;
        info.nparams = 0;
        info.isvararg = true;
        info.name = None;
        info.namewhat = String::new();
        return true;
    }

    // level >= 2: 从 call_info 中获取
    // call_info 记录的是"被调用的函数"的信息
    // call_info[0] = 第一个被调用的函数（最外层）
    // call_info[last] = 最近被调用的函数（调用当前帧的函数）
    //
    // level 1 = 当前 Lua 帧
    // level 2 = 调用当前帧的函数 = call_info[last]
    // level 3 = 调用 level 2 的函数 = call_info[last-1]
    // level n = call_info[len - n + 1]
    //
    // 例: call_info = [f, g.x] (len=2)
    // level 2 = f = call_info[0] = call_info[2 - 2] = call_info[len - level]
    // level 3 = 主函数 = 不在 call_info 中 (越界)
    let ci_idx = match state.call_info.len().checked_sub(level as usize) {
        Some(idx) => idx,
        None => return false,
    };

    let entry = &state.call_info[ci_idx];
    if let Some(ref closure) = entry.closure {
        let proto = &closure.proto;
        info.func = Some(TValue::LClosure(closure.clone()));
        info.closure = Some(closure.clone());

        if what.contains('S') {
            info.source = proto
                .source
                .as_ref()
                .map(|s| s.as_str().to_string())
                .unwrap_or_else(|| "=?".to_string());
            info.short_src = proto
                .source
                .as_ref()
                .map(short_src)
                .unwrap_or_else(|| "?".to_string());
            info.linedefined = proto.line_defined;
            info.lastlinedefined = proto.last_line_defined;
            info.what = "Lua".to_string();
        }
        if what.contains('l') {
            info.currentline = get_proto_line(proto, entry.saved_pc);
        }
        if what.contains('u') {
            info.nups = closure.upvals.len();
            info.nparams = proto.num_params as usize;
            info.isvararg = proto.is_vararg();
        }
        if what.contains('n') {
            info.name = if entry.name.is_empty() {
                None
            } else {
                Some(entry.name.clone())
            };
            info.namewhat = entry.namewhat.clone();
        }
        if what.contains('t') {
            info.istailcall = false;
            info.extraargs = 0;
        }
        if what.contains('r') {
            info.ftransfer = 0;
            info.ntransfer = 0;
        }
        return true;
    }

    // C 函数帧
    info.what = "C".to_string();
    info.short_src = "[C]".to_string();
    info.source = "=[C]".to_string();
    info.currentline = -1;
    info.nups = 0;
    info.nparams = 0;
    info.isvararg = true;
    info.name = None;
    info.namewhat = String::new();
    true
}

/// 填充活动行表 — 对应 C 的 collectvalidlines
///
/// 算法:
/// 1. 从 linedefined 开始
/// 2. 对每条指令, 计算其行号 (nextline)
/// 3. 将该行标记为 true
fn fill_active_lines(table: &mut Table, proto: &Proto) {
    if proto.line_info.is_empty() {
        return;
    }

    const ABSLINEINFO: i8 = -0x80;
    const MAXIWTHABS: i32 = 128;

    /// 获取 baseline — 对应 C 的 getbaseline
    fn get_baseline(proto: &Proto, pc: i32) -> (i32, i32) {
        if proto.abs_line_info.is_empty() || pc < proto.abs_line_info[0].pc {
            return (-1, proto.line_defined);
        }
        let mut i = pc / MAXIWTHABS - 1;
        if i < 0 {
            i = 0;
        }
        while (i + 1) < proto.abs_line_info.len() as i32
            && pc >= proto.abs_line_info[(i + 1) as usize].pc
        {
            i += 1;
        }
        let base_pc = proto.abs_line_info[i as usize].pc;
        let base_line = proto.abs_line_info[i as usize].line;
        (base_pc, base_line)
    }

    /// 获取指令 pc 对应的行号 — 对应 C 的 luaG_getfuncline
    fn get_func_line(proto: &Proto, pc: i32) -> i32 {
        if proto.line_info.is_empty() {
            return -1;
        }
        let (mut base_pc, mut baseline) = get_baseline(proto, pc);
        base_pc += 1;
        while base_pc < pc {
            if proto.line_info[base_pc as usize] != ABSLINEINFO {
                baseline += proto.line_info[base_pc as usize] as i32;
            }
            base_pc += 1;
        }
        baseline
    }

    /// 计算下一条指令的行号 — 对应 C 的 nextline
    fn next_line(proto: &Proto, currentline: i32, pc: usize) -> i32 {
        if proto.line_info[pc] != ABSLINEINFO {
            currentline + proto.line_info[pc] as i32
        } else {
            get_func_line(proto, pc as i32)
        }
    }

    let mut currentline = proto.line_defined;
    let mut start = 0;

    // 处理变参函数: 跳过第一条指令 (OP_VARARGPREP)
    if proto.is_vararg() && !proto.code.is_empty() {
        currentline = next_line(proto, currentline, 0);
        start = 1;
    }

    // 对每条指令, 计算行号并标记
    for i in start..proto.line_info.len() {
        currentline = next_line(proto, currentline, i);
        if currentline > 0 {
            table.set_int(currentline as i64, TValue::Boolean(true));
        }
    }
}

/// debug.getlocal([thread,] level, local) — 对应 C 的 db_getlocal
///
/// 返回局部变量名和值
fn call_getlocal(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let mut arg_offset = 0;
    let arg0 = get_arg(state, a, 0);
    if matches!(arg0, TValue::Thread(_)) {
        arg_offset = 1;
    }

    let arg1 = get_arg(state, a, arg_offset);
    let nvar = get_arg(state, a, arg_offset + 1)
        .as_integer()
        .unwrap_or(0) as i32;

    // 函数参数模式
    if matches!(arg1, TValue::LClosure(_)) {
        if let TValue::LClosure(closure) = &arg1 {
            // 获取函数的局部变量名
            let name = get_local_name(&closure.proto, nvar as usize, 0);
            match name {
                Some(n) => {
                    push_single_result(
                        state,
                        a,
                        nresults,
                        TValue::Str(state.intern_str(&n)),
                    );
                }
                None => {
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                }
            }
            return Ok(());
        }
    }

    // 栈级别模式
    let level = arg1.as_integer().unwrap_or(0) as i32;

    if level == 0 {
        // C 临时变量 (简化实现)
        if nvar == 1 {
            push_results(state, a, nresults, vec![
                TValue::Str(state.intern_str("(C temporary)")),
                TValue::Integer(0),
            ]);
        } else if nvar == 2 {
            push_results(state, a, nresults, vec![
                TValue::Str(state.intern_str("(C temporary)")),
                TValue::Integer(2),
            ]);
        } else {
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
        }
        return Ok(());
    }

    if level == 1 {
        // 当前 Lua 帧
        if state.base == 0 || state.base > state.stack.len() {
            return Err(VmError::RuntimeError(
                "level out of range".to_string(),
            ));
        }
        if let TValue::LClosure(closure) = &state.stack[state.base - 1].clone() {
            let proto = &closure.proto;
            let pc = state.pc;

            // 处理负数索引 (vararg)
            if nvar < 0 {
                // vararg 访问 — 简化实现
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                return Ok(());
            }

            // 正数索引: 局部变量
            let name = get_local_name(proto, nvar as usize, pc);
            match name {
                Some(n) => {
                    // 获取栈上的值
                    let stack_idx = state.base + (nvar as usize) - 1;
                    let val = if stack_idx < state.stack.len() {
                        state.stack[stack_idx].clone()
                    } else {
                        TValue::Nil(NilKind::Strict)
                    };
                    push_results(state, a, nresults, vec![
                        TValue::Str(state.intern_str(&n)),
                        val,
                    ]);
                }
                None => {
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                }
            }
            return Ok(());
        }
    }

    // 超出范围
    Err(VmError::RuntimeError("level out of range".to_string()))
}

/// debug.setlocal([thread,] level, local, value) — 对应 C 的 db_setlocal
///
/// 设置局部变量值, 返回变量名
fn call_setlocal(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let mut arg_offset = 0;
    let arg0 = get_arg(state, a, 0);
    if matches!(arg0, TValue::Thread(_)) {
        arg_offset = 1;
    }

    let level = get_arg(state, a, arg_offset)
        .as_integer()
        .unwrap_or(0) as i32;
    let nvar = get_arg(state, a, arg_offset + 1)
        .as_integer()
        .unwrap_or(0) as i32;
    let value = get_arg(state, a, arg_offset + 2);

    if level == 1 {
        if state.base == 0 || state.base > state.stack.len() {
            return Err(VmError::RuntimeError(
                "level out of range".to_string(),
            ));
        }
        if let TValue::LClosure(closure) = &state.stack[state.base - 1].clone() {
            let proto = &closure.proto;
            let pc = state.pc;

            if nvar < 0 {
                // vararg — 简化实现
                push_single_result(state, a, nresults, TValue::Str(state.intern_str("(vararg)")));
                return Ok(());
            }

            let name = get_local_name(proto, nvar as usize, pc);
            match name {
                Some(n) => {
                    // 设置栈上的值
                    let stack_idx = state.base + (nvar as usize) - 1;
                    if stack_idx < state.stack.len() {
                        state.stack[stack_idx] = value;
                    }
                    push_single_result(state, a, nresults, TValue::Str(state.intern_str(&n)));
                }
                None => {
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                }
            }
            return Ok(());
        }
    }

    Err(VmError::RuntimeError("level out of range".to_string()))
}

/// debug.getupvalue(f, n) — 对应 C 的 db_getupvalue
///
/// 返回上值名和值
fn call_getupvalue(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg1 = get_arg(state, a, 0);
    let n = get_arg(state, a, 1)
        .as_integer()
        .unwrap_or(0) as usize;

    if n == 0 {
        push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
        return Ok(());
    }

    match &arg1 {
        TValue::LClosure(closure) => {
            if n > 0 && n <= closure.upvals.len() {
                let uv_ref = closure.upvals[n - 1].borrow();
                let val = match &*uv_ref {
                    UpVal::Closed { value } => (**value).clone(),
                    UpVal::Open { stack_index, .. } => {
                        if *stack_index < state.stack.len() {
                            state.stack[*stack_index].clone()
                        } else {
                            TValue::Nil(NilKind::Strict)
                        }
                    }
                };
                // 获取上值名
                let name = closure
                    .proto
                    .upvalues
                    .get(n - 1)
                    .and_then(|u| u.name.as_ref())
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_default();
                push_results(state, a, nresults, vec![
                    TValue::Str(state.intern_str(&name)),
                    val,
                ]);
            } else {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            }
            Ok(())
        }
        TValue::CClosure(cc) => {
            // C 闭包的上值名始终为空字符串
            if n > 0 && n <= cc.upvalue.len() {
                push_results(state, a, nresults, vec![
                    TValue::Str(state.intern_str("")),
                    cc.upvalue[n - 1].clone(),
                ]);
            } else {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            }
            Ok(())
        }
        TValue::LightUserData(_) => {
            // 轻量 C 函数没有上值
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            Ok(())
        }
        _ => Err(VmError::RuntimeError(
            "bad argument #1 to 'getupvalue' (function expected)".to_string(),
        )),
    }
}

/// debug.setupvalue(f, n, v) — 对应 C 的 db_setupvalue
///
/// 设置上值, 返回上值名
fn call_setupvalue(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg1 = get_arg(state, a, 0);
    let n = get_arg(state, a, 1)
        .as_integer()
        .unwrap_or(0) as usize;
    let value = get_arg(state, a, 2);

    if n == 0 {
        push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
        return Ok(());
    }

    match &arg1 {
        TValue::LClosure(closure) => {
            if n > 0 && n <= closure.upvals.len() {
                // 获取上值名
                let name = closure
                    .proto
                    .upvalues
                    .get(n - 1)
                    .and_then(|u| u.name.as_ref())
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_default();

                // 设置上值
                // 由于 upvals 是 Rc<RefCell>, 我们需要可变访问
                // 但 arg1 是 clone 的, 我们需要修改栈上的原始闭包
                if a + 1 < state.stack.len() {
                    if let TValue::LClosure(ref mut cl) = state.stack[a + 1] {
                        if n <= cl.upvals.len() {
                            // 先取出 stack_index (如果是 Open), 然后释放 borrow, 再修改 stack
                            let action = {
                                let mut uv_ref = cl.upvals[n - 1].borrow_mut();
                                match &mut *uv_ref {
                                    UpVal::Closed { value: val } => {
                                        **val = value.clone();
                                        None
                                    }
                                    UpVal::Open { stack_index, .. } => {
                                        Some(*stack_index)
                                    }
                                }
                            };
                            if let Some(idx) = action {
                                if idx < state.stack.len() {
                                    state.stack[idx] = value.clone();
                                }
                            }
                        }
                    }
                }
                push_single_result(state, a, nresults, TValue::Str(state.intern_str(&name)));
            } else {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            }
            Ok(())
        }
        TValue::CClosure(_) => {
            // C 闭包
            if a + 1 < state.stack.len() {
                if let TValue::CClosure(ref mut cc) = state.stack[a + 1] {
                    if n > 0 && n <= cc.upvalue.len() {
                        cc.upvalue[n - 1] = value;
                        push_single_result(state, a, nresults, TValue::Str(state.intern_str("")));
                        return Ok(());
                    }
                }
            }
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            Ok(())
        }
        _ => Err(VmError::RuntimeError(
            "bad argument #1 to 'setupvalue' (function expected)".to_string(),
        )),
    }
}

/// debug.upvalueid(f, n) — 对应 C 的 db_upvalueid
///
/// 返回上值的唯一标识 (light userdata)
fn call_upvalueid(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let arg1 = get_arg(state, a, 0);
    let n = get_arg(state, a, 1)
        .as_integer()
        .unwrap_or(0) as usize;

    match &arg1 {
        TValue::LClosure(closure) => {
            if n > 0 && n <= closure.upvals.len() {
                // 使用 Rc 的指针作为唯一标识
                let ptr = Rc::as_ptr(&closure.upvals[n - 1]) as *mut std::ffi::c_void;
                push_single_result(state, a, nresults, TValue::LightUserData(ptr));
            } else {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            }
            Ok(())
        }
        TValue::CClosure(cc) => {
            if n > 0 && n <= cc.upvalue.len() {
                // 使用 upvalue 的引用地址
                let ptr = &cc.upvalue[n - 1] as *const TValue as *mut std::ffi::c_void;
                push_single_result(state, a, nresults, TValue::LightUserData(ptr));
            } else {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            }
            Ok(())
        }
        _ => {
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            Ok(())
        }
    }
}

/// debug.upvaluejoin(f1, n1, f2, n2) — 对应 C 的 db_upvaluejoin
///
/// 让 f1 的第 n1 个上值共享 f2 的第 n2 个上值
fn call_upvaluejoin(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let f1 = get_arg(state, a, 0);
    let n1 = get_arg(state, a, 1).as_integer().unwrap_or(0) as usize;
    let f2 = get_arg(state, a, 2);
    let n2 = get_arg(state, a, 3).as_integer().unwrap_or(0) as usize;

    if let (TValue::LClosure(_), TValue::LClosure(_)) = (&f1, &f2) {
        // 获取 f2 的上值引用
        let f2_upval = if let TValue::LClosure(c2) = &f2 {
            if n2 > 0 && n2 <= c2.upvals.len() {
                Some(c2.upvals[n2 - 1].clone())
            } else {
                None
            }
        } else {
            None
        };

        if let Some(upval) = f2_upval {
            // 设置到 f1 的上值
            if a + 1 < state.stack.len() {
                if let TValue::LClosure(ref mut c1) = state.stack[a + 1] {
                    if n1 > 0 && n1 <= c1.upvals.len() {
                        c1.upvals[n1 - 1] = upval;
                    }
                }
            }
        }
    }

    // 不返回结果
    push_results(state, a, nresults, vec![]);
    Ok(())
}

/// debug.sethook([thread,] hook, mask, [count]) — 对应 C 的 db_sethook
///
/// 设置钩子函数
fn call_sethook(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let mut arg_offset = 0;
    let arg0 = get_arg(state, a, 0);
    if matches!(arg0, TValue::Thread(_)) {
        arg_offset = 1;
    }

    let hook = get_arg(state, a, arg_offset);

    if matches!(hook, TValue::Nil(_)) || matches!(hook, TValue::Nil(NilKind::Empty)) {
        // 关闭钩子
        set_hook_in_registry(state, None, 0, 0);
    } else {
        let mask_str = match &get_arg(state, a, arg_offset + 1) {
            TValue::Str(s) => s.as_str().to_string(),
            _ => String::new(),
        };
        let count = if nargs > arg_offset + 2 {
            get_arg(state, a, arg_offset + 2).as_integer().unwrap_or(0) as i32
        } else {
            0
        };
        let mask = make_mask(&mask_str, count);
        set_hook_in_registry(state, Some(hook), mask, count);
    }

    push_results(state, a, nresults, vec![]);
    Ok(())
}

/// debug.gethook([thread]) — 对应 C 的 db_gethook
///
/// 返回当前钩子函数、掩码字符串和计数
fn call_gethook(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let mut arg_offset = 0;
    let arg0 = get_arg(state, a, 0);
    if matches!(arg0, TValue::Thread(_)) {
        arg_offset = 1;
    }

    // 从注册表获取 hook 表
    let hookkey = TValue::Str(state.intern_str(HOOKKEY));
    let hook_table = match state.registry.get(&hookkey) {
        Some(TValue::Table(t)) => t.clone(),
        _ => {
            // 没有钩子
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            return Ok(());
        }
    };

    // 获取当前线程的钩子 (简化: 使用固定键)
    let thread_key = TValue::Integer(1); // 简化: 使用 1 作为当前线程的键
    let hook_fn = hook_table.get(&thread_key);

    match hook_fn {
        Some(f) if !matches!(f, TValue::Nil(_)) => {
            // 获取掩码和计数 (存储在另一个表中)
            let maskkey = TValue::Str(state.intern_str("_mask"));
            let countkey = TValue::Str(state.intern_str("_count"));
            let mask = hook_table
                .get(&maskkey)
                .and_then(|v| v.as_integer())
                .unwrap_or(0) as i32;
            let count = hook_table
                .get(&countkey)
                .and_then(|v| v.as_integer())
                .unwrap_or(0) as i32;
            let mask_str = unmake_mask(mask);
            push_results(state, a, nresults, vec![
                f.clone(),
                TValue::Str(state.intern_str(&mask_str)),
                TValue::Integer(count as i64),
            ]);
        }
        _ => {
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
        }
    }
    Ok(())
}

/// 在注册表中设置钩子
fn set_hook_in_registry(state: &mut LuaState, hook: Option<TValue>, mask: i32, count: i32) {
    let hookkey = TValue::Str(state.intern_str(HOOKKEY));

    // 获取或创建 hook 表
    let mut hook_table = match state.registry.get(&hookkey) {
        Some(TValue::Table(t)) => t.clone(),
        _ => {
            // 创建新表并设置元表 (__mode = "k")
            let mut t = Table::new();
            let mut mt = Table::new();
            mt.set(
                TValue::Str(state.intern_str("__mode")),
                TValue::Str(state.intern_str("k")),
            );
            t.metatable = Some(Box::new(mt));
            t
        }
    };

    // 存储钩子函数 (使用固定键代表当前线程)
    let thread_key = TValue::Integer(1);
    match &hook {
        Some(f) => {
            hook_table.set(thread_key, f.clone());
        }
        None => {
            hook_table.set(thread_key, TValue::Nil(NilKind::Strict));
        }
    }

    // 存储掩码和计数
    hook_table.set(
        TValue::Str(state.intern_str("_mask")),
        TValue::Integer(mask as i64),
    );
    hook_table.set(
        TValue::Str(state.intern_str("_count")),
        TValue::Integer(count as i64),
    );

    state.registry.set(hookkey, TValue::Table(hook_table));

    // 同时设置 state 的 hook 字段，供 VM 执行循环快速访问
    state.hook_func = hook;
    state.hook_mask = mask;
    state.hook_count = count;
    // 不修改 hook_old_pc — 对应 C 的 sethook 不修改 oldpc
    // oldpc 只在 luaD_hookcall（函数调用）和 rethook（函数返回）中被修改
}

/// debug.traceback([thread,] [message [, level]]) — 对应 C 的 db_traceback
///
/// 返回堆栈回溯字符串
fn call_traceback(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let mut arg_offset = 0;
    let arg0 = get_arg(state, a, 0);
    if matches!(arg0, TValue::Thread(_)) {
        arg_offset = 1;
    }

    let msg_val = get_arg(state, a, arg_offset);
    let level = if nargs > arg_offset + 1 {
        get_arg(state, a, arg_offset + 1)
            .as_integer()
            .unwrap_or(1) as i32
    } else {
        1
    };

    // 如果 msg 不是字符串且不是 nil, 直接返回
    if !matches!(msg_val, TValue::Str(_) | TValue::Nil(_)) {
        push_single_result(state, a, nresults, msg_val);
        return Ok(());
    }

    let msg = match &msg_val {
        TValue::Str(s) => s.as_str().to_string(),
        _ => String::new(),
    };

    // 构建回溯
    let traceback = build_traceback(state, &msg, level);
    push_single_result(state, a, nresults, TValue::Str(state.intern_str(&traceback)));
    Ok(())
}

/// 构建堆栈回溯字符串 — 对应 C 的 luaL_traceback
fn build_traceback(state: &LuaState, msg: &str, level: i32) -> String {
    let mut result = String::new();
    if !msg.is_empty() {
        result.push_str(msg);
        result.push('\n');
    }
    result.push_str("stack traceback:");

    // 从 call_info 构建
    if state.call_info.is_empty() {
        // 使用当前帧
        let (source, line) = get_current_source_line(state);
        if line > 0 {
            result.push_str(&format!("\n\t{}:{}: in main chunk", source, line));
        } else {
            result.push_str("\n\t[C]: in ?");
        }
    } else {
        let start = level as usize;
        for (i, entry) in state.call_info.iter().enumerate() {
            if i < start {
                continue;
            }
            result.push('\n');
            result.push('\t');
            if entry.is_c {
                result.push_str("[C]: in ");
                result.push_str(&entry.name);
            } else {
                if entry.line > 0 {
                    result.push_str(&format!("{}:{}: in ", entry.source, entry.line));
                } else {
                    result.push_str(&format!("{}: in ", entry.source));
                }
                result.push_str(&entry.name);
            }
        }
    }
    result.push_str("\n\t[C]: in ?");
    result
}

/// debug.debug() — 对应 C 的 db_debug
///
/// 交互式调试器 (简化实现)
fn call_debug(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 简化实现: 不进入交互模式, 直接返回
    push_results(state, a, nresults, vec![]);
    Ok(())
}

// ============================================================================
// 打开调试库 — 对应 C 的 luaopen_debug
// ============================================================================

/// 创建调试库表
///
/// 对应 C 源码 ldblib.cpp 的 luaopen_debug 函数
pub fn create_debug_lib_table(state: &LuaState) -> Table {
    let mut lib = Table::new();

    let register = |lib: &mut Table, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        lib.set(key, TValue::LightUserData(tag as *mut std::ffi::c_void));
    };

    register(&mut lib, "debug", DEBUG_DEBUG);
    register(&mut lib, "getuservalue", DEBUG_GETUSERVALUE);
    register(&mut lib, "gethook", DEBUG_GETHOOK);
    register(&mut lib, "getinfo", DEBUG_GETINFO);
    register(&mut lib, "getlocal", DEBUG_GETLOCAL);
    register(&mut lib, "getregistry", DEBUG_GETREGISTRY);
    register(&mut lib, "getmetatable", DEBUG_GETMETATABLE);
    register(&mut lib, "getupvalue", DEBUG_GETUPVALUE);
    register(&mut lib, "upvaluejoin", DEBUG_UPVALUEJOIN);
    register(&mut lib, "upvalueid", DEBUG_UPVALUEID);
    register(&mut lib, "setuservalue", DEBUG_SETUSERVALUE);
    register(&mut lib, "sethook", DEBUG_SETHOOK);
    register(&mut lib, "setlocal", DEBUG_SETLOCAL);
    register(&mut lib, "setmetatable", DEBUG_SETMETATABLE);
    register(&mut lib, "setupvalue", DEBUG_SETUPVALUE);
    register(&mut lib, "traceback", DEBUG_TRACEBACK);

    lib
}

/// 打开调试库并注册到全局变量 debug
pub fn open_debug_lib(state: &mut LuaState) {
    let lib = create_debug_lib_table(state);
    let key = TValue::Str(state.intern_str("debug"));
    state.globals.set(key, TValue::Table(lib));
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_debug_tag() {
        assert!(is_debug_tag(500));
        assert!(is_debug_tag(519));
        assert!(!is_debug_tag(499));
        assert!(!is_debug_tag(520));
        assert!(!is_debug_tag(100));
    }

    #[test]
    fn test_debug_function_name() {
        assert_eq!(debug_function_name(DEBUG_DEBUG), Some("debug"));
        assert_eq!(debug_function_name(DEBUG_GETINFO), Some("getinfo"));
        assert_eq!(debug_function_name(DEBUG_TRACEBACK), Some("traceback"));
        assert_eq!(debug_function_name(999), None);
    }

    #[test]
    fn test_make_mask() {
        assert_eq!(make_mask("c", 0), LUA_MASKCALL);
        assert_eq!(make_mask("r", 0), LUA_MASKRET);
        assert_eq!(make_mask("l", 0), LUA_MASKLINE);
        assert_eq!(make_mask("crl", 0), LUA_MASKCALL | LUA_MASKRET | LUA_MASKLINE);
        assert_eq!(make_mask("", 1), LUA_MASKCOUNT);
        assert_eq!(make_mask("c", 1), LUA_MASKCALL | LUA_MASKCOUNT);
    }

    #[test]
    fn test_unmake_mask() {
        assert_eq!(unmake_mask(LUA_MASKCALL), "c");
        assert_eq!(unmake_mask(LUA_MASKRET), "r");
        assert_eq!(unmake_mask(LUA_MASKLINE), "l");
        assert_eq!(unmake_mask(LUA_MASKCALL | LUA_MASKRET), "cr");
        assert_eq!(unmake_mask(LUA_MASKCALL | LUA_MASKRET | LUA_MASKLINE), "crl");
        assert_eq!(unmake_mask(0), "");
    }

    #[test]
    fn test_open_debug_lib_registers_table() {
        let mut state = LuaState::new();
        open_debug_lib(&mut state);

        let key = TValue::Str(state.intern_str("debug"));
        let val = state.globals.get(&key);
        assert!(val.is_some(), "debug must be registered");
        assert!(matches!(val, Some(TValue::Table(_))));

        if let Some(TValue::Table(t)) = val {
            // 验证所有函数都已注册
            for name in &[
                "debug", "getuservalue", "gethook", "getinfo", "getlocal",
                "getregistry", "getmetatable", "getupvalue", "upvaluejoin",
                "upvalueid", "setuservalue", "sethook", "setlocal",
                "setmetatable", "setupvalue", "traceback",
            ] {
                let key = TValue::Str(state.intern_str(name));
                assert!(t.get(&key).is_some(), "{} must be registered", name);
            }
        }
    }

    #[test]
    fn test_call_getregistry() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETREGISTRY as *mut std::ffi::c_void));
        call_debug_function(DEBUG_GETREGISTRY, &mut state, 0, 0, 1).unwrap();
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(state.stack[0], TValue::Table(_)));
    }

    #[test]
    fn test_call_getmetatable_table() {
        let mut state = LuaState::new();
        let mut t = Table::new();
        t.metatable = Some(Box::new(Table::new()));
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETMETATABLE as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        call_debug_function(DEBUG_GETMETATABLE, &mut state, 0, 1, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Table(_)));
    }

    #[test]
    fn test_call_getmetatable_no_metatable() {
        let mut state = LuaState::new();
        let t = Table::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETMETATABLE as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        call_debug_function(DEBUG_GETMETATABLE, &mut state, 0, 1, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_setmetatable() {
        let mut state = LuaState::new();
        let t = Table::new();
        let mt = Table::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_SETMETATABLE as *mut std::ffi::c_void));
        state.stack.push(TValue::Table(t));
        state.stack.push(TValue::Table(mt));
        call_debug_function(DEBUG_SETMETATABLE, &mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Table(t) => assert!(t.metatable.is_some()),
            _ => panic!("expected table result"),
        }
    }

    #[test]
    fn test_call_getuservalue_non_userdata() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETUSERVALUE as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(42));
        call_debug_function(DEBUG_GETUSERVALUE, &mut state, 0, 1, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_getupvalue_closure() {
        use crate::gc::GCObjectHeader;
        use crate::objects::UpvalDesc;
        use crate::strings::LuaString;
        use std::sync::Arc;

        let mut state = LuaState::new();
        let proto = Proto {
            num_params: 0,
            flag: 0,
            max_stack_size: 2,
            size_upvalues: 1,
            size_k: 0,
            size_code: 0,
            size_line_info: 0,
            size_p: 0,
            size_loc_vars: 0,
            size_abs_line_info: 0,
            line_defined: 0,
            last_line_defined: 0,
            constants: vec![],
            code: vec![],
            protos: vec![],
            upvalues: vec![UpvalDesc {
                name: Some(LuaString::Short(Arc::new(crate::strings::ShortString {
                    hash: 0,
                    contents: "x".to_string(),
                }))),
                in_stack: false,
                idx: 0,
                parent_local_idx: 0,
            }],
            line_info: vec![],
            abs_line_info: vec![],
            loc_vars: vec![],
            source: None,
        };
        let closure = LClosure {
            gc_header: GCObjectHeader::new(),
            proto,
            upvals: vec![Rc::new(RefCell::new(UpVal::Closed {
                value: Box::new(TValue::Integer(42)),
            }))],
        };
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETUPVALUE as *mut std::ffi::c_void));
        state.stack.push(TValue::LClosure(closure));
        state.stack.push(TValue::Integer(1));
        call_debug_function(DEBUG_GETUPVALUE, &mut state, 0, 2, 2).unwrap();
        assert_eq!(state.stack.len(), 2);
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "x"),
            _ => panic!("expected string 'x'"),
        }
        match &state.stack[1] {
            TValue::Integer(n) => assert_eq!(*n, 42),
            _ => panic!("expected integer 42"),
        }
    }

    #[test]
    fn test_call_getupvalue_out_of_range() {
        use crate::gc::GCObjectHeader;
        let mut state = LuaState::new();
        let closure = LClosure {
            gc_header: GCObjectHeader::new(),
            proto: crate::func::new_proto(),
            upvals: vec![],
        };
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETUPVALUE as *mut std::ffi::c_void));
        state.stack.push(TValue::LClosure(closure));
        state.stack.push(TValue::Integer(1));
        call_debug_function(DEBUG_GETUPVALUE, &mut state, 0, 2, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_gethook_no_hook() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETHOOK as *mut std::ffi::c_void));
        call_debug_function(DEBUG_GETHOOK, &mut state, 0, 0, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_sethook_and_gethook() {
        let mut state = LuaState::new();
        // 设置钩子
        let hook_fn = TValue::LightUserData(999 as *mut std::ffi::c_void);
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_SETHOOK as *mut std::ffi::c_void));
        state.stack.push(hook_fn.clone());
        state.stack.push(TValue::Str(state.intern_str("crl")));
        state.stack.push(TValue::Integer(0));
        call_debug_function(DEBUG_SETHOOK, &mut state, 0, 3, 0).unwrap();

        // 获取钩子
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETHOOK as *mut std::ffi::c_void));
        call_debug_function(DEBUG_GETHOOK, &mut state, 0, 0, 3).unwrap();
        assert_eq!(state.stack.len(), 3);
        assert_eq!(state.stack[0], hook_fn);
        match &state.stack[1] {
            TValue::Str(s) => assert_eq!(s.as_str(), "crl"),
            _ => panic!("expected mask string 'crl'"),
        }
        match &state.stack[2] {
            TValue::Integer(n) => assert_eq!(*n, 0),
            _ => panic!("expected count 0"),
        }
    }

    #[test]
    fn test_call_sethook_nil_clears() {
        let mut state = LuaState::new();
        // 先设置钩子
        let hook_fn = TValue::LightUserData(999 as *mut std::ffi::c_void);
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_SETHOOK as *mut std::ffi::c_void));
        state.stack.push(hook_fn);
        state.stack.push(TValue::Str(state.intern_str("l")));
        call_debug_function(DEBUG_SETHOOK, &mut state, 0, 2, 0).unwrap();

        // 用 nil 清除
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_SETHOOK as *mut std::ffi::c_void));
        state.stack.push(TValue::Nil(NilKind::Strict));
        call_debug_function(DEBUG_SETHOOK, &mut state, 0, 1, 0).unwrap();

        // 验证已清除
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETHOOK as *mut std::ffi::c_void));
        call_debug_function(DEBUG_GETHOOK, &mut state, 0, 0, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_traceback_empty() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_TRACEBACK as *mut std::ffi::c_void));
        call_debug_function(DEBUG_TRACEBACK, &mut state, 0, 0, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert!(s.as_str().starts_with("stack traceback:")),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_traceback_with_message() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_TRACEBACK as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("error message")));
        call_debug_function(DEBUG_TRACEBACK, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => {
                let s = s.as_str();
                assert!(s.starts_with("error message\n"));
                assert!(s.contains("stack traceback:"));
            }
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_traceback_non_string_msg() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_TRACEBACK as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(42));
        call_debug_function(DEBUG_TRACEBACK, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 42),
            _ => panic!("expected integer 42 (returned untouched)"),
        }
    }

    #[test]
    fn test_call_getinfo_out_of_range() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETINFO as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(1000));
        call_debug_function(DEBUG_GETINFO, &mut state, 0, 1, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_getinfo_negative_level() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETINFO as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(-1));
        call_debug_function(DEBUG_GETINFO, &mut state, 0, 1, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_getlocal_out_of_range() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_GETLOCAL as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(20));
        state.stack.push(TValue::Integer(1));
        let result = call_debug_function(DEBUG_GETLOCAL, &mut state, 0, 2, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_short_src() {
        use crate::strings::LuaString;
        use std::sync::Arc;

        let make_str = |s: &str| -> LuaString {
            LuaString::Short(Arc::new(crate::strings::ShortString {
                hash: 0,
                contents: s.to_string(),
            }))
        };

        // 等号前缀
        let s = make_str("=test.lua");
        assert_eq!(short_src(&s), "test.lua");

        // @ 前缀
        let s = make_str("@test.lua");
        assert_eq!(short_src(&s), "test.lua");

        // 短字符串
        let s = make_str("print(1)");
        assert_eq!(short_src(&s), "[string \"print(1)\"]");

        // 长字符串
        let long_str = "a".repeat(50);
        let s = make_str(&long_str);
        let result = short_src(&s);
        assert!(result.starts_with("[string \""));
        assert!(result.ends_with("...\"]"));
    }

    #[test]
    fn test_get_proto_line_empty() {
        let proto = crate::func::new_proto();
        assert_eq!(get_proto_line(&proto, 0), -1);
    }

    #[test]
    fn test_get_local_name_empty() {
        let proto = crate::func::new_proto();
        assert!(get_local_name(&proto, 1, 0).is_none());
    }

    #[test]
    fn test_call_upvalueid_closure() {
        use crate::gc::GCObjectHeader;
        let mut state = LuaState::new();
        let closure = LClosure {
            gc_header: GCObjectHeader::new(),
            proto: crate::func::new_proto(),
            upvals: vec![Rc::new(RefCell::new(UpVal::Closed {
                value: Box::new(TValue::Integer(42)),
            }))],
        };
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_UPVALUEID as *mut std::ffi::c_void));
        state.stack.push(TValue::LClosure(closure));
        state.stack.push(TValue::Integer(1));
        call_debug_function(DEBUG_UPVALUEID, &mut state, 0, 2, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::LightUserData(_)));
    }

    #[test]
    fn test_call_upvalueid_out_of_range() {
        use crate::gc::GCObjectHeader;
        let mut state = LuaState::new();
        let closure = LClosure {
            gc_header: GCObjectHeader::new(),
            proto: crate::func::new_proto(),
            upvals: vec![],
        };
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_UPVALUEID as *mut std::ffi::c_void));
        state.stack.push(TValue::LClosure(closure));
        state.stack.push(TValue::Integer(1));
        call_debug_function(DEBUG_UPVALUEID, &mut state, 0, 2, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_debug_returns_nothing() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(DEBUG_DEBUG as *mut std::ffi::c_void));
        call_debug_function(DEBUG_DEBUG, &mut state, 0, 0, 0).unwrap();
        assert_eq!(state.stack.len(), 0);
    }

    #[test]
    fn test_call_unknown_tag() {
        let mut state = LuaState::new();
        let result = call_debug_function(999, &mut state, 0, 0, 0);
        assert!(result.is_err());
    }
}
