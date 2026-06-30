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

use crate::objects::{NilKind, TValue, LClosure, Proto, UpVal, UpValRef, PF_VAHID};
use crate::state::LuaState;
use crate::table::Table;
use crate::execute::VmError;
use crate::strings::LuaString;
use crate::tm::Metatable;
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
    state.adjust_results(a, nresults, results);
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
    let mut n = local_number as i32;
    for loc_var in &proto.loc_vars {
        if (loc_var.start_pc as usize) <= pc && pc < (loc_var.end_pc as usize) {
            n -= 1;
            if n == 0 {
                if let Some(ref name) = loc_var.varname {
                    return Some(name.as_str().to_string());
                }
                return None;
            }
        }
    }
    None
}

/// 栈帧信息 — 用于 debug.getlocal/setlocal 的 level > 1 支持
struct FrameInfo {
    base: usize,
    pc: usize,
    proto_flag: u8,
    nextraargs: i32,
    /// 指向栈上的 LClosure（已 clone）；C 函数帧为 None
    closure: Option<LClosure>,
    /// 栈上有效槽位的上限（对应 C 的 limit = ci->next->func.p 或 L->top）
    /// 槽位 n 满足 limit - base >= n && n > 0 时为 "(temporary)" / "(C temporary)"
    limit: usize,
    /// 是否为 C 函数帧
    is_c: bool,
}

/// 获取指定 level 的栈帧信息
///
/// level 1 = 当前函数, level 2 = 调用者, ...
/// 返回 None 表示 level 超出范围
fn get_frame_info(state: &LuaState, level: i32) -> Option<FrameInfo> {
    if level < 1 {
        return None;
    }
    if level == 1 {
        // 当前函数
        if state.base == 0 || state.base > state.stack.len() {
            return None;
        }
        let limit = state.stack.len();
        if let TValue::LClosure(closure) = &state.stack[state.base - 1] {
            return Some(FrameInfo {
                base: state.base,
                pc: state.pc,
                proto_flag: state.proto_flag,
                nextraargs: state.nextraargs,
                closure: Some(closure.clone()),
                limit,
                is_c: false,
            });
        }
        // C 函数帧
        return Some(FrameInfo {
            base: state.base,
            pc: 0,
            proto_flag: 0,
            nextraargs: 0,
            closure: None,
            limit,
            is_c: true,
        });
    }
    // level >= 2: 从 call_info 获取调用者信息
    // 当最后一个 call_info 条目是 C 函数时，它代表当前正在执行的 C 函数（level 0），
    // 需要跳过它来计算 level >= 2 的索引。
    let c_func_offset = if state
        .call_info
        .last()
        .map(|e| e.is_c)
        .unwrap_or(false)
    {
        1
    } else {
        0
    };
    let idx = state
        .call_info
        .len()
        .checked_sub((level as usize).saturating_sub(1) + c_func_offset)?;
    let entry = &state.call_info[idx];
    if entry.base == 0 || entry.base > state.stack.len() {
        return None;
    }
    // 计算 limit: 对应 C 的 ci->next->func.p
    let limit = if level == 2 {
        // 被调用者是当前函数 (level 1), func.p = state.base - 1
        state.base.saturating_sub(1)
    } else {
        // 被调用者是 level-1, 其 call_info 条目在 idx+1
        let callee_entry = &state.call_info[idx + 1];
        callee_entry.base.saturating_sub(1)
    };
    if let TValue::LClosure(closure) = &state.stack[entry.base - 1] {
        return Some(FrameInfo {
            base: entry.base,
            pc: entry.saved_pc,
            proto_flag: entry.proto_flag,
            nextraargs: entry.nextraargs,
            closure: Some(closure.clone()),
            limit,
            is_c: false,
        });
    }
    // C 函数帧 — 对应 C 的 CIST_C
    Some(FrameInfo {
        base: entry.base,
        pc: entry.saved_pc,
        proto_flag: entry.proto_flag,
        nextraargs: entry.nextraargs,
        closure: None,
        limit,
        is_c: true,
    })
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
            if let Some(mt) = t.get_metatable() {
                TValue::Table(mt)
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
        // 基本类型: 从全局 G(L)->mt[type] 读取
        _ => match state.dmt.get(arg.ty()) {
            Some(mt) => TValue::Table(mt.clone()),
            None => TValue::Nil(NilKind::Strict),
        },
    };
    push_single_result(state, a, nresults, result);
    Ok(())
}

/// debug.setmetatable(v, mt) — 对应 C 的 db_setmetatable → lua_setmetatable
///
/// 设置值的元表, 返回原值。对 Table/UserData 设置自身元表;
/// 对基本类型(number/boolean/nil/string)设置全局 G(L)->mt[type]。
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

    match (&arg1, &arg2) {
        (TValue::Table(_), TValue::Table(mt)) => {
            // 修改栈上的表
            if a + 1 < state.stack.len() {
                if let TValue::Table(ref mut t) = state.stack[a + 1] {
                    t.set_metatable(Some(mt.clone()));
                }
            }
            // 检查 __mode 弱引用表
            if let TValue::Table(ref t) = arg1 {
                let has_mode = {
                    let data = t.data.borrow();
                    if let Some(ref mt) = data.metatable {
                        let mode_key = TValue::Str(state.intern_str("__mode"));
                        mt.get(&mode_key).is_some()
                    } else {
                        false
                    }
                };
                if has_mode {
                    state.register_weak_table(t);
                }
            }
        }
        (TValue::Table(_), TValue::Nil(_)) => {
            if a + 1 < state.stack.len() {
                if let TValue::Table(ref mut t) = state.stack[a + 1] {
                    t.set_metatable(None);
                }
            }
        }
        // 基本类型: 设置全局 mt[type] — 对应 C 的 G(L)->mt[ttype(obj)]
        (_, TValue::Table(mt)) => {
            let ty = arg1.ty();
            state.dmt.set(ty, Metatable::new(mt.clone()));
        }
        (_, TValue::Nil(_)) => {
            let ty = arg1.ty();
            state.dmt.clear(ty);
        }
        _ => unreachable!(),
    }
    push_single_result(state, a, nresults, arg1);
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
    let co_thread = if let TValue::Thread(t) = &arg0 {
        arg_offset = 1;
        Some(t.clone())
    } else {
        None
    };

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
        info.nparams = 0;
        info.isvararg = true;
        info.name = None;
        info.namewhat = String::new();
        info.nups = 0;
    } else if let TValue::Table(t) = &level_or_func {
        // 可调用表 (带 __call 元方法的表)
        // string.gmatch 返回的迭代器在 C 中是带 3 个上值的 C 闭包
        // (字符串、模式、userdata 状态)，Rust 版本用带 __call 的表模拟
        let is_gmatch_iter = t
            .get_metatable()
            .and_then(|mt| mt.get(&TValue::Str(state.intern_str("__call"))))
            .map(|v| {
                if let TValue::LightUserData(p) = &v {
                    *p as usize == crate::stdlib::string_lib::STR_GMATCH_ITER
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if is_gmatch_iter {
            info.what = "C".to_string();
            info.short_src = "[C]".to_string();
            info.source = "=[C]".to_string();
            info.currentline = -1;
            info.nparams = 0;
            info.isvararg = true;
            info.name = None;
            info.namewhat = String::new();
            info.nups = 3;
        } else {
            // 非可调用表,按栈级别处理
            let level = level_or_func.as_integer().unwrap_or(0) as i32;
            if level < 0 {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                return Ok(());
            }
            let filled = if let Some(thread) = &co_thread {
                let ctx = thread.context.borrow();
                fill_info_from_thread(&ctx, &mut info, level, &what)
            } else {
                fill_info_from_level(state, &mut info, level, &what)
            };
            if !filled {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                return Ok(());
            }
        }
    } else {
        // 栈级别模式
        let level = level_or_func.as_integer().unwrap_or(0) as i32;
        if level < 0 {
            // 超出范围
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            return Ok(());
        }
        let filled = if let Some(thread) = &co_thread {
            let ctx = thread.context.borrow();
            fill_info_from_thread(&ctx, &mut info, level, &what)
        } else {
            fill_info_from_level(state, &mut info, level, &what)
        };
        if !filled {
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
        // level 模式下使用 info.func (由 fill_info_from_level 设置)
        // 函数参数模式下从栈上获取原始函数值 (保持引用语义)
        if let Some(func_val) = info.func.take() {
            result_table.set(
                TValue::Str(state.intern_str("func")),
                func_val,
            );
        } else {
            // 函数参数模式: 从栈上获取原始函数值
            // 对应 C 的 lua_pushvalue(L, arg + 1)
            let func_idx = a + 1 + arg_offset;
            if func_idx < state.stack.len() {
                let func_val = state.stack[func_idx].clone();
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
        info.what = if proto.line_defined == 0 { "main" } else { "Lua" }.to_string();
    }
    if what.contains('l') {
        info.currentline = -1; // 函数参数模式没有当前行
    }
    if what.contains('u') {
        info.nups = closure.upvals.borrow().len();
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
        // 函数参数模式: ci == NULL, ftransfer/ntransfer 始终为 0
        // 对应 C: if (ci == NULL || !(ci->callstatus & CIST_HOOKED)) ar->ftransfer = ar->ntransfer = 0;
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

            // 从 call_info 获取 name 和 namewhat
            // call_info[last] 记录了当前函数是如何被调用的
            // 当最后一个条目是 C 函数时，跳过它（C 函数条目记录的是 C 函数的调用信息）
            let name_idx = if state
                .call_info
                .last()
                .map(|e| e.is_c)
                .unwrap_or(false)
                && state.call_info.len() >= 2
            {
                state.call_info.len() - 2
            } else {
                state.call_info.len().saturating_sub(1)
            };
            let (name, namewhat) = state
                .call_info
                .get(name_idx)
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
                info.what = if proto.line_defined == 0 { "main" } else { "Lua" }.to_string();
            }
            if what.contains('l') {
                // state.pc 指向当前正在执行的指令（等价于 C 的 currentpc）
                // op_call 中 C 函数调用期间 state.pc 仍指向 CALL 指令（递增在调用后）
                info.currentline = get_proto_line(proto, state.pc);
            }
            if what.contains('u') {
                info.nups = closure.upvals.borrow().len();
                info.nparams = proto.num_params as usize;
                info.isvararg = proto.is_vararg();
            }
            if what.contains('n') {
                info.name = name;
                info.namewhat = namewhat;
            }
            if what.contains('t') {
                // 从 call_info 读取 is_tailcall — 对应 C 的 ci->callstatus & CIST_TAIL
                // call_info[last] 是 debug.getinfo 自身（C 函数），需要跳过它
                // 获取调用 debug.getinfo 的函数（level 1）的 CallInfoEntry
                let tail_idx = if state
                    .call_info
                    .last()
                    .map(|e| e.is_c)
                    .unwrap_or(false)
                    && state.call_info.len() >= 2
                {
                    state.call_info.len() - 2
                } else {
                    state.call_info.len().saturating_sub(1)
                };
                let istailcall = state
                    .call_info
                    .get(tail_idx)
                    .map(|e| e.is_tailcall)
                    .unwrap_or(false);
                info.istailcall = istailcall;
                info.extraargs = state.nextraargs;
            }
            if what.contains('r') {
                if !state.allowhook {
                    info.ftransfer = state.transferinfo_ftransfer;
                    info.ntransfer = state.transferinfo_ntransfer;
                } else {
                    info.ftransfer = 0;
                    info.ntransfer = 0;
                }
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
    //
    // CallInfoEntry 在 op_call 时推入，记录的是"调用转换"信息:
    //   closure = 被调用者（callee）的闭包
    //   base    = 调用者（caller）的 base
    //   saved_pc = 调用者（caller）的 pc
    //   name/namewhat = 被调用者（callee）的名字
    //
    // 当最后一个 call_info 条目是 C 函数时，它代表当前正在执行的 C 函数（level 0），
    // 需要跳过它来计算 level >= 2 的索引。
    //
    // level n 对应的函数 = 调用 level n-1 的函数
    // call_info[last] 记录了"谁调用了当前函数"
    //   → level 2 = call_info[last] = call_info[len - 1]
    //   → level n = call_info[len - (n - 1)]
    //
    // 要获取 level n 的函数信息:
    //   closure = state.stack[entry.base - 1]  (调用者的闭包)
    //   pc      = entry.saved_pc               (调用者的 pc)
    //   name    = call_info[ci_idx - 1].name   (调用者被调用时的名字，若存在)

    // 检查最后一个 call_info 条目是否是 C 函数（当前正在执行的 C 函数）
    let c_func_offset = if state
        .call_info
        .last()
        .map(|e| e.is_c)
        .unwrap_or(false)
    {
        1 // 跳过最后一个 C 函数条目
    } else {
        0
    };

    let ci_idx = match state
        .call_info
        .len()
        .checked_sub((level as usize).saturating_sub(1) + c_func_offset)
    {
        Some(idx) => idx,
        None => return false,
    };

    let entry = &state.call_info[ci_idx];

    // 调用者的名字来自前一个 entry（调用者被调用时推入的记录）
    // 若 ci_idx == 0，调用者是主函数，没有名字
    let (caller_name, caller_namewhat) = if ci_idx > 0 {
        let prev = &state.call_info[ci_idx - 1];
        (
            if prev.name.is_empty() { None } else { Some(prev.name.clone()) },
            prev.namewhat.clone(),
        )
    } else {
        (None, String::new())
    };

    // 从栈上获取调用者的闭包（entry.base 是调用者的 base）
    if entry.base > 0 && entry.base <= state.stack.len() {
        if let TValue::LClosure(closure) = &state.stack[entry.base - 1] {
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
                info.what = if proto.line_defined == 0 { "main" } else { "Lua" }.to_string();
            }
            if what.contains('l') {
                info.currentline = get_proto_line(proto, entry.saved_pc);
            }
            if what.contains('u') {
                info.nups = closure.upvals.borrow().len();
                info.nparams = proto.num_params as usize;
                info.isvararg = proto.is_vararg();
            }
            if what.contains('n') {
                info.name = caller_name;
                info.namewhat = caller_namewhat;
            }
            if what.contains('t') {
                // level N 的 istailcall = level N 是否是尾调用
                // entry (call_info[ci_idx]) 记录 "level N-1 被调用时" 的信息:
                //   is_tailcall = level N-1 是否是尾调用
                // level N 的 istailcall 记录在 "level N 被调用时" 的 entry 中 = call_info[ci_idx - 1]
                // (tail call 重用 entry, 修改 closure 和 is_tailcall, 所以 call_info[ci_idx-1]
                //  在 tail call 后记录的是重用后的函数的 istailcall)
                info.istailcall = if ci_idx > 0 {
                    state.call_info[ci_idx - 1].is_tailcall
                } else {
                    false
                };
                info.extraargs = 0;
            }
            if what.contains('r') {
                if !state.allowhook {
                    info.ftransfer = state.transferinfo_ftransfer;
                    info.ntransfer = state.transferinfo_ntransfer;
                } else {
                    info.ftransfer = 0;
                    info.ntransfer = 0;
                }
            }
            return true;
        }
    }

    // C 函数帧 — 对应 C 的 CIST_C
    info.what = "C".to_string();
    info.short_src = "[C]".to_string();
    info.source = "=[C]".to_string();
    info.currentline = -1;
    info.nups = 0;
    info.nparams = 0;
    info.isvararg = true;
    if what.contains('n') {
        info.name = caller_name;
        info.namewhat = caller_namewhat;
    }
    if what.contains('t') {
        info.istailcall = false;
        info.extraargs = 0;
    }
    if what.contains('r') {
        // 在 hook 期间（allowhook==false），从 transferinfo 读取
        // 对应 C 的 ci->callstatus & CIST_HOOKED 检查
        if !state.allowhook {
            info.ftransfer = state.transferinfo_ftransfer;
            info.ntransfer = state.transferinfo_ntransfer;
        } else {
            info.ftransfer = 0;
            info.ntransfer = 0;
        }
    }
    // 'f' 选项: 从栈上读取 C 函数值 (entry.base - 1 = 函数位置)
    if what.contains('f') {
        if entry.base > 0 && entry.base <= state.stack.len() {
            info.func = Some(state.stack[entry.base - 1].clone());
        }
    }
    true
}

/// 从 ThreadContext 填充 DebugInfo — 用于 debug.getinfo(co, level, ...)
///
/// 协程挂起时，调用栈信息保存在 ThreadContext 中。
/// Level 映射与 build_traceback_from_thread 类似：
/// - Level 0 到 c_chain_len-1: C 函数链（如 yield）
/// - Level c_chain_len: 当前 Lua 帧（saved_base/saved_pc）
/// - Level c_chain_len+1+: 剩余 call_info 条目
fn fill_info_from_thread(
    ctx: &crate::objects::ThreadContext,
    info: &mut DebugInfo,
    level: i32,
    what: &str,
) -> bool {
    if level < 0 {
        return false;
    }

    let call_info = &ctx.saved_call_info;
    let n = call_info.len();

    // 计算 C 函数链长度
    let c_chain_len = call_info.iter().rev().take_while(|e| e.is_c).count();

    // Level 0 到 c_chain_len-1: C 函数链
    if (level as usize) < c_chain_len {
        let idx = n - 1 - level as usize;
        let entry = &call_info[idx];
        info.what = "C".to_string();
        info.short_src = "[C]".to_string();
        info.source = "=[C]".to_string();
        info.currentline = -1;
        info.nups = 0;
        info.nparams = 0;
        info.isvararg = true;
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
        if what.contains('f') {
            info.func = None;
        }
        return true;
    }

    let lua_level = c_chain_len as i32;

    // Level c_chain_len: 当前 Lua 帧
    if level == lua_level {
        if ctx.saved_base == 0 || ctx.saved_base > ctx.saved_stack.len() {
            return false;
        }
        if let TValue::LClosure(closure) = &ctx.saved_stack[ctx.saved_base - 1] {
            let proto = &closure.proto;
            info.func = Some(TValue::LClosure(closure.clone()));
            info.closure = Some(closure.clone());

            // 名字来自调用当前帧的 call_info 条目
            let (name, namewhat) = if n > c_chain_len {
                let name_entry = &call_info[n - 1 - c_chain_len];
                (
                    if name_entry.name.is_empty() {
                        None
                    } else {
                        Some(name_entry.name.clone())
                    },
                    name_entry.namewhat.clone(),
                )
            } else {
                (None, String::new())
            };

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
                info.what = if proto.line_defined == 0 {
                    "main"
                } else {
                    "Lua"
                }
                .to_string();
            }
            if what.contains('l') {
                // ctx.saved_pc = state.pc + 1 (等价于 C 的 savedpc)，需要 -1 得到 currentpc
                info.currentline = get_proto_line(proto, ctx.saved_pc.saturating_sub(1));
            }
            if what.contains('u') {
                info.nups = closure.upvals.borrow().len();
                info.nparams = proto.num_params as usize;
                info.isvararg = proto.is_vararg();
            }
            if what.contains('n') {
                info.name = name;
                info.namewhat = namewhat;
            }
            if what.contains('t') {
                info.istailcall = if n > c_chain_len {
                    call_info[n - 1 - c_chain_len].is_tailcall
                } else {
                    false
                };
                info.extraargs = ctx.saved_nextraargs;
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
        return true;
    }

    // Level > lua_level: 从 call_info 获取
    // level c_chain_len+1+offset: 函数/名字来自 call_info[remaining-2-offset]，
    // source/line 来自 call_info[remaining-1-offset]（调用者的位置）
    let remaining = n.saturating_sub(c_chain_len);
    if remaining <= 1 {
        return false;
    }
    let offset = match (level as usize).checked_sub((c_chain_len + 1) as usize) {
        Some(o) => o,
        None => return false,
    };
    if offset >= remaining - 1 {
        return false;
    }
    let target_idx = remaining - 2 - offset;
    let entry = &call_info[target_idx];          // 函数/名字/closure
    let next_entry = &call_info[target_idx + 1]; // source/line（调用者的位置）

    if entry.is_c {
        // C 函数帧
        info.what = "C".to_string();
        info.short_src = "[C]".to_string();
        info.source = "=[C]".to_string();
        info.currentline = -1;
        info.nups = 0;
        info.nparams = 0;
        info.isvararg = true;
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
        if what.contains('f') {
            info.func = None;
        }
        return true;
    }

    // Lua 函数帧 — 从 entry.closure 获取信息
    if let Some(closure) = &entry.closure {
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
            info.what = if proto.line_defined == 0 {
                "main"
            } else {
                "Lua"
            }
            .to_string();
        }
        if what.contains('l') {
            info.currentline = next_entry.line;
        }
        if what.contains('u') {
            info.nups = closure.upvals.borrow().len();
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
            info.istailcall = entry.is_tailcall;
            info.extraargs = next_entry.nextraargs;
        }
        if what.contains('r') {
            info.ftransfer = 0;
            info.ntransfer = 0;
        }
        return true;
    }

    false
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
        while base_pc <= pc {
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

/// 从 ThreadContext 获取局部变量名和值 — 用于 debug.getlocal(co, level, nvar)
///
/// 返回 Some((name, value)) 或 None（超出范围）
fn get_local_from_thread(
    ctx: &crate::objects::ThreadContext,
    level: i32,
    nvar: i32,
) -> Option<(String, TValue)> {
    if level < 0 {
        return None;
    }

    let call_info = &ctx.saved_call_info;
    let n = call_info.len();
    let c_chain_len = call_info.iter().rev().take_while(|e| e.is_c).count();

    let read_stack = |idx: usize| -> TValue {
        if idx < ctx.saved_stack.len() {
            ctx.saved_stack[idx].clone()
        } else {
            TValue::Nil(NilKind::Strict)
        }
    };

    // Level 0 到 c_chain_len-1: C 函数链
    if (level as usize) < c_chain_len {
        let idx = n - 1 - level as usize;
        let entry = &call_info[idx];
        if nvar <= 0 {
            return None;
        }
        // limit: 对于 C 函数链中的帧，limit = 下一帧的 base - 1
        let limit = if idx + 1 < n {
            call_info[idx + 1].base.saturating_sub(1)
        } else {
            // 最后一帧（level 0），limit = saved_top
            ctx.saved_top
        };
        let cond = nvar > 0 && limit >= entry.base && (limit - entry.base) >= (nvar as usize);
        if cond {
            let stack_idx = entry.base + (nvar as usize) - 1;
            return Some(("(C temporary)".to_string(), read_stack(stack_idx)));
        }
        return None;
    }

    let lua_level = c_chain_len as i32;

    // Level c_chain_len: 当前 Lua 帧
    if level == lua_level {
        if ctx.saved_base == 0 || ctx.saved_base > ctx.saved_stack.len() {
            return None;
        }
        if let TValue::LClosure(closure) = &ctx.saved_stack[ctx.saved_base - 1] {
            let proto = &closure.proto;
            let pc = ctx.saved_pc.saturating_sub(1);

            // 负数索引 (vararg)
            if nvar < 0 {
                if (ctx.saved_proto_flag & PF_VAHID) != 0 {
                    let nextra = ctx.saved_nextraargs;
                    if nvar >= -nextra {
                        let pos = (ctx.saved_base as i32) - 1 - nextra - (nvar + 1);
                        let pos = pos as usize;
                        return Some(("(vararg)".to_string(), read_stack(pos)));
                    }
                }
                return None;
            }

            // 正数索引: 局部变量
            if let Some(name_str) = get_local_name(proto, nvar as usize, pc) {
                let stack_idx = ctx.saved_base + (nvar as usize) - 1;
                return Some((name_str, read_stack(stack_idx)));
            }

            // 临时变量
            let limit = ctx.saved_top;
            let cond = nvar > 0 && limit >= ctx.saved_base && (limit - ctx.saved_base) >= (nvar as usize);
            if cond {
                let stack_idx = ctx.saved_base + (nvar as usize) - 1;
                return Some(("(temporary)".to_string(), read_stack(stack_idx)));
            }
            return None;
        }
        // C 函数帧
        return None;
    }

    // Level > c_chain_len: 从 saved_call_info 获取
    // saved_call_info[i] = "调用者 A 调用被调用者 B"
    //   closure = B, base = B 的 base, saved_pc = A 的 pc
    // 当前帧（level c_chain_len）的信息不在 saved_call_info 中（从 ctx 获取）
    // level c_chain_len+1+offset 对应 saved_call_info[offset]
    //   (因为 saved_call_info[remaining-1] 是当前帧的调用关系，不用于 level > c_chain_len)
    let remaining = n.saturating_sub(c_chain_len);
    if remaining <= 1 {
        return None;
    }
    let offset = match (level as usize).checked_sub((c_chain_len + 1) as usize) {
        Some(o) => o,
        None => return None,
    };
    if offset + 1 >= remaining {
        return None;
    }
    let target_idx = offset;
    let entry = &call_info[target_idx];

    // limit: 下一帧的 base - 1
    let limit = call_info[target_idx + 1].base.saturating_sub(1);

    if entry.is_c {
        if nvar <= 0 {
            return None;
        }
        let cond = nvar > 0 && limit >= entry.base && (limit - entry.base) >= (nvar as usize);
        if cond {
            let stack_idx = entry.base + (nvar as usize) - 1;
            return Some(("(C temporary)".to_string(), read_stack(stack_idx)));
        }
        return None;
    }

    // Lua 函数帧
    if let Some(closure) = &entry.closure {
        let proto = &closure.proto;
        let pc = call_info[target_idx + 1].saved_pc;

        // 负数索引 (vararg)
        if nvar < 0 {
            if (entry.proto_flag & PF_VAHID) != 0 {
                let nextra = entry.nextraargs;
                if nvar >= -nextra {
                    let pos = (entry.base as i32) - 1 - nextra - (nvar + 1);
                    let pos = pos as usize;
                    return Some(("(vararg)".to_string(), read_stack(pos)));
                }
            }
            return None;
        }

        // 正数索引: 局部变量
        if let Some(name_str) = get_local_name(proto, nvar as usize, pc) {
            let stack_idx = entry.base + (nvar as usize) - 1;
            return Some((name_str, read_stack(stack_idx)));
        }

        // 临时变量
        let cond = nvar > 0 && limit >= entry.base && (limit - entry.base) >= (nvar as usize);
        if cond {
            let stack_idx = entry.base + (nvar as usize) - 1;
            return Some(("(temporary)".to_string(), read_stack(stack_idx)));
        }
    }

    None
}

/// 从 ThreadContext 设置局部变量值 — 用于 debug.setlocal(co, level, nvar, value)
///
/// 返回变量名（成功）或 None（超出范围）
fn set_local_from_thread(
    ctx: &mut crate::objects::ThreadContext,
    level: i32,
    nvar: i32,
    value: TValue,
) -> Option<String> {
    if level < 0 {
        return None;
    }

    let call_info = ctx.saved_call_info.clone();
    let n = call_info.len();
    let c_chain_len = call_info.iter().rev().take_while(|e| e.is_c).count();

    let write_stack = |stack: &mut Vec<TValue>, idx: usize| {
        if idx < stack.len() {
            stack[idx] = value.clone();
        }
    };

    // Level 0 到 c_chain_len-1: C 函数链
    if (level as usize) < c_chain_len {
        let idx = n - 1 - level as usize;
        let entry = &call_info[idx];
        if nvar <= 0 {
            return None;
        }
        let limit = if idx + 1 < n {
            call_info[idx + 1].base.saturating_sub(1)
        } else {
            ctx.saved_top
        };
        let cond = nvar > 0 && limit >= entry.base && (limit - entry.base) >= (nvar as usize);
        if cond {
            let stack_idx = entry.base + (nvar as usize) - 1;
            write_stack(&mut ctx.saved_stack, stack_idx);
            return Some("(C temporary)".to_string());
        }
        return None;
    }

    let lua_level = c_chain_len as i32;

    // Level c_chain_len: 当前 Lua 帧
    if level == lua_level {
        if ctx.saved_base == 0 || ctx.saved_base > ctx.saved_stack.len() {
            return None;
        }
        if let TValue::LClosure(closure) = &ctx.saved_stack[ctx.saved_base - 1].clone() {
            let proto = &closure.proto;
            let pc = ctx.saved_pc.saturating_sub(1);

            // 负数索引 (vararg)
            if nvar < 0 {
                if (ctx.saved_proto_flag & PF_VAHID) != 0 {
                    let nextra = ctx.saved_nextraargs;
                    if nvar >= -nextra {
                        let pos = (ctx.saved_base as i32) - 1 - nextra - (nvar + 1);
                        let pos = pos as usize;
                        write_stack(&mut ctx.saved_stack, pos);
                        return Some("(vararg)".to_string());
                    }
                }
                return None;
            }

            // 正数索引: 局部变量
            if let Some(name_str) = get_local_name(proto, nvar as usize, pc) {
                let stack_idx = ctx.saved_base + (nvar as usize) - 1;
                write_stack(&mut ctx.saved_stack, stack_idx);
                return Some(name_str);
            }

            // 临时变量
            let limit = ctx.saved_top;
            let cond = nvar > 0 && limit >= ctx.saved_base && (limit - ctx.saved_base) >= (nvar as usize);
            if cond {
                let stack_idx = ctx.saved_base + (nvar as usize) - 1;
                write_stack(&mut ctx.saved_stack, stack_idx);
                return Some("(temporary)".to_string());
            }
            return None;
        }
        return None;
    }

    // Level > c_chain_len: 从 saved_call_info 获取
    let remaining = n.saturating_sub(c_chain_len);
    if remaining <= 1 {
        return None;
    }
    let offset = match (level as usize).checked_sub((c_chain_len + 1) as usize) {
        Some(o) => o,
        None => return None,
    };
    if offset + 1 >= remaining {
        return None;
    }
    let target_idx = offset;
    let entry = call_info[target_idx].clone();

    let limit = call_info[target_idx + 1].base.saturating_sub(1);

    if entry.is_c {
        if nvar <= 0 {
            return None;
        }
        let cond = nvar > 0 && limit >= entry.base && (limit - entry.base) >= (nvar as usize);
        if cond {
            let stack_idx = entry.base + (nvar as usize) - 1;
            write_stack(&mut ctx.saved_stack, stack_idx);
            return Some("(C temporary)".to_string());
        }
        return None;
    }

    // Lua 函数帧
    if let Some(closure) = &entry.closure {
        let proto = &closure.proto;
        let pc = call_info[target_idx + 1].saved_pc;

        // 负数索引 (vararg)
        if nvar < 0 {
            if (entry.proto_flag & PF_VAHID) != 0 {
                let nextra = entry.nextraargs;
                if nvar >= -nextra {
                    let pos = (entry.base as i32) - 1 - nextra - (nvar + 1);
                    let pos = pos as usize;
                    write_stack(&mut ctx.saved_stack, pos);
                    return Some("(vararg)".to_string());
                }
            }
            return None;
        }

        // 正数索引: 局部变量
        if let Some(name_str) = get_local_name(proto, nvar as usize, pc) {
            let stack_idx = entry.base + (nvar as usize) - 1;
            write_stack(&mut ctx.saved_stack, stack_idx);
            return Some(name_str);
        }

        // 临时变量
        let cond = nvar > 0 && limit >= entry.base && (limit - entry.base) >= (nvar as usize);
        if cond {
            let stack_idx = entry.base + (nvar as usize) - 1;
            write_stack(&mut ctx.saved_stack, stack_idx);
            return Some("(temporary)".to_string());
        }
    }

    None
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
    let co_thread = if let TValue::Thread(t) = &arg0 {
        arg_offset = 1;
        Some(t.clone())
    } else {
        None
    };

    let arg1 = get_arg(state, a, arg_offset);
    let nvar = get_arg(state, a, arg_offset + 1)
        .as_integer()
        .unwrap_or(0) as i32;

    // 函数参数模式: if arg1 is a function (Lua or C), get local name.
    // In C, lua_isfunction returns true for both Lua and C functions.
    // In Rust, C functions are stored as LightUserData with tags.
    if matches!(arg1, TValue::LClosure(_) | TValue::CClosure(_) | TValue::LCFn(_) | TValue::LightUserData(_)) {
        if let TValue::LClosure(closure) = &arg1 {
            // Lua function: get the local variable name
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
        } else {
            // C function (not a Lua function): no local variables
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            return Ok(());
        }
    }

    // 栈级别模式
    let level = arg1.as_integer().unwrap_or(0) as i32;

    // 协程模式: 从 ThreadContext 获取局部变量
    if let Some(thread) = &co_thread {
        let ctx = thread.context.borrow();
        match get_local_from_thread(&ctx, level, nvar) {
            Some((name, val)) => {
                push_results(state, a, nresults, vec![
                    TValue::Str(state.intern_str(&name)),
                    val,
                ]);
            }
            None => {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            }
        }
        return Ok(());
    }

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

    // level >= 1: 使用 get_frame_info 获取栈帧信息
    match get_frame_info(state, level) {
        Some(frame) => {
            // C 函数帧: 没有命名局部变量，所有正数索引都是 "(C temporary)"
            // 对应 C 的 luaG_findlocal: isLua(ci) 为 false 时 name 保持 NULL
            if frame.is_c {
                if nvar < 0 {
                    // C 函数没有 vararg
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                    return Ok(());
                }
                let cond = nvar > 0 && frame.limit >= frame.base && (frame.limit - frame.base) >= (nvar as usize);
                if cond {
                    let stack_idx = frame.base + (nvar as usize) - 1;
                    let val = if stack_idx < state.stack.len() {
                        state.stack[stack_idx].clone()
                    } else {
                        TValue::Nil(NilKind::Strict)
                    };
                    push_results(state, a, nresults, vec![
                        TValue::Str(state.intern_str("(C temporary)")),
                        val,
                    ]);
                } else {
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                }
                return Ok(());
            }

            let proto = &frame.closure.as_ref().unwrap().proto;

            // 处理负数索引 (vararg) — 对应 C 的 findvararg
            if nvar < 0 {
                if (frame.proto_flag & PF_VAHID) != 0 {
                    let nextra = frame.nextraargs;
                    if nvar >= -nextra {
                        // pos = ci->func.p - nextra - (n + 1)
                        // ci->func.p = frame.base - 1
                        let pos = (frame.base as i32) - 1 - nextra - (nvar + 1);
                        let pos = pos as usize;
                        let val = if pos < state.stack.len() {
                            state.stack[pos].clone()
                        } else {
                            TValue::Nil(NilKind::Strict)
                        };
                        push_results(state, a, nresults, vec![
                            TValue::Str(state.intern_str("(vararg)")),
                            val,
                        ]);
                    } else {
                        push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                    }
                } else {
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                }
                return Ok(());
            }

            // 正数索引: 局部变量
            let name = get_local_name(proto, nvar as usize, frame.pc);
            match name {
                Some(n) => {
                    // 获取栈上的值
                    let stack_idx = frame.base + (nvar as usize) - 1;
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
                    // 对应 C 的 luaG_findlocal: 没有命名局部变量时，检查是否是临时变量
                    // limit - base >= n && n > 0 时为 "(temporary)"
                    let cond = nvar > 0 && frame.limit >= frame.base && (frame.limit - frame.base) >= (nvar as usize);
                    if cond {
                        let stack_idx = frame.base + (nvar as usize) - 1;
                        let val = if stack_idx < state.stack.len() {
                            state.stack[stack_idx].clone()
                        } else {
                            TValue::Nil(NilKind::Strict)
                        };
                        push_results(state, a, nresults, vec![
                            TValue::Str(state.intern_str("(temporary)")),
                            val,
                        ]);
                    } else {
                        push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                    }
                }
            }
            return Ok(());
        }
        None => {
            Err(VmError::RuntimeError("level out of range".to_string()))
        }
    }
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
    let co_thread = if let TValue::Thread(t) = &arg0 {
        arg_offset = 1;
        Some(t.clone())
    } else {
        None
    };

    let level = get_arg(state, a, arg_offset)
        .as_integer()
        .unwrap_or(0) as i32;
    let nvar = get_arg(state, a, arg_offset + 1)
        .as_integer()
        .unwrap_or(0) as i32;
    let value = get_arg(state, a, arg_offset + 2);

    if level < 1 {
        return Err(VmError::RuntimeError("level out of range".to_string()));
    }

    // 协程模式: 从 ThreadContext 设置局部变量
    if let Some(thread) = &co_thread {
        let mut ctx = thread.context.borrow_mut();
        match set_local_from_thread(&mut ctx, level, nvar, value) {
            Some(name) => {
                push_single_result(state, a, nresults, TValue::Str(state.intern_str(&name)));
            }
            None => {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            }
        }
        return Ok(());
    }

    // 使用 get_frame_info 获取栈帧信息
    match get_frame_info(state, level) {
        Some(frame) => {
            // C 函数帧: 没有命名局部变量，所有正数索引都是 "(C temporary)"
            if frame.is_c {
                if nvar < 0 {
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                    return Ok(());
                }
                let cond = nvar > 0 && frame.limit >= frame.base && (frame.limit - frame.base) >= (nvar as usize);
                if cond {
                    let stack_idx = frame.base + (nvar as usize) - 1;
                    if stack_idx < state.stack.len() {
                        state.stack[stack_idx] = value;
                    }
                    push_single_result(state, a, nresults, TValue::Str(state.intern_str("(C temporary)")));
                } else {
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                }
                return Ok(());
            }

            let proto = &frame.closure.as_ref().unwrap().proto;

            // 处理负数索引 (vararg) — 对应 C 的 findvararg
            if nvar < 0 {
                if (frame.proto_flag & PF_VAHID) != 0 {
                    let nextra = frame.nextraargs;
                    if nvar >= -nextra {
                        // pos = ci->func.p - nextra - (n + 1)
                        let pos = (frame.base as i32) - 1 - nextra - (nvar + 1);
                        let pos = pos as usize;
                        if pos < state.stack.len() {
                            state.stack[pos] = value;
                        }
                        push_single_result(state, a, nresults, TValue::Str(state.intern_str("(vararg)")));
                    } else {
                        push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                    }
                } else {
                    push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                }
                return Ok(());
            }

            let name = get_local_name(proto, nvar as usize, frame.pc);
            match name {
                Some(n) => {
                    // 设置栈上的值
                    let stack_idx = frame.base + (nvar as usize) - 1;
                    if stack_idx < state.stack.len() {
                        state.stack[stack_idx] = value;
                    }
                    push_single_result(state, a, nresults, TValue::Str(state.intern_str(&n)));
                }
                None => {
                    // 对应 C 的 luaG_findlocal: 临时变量
                    let cond = nvar > 0 && frame.limit >= frame.base && (frame.limit - frame.base) >= (nvar as usize);
                    if cond {
                        let stack_idx = frame.base + (nvar as usize) - 1;
                        if stack_idx < state.stack.len() {
                            state.stack[stack_idx] = value;
                        }
                        push_single_result(state, a, nresults, TValue::Str(state.intern_str("(temporary)")));
                    } else {
                        push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
                    }
                }
            }
            return Ok(());
        }
        None => {
            Err(VmError::RuntimeError("level out of range".to_string()))
        }
    }
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
            if n > 0 && n <= closure.upvals.borrow().len() {
                let upvals_ref = closure.upvals.borrow();
                let uv_ref = upvals_ref[n - 1].borrow();
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
                // 获取上值名 — 对应 C 的 luaF_getupname: NULL name 返回 "(no name)"
                let name = closure
                    .proto
                    .upvalues
                    .get(n - 1)
                    .and_then(|u| u.name.as_ref())
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_else(|| "(no name)".to_string());
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
        TValue::Table(t) => {
            // 带有 __call 元方法的 Table 是可调用对象 (如 string.gmatch 返回值)
            // 模拟 C 闭包行为: upvalue 名为空字符串, 值为 nil
            if t.get_metatable()
                .and_then(|mt| mt.get(&TValue::Str(state.intern_str("__call"))))
                .is_some()
            {
                push_results(state, a, nresults, vec![
                    TValue::Str(state.intern_str("")),
                    TValue::Nil(NilKind::Strict),
                ]);
            } else {
                return Err(VmError::RuntimeError(
                    "bad argument #1 to 'getupvalue' (function expected)".to_string(),
                ));
            }
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
            if n > 0 && n <= closure.upvals.borrow().len() {
                // 获取上值名 — 对应 C 的 luaF_getupname: NULL name 返回 "(no name)"
                let name = closure
                    .proto
                    .upvalues
                    .get(n - 1)
                    .and_then(|u| u.name.as_ref())
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_else(|| "(no name)".to_string());

                // 设置上值
                // 由于 upvals 是 Rc<RefCell>, 我们需要可变访问
                // 但 arg1 是 clone 的, 我们需要修改栈上的原始闭包
                if a + 1 < state.stack.len() {
                    if let TValue::LClosure(ref mut cl) = state.stack[a + 1] {
                        if n <= cl.upvals.borrow().len() {
                            // 先取出 stack_index (如果是 Open), 然后释放 borrow, 再修改 stack
                            let action = {
                                let upvals_ref = cl.upvals.borrow();
                                let mut uv_ref = upvals_ref[n - 1].borrow_mut();
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
        TValue::Table(t) => {
            // 带有 __call 元方法的 Table 是可调用对象 (如 string.gmatch 返回值)
            // 模拟 C 闭包行为: 无法设置 upvalue, 返回 nil
            if t.get_metatable()
                .and_then(|mt| mt.get(&TValue::Str(state.intern_str("__call"))))
                .is_some()
            {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            } else {
                return Err(VmError::RuntimeError(
                    "bad argument #1 to 'setupvalue' (function expected)".to_string(),
                ));
            }
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
            let upvals_ref = closure.upvals.borrow();
            if n > 0 && n <= upvals_ref.len() {
                // 使用 Rc 的指针作为唯一标识
                let ptr = Rc::as_ptr(&upvals_ref[n - 1]) as *mut std::ffi::c_void;
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
    // 对应 C 的 db_upvaluejoin -> lua_upvaluejoin:
    //   *up1 = *up2;  (f1 的第 n1 个上值指向 f2 的第 n2 个上值对象)
    //
    // 关键: upvals 是 Rc<RefCell<Vec>>，所有 LClosure clone 共享同一个 Vec。
    // 修改栈上 clone 的 upvals 会影响所有共享同一 Rc 的闭包（包括原始 LClosure）。
    let f1_stack_idx = a + 1;  // f1 在栈上的位置
    let f2 = get_arg(state, a, 2);
    let n1 = get_arg(state, a, 1).as_integer().unwrap_or(0) as usize;
    let n2 = get_arg(state, a, 3).as_integer().unwrap_or(0) as usize;

    // 获取 f2 的第 n2 个上值引用 (Rc<RefCell<UpVal>>)
    let f2_upval: Option<UpValRef> = if let TValue::LClosure(c2) = &f2 {
        let c2_upvals = c2.upvals.borrow();
        if n2 > 0 && n2 <= c2_upvals.len() {
            Some(c2_upvals[n2 - 1].clone())
        } else {
            None
        }
    } else {
        None
    };

    // 将 f1 的第 n1 个上值指向 f2 的第 n2 个上值
    // 通过 borrow_mut 修改共享的 Vec，影响所有共享同一 Rc 的闭包
    if let Some(upval) = f2_upval {
        if f1_stack_idx < state.stack.len() {
            if let TValue::LClosure(ref mut c1) = state.stack[f1_stack_idx] {
                let mut c1_upvals = c1.upvals.borrow_mut();
                if n1 > 0 && n1 <= c1_upvals.len() {
                    c1_upvals[n1 - 1] = upval;
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
    let target_thread = if let TValue::Thread(t) = &arg0 {
        arg_offset = 1;
        Some(t.clone())
    } else {
        None
    };

    let hook = get_arg(state, a, arg_offset);

    if matches!(hook, TValue::Nil(_)) || matches!(hook, TValue::Nil(NilKind::Empty)) {
        // 关闭钩子
        if let Some(thread) = target_thread {
            // 设置到指定协程的 ThreadContext
            let mut ctx = thread.context.borrow_mut();
            ctx.saved_hook_func = None;
            ctx.saved_hook_mask = 0;
            ctx.saved_hook_count = 0;
            ctx.saved_current_hook_count = 0;
            ctx.saved_allowhook = true;
        } else {
            set_hook_in_registry(state, None, 0, 0);
        }
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
        if let Some(thread) = target_thread {
            // 设置到指定协程的 ThreadContext（resume 时恢复到 state）
            // 同时把 hook 函数的 Open upvalue 转为 Closed，
            // 避免协程执行期间 state.stack 被替换后 upvalue 失效
            crate::stdlib::coroutine_lib::close_hook_upvals(&hook, state);
            let mut ctx = thread.context.borrow_mut();
            ctx.saved_hook_func = Some(hook);
            ctx.saved_hook_mask = mask;
            ctx.saved_hook_count = count;
            ctx.saved_current_hook_count = count;
            ctx.saved_allowhook = true;
        } else {
            set_hook_in_registry(state, Some(hook), mask, count);
        }
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
    let co_thread = if let TValue::Thread(t) = &arg0 {
        arg_offset = 1;
        Some(t.clone())
    } else {
        None
    };

    // 协程模式: 从 ThreadContext 获取 hook
    if let Some(thread) = &co_thread {
        let ctx = thread.context.borrow();
        match &ctx.saved_hook_func {
            Some(f) if !matches!(f, TValue::Nil(_)) => {
                let mask_str = unmake_mask(ctx.saved_hook_mask);
                push_results(state, a, nresults, vec![
                    f.clone(),
                    TValue::Str(state.intern_str(&mask_str)),
                    TValue::Integer(ctx.saved_hook_count as i64),
                ]);
            }
            _ => {
                push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            }
        }
        return Ok(());
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
            t.set_metatable(Some(mt));
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
    state.current_hook_count = count;
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
    let co_thread = if let TValue::Thread(t) = &arg0 {
        arg_offset = 1;
        Some(t.clone())
    } else {
        None
    };

    let msg_val = get_arg(state, a, arg_offset);
    // 对应 C: level = (L == L1) ? 1 : 0
    let default_level: i32 = if co_thread.is_some() { 0 } else { 1 };
    let level = if nargs > arg_offset + 1 {
        get_arg(state, a, arg_offset + 1)
            .as_integer()
            .unwrap_or(default_level as i64) as i32
    } else {
        default_level
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
    let traceback = if let Some(thread) = &co_thread {
        // 协程: 从 ThreadContext 构建 traceback
        let ctx = thread.context.borrow();
        build_traceback_from_thread(&ctx, &msg, level)
    } else {
        build_traceback(state, &msg, level)
    };
    push_single_result(state, a, nresults, TValue::Str(state.intern_str(&traceback)));
    Ok(())
}

/// 构建堆栈回溯字符串 — 对应 C 的 luaL_traceback
///
/// Level 映射:
/// - Level 0 = debug.traceback 自身 (C 函数, 不显示)
/// - Level 1 = 调用 debug.traceback 的函数
/// - Level 2 = 调用 level 1 的函数
///
/// 当 call_info 末尾有 C 函数链时（C 函数调用 C 函数），
/// state.base 不随 C 函数调用改变，所以需要计算 C 函数链长度来正确映射 level。
fn build_traceback(state: &LuaState, msg: &str, level: i32) -> String {
    let mut lines: Vec<String> = Vec::new();

    // Level 0: debug.traceback 自身 (C/tagged 函数)
    if level <= 0 {
        lines.push("[C]: in field 'traceback'".to_string());
    }

    // 计算 call_info 末尾的 C 函数链长度
    // 这些 C 函数条目代表当前正在执行的 C 函数及其 C 函数调用者
    // 最后一个条目是当前函数（level 0），倒数第二个是 level 1，等等
    let c_chain_len = count_c_function_chain(state);
    let n = state.call_info.len();

    // Level 1 到 c_chain_len-1: C 函数调用者（call_info[last-1] 到 call_info[last-c_chain_len+1]）
    // Level c_chain_len: state.base（Lua 函数）
    // Level c_chain_len+1+: 剩余 call_info 条目

    // 输出 C 函数调用者（level 1 到 c_chain_len-1）
    if c_chain_len >= 2 {
        for i in 1..c_chain_len {
            let current_level = i as i32;
            if current_level < level {
                continue;
            }
            let idx = n - 1 - i;
            if idx < n {
                let entry = &state.call_info[idx];
                lines.push(make_traceback_line("[C]", -1, &entry.name, &entry.namewhat, false, true, 0));
            }
        }
    }

    // 输出 state.base（Lua 函数，level c_chain_len）
    let lua_level = c_chain_len as i32;
    if lua_level >= level {
        if let Some((src, line, name, namewhat, is_main)) = get_current_frame_info(state) {
            let (is_c, linedefined) = if state.base > 0 && state.base <= state.stack.len() {
                if let TValue::LClosure(closure) = &state.stack[state.base - 1] {
                    (false, closure.proto.line_defined)
                } else {
                    (true, 0)
                }
            } else {
                (true, 0)
            };
            lines.push(make_traceback_line(&src, line, &name, &namewhat, is_main, is_c, linedefined));
        }
    }

    // 输出剩余 call_info 条目（level c_chain_len+1+）
    // level c_chain_len+1+offset: 函数/名字来自 call_info[offset]，source/line 来自 call_info[offset+1]
    // （call_info[offset+1].source/line 是调用者 call_info[offset].closure 的位置）
    let remaining = n.saturating_sub(c_chain_len);
    if remaining > 1 {
        for i in (0..remaining - 1).rev() {
            let current_level = (c_chain_len + 1 + (remaining - 2 - i)) as i32;
            if current_level < level {
                continue;
            }
            let entry = &state.call_info[i];           // 函数/名字/closure
            let next_entry = &state.call_info[i + 1];  // source/line（调用者的位置）
            if entry.is_c {
                lines.push(make_traceback_line("[C]", -1, &entry.name, &entry.namewhat, false, true, 0));
            } else {
                let src = short_src_from_source(&next_entry.source);
                let is_main = entry
                    .closure
                    .as_ref()
                    .map(|c| c.proto.line_defined == 0)
                    .unwrap_or(false);
                let linedefined = entry.closure.as_ref().map(|c| c.proto.line_defined).unwrap_or(0);
                lines.push(make_traceback_line(&src, next_entry.line, &entry.name, &entry.namewhat, is_main, entry.is_c, linedefined));
            }
        }
    }

    // 仅主线程末尾加 [C]: in ?（协程入口是 Lua 函数，不需要）
    if state.current_thread.is_none() {
        lines.push("[C]: in ?".to_string());
    }

    // 应用截断 — 对应 C Lua 的 LEVELS1/LEVELS2
    // 如果行数 > LEVELS1 + LEVELS2 (21)，保留前 10 行 + skip 消息 + 后 11 行
    const LEVELS1: usize = 10;
    const LEVELS2: usize = 11;
    let mut display_lines: Vec<String> = Vec::new();
    if lines.len() > LEVELS1 + LEVELS2 {
        let skip_count = lines.len() - LEVELS1 - LEVELS2;
        display_lines.extend(lines[..LEVELS1].iter().cloned());
        display_lines.push(format!("...\t(skipping {} levels)", skip_count));
        display_lines.extend(lines[lines.len() - LEVELS2..].iter().cloned());
    } else {
        display_lines = lines;
    }

    // 构建最终字符串
    let mut result = String::new();
    if !msg.is_empty() {
        result.push_str(msg);
        result.push('\n');
    }
    result.push_str("stack traceback:");
    for line in &display_lines {
        result.push('\n');
        result.push('\t');
        result.push_str(line);
    }
    result
}

/// 从 ThreadContext 构建协程的 traceback — 对应 C 的 luaL_traceback(L, L1, msg, level)
///
/// 协程挂起时，调用栈信息保存在 ThreadContext 中：
/// - saved_call_info: 调用栈信息（与 state.call_info 结构相同）
/// - saved_stack/saved_base/saved_pc: 当前 Lua 帧的信息
///
/// Level 映射（与 build_traceback 类似）：
/// - Level 0 到 c_chain_len-1: C 函数链（call_info 末尾的 C 函数，如 yield）
/// - Level c_chain_len: 当前 Lua 帧（saved_base/saved_pc）
/// - Level c_chain_len+1+: 剩余 call_info 条目
///
/// 与 build_traceback 的区别：
/// - 不在最后加 "[C]: in ?"（协程入口是 Lua 函数，不是 C 函数）
/// - 从 saved_call_info/saved_stack 读取信息，而非 state
fn build_traceback_from_thread(
    ctx: &crate::objects::ThreadContext,
    msg: &str,
    level: i32,
) -> String {
    let mut result = String::new();
    if !msg.is_empty() {
        result.push_str(msg);
        result.push('\n');
    }
    result.push_str("stack traceback:");

    let call_info = &ctx.saved_call_info;
    let n = call_info.len();
    if n == 0 {
        return result;
    }

    // 计算 call_info 末尾的 C 函数链长度
    let c_chain_len = call_info.iter().rev().take_while(|e| e.is_c).count();

    // 输出 C 函数链（level 0 到 c_chain_len-1）
    for i in 0..c_chain_len {
        let current_level = i as i32;
        if current_level < level {
            continue;
        }
        let idx = n - 1 - i;
        let entry = &call_info[idx];
        // C 函数: src="[C]", line=-1, 使用 namewhat + name 格式
        push_traceback_line(&mut result, "[C]", -1, &entry.name, &entry.namewhat, false, true, 0);
    }

    // 输出当前 Lua 帧（level c_chain_len）
    let lua_level = c_chain_len as i32;
    if lua_level >= level {
        if ctx.saved_base > 0 && ctx.saved_base <= ctx.saved_stack.len() {
            if let TValue::LClosure(closure) = &ctx.saved_stack[ctx.saved_base - 1] {
                let proto = &closure.proto;
                let src = proto
                    .source
                    .as_ref()
                    .map(short_src)
                    .unwrap_or_else(|| "?".to_string());
                let line = get_proto_line(proto, ctx.saved_pc.saturating_sub(1));
                // 名字来自调用当前帧的 call_info 条目
                let (name, namewhat) = if n > c_chain_len {
                    let name_entry = &call_info[n - 1 - c_chain_len];
                    (name_entry.name.clone(), name_entry.namewhat.clone())
                } else {
                    (String::new(), String::new())
                };
                let is_main = proto.line_defined == 0;
                push_traceback_line(&mut result, &src, line, &name, &namewhat, is_main, false, proto.line_defined);
            }
        }
    }

    // 输出剩余 call_info 条目（level c_chain_len+1+）
    // level c_chain_len+1+offset: 函数/名字来自 call_info[offset]，source/line 来自 call_info[offset+1]
    // （call_info[offset+1].source/line 是调用者 call_info[offset].closure 的位置）
    let remaining = n.saturating_sub(c_chain_len);
    if remaining > 1 {
        for i in (0..remaining - 1).rev() {
            let current_level = (c_chain_len + 1 + (remaining - 2 - i)) as i32;
            if current_level < level {
                continue;
            }
            let entry = &call_info[i];           // 函数/名字/closure
            let next_entry = &call_info[i + 1];  // source/line（调用者的位置）
            if entry.is_c {
                push_traceback_line(&mut result, "[C]", -1, &entry.name, &entry.namewhat, false, true, 0);
            } else {
                let src = short_src_from_source(&next_entry.source);
                let is_main = entry
                    .closure
                    .as_ref()
                    .map(|c| c.proto.line_defined == 0)
                    .unwrap_or(false);
                let linedefined = entry.closure.as_ref().map(|c| c.proto.line_defined).unwrap_or(0);
                push_traceback_line(&mut result, &src, next_entry.line, &entry.name, &entry.namewhat, is_main, entry.is_c, linedefined);
            }
        }
    }

    result
}

/// 计算 call_info 末尾的 C 函数链长度
/// 返回末尾连续 C 函数条目的数量（包括当前正在执行的 C 函数）
fn count_c_function_chain(state: &LuaState) -> usize {
    let mut count = 0;
    for i in (0..state.call_info.len()).rev() {
        if state.call_info[i].is_c {
            count += 1;
        } else {
            break;
        }
    }
    count
}

/// 获取当前 Lua 帧的信息 (用于 traceback 的 level 1)
fn get_current_frame_info(state: &LuaState) -> Option<(String, i32, String, String, bool)> {
    if state.base == 0 || state.base > state.stack.len() {
        return None;
    }
    if let TValue::LClosure(closure) = &state.stack[state.base - 1] {
        let proto = &closure.proto;
        let src = proto
            .source
            .as_ref()
            .map(short_src)
            .unwrap_or_else(|| "?".to_string());
        // state.pc 指向当前正在执行的指令（等价于 C 的 currentpc）
        let line = get_proto_line(proto, state.pc);
        // 当最后一个 call_info 条目是 C 函数时，跳过它获取 name/namewhat
        // 因为 C 函数条目记录的是 C 函数的调用信息，不是当前 Lua 帧的
        let (name, namewhat) = if state
            .call_info
            .last()
            .map(|e| e.is_c)
            .unwrap_or(false)
            && state.call_info.len() >= 2
        {
            let entry = &state.call_info[state.call_info.len() - 2];
            (entry.name.clone(), entry.namewhat.clone())
        } else {
            state
                .call_info
                .last()
                .map(|e| (e.name.clone(), e.namewhat.clone()))
                .unwrap_or((String::new(), String::new()))
        };
        let is_main = proto.line_defined == 0;
        return Some((src, line, name, namewhat, is_main));
    }
    None
}

/// 将 source 字符串转换为 short_src
fn short_src_from_source(source: &str) -> String {
    const LUA_IDSIZE: usize = 60;
    const RETS: &str = "...";

    let bytes = source.as_bytes();
    match bytes.first() {
        Some(&b'=') => {
            let content = &bytes[1..];
            if content.len() <= LUA_IDSIZE {
                String::from_utf8_lossy(content).into_owned()
            } else {
                String::from_utf8_lossy(&content[..LUA_IDSIZE - 1]).into_owned()
            }
        }
        Some(&b'@') => {
            let content = &bytes[1..];
            if content.len() <= LUA_IDSIZE {
                String::from_utf8_lossy(content).into_owned()
            } else {
                let keep = LUA_IDSIZE - RETS.len();
                let start = content.len() - keep;
                format!("{}{}", RETS, String::from_utf8_lossy(&content[start..]))
            }
        }
        _ => source.to_string(),
    }
}

/// 格式化 traceback 的一行内容（不含前缀 "\n\t"）— 对应 C 的 luaL_traceback + pushfuncname
fn make_traceback_line(
    src: &str,
    line: i32,
    name: &str,
    namewhat: &str,
    is_main: bool,
    is_c: bool,
    linedefined: i32,
) -> String {
    let mut s = String::new();
    if line > 0 {
        s.push_str(&format!("{}:{}: in ", src, line));
    } else {
        s.push_str(&format!("{}: in ", src));
    }
    if !namewhat.is_empty() {
        s.push_str(&format!("{} '{}'", namewhat, name));
    } else if is_main {
        s.push_str("main chunk");
    } else if !is_c {
        s.push_str(&format!("function <{}:{}>", src, linedefined));
    } else if !name.is_empty() {
        s.push_str(&format!("function '{}'", name));
    } else {
        s.push_str("?");
    }
    s
}

/// 格式化 traceback 的一行 — 对应 C 的 luaL_traceback + pushfuncname
fn push_traceback_line(
    result: &mut String,
    src: &str,
    line: i32,
    name: &str,
    namewhat: &str,
    is_main: bool,
    is_c: bool,
    linedefined: i32,
) {
    result.push('\n');
    result.push('\t');
    result.push_str(&make_traceback_line(src, line, name, namewhat, is_main, is_c, linedefined));
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
        let t = Table::new();
        t.set_metatable(Some(Table::new()));
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
            TValue::Table(t) => assert!(t.has_metatable()),
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
                kind: 0,
            }],
            line_info: vec![],
            abs_line_info: vec![],
            loc_vars: vec![],
            source: None,
        };
        let closure = LClosure {
            gc_header: GCObjectHeader::new(),
            proto,
            upvals: Rc::new(RefCell::new(vec![Rc::new(RefCell::new(UpVal::Closed {
                value: Box::new(TValue::Integer(42)),
            }))])),
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
            upvals: Rc::new(RefCell::new(vec![])),
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
            upvals: Rc::new(RefCell::new(vec![Rc::new(RefCell::new(UpVal::Closed {
                value: Box::new(TValue::Integer(42)),
            }))])),
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
            upvals: Rc::new(RefCell::new(vec![])),
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
