//! Coroutine 库 (lcorolib.cpp → Rust)
//!
//! 对应 C 源码: lcorolib.cpp
//!
//! ## 主要功能
//! - 注册 coroutine 全局表，包含协程操作函数
//! - 提供 coroutine.create, coroutine.resume, coroutine.yield,
//!   coroutine.status, coroutine.wrap, coroutine.running, coroutine.isyieldable
//!
//! ## 标签分配
//! - 标签 700+: Coroutine 库

use crate::objects::{LuaThread, NilKind, ThreadStatus, TValue};
use crate::state::LuaState;
use crate::execute::VmError;

// ============================================================================
// 函数标签 (LightUserData 占位符值)
// ============================================================================

pub const CORO_CREATE: usize = 700;
pub const CORO_ISYIELDABLE: usize = 701;
pub const CORO_RESUME: usize = 702;
pub const CORO_RUNNING: usize = 703;
pub const CORO_STATUS: usize = 704;
pub const CORO_WRAP: usize = 705;
pub const CORO_YIELD: usize = 706;

/// Coroutine 库标签范围: [700, 710)
pub fn is_coro_tag(tag: usize) -> bool {
    (700..710).contains(&tag)
}

/// 将 coroutine 库函数 tag 映射到函数名（用于 traceback）
pub fn coro_function_name(tag: usize) -> Option<&'static str> {
    match tag {
        CORO_CREATE => Some("create"),
        CORO_ISYIELDABLE => Some("isyieldable"),
        CORO_RESUME => Some("resume"),
        CORO_RUNNING => Some("running"),
        CORO_STATUS => Some("status"),
        CORO_WRAP => Some("wrap"),
        CORO_YIELD => Some("yield"),
        _ => None,
    }
}

// ============================================================================
// 栈操作辅助函数
// ============================================================================

fn get_arg(state: &LuaState, a: usize, idx: usize) -> TValue {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return TValue::Nil(NilKind::Strict);
    }
    state.stack[stack_idx].clone()
}

fn push_single_result(state: &mut LuaState, a: usize, nresults: i32, result: TValue) {
    state.stack.truncate(a);
    if nresults != 0 {
        state.stack.push(result);
    }
    let current = state.stack.len() - a;
    if nresults > 0 && (current as i32) < nresults {
        for _ in current..nresults as usize {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
    }
}

// ============================================================================
// coroutine.create(f) — 对应 C 的 lua_cocreate
// ============================================================================

/// coroutine.create(f) — 创建一个新协程，主体为函数 f
///
/// 返回一个 thread 对象，状态为 "suspended"
fn call_create(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'create' (function expected)".to_string(),
        ));
    }
    let func = get_arg(state, a, 0);
    if !func.is_function() {
        return Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'create' (function expected, got {})",
            func.ty()
        )));
    }
    let thread = LuaThread {
        stack: Vec::new(),
        status: ThreadStatus::Suspended,
        function: Some(Box::new(func)),
    };
    push_single_result(state, a, nresults, TValue::Thread(thread));
    Ok(())
}

// ============================================================================
// coroutine.status(co) — 对应 C 的 lua_costatus
// ============================================================================

/// coroutine.status(co) — 返回协程的状态字符串
///
/// 返回值: "running", "suspended", "normal", "dead"
fn call_status(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'status' (thread expected)".to_string(),
        ));
    }
    let arg = get_arg(state, a, 0);
    let status_str = match &arg {
        TValue::Thread(t) => match t.status {
            ThreadStatus::Suspended => "suspended",
            ThreadStatus::Normal => "normal",
            ThreadStatus::OK => "dead",
            ThreadStatus::Error => "dead",
        },
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'status' (thread expected, got {})",
                arg.ty()
            )));
        }
    };
    push_single_result(state, a, nresults, TValue::Str(state.intern_str(status_str)));
    Ok(())
}

// ============================================================================
// coroutine.isyieldable() — 对应 C 的 lua_coyieldable
// ============================================================================

/// coroutine.isyieldable() — 返回当前协程是否可 yield
///
/// 主线程中返回 false
fn call_isyieldable(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    push_single_result(state, a, nresults, TValue::Boolean(false));
    Ok(())
}

// ============================================================================
// coroutine.running() — 对应 C 的 lua_corunning
// ============================================================================

/// coroutine.running() — 返回当前运行的协程
///
/// 主线程中返回 nil, true
fn call_running(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 主线程返回 nil (is-main = true)
    if nresults >= 1 {
        state.stack.truncate(a);
        state.stack.push(TValue::Nil(NilKind::Strict));
        if nresults >= 2 {
            state.stack.push(TValue::Boolean(true));
        }
        let current = state.stack.len() - a;
        for _ in current..nresults as usize {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
    } else if nresults == 0 {
        // 不需要结果
    } else {
        // nresults < 0 (MULTRET): 返回 2 个值
        state.stack.truncate(a);
        state.stack.push(TValue::Nil(NilKind::Strict));
        state.stack.push(TValue::Boolean(true));
    }
    Ok(())
}

// ============================================================================
// coroutine.resume(co, ...) — 对应 C 的 lua_coresume
// ============================================================================

/// coroutine.resume(co, ...) — 恢复协程执行
///
/// 当前 VM 不支持真正的协程执行，返回错误
fn call_resume(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'resume' (thread expected)".to_string(),
        ));
    }
    let arg = get_arg(state, a, 0);
    match &arg {
        TValue::Thread(_t) => {
            // VM 暂不支持协程执行，标记为 dead 并返回 false, error message
            // 对应 C: resume 尚未开始或已完成的协程
            // 返回 false, "cannot resume non-suspended coroutine"
            // 这里简化处理：标记线程为 dead 状态后返回 false + 错误消息
            // 实际上需要完整的 VM 协程支持才能正确执行
            push_single_result(state, a, nresults, TValue::Boolean(false));
            if nresults > 1 || nresults < 0 {
                state.stack.push(TValue::Str(
                    state.intern_str("cannot resume dead coroutine"),
                ));
            }
        }
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'resume' (thread expected, got {})",
                arg.ty()
            )));
        }
    }
    Ok(())
}

// ============================================================================
// coroutine.yield(...) — 对应 C 的 lua_coyield
// ============================================================================

/// coroutine.yield(...) — 挂起当前协程
///
/// 主线程中调用 yield 是错误的
fn call_yield(
    _state: &mut LuaState,
    _a: usize,
    _nargs: usize,
    _nresults: i32,
) -> Result<(), VmError> {
    Err(VmError::RuntimeError(
        "attempt to yield from outside a coroutine".to_string(),
    ))
}

// ============================================================================
// coroutine.wrap(f) — 对应 C 的 lua_cowrap
// ============================================================================

/// coroutine.wrap(f) — 创建协程并返回一个恢复函数
///
/// 返回一个函数，调用该函数即恢复协程
fn call_wrap(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'wrap' (function expected)".to_string(),
        ));
    }
    let func = get_arg(state, a, 0);
    if !func.is_function() {
        return Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'wrap' (function expected, got {})",
            func.ty()
        )));
    }
    // 创建协程，但返回一个函数而不是 thread 对象
    // 这里返回原始函数作为简化实现
    push_single_result(state, a, nresults, func);
    Ok(())
}

// ============================================================================
// 派发函数
// ============================================================================

/// 派发 Coroutine 库函数调用
pub fn call_coro_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    match tag {
        CORO_CREATE => call_create(state, a, nargs, nresults),
        CORO_ISYIELDABLE => call_isyieldable(state, a, nargs, nresults),
        CORO_RESUME => call_resume(state, a, nargs, nresults),
        CORO_RUNNING => call_running(state, a, nargs, nresults),
        CORO_STATUS => call_status(state, a, nargs, nresults),
        CORO_WRAP => call_wrap(state, a, nargs, nresults),
        CORO_YIELD => call_yield(state, a, nargs, nresults),
        _ => Ok(()),
    }
}

// ============================================================================
// 打开 Coroutine 库 — 对应 C 的 luaopen_coroutine
// ============================================================================

/// 打开 Coroutine 库并注册到全局变量 coroutine
pub fn open_coroutine_lib(state: &mut LuaState) {
    let mut lib = crate::table::Table::new();

    let register = |lib: &mut crate::table::Table, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        lib.set(key, TValue::LightUserData(tag as *mut std::ffi::c_void));
    };

    register(&mut lib, "create", CORO_CREATE);
    register(&mut lib, "isyieldable", CORO_ISYIELDABLE);
    register(&mut lib, "resume", CORO_RESUME);
    register(&mut lib, "running", CORO_RUNNING);
    register(&mut lib, "status", CORO_STATUS);
    register(&mut lib, "wrap", CORO_WRAP);
    register(&mut lib, "yield", CORO_YIELD);

    let key = TValue::Str(state.intern_str("coroutine"));
    state.globals.set(key, TValue::Table(lib));
}
