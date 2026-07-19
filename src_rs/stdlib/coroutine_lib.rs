//! Coroutine 库 (lcorolib.cpp → Rust)
//!
//! 对应 C 源码: lcorolib.cpp
//!
//! ## 主要功能
//! - 注册 coroutine 全局表，包含协程操作函数
//! - 提供 coroutine.create, coroutine.resume, coroutine.yield,
//!   coroutine.status, coroutine.wrap, coroutine.running, coroutine.isyieldable
//!
//! ## 实现
//! - 所有 coroutine 库函数（create/resume/yield 等）用 BuiltinFn 注册
//! - coroutine.wrap 返回 RustClosure（携带 upvalues[0] = Thread），
//!   由 op_call 的 RustClosure 分支派发到 call_wrap_fn

use crate::execute::{VmError, VmExecutor, VmResult};
use crate::objects::{BuiltinFn, LuaThread, NilKind, TValue, Table, ThreadContext, ThreadStatus, UpVal, UpValRef};
use crate::state::LuaState;
use std::cell::RefCell;
use std::rc::Rc;

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
    state.adjust_results(a, nresults, vec![result]);
}

/// 推送 resume 的结果: success flag + values，并根据 nresults 调整
fn push_resume_results(
    state: &mut LuaState,
    a: usize,
    nresults: i32,
    success: bool,
    values: Vec<TValue>,
) {
    state.stack.truncate(a);
    if nresults == 0 {
        return;
    }
    state.stack.push(TValue::Boolean(success));
    for v in values {
        state.stack.push(v);
    }
    if nresults > 0 {
        let current = state.stack.len() - a;
        if current > nresults as usize {
            state.stack.truncate(a + nresults as usize);
        } else {
            while (state.stack.len() - a) < nresults as usize {
                state.stack.push(TValue::Nil(NilKind::Strict));
            }
        }
    }
    state.top = state.stack.len();
}

/// 推送 resume 结果(从协程栈中直接读取返回值，避免创建中间 Vec)
/// co_stack 是取出的协程栈，返回值在 [result_base, result_base+n) 范围
fn push_resume_results_from_stack(
    state: &mut LuaState,
    a: usize,
    nresults: i32,
    success: bool,
    mut co_stack: Vec<TValue>,
    result_base: usize,
    n: usize,
) {
    state.stack.truncate(a);
    if nresults == 0 {
        return;
    }
    state.stack.push(TValue::Boolean(success));
    if nresults > 0 {
        // 固定结果数: 只需 nresults-1 个返回值，避免 push 全部 n 个值导致 OOM
        let nvals = (nresults as usize).saturating_sub(1);
        for i in 0..nvals {
            let val = if i < n && result_base + i < co_stack.len() {
                std::mem::take(&mut co_stack[result_base + i])
            } else {
                TValue::Nil(NilKind::Strict)
            };
            state.stack.push(val);
        }
        // 不足则补 nil
        while (state.stack.len() - a) < nresults as usize {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
    } else {
        // LUA_MULTRET: push 全部返回值
        for i in 0..n {
            let val = if result_base + i < co_stack.len() {
                std::mem::take(&mut co_stack[result_base + i])
            } else {
                TValue::Nil(NilKind::Strict)
            };
            state.stack.push(val);
        }
    }
    // co_stack 在此 drop，释放协程栈内存
    state.top = state.stack.len();
}

/// 推送 resume 错误结果: false + error message
fn push_resume_error(
    state: &mut LuaState,
    a: usize,
    nresults: i32,
    msg: &str,
) -> Result<(), VmError> {
    push_resume_results(
        state,
        a,
        nresults,
        false,
        vec![TValue::Str(state.intern_str(msg))],
    );
    Ok(())
}

// ============================================================================
// 调用者上下文保存/恢复 — 用于 call_resume 切换到协程上下文
// ============================================================================

/// 保存调用者的 VM 执行上下文到局部变量（使用 mem::take 避免克隆）
struct CallerContext {
    code: Rc<Vec<crate::objects::Instruction>>,
    constants: Rc<Vec<TValue>>,
    upval_descs: Rc<Vec<crate::objects::UpvalDesc>>,
    /// 调用者子原型列表 — Rc 共享，save/restore 时 O(1) 引用计数
    protos: Rc<Vec<Rc<crate::objects::Proto>>>,
    base: usize,
    pc: usize,
    top: usize,
    num_params: u8,
    is_vararg: bool,
    proto_flag: u8,
    nextraargs: i32,
    closure_upvals: Vec<crate::objects::UpValRef>,
    open_upvals: Vec<crate::objects::UpValRef>,
    open_upval: Option<usize>,
    tbc_list: Option<usize>,
    call_stack: Vec<crate::objects::CallFrame>,
    stack: Vec<TValue>,
    hook_old_pc: i32,
    hook_func: Option<TValue>,
    hook_mask: i32,
    hook_count: i32,
    current_hook_count: i32,
    allowhook: bool,
    current_thread: Option<Rc<RefCell<ThreadContext>>>,
    call_info: Vec<crate::state::CallInfoEntry>,
}

/// 保存调用者上下文（使用 mem::take 避免克隆）
fn save_caller_context(state: &mut LuaState) -> CallerContext {
    CallerContext {
        code: std::mem::take(&mut state.code),
        constants: std::mem::take(&mut state.constants),
        upval_descs: std::mem::take(&mut state.upval_descs),
        protos: std::mem::take(&mut state.protos),
        base: state.base,
        pc: state.pc,
        top: state.top,
        num_params: state.num_params,
        is_vararg: state.is_vararg,
        proto_flag: state.proto_flag,
        nextraargs: state.nextraargs,
        closure_upvals: std::mem::take(&mut state.closure_upvals),
        open_upvals: std::mem::take(&mut state.open_upvals),
        open_upval: state.open_upval,
        tbc_list: state.tbc_list,
        call_stack: std::mem::take(&mut state.call_stack),
        stack: std::mem::take(&mut state.stack),
        hook_old_pc: state.hook_old_pc,
        hook_func: state.hook_func.take(),
        hook_mask: state.hook_mask,
        hook_count: state.hook_count,
        current_hook_count: state.current_hook_count,
        allowhook: state.allowhook,
        current_thread: state.current_thread.take(),
        call_info: std::mem::take(&mut state.call_info),
    }
}

/// 恢复调用者上下文
fn restore_caller_context(state: &mut LuaState, ctx: CallerContext) {
    state.code = ctx.code;
    state.constants = ctx.constants;
    state.upval_descs = ctx.upval_descs;
    state.protos = ctx.protos;
    state.base = ctx.base;
    state.pc = ctx.pc;
    state.top = ctx.top;
    state.num_params = ctx.num_params;
    state.is_vararg = ctx.is_vararg;
    state.proto_flag = ctx.proto_flag;
    state.nextraargs = ctx.nextraargs;
    state.closure_upvals = ctx.closure_upvals;
    state.open_upvals = ctx.open_upvals;
    state.open_upval = ctx.open_upval;
    state.tbc_list = ctx.tbc_list;
    state.call_stack = ctx.call_stack;
    state.stack = ctx.stack;
    state.hook_old_pc = ctx.hook_old_pc;
    state.hook_func = ctx.hook_func;
    state.hook_mask = ctx.hook_mask;
    state.hook_count = ctx.hook_count;
    state.current_hook_count = ctx.current_hook_count;
    state.allowhook = ctx.allowhook;
    state.current_thread = ctx.current_thread;
    state.call_info = ctx.call_info;
    // 确保 top 与 stack 同步
    if state.top > state.stack.len() {
        state.top = state.stack.len();
    }
}

// ============================================================================
// 跨协程 upvalue 处理
// ============================================================================
// 协程使用独立栈执行，但闭包的开 upvalue 的 stack_index 指向父栈。
// 切换到协程栈后，Open upvalue 的 stack_index 失效。
// 方案：协程首次 resume 前，把开 upvalue 转为 Closed（值的副本）。
// 协程执行期间，SETUPVAL 修改 Closed 值。
// 协程退出时（yield/return/error），把 Closed 值同步回父栈原位置。
// 协程完全结束后（return/error），把 Closed 恢复为 Open（指向父栈）。

/// 开 upvalue 信息（首次 resume 时收集）
struct OpenUpvalInfo {
    uv_ref: Rc<RefCell<UpVal>>,
    original_stack_index: usize,
}

/// 第一步: 在 save_caller_context 之前，把开 upvalue 转为 Closed
/// 返回 (uv_ref, original_stack_index) 列表，供退出时同步
/// 首次 resume 时同时把信息保存到 ThreadContext，供 close_suspended_coroutine 使用
fn close_open_upvals(thread: &LuaThread, state: &mut LuaState) -> Vec<OpenUpvalInfo> {
    let mut result = Vec::new();
    if !thread.context.borrow().started {
        if let Some(boxed_func) = &thread.function {
            if let TValue::LClosure(closure) = boxed_func.as_ref() {
                // Lua 函数体: 递归收集所有可达 LClosure 的 Open upvalue（包括嵌套闭包的 upvalue）
                let mut visited = std::collections::HashSet::new();
                collect_and_close_upvals(
                    &closure.upvals.borrow(),
                    state,
                    &mut result,
                    &mut visited,
                );
            } else {
                // C 函数体 (如 pcall): 函数本身没有 upvalue，但其参数中可能有 LClosure
                // 这些 LClosure 的 Open upvalue 仍指向父栈，需要转为 Closed
                // 参数在 call_resume 中通过 state.stack[a+2..] 访问（resume_args 之前）
                // 但此时还未 save_caller_context，state.stack 仍是父栈
                // 直接扫描栈上的 LClosure 参数
                let mut visited = std::collections::HashSet::new();
                scan_stack_for_closures(state, &mut result, &mut visited);
            }
        }
        // 保存到 ThreadContext，供 close_suspended_coroutine 使用
        let origins: Vec<_> = result
            .iter()
            .map(|info| (info.uv_ref.clone(), info.original_stack_index))
            .collect();
        thread.context.borrow_mut().upval_origins = origins;
    }
    result
}

/// 扫描栈上的 LClosure 参数，收集其 Open upvalue
/// 用于 C 函数体协程（如 coroutine.create(pcall)）首次 resume 时
fn scan_stack_for_closures(
    state: &mut LuaState,
    result: &mut Vec<OpenUpvalInfo>,
    visited: &mut std::collections::HashSet<usize>,
) {
    let mut visited_tables = std::collections::HashSet::new();
    // 先 clone 栈上的 LClosure/Table 引用（避免遍历时借用 state.stack）
    let closures: Vec<Rc<RefCell<Vec<UpValRef>>>> = state
        .stack
        .iter()
        .filter_map(|v| {
            if let TValue::LClosure(closure) = v {
                Some(closure.upvals.clone())
            } else {
                None
            }
        })
        .collect();
    let tables: Vec<Table> = state
        .stack
        .iter()
        .filter_map(|v| {
            if let TValue::Table(t) = v {
                Some(t.clone())
            } else {
                None
            }
        })
        .collect();
    for upvals in &closures {
        collect_and_close_upvals_impl(
            &upvals.borrow(),
            state,
            result,
            visited,
            &mut visited_tables,
        );
    }
    for t in &tables {
        scan_table_and_close_upvals(t, state, result, visited, &mut visited_tables);
    }
}

/// 通过 Rc 指针关闭 upvalue（unlink + 设置为 Closed）
/// 协程场景下必须先从 open_upval 链表移除再设为 Closed，
/// 否则链表中残留的 Closed upvalue 会让 func::close 遍历中断（Closed 无 next 字段）
fn close_upval_by_ref(state: &mut LuaState, uv_ref: &Rc<RefCell<UpVal>>, val: TValue) {
    let ptr = Rc::as_ptr(uv_ref) as usize;
    if let Some(uv_idx) = state
        .open_upvals
        .iter()
        .position(|r| Rc::as_ptr(r) as usize == ptr)
    {
        crate::func::unlink_upval(state, uv_idx);
    }
    *uv_ref.borrow_mut() = UpVal::Closed {
        value: Box::new(val),
    };
}

/// 递归收集并关闭所有可达的 Open upvalue
/// 当 upvalue 的值是 LClosure 时，递归处理该闭包的 upvalue
/// 当 upvalue 的值是 Table 时，递归扫描 Table（包括元表）中的 LClosure
fn collect_and_close_upvals(
    upvals: &[Rc<RefCell<UpVal>>],
    state: &mut LuaState,
    result: &mut Vec<OpenUpvalInfo>,
    visited: &mut std::collections::HashSet<usize>,
) {
    let mut visited_tables = std::collections::HashSet::new();
    collect_and_close_upvals_impl(upvals, state, result, visited, &mut visited_tables);
}

fn collect_and_close_upvals_impl(
    upvals: &[Rc<RefCell<UpVal>>],
    state: &mut LuaState,
    result: &mut Vec<OpenUpvalInfo>,
    visited: &mut std::collections::HashSet<usize>,
    visited_tables: &mut std::collections::HashSet<usize>,
) {
    for uv_ref in upvals.iter() {
        let ptr = Rc::as_ptr(uv_ref) as usize;
        if !visited.insert(ptr) {
            continue;
        }
        // 读取当前 upvalue 状态和值
        let (is_open, original_idx, val) = {
            let uv = uv_ref.borrow();
            match &*uv {
                UpVal::Open { stack_index, .. } => {
                    let original_idx = *stack_index;
                    let val = state
                        .stack
                        .get(original_idx)
                        .cloned()
                        .unwrap_or(TValue::Nil(NilKind::Strict));
                    (true, original_idx, val)
                }
                UpVal::Closed { value } => (false, 0, (**value).clone()),
            }
        };
        // 如果是 Open，转为 Closed（先从链表移除，再设为 Closed）
        if is_open {
            close_upval_by_ref(state, uv_ref, val.clone());
            result.push(OpenUpvalInfo {
                uv_ref: uv_ref.clone(),
                original_stack_index: original_idx,
            });
        }
        // 递归处理 LClosure 的 upvalue
        if let TValue::LClosure(inner) = &val {
            collect_and_close_upvals_impl(
                &inner.upvals.borrow(),
                state,
                result,
                visited,
                visited_tables,
            );
        }
        // 递归扫描 Table 中的 LClosure（包括元表）
        if let TValue::Table(t) = &val {
            scan_table_and_close_upvals(t, state, result, visited, visited_tables);
        }
    }
}

/// 扫描 Table 中的 LClosure，关闭其 Open upvalue
/// 同时递归扫描嵌套 Table 和元表
fn scan_table_and_close_upvals(
    table: &Table,
    state: &mut LuaState,
    result: &mut Vec<OpenUpvalInfo>,
    visited: &mut std::collections::HashSet<usize>,
    visited_tables: &mut std::collections::HashSet<usize>,
) {
    let table_ptr = Rc::as_ptr(&table.data) as usize;
    if !visited_tables.insert(table_ptr) {
        return;
    }
    let data = table.data.borrow();
    // 扫描数组部分
    for v in data.array.iter() {
        match v {
            TValue::LClosure(closure) => {
                collect_and_close_upvals_impl(
                    &closure.upvals.borrow(),
                    state,
                    result,
                    visited,
                    visited_tables,
                );
            }
            TValue::Table(inner_t) => {
                scan_table_and_close_upvals(inner_t, state, result, visited, visited_tables);
            }
            _ => {}
        }
    }
    // 扫描哈希部分
    for (k, v) in &data.hash_buckets {
        match v {
            TValue::LClosure(closure) => {
                collect_and_close_upvals_impl(
                    &closure.upvals.borrow(),
                    state,
                    result,
                    visited,
                    visited_tables,
                );
            }
            TValue::Table(inner_t) => {
                scan_table_and_close_upvals(inner_t, state, result, visited, visited_tables);
            }
            _ => {}
        }
        match k {
            TValue::LClosure(closure) => {
                collect_and_close_upvals_impl(
                    &closure.upvals.borrow(),
                    state,
                    result,
                    visited,
                    visited_tables,
                );
            }
            TValue::Table(inner_t) => {
                scan_table_and_close_upvals(inner_t, state, result, visited, visited_tables);
            }
            _ => {}
        }
    }
    // 扫描元表
    if let Some(mt) = &data.metatable {
        scan_table_and_close_upvals(mt, state, result, visited, visited_tables);
    }
}

/// 关闭 hook 函数的 Open upvalue（供 debug.sethook 使用）
/// 把 hook 函数及其嵌套 LClosure 的 Open upvalue 转为 Closed，
/// 避免协程执行期间 state.stack 被替换后 upvalue 失效
pub fn close_hook_upvals(hook: &TValue, state: &mut LuaState) {
    if let TValue::LClosure(closure) = hook {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        collect_and_close_upvals(&closure.upvals.borrow(), state, &mut result, &mut visited);
    }
}

/// 收集 wrap 协程函数体的开 upvalue 信息（不关闭），返回 (uv_ref, original_stack_index, saved_value)
/// 在 call_wrap 时调用，保存到 ThreadContext.pending_wrap_upvals
/// 首次 resume 时根据同栈/跨栈决定用最新值还是 saved_value 关闭
fn collect_wrap_upvals_info(
    thread: &LuaThread,
    state: &LuaState,
) -> Vec<(UpValRef, usize, TValue)> {
    let mut result = Vec::new();
    if let Some(boxed_func) = &thread.function {
        let mut visited = std::collections::HashSet::new();
        if let TValue::LClosure(closure) = boxed_func.as_ref() {
            collect_open_upvals_recursive(
                &closure.upvals.borrow(),
                state,
                &mut result,
                &mut visited,
            );
        } else {
            // C 函数体: 扫描栈上的 LClosure 和 Table 参数
            let mut visited_tables = std::collections::HashSet::new();
            for v in state.stack.iter() {
                match v {
                    TValue::LClosure(closure) => {
                        collect_open_upvals_recursive_impl(
                            &closure.upvals.borrow(),
                            state,
                            &mut result,
                            &mut visited,
                            &mut visited_tables,
                        );
                    }
                    TValue::Table(t) => {
                        scan_table_and_collect_upvals(
                            t,
                            state,
                            &mut result,
                            &mut visited,
                            &mut visited_tables,
                        );
                    }
                    _ => {}
                }
            }
        }
    }
    result
}

/// 递归收集开 upvalue 信息（不关闭）
fn collect_open_upvals_recursive(
    upvals: &[Rc<RefCell<UpVal>>],
    state: &LuaState,
    result: &mut Vec<(UpValRef, usize, TValue)>,
    visited: &mut std::collections::HashSet<usize>,
) {
    let mut visited_tables = std::collections::HashSet::new();
    collect_open_upvals_recursive_impl(upvals, state, result, visited, &mut visited_tables);
}

fn collect_open_upvals_recursive_impl(
    upvals: &[Rc<RefCell<UpVal>>],
    state: &LuaState,
    result: &mut Vec<(UpValRef, usize, TValue)>,
    visited: &mut std::collections::HashSet<usize>,
    visited_tables: &mut std::collections::HashSet<usize>,
) {
    for uv_ref in upvals.iter() {
        let ptr = Rc::as_ptr(uv_ref) as usize;
        if !visited.insert(ptr) {
            continue;
        }
        let (is_open, original_idx, val) = {
            let uv = uv_ref.borrow();
            match &*uv {
                UpVal::Open { stack_index, .. } => {
                    let original_idx = *stack_index;
                    let val = state
                        .stack
                        .get(original_idx)
                        .cloned()
                        .unwrap_or(TValue::Nil(NilKind::Strict));
                    (true, original_idx, val)
                }
                UpVal::Closed { value } => (false, 0, (**value).clone()),
            }
        };
        if is_open {
            result.push((uv_ref.clone(), original_idx, val.clone()));
        }
        if let TValue::LClosure(inner) = &val {
            collect_open_upvals_recursive_impl(
                &inner.upvals.borrow(),
                state,
                result,
                visited,
                visited_tables,
            );
        }
        // 递归扫描 Table 中的 LClosure（包括元表）
        if let TValue::Table(t) = &val {
            scan_table_and_collect_upvals(t, state, result, visited, visited_tables);
        }
    }
}

/// 扫描 Table 中的 LClosure，收集其 Open upvalue 信息（不关闭）
fn scan_table_and_collect_upvals(
    table: &Table,
    state: &LuaState,
    result: &mut Vec<(UpValRef, usize, TValue)>,
    visited: &mut std::collections::HashSet<usize>,
    visited_tables: &mut std::collections::HashSet<usize>,
) {
    let table_ptr = Rc::as_ptr(&table.data) as usize;
    if !visited_tables.insert(table_ptr) {
        return;
    }
    let data = table.data.borrow();
    for v in data.array.iter() {
        match v {
            TValue::LClosure(closure) => {
                collect_open_upvals_recursive_impl(
                    &closure.upvals.borrow(),
                    state,
                    result,
                    visited,
                    visited_tables,
                );
            }
            TValue::Table(inner_t) => {
                scan_table_and_collect_upvals(inner_t, state, result, visited, visited_tables);
            }
            _ => {}
        }
    }
    for (k, v) in &data.hash_buckets {
        match v {
            TValue::LClosure(closure) => {
                collect_open_upvals_recursive_impl(
                    &closure.upvals.borrow(),
                    state,
                    result,
                    visited,
                    visited_tables,
                );
            }
            TValue::Table(inner_t) => {
                scan_table_and_collect_upvals(inner_t, state, result, visited, visited_tables);
            }
            _ => {}
        }
        match k {
            TValue::LClosure(closure) => {
                collect_open_upvals_recursive_impl(
                    &closure.upvals.borrow(),
                    state,
                    result,
                    visited,
                    visited_tables,
                );
            }
            TValue::Table(inner_t) => {
                scan_table_and_collect_upvals(inner_t, state, result, visited, visited_tables);
            }
            _ => {}
        }
    }
    if let Some(mt) = &data.metatable {
        scan_table_and_collect_upvals(mt, state, result, visited, visited_tables);
    }
}

/// 第二步: 协程退出后（restore_caller_context 之后），把 Closed 值同步回父栈
/// 如果协程已结束（return/error），恢复为 Open；否则保持 Closed（后续 resume 仍用 Closed）
/// write_back=false 时跳过写回栈（跨栈场景：父栈不可访问），但仍恢复 Open（若 co_finished）
fn sync_upvals_back(
    state: &mut LuaState,
    open_upvals: &[OpenUpvalInfo],
    co_finished: bool,
    write_back: bool,
) {
    for info in open_upvals {
        // 读取 Closed upvalue 的最新值
        let latest_val = {
            let uv = info.uv_ref.borrow();
            match &*uv {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { .. } => continue, // 已是 Open，跳过
            }
        };
        // 写入父栈原位置（跨栈时跳过：state.stack 不是原始父栈）
        if write_back && info.original_stack_index < state.stack.len() {
            state.stack[info.original_stack_index] = latest_val.clone();
        }
        // 协程已结束: 恢复为 Open（指向父栈原位置）并重新加入链表
        if co_finished {
            let need_relink = {
                let mut uv = info.uv_ref.borrow_mut();
                if let UpVal::Closed { .. } = &*uv {
                    *uv = UpVal::Open {
                        stack_index: info.original_stack_index,
                        next: None,
                        previous: None,
                        tbc: false,
                    };
                    true
                } else {
                    false
                }
            };
            if need_relink {
                let ptr = Rc::as_ptr(&info.uv_ref) as usize;
                if let Some(uv_idx) = state
                    .open_upvals
                    .iter()
                    .position(|r| Rc::as_ptr(r) as usize == ptr)
                {
                    relink_upval(state, uv_idx);
                }
            }
        }
    }
}

/// yield 时关闭 yield 出来的闭包的 Open upvalue（指向协程栈）
/// 在 saved_stack = take(state.stack) 之前调用（state.stack 仍是协程栈）
/// 返回 (uv_ref, original_stack_index) 列表，供 resume 时同步回协程栈
fn close_yield_upvals(yield_values: &[TValue], state: &mut LuaState) -> Vec<(UpValRef, usize)> {
    let mut result_info: Vec<OpenUpvalInfo> = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut visited_tables = std::collections::HashSet::new();
    for v in yield_values {
        match v {
            TValue::LClosure(closure) => {
                collect_and_close_upvals_impl(
                    &closure.upvals.borrow(),
                    state,
                    &mut result_info,
                    &mut visited,
                    &mut visited_tables,
                );
            }
            TValue::Table(t) => {
                scan_table_and_close_upvals(
                    t,
                    state,
                    &mut result_info,
                    &mut visited,
                    &mut visited_tables,
                );
            }
            _ => {}
        }
    }
    // 遍历 state.open_upval 链表，关闭所有剩余的 open upvalue。
    // 这些 upvalue 指向协程栈，yield 后 state.stack 切回主线程栈，
    // 若不关闭，外部持有的闭包（前一次 yield 传出但不在本次 yield 值中）
    // 会通过 Open upvalue 访问主线程栈的错误位置。
    // 跳过 TBC upvalue：它们只在协程内部通过 func::close 访问，
    // yield 后协程栈被保存到 ThreadContext，resume 时恢复，TBC upvalue 仍指向正确位置。
    // 若关闭 TBC upvalue，sync_yield_upvals_back 恢复 Open 时会丢失 tbc 标记，
    // 导致后续 close 不调用 __close。
    // 先收集要关闭的 uv_idx（遍历链表时不能修改链表），再逐个 unlink + close
    let mut to_close: Vec<(usize, usize)> = Vec::new(); // (uv_idx, stack_index)
    let mut current = state.open_upval;
    while let Some(uv_idx) = current {
        if uv_idx >= state.open_upvals.len() {
            break;
        }
        let uv_ref = state.open_upvals[uv_idx].clone();
        let (stack_index, next, is_open, is_tbc) = {
            let uv = uv_ref.borrow();
            match &*uv {
                UpVal::Open {
                    stack_index,
                    next,
                    tbc,
                    ..
                } => (*stack_index, *next, true, *tbc),
                UpVal::Closed { .. } => (0, None, false, false),
            }
        };
        if is_open && !is_tbc {
            let ptr = Rc::as_ptr(&uv_ref) as usize;
            if visited.insert(ptr) {
                to_close.push((uv_idx, stack_index));
            }
        }
        current = next;
    }
    for (uv_idx, stack_index) in to_close {
        let val = state
            .stack
            .get(stack_index)
            .cloned()
            .unwrap_or(TValue::Nil(NilKind::Strict));
        let uv_ref = state.open_upvals[uv_idx].clone();
        crate::func::unlink_upval(state, uv_idx);
        *uv_ref.borrow_mut() = UpVal::Closed {
            value: Box::new(val),
        };
        result_info.push(OpenUpvalInfo {
            uv_ref,
            original_stack_index: stack_index,
        });
    }
    result_info
        .into_iter()
        .map(|info| (info.uv_ref, info.original_stack_index))
        .collect()
}

/// 把已有的 Open upvalue 重新插入 open_upval 链表（按 stack_index 降序）
/// 用于 sync_yield_upvals_back 恢复 Open 后重新加入链表，
/// 否则后续 close_yield_upvals 遍历链表时找不到该 upvalue
fn relink_upval(state: &mut LuaState, uv_idx: usize) {
    let stack_index = {
        let uv = state.open_upvals[uv_idx].borrow();
        match &*uv {
            UpVal::Open { stack_index, .. } => *stack_index,
            _ => return,
        }
    };
    let mut prev: Option<usize> = None;
    let mut current = state.open_upval;
    while let Some(idx) = current {
        if idx == uv_idx {
            return; // 已在链表中
        }
        let (cur_level, next) = {
            let uv = state.open_upvals[idx].borrow();
            match &*uv {
                UpVal::Open {
                    stack_index, next, ..
                } => (*stack_index, *next),
                _ => break,
            }
        };
        if cur_level < stack_index {
            break;
        }
        prev = Some(idx);
        current = next;
    }
    let next_node = current;
    {
        let mut uv = state.open_upvals[uv_idx].borrow_mut();
        if let UpVal::Open {
            ref mut previous,
            ref mut next,
            ..
        } = &mut *uv
        {
            *previous = prev;
            *next = next_node;
        }
    }
    match prev {
        Some(p_idx) => {
            let mut p = state.open_upvals[p_idx].borrow_mut();
            if let UpVal::Open { ref mut next, .. } = &mut *p {
                *next = Some(uv_idx);
            }
        }
        None => {
            state.open_upval = Some(uv_idx);
        }
    }
    if let Some(n_idx) = next_node {
        let mut n = state.open_upvals[n_idx].borrow_mut();
        if let UpVal::Open {
            ref mut previous, ..
        } = &mut *n
        {
            *previous = Some(uv_idx);
        }
    }
}

/// resume 时把 yield 时关闭的 upvalue 的 Closed 值同步回协程栈，并恢复 Open
/// 在 setup_subsequent_resume 恢复 state.stack 之后调用
fn sync_yield_upvals_back(state: &mut LuaState, origins: &[(UpValRef, usize)]) {
    for (uv_ref, stack_index) in origins {
        let val = {
            let uv = uv_ref.borrow();
            match &*uv {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { .. } => continue, // 已是 Open，跳过
            }
        };
        // 写回协程栈
        if *stack_index < state.stack.len() {
            state.stack[*stack_index] = val;
        }
        // 恢复为 Open（指向协程栈）
        *uv_ref.borrow_mut() = UpVal::Open {
            stack_index: *stack_index,
            next: None,
            previous: None,
            tbc: false,
        };
        // 重新加入 open_upval 链表
        let ptr = Rc::as_ptr(uv_ref) as usize;
        if let Some(uv_idx) = state
            .open_upvals
            .iter()
            .position(|r| Rc::as_ptr(r) as usize == ptr)
        {
            relink_upval(state, uv_idx);
        }
    }
}

// ============================================================================
// coroutine.create(f) — 对应 C 的 lua_cocreate
// ============================================================================

/// 判断值是否可作为协程主体调用：
/// - 真正的函数（LClosure/CClosure/LCFn/BuiltinFn）
/// - LightUserData 形式的内置函数（tag 落入内置范围）
/// - 带 __call 元方法的 Table
fn is_callable(v: &TValue) -> bool {
    v.is_callable() || matches!(v, TValue::Table(_))
}

fn call_create(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'create' (function expected)".to_string(),
        ));
    }
    let func = get_arg(state, a, 0);
    if !is_callable(&func) {
        return Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'create' (function expected, got {})",
            func.ty()
        )));
    }
    let context = Rc::new(RefCell::new(ThreadContext::default()));
    // 初始化状态为 Suspended（Default 已是 Suspended，显式设置以示清晰）
    context.borrow_mut().status = ThreadStatus::Suspended;
    let thread = LuaThread {
        stack: Vec::new(),
        status: ThreadStatus::Suspended,
        function: Some(Box::new(func)),
        is_main: false,
        context,
    };
    push_single_result(state, a, nresults, TValue::Thread(Rc::new(thread)));
    Ok(())
}

// ============================================================================
// coroutine.status(co) — 对应 C 的 lua_costatus
// ============================================================================

fn call_status(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'status' (thread expected)".to_string(),
        ));
    }
    let arg = get_arg(state, a, 0);
    let status_str = match &arg {
        TValue::Thread(t) => {
            if t.is_main {
                // 主线程始终 "running"（简化处理）
                "running"
            } else {
                // 检查是否为当前正在运行的协程
                let is_current = state
                    .current_thread
                    .as_ref()
                    .map(|ctx| Rc::ptr_eq(ctx, &t.context))
                    .unwrap_or(false);
                if is_current {
                    "running"
                } else {
                    // 从共享的 ThreadContext 读取状态
                    let st = t.context.borrow().status;
                    match st {
                        ThreadStatus::Suspended => "suspended",
                        ThreadStatus::Normal => "normal",
                        ThreadStatus::OK => "dead",
                        ThreadStatus::Error => "dead",
                    }
                }
            }
        }
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'status' (thread expected, got {})",
                arg.ty()
            )));
        }
    };
    push_single_result(
        state,
        a,
        nresults,
        TValue::Str(state.intern_str(status_str)),
    );
    Ok(())
}

// ============================================================================
// coroutine.close(co) — 对应 C 的 lua_coclose
// ============================================================================

fn call_close(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 1 {
        // 对应 C Lua 的 getoptco: 无参数时关闭当前协程自身
        // C Lua 中 coroutine.close() 会调用 lua_closethread(co, L) 立即关闭所有 TBC 变量，
        // 并通过 luaD_throwbaselevel 抛到 base level。我们的实现未完整支持此语义，
        // 改为设置 force_noyield_close 标志，让后续 OP_RETURN 的 func::close 使用
        // 不可 yield 模式 (yy=0)，使 __close 中的 yield 失败。
        state.force_noyield_close = true;
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'close' (thread expected)".to_string(),
        ));
    }
    let arg = get_arg(state, a, 0);
    let thread = match &arg {
        TValue::Thread(t) => t.clone(),
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'close' (thread expected, got {})",
                arg.ty()
            )));
        }
    };

    // 主线程不可关闭
    if thread.is_main {
        // 判断 main 当前状态: 若在协程中执行,main 是 "normal";否则 "running"
        let in_coroutine = state.current_thread.is_some();
        if in_coroutine {
            return Err(VmError::RuntimeError(
                "cannot close a normal coroutine".to_string(),
            ));
        } else {
            return Err(VmError::RuntimeError(
                "cannot close the main thread".to_string(),
            ));
        }
    }

    let co_status = thread.context.borrow().status;
    match co_status {
        ThreadStatus::Normal => {
            // 正在运行的协程：如果是当前协程自身（close itself，在 __close 内调用），
            // 返回 (true, nil)（对应 C 的 lua_closethread(co, co) close itself）；
            // 否则报错
            let is_current = state
                .current_thread
                .as_ref()
                .map(|ct| Rc::ptr_eq(ct, &thread.context))
                .unwrap_or(false);
            if is_current {
                push_resume_results(state, a, nresults, true, Vec::new());
                return Ok(());
            }
            return Err(VmError::RuntimeError(
                "cannot close a normal coroutine".to_string(),
            ));
        }
        ThreadStatus::OK => {
            // 已正常结束的协程: 返回 true, nil
            push_resume_results(state, a, nresults, true, Vec::new());
            return Ok(());
        }
        ThreadStatus::Error => {
            // 错误结束的协程: 返回 false + 错误值，并将状态改为 OK
            // (对应 C 的 lua_closethread: 错误后关闭，后续 close 返回 true)
            let err = thread
                .context
                .borrow()
                .error_msg
                .clone()
                .unwrap_or_else(|| TValue::Str(state.intern_str("unknown error")));
            thread.context.borrow_mut().status = ThreadStatus::OK;
            thread.context.borrow_mut().error_msg = None;
            push_resume_results(state, a, nresults, false, vec![err]);
            return Ok(());
        }
        ThreadStatus::Suspended => {
            // 挂起的协程: 切换到协程上下文，运行 to-be-closed 变量的 __close metamethod
            // 对应 C 的 lua_coclose → luaD_closeprotected → luaF_close
            return close_suspended_coroutine(state, &thread, a, nresults);
        }
    }
}

// ============================================================================
// close_suspended_coroutine — 关闭挂起的协程，运行 to-be-closed 变量
// ============================================================================

/// 关闭挂起的协程：切换到协程上下文，运行所有 to-be-closed 变量的 __close metamethod
/// 对应 C 的 lua_coclose → luaD_closeprotected → luaF_close(L, base, status, 0)
fn close_suspended_coroutine(
    state: &mut LuaState,
    thread: &LuaThread,
    a: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 保存调用者上下文
    let caller_ctx = save_caller_context(state);
    let saved_n_ccalls = state.n_ccalls;

    // 从 ThreadContext 恢复协程上下文（不推入 resume 参数，因为我们要关闭而非恢复）
    let co_context = thread.context.clone();
    {
        let ctx = co_context.borrow();
        state.code = ctx.saved_code.clone();
        state.constants = ctx.saved_constants.clone();
        state.upval_descs = ctx.saved_upval_descs.clone();
        state.protos = ctx.saved_protos.clone();
        state.base = ctx.saved_base;
        state.pc = ctx.saved_pc;
        state.num_params = ctx.saved_num_params;
        state.is_vararg = ctx.saved_is_vararg;
        state.proto_flag = ctx.saved_proto_flag;
        state.nextraargs = ctx.saved_nextraargs;
        state.closure_upvals = ctx.saved_closure_upvals.clone();
        state.open_upvals = ctx.saved_open_upvals.clone();
        state.open_upval = ctx.saved_open_upval;
        state.tbc_list = ctx.saved_tbc_list;
        state.call_stack = ctx.saved_call_stack.clone();
        state.stack = ctx.saved_stack.clone();
        state.top = ctx.saved_top;
        state.hook_old_pc = ctx.saved_hook_old_pc;
        state.call_info = Vec::new();
    }

    // 设置 current_thread 和状态
    state.current_thread = Some(co_context.clone());
    co_context.borrow_mut().status = ThreadStatus::Normal;

    // 保存 close 前的 last_error_msg 状态（用于检测 __close 是否出错）
    let saved_err_msg = state.last_error_msg.clone();
    let saved_err_value = state.last_error_value.take();
    state.last_error_msg.clear();

    // 调用 close 关闭所有 TBC upvalue（运行 __close metamethod）
    // close 函数内部对 TBC upvalue 调用 call_close_method，使用 pcall 处理错误
    // status=0 表示正常关闭（err 参数为 nil）
    crate::func::close(state, state.base, 0, 1).ok();

    // 检查 close 过程中是否有错误
    let close_error: Option<TValue> = if !state.last_error_msg.is_empty() {
        // __close 出错：提取错误值
        let err_val = state
            .last_error_value
            .take()
            .unwrap_or_else(|| TValue::Str(state.intern_str(&state.last_error_msg.clone())));
        Some(err_val)
    } else {
        None
    };

    // 清空协程的 ThreadContext（协程已结束）
    {
        let mut ctx = co_context.borrow_mut();
        ctx.saved_code = Rc::new(Vec::new());
        ctx.saved_constants = Rc::new(Vec::new());
        ctx.saved_upval_descs = Rc::new(Vec::new());
        ctx.saved_protos = Rc::new(Vec::new());
        ctx.saved_closure_upvals = Vec::new();
        ctx.saved_call_stack = Vec::new();
        ctx.saved_stack = Vec::new();
        ctx.saved_open_upval = None;
        ctx.saved_tbc_list = None;
        ctx.status = ThreadStatus::OK;
        ctx.error_msg = None;
    }

    // 恢复调用者上下文
    restore_caller_context(state, caller_ctx);
    state.n_ccalls = saved_n_ccalls;

    // 恢复 last_error_msg 状态（清理 close 期间的错误）
    state.last_error_msg = saved_err_msg;
    state.last_error_value = saved_err_value;

    // 同步 upvalue（co_finished=true）
    // 使用 ThreadContext 中保存的 upval_origins（首次 resume 时收集）
    let origins = co_context.borrow().upval_origins.clone();
    let open_upvals: Vec<OpenUpvalInfo> = origins
        .into_iter()
        .map(|(uv_ref, original_stack_index)| OpenUpvalInfo {
            uv_ref,
            original_stack_index,
        })
        .collect();
    sync_upvals_back(state, &open_upvals, true, true);

    // 推送结果
    let (success, values) = match close_error {
        Some(err) => (false, vec![err]),
        None => (true, Vec::new()),
    };
    push_resume_results(state, a, nresults, success, values);

    Ok(())
}

// ============================================================================
// coroutine.isyieldable([co]) — 对应 C 的 lua_coyieldable
// ============================================================================

fn call_isyieldable(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let yieldable = if nargs >= 1 {
        let arg = get_arg(state, a, 0);
        match &arg {
            TValue::Thread(t) => !t.is_main,
            _ => false,
        }
    } else {
        // 无参数：当前是否可 yield（在协程中且无非可 yield 的 C 函数调用）
        state.current_thread.is_some() && state.n_ny_calls == 0
    };
    push_single_result(state, a, nresults, TValue::Boolean(yieldable));
    Ok(())
}

// ============================================================================
// coroutine.running() — 对应 C 的 lua_corunning
// ============================================================================

fn call_running(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let (thread_val, ismain) = match &state.current_thread {
        Some(ctx) => {
            // 在协程中 — 返回该协程 + false
            let thread = LuaThread {
                stack: Vec::new(),
                status: ctx.borrow().status,
                function: None,
                is_main: false,
                context: ctx.clone(),
            };
            (TValue::Thread(Rc::new(thread)), false)
        }
        None => {
            // 主线程 — 返回 main_thread + true
            (TValue::Thread(Rc::new(state.main_thread.clone())), true)
        }
    };

    state.stack.truncate(a);
    if nresults >= 1 {
        state.stack.push(thread_val);
        if nresults >= 2 {
            state.stack.push(TValue::Boolean(ismain));
        }
        let current = state.stack.len() - a;
        for _ in current..nresults as usize {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
    } else if nresults < 0 {
        // MULTRET
        state.stack.push(thread_val);
        state.stack.push(TValue::Boolean(ismain));
    }
    state.top = state.stack.len();
    Ok(())
}

// ============================================================================
// coroutine.resume(co, ...) — 对应 C 的 lua_coresume
// ============================================================================

fn call_resume(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'resume' (thread expected)".to_string(),
        ));
    }
    let arg = get_arg(state, a, 0);
    let thread = match &arg {
        TValue::Thread(t) => t.clone(),
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'resume' (thread expected, got {})",
                arg.ty()
            )));
        }
    };

    // 主线程不可 resume
    if thread.is_main {
        return push_resume_error(state, a, nresults, "cannot resume non-suspended coroutine");
    }

    // 检查协程状态
    let co_status = thread.context.borrow().status;
    match co_status {
        ThreadStatus::Suspended => {
            // OK to resume
        }
        ThreadStatus::Normal => {
            return push_resume_error(state, a, nresults, "cannot resume non-suspended coroutine");
        }
        ThreadStatus::OK | ThreadStatus::Error => {
            return push_resume_error(state, a, nresults, "cannot resume dead coroutine");
        }
    }

    // 收集 resume 参数（thread 之后的参数）
    let resume_args: Vec<TValue> = if nargs > 1 {
        (0..nargs - 1)
            .map(|i| state.stack[a + 2 + i].clone())
            .collect()
    } else {
        Vec::new()
    };

    // 收集开 upvalue 信息（在 save_caller_context 之前，state.stack 仍是父栈）
    let open_upvals = close_open_upvals(&thread, state);

    // 保存调用者上下文
    let caller_ctx = save_caller_context(state);
    // 保存 n_ccalls (协程执行期间可能修改,退出时恢复)
    let saved_n_ccalls = state.n_ccalls;
    // 保存 n_ny_calls 并重置为 0（协程初始状态是可 yield 的）
    let saved_n_ny_calls = state.n_ny_calls;
    state.n_ny_calls = 0;
    // 保存 force_noyield_close 并重置为 false
    // 协程内 coroutine.close() 关闭自身时设置，只在协程内有效；
    // 调用者的 force_noyield_close 不应泄漏到协程，反之亦然。
    let saved_force_noyield_close = state.force_noyield_close;
    state.force_noyield_close = false;
    // 保存 pcall_protection_stack 长度（协程内部 push 的 PcallProtection 不应泄漏到外部）
    let saved_pcall_protection_len = state.pcall_protection_stack.len();

    // 设置协程上下文
    let co_context = thread.context.clone();
    let is_first_resume = !co_context.borrow().started;

    let setup_result = if is_first_resume {
        // 首次 resume — 从协程体函数初始化
        setup_first_resume(state, &thread, &resume_args)
    } else {
        // 后续 resume — 从 ThreadContext 恢复
        setup_subsequent_resume(state, &co_context, &resume_args)
    };

    if let Err(e) = setup_result {
        restore_caller_context(state, caller_ctx);
        // 恢复 n_ccalls (协程 yield/error 可能导致 n_ccalls 不准确)
        state.n_ccalls = saved_n_ccalls;
        state.n_ny_calls = saved_n_ny_calls;
        state.force_noyield_close = saved_force_noyield_close;
        return Err(e);
    }

    // 从 ThreadContext 恢复协程的 hook 设置到 state
    // (save_caller_context 已保存主线程的 hook，restore_caller_context 会恢复)
    {
        let ctx = co_context.borrow();
        state.hook_func = ctx.saved_hook_func.clone();
        state.hook_mask = ctx.saved_hook_mask;
        state.hook_count = ctx.saved_hook_count;
        state.current_hook_count = ctx.saved_current_hook_count;
        state.allowhook = ctx.saved_allowhook;
    }

    // 递增 n_ccalls 防止协程嵌套过深导致 Rust 栈溢出
    // (对应 C Lua 的 luaD_resume: L->ci->nCcalls = L->nCcalls + 1)
    state.n_ccalls = state.n_ccalls.saturating_add(1);
    if state.n_ccalls >= crate::state::LUAI_MAXCCALLS {
        state.n_ccalls = saved_n_ccalls;
        state.n_ny_calls = saved_n_ny_calls;
        state.force_noyield_close = saved_force_noyield_close;
        restore_caller_context(state, caller_ctx);
        return push_resume_error(state, a, nresults, "C stack overflow");
    }

    // 设置 current_thread 和状态
    state.current_thread = Some(co_context.clone());
    co_context.borrow_mut().status = ThreadStatus::Normal;

    // 首次 resume 时触发 call hook（对应 C Lua 的 luaD_hook(L, LUA_HOOKCALL, -1, 0, 0)）
    if is_first_resume && state.hook_mask & 1 != 0 && state.hook_func.is_some() {
        VmExecutor::call_hook(state, "call", -1, None, 0, 0)?;
    }

    // 调用 execute_loop
    let exec_result = VmExecutor::execute_with_state(state);

    // 处理结果
    // co_stack_info: Return 分支取出协程整个栈，避免 clone 大量返回值导致 OOM
    // (stack, result_base, n) — 从取出的栈的 [result_base, result_base+n) 读取返回值
    let (success, result_values, mut co_stack_info): (
        bool,
        Vec<TValue>,
        Option<(Vec<TValue>, usize, usize)>,
    ) = match exec_result {
        Ok(VmResult::Yield { values }) => {
            // 关闭 yield 出来的闭包的 Open upvalue（协程栈还有效时）
            let yield_origins = close_yield_upvals(&values, state);
            // 协程 yield — 保存上下文到 ThreadContext
            {
                let mut ctx = co_context.borrow_mut();
                ctx.saved_code = std::mem::take(&mut state.code);
                ctx.saved_constants = std::mem::take(&mut state.constants);
                ctx.saved_upval_descs = std::mem::take(&mut state.upval_descs);
                ctx.saved_protos = std::mem::take(&mut state.protos);
                ctx.saved_base = state.base;
                // C 函数 __close (如 coroutine.yield 作为 __close) yield 时，
                // state.pc 指向 OP_RETURN/OP_CLOSE（非 CALL 指令），不应 +1。
                // 此时 PcallProtection.saved_pc == state.pc（都指向 OP_RETURN/OP_CLOSE）。
                // Lua __close yield 时，state.pc 指向 __close 中的 CALL 指令，
                // saved_pc 指向 OP_RETURN/OP_CLOSE，二者不同，需要 +1 跳过 CALL。
                let is_c_close_yield = state.pcall_protection_stack.last().map_or(false, |t| {
                    t.is_close_continuation && t.saved_filled && t.saved_pc == state.pc
                });
                ctx.saved_pc = if is_c_close_yield {
                    state.pc
                } else {
                    state.pc + 1
                };
                ctx.saved_top = state.top;
                ctx.saved_num_params = state.num_params;
                ctx.saved_is_vararg = state.is_vararg;
                ctx.saved_proto_flag = state.proto_flag;
                ctx.saved_nextraargs = state.nextraargs;
                ctx.saved_closure_upvals = std::mem::take(&mut state.closure_upvals);
                ctx.saved_open_upvals = std::mem::take(&mut state.open_upvals);
                ctx.saved_open_upval = state.open_upval;
                ctx.saved_tbc_list = state.tbc_list;
                ctx.saved_call_stack = std::mem::take(&mut state.call_stack);
                ctx.saved_stack = std::mem::take(&mut state.stack);
                ctx.saved_call_info = std::mem::take(&mut state.call_info);
                ctx.yield_upval_origins = yield_origins;
                ctx.saved_hook_old_pc = state.hook_old_pc;
                ctx.saved_hook_func = state.hook_func.take();
                ctx.saved_hook_mask = state.hook_mask;
                ctx.saved_hook_count = state.hook_count;
                ctx.saved_current_hook_count = state.current_hook_count;
                ctx.saved_allowhook = state.allowhook;
                ctx.saved_pcall_protection_stack =
                    state.pcall_protection_stack[saved_pcall_protection_len..].to_vec();
                // 保存 close_error_status（close continuation 的 pending error）
                // 对应 C Lua 的 CIST_RECST 保存的错误状态，跨 yield/resume 保留
                ctx.saved_close_error_status = state.close_error_status.take();
                ctx.status = ThreadStatus::Suspended;
                // saved_yield_nresults 已由 call_yield 设置
            }
            // yield 时从 state 中移除协程的 PcallProtection（避免污染调用者 state）
            state
                .pcall_protection_stack
                .truncate(saved_pcall_protection_len);
            (true, values, None)
        }
        Ok(VmResult::Return {
            nresults: ret_n,
            result_base,
        }) => {
            // 协程返回 — 取出协程整个栈(用 mem::take 避免分配新 Vec)
            // 返回值在 stack[result_base..result_base+ret_n]，后续从取出的栈中读取
            let co_stack = std::mem::take(&mut state.stack);
            {
                let mut ctx = co_context.borrow_mut();
                ctx.status = ThreadStatus::OK;
                ctx.started = true;
                // 协程结束，清空 saved_call_info（对应 C 中协程 dead 后 ci 链为空）
                ctx.saved_call_info.clear();
            }
            (true, Vec::new(), Some((co_stack, result_base, ret_n)))
        }
        Ok(_) => {
            let mut ctx = co_context.borrow_mut();
            ctx.status = ThreadStatus::OK;
            ctx.saved_call_info.clear();
            (true, Vec::new(), None)
        }
        Err(e) => {
            // 保存 error 状态到 ctx — 对应 C 中 lua_resume 不可恢复错误时
            // CallInfo 链不展开（luaD_throw longjmp 跳过正常清理），
            // 保留 error 时的状态供 debug.traceback 使用。
            // 必须在 TBC 关闭之前保存（TBC 关闭会修改 stack[base-1]）。
            // 只克隆到 base 的部分栈（build_traceback_from_thread 只需要
            // saved_stack[saved_base-1] 处的 LClosure），避免克隆整个栈导致 OOM。
            {
                let mut ctx = co_context.borrow_mut();
                ctx.saved_call_info = std::mem::take(&mut state.call_info);
                let save_end = state.base.min(state.stack.len());
                ctx.saved_stack = state.stack[..save_end].to_vec();
                ctx.saved_base = state.base;
                ctx.saved_pc = state.pc.wrapping_add(1);
            }
            // 第二次 resume 时 pcall 的保护已丢失（state.pcall 的 saved 状态是局部变量，
            // yield 后被销毁）。需要在此手动关闭协程（foo）的 TBC 变量，
            // 对应 C Lua 中 pcall 错误时 luaD_closeprotected -> luaF_close 的行为。
            let close_level = state.base;
            if close_level > 0 && close_level <= state.stack.len() {
                // 设为 nil 让 debug.getinfo 返回 "C"（对应 pcall 的 C 函数帧）
                state.stack[close_level - 1] = TValue::Nil(NilKind::Strict);
            }
            let _ = crate::func::close(state, close_level, 1, 0);
            // 获取最终错误值（经过 __close 错误传播后）
            let final_err = state.last_error_value.take().unwrap_or_else(|| match &e {
                VmError::RuntimeErrorValue(val) => val.clone(),
                _ => {
                    let msg = if !state.last_error_msg.is_empty() {
                        state.last_error_msg.clone()
                    } else {
                        format!("{}", e)
                    };
                    TValue::Str(state.intern_str(&msg))
                }
            });

            // 检查协程体是否为 pcall/xpcall（C 函数提供错误保护）
            // 第二次 resume 时这些 C 函数的保护丢失，但语义上错误应被它们捕获，
            // 协程正常返回 (false, err) 而非报错。
            // base 库已迁移到 BuiltinFn，通过比较函数指针判定 pcall/xpcall。
            let body_is_protective = thread
                .function
                .as_ref()
                .map(|f| {
                    if let TValue::BuiltinFn(bf) = f.as_ref() {
                        let func_ptr = bf.func as *const () as usize;
                        func_ptr == crate::stdlib::base_lib::call_pcall as *const () as usize
                            || func_ptr == crate::stdlib::base_lib::call_xpcall as *const () as usize
                    } else {
                        false
                    }
                })
                .unwrap_or(false);

            if body_is_protective {
                // pcall/xpcall 捕获了错误，协程正常返回 (false, err)
                // resume 返回 (true, false, err)
                co_context.borrow_mut().status = ThreadStatus::OK;
                co_context.borrow_mut().error_msg = None;
                (true, vec![TValue::Boolean(false), final_err], None)
            } else {
                // 协程错误，resume 返回 (false, err)
                co_context.borrow_mut().status = ThreadStatus::Error;
                co_context.borrow_mut().error_msg = Some(final_err.clone());
                let result_val = match &final_err {
                    TValue::Str(_) => {
                        let msg = if !state.last_error_msg.is_empty() {
                            state.last_error_msg.clone()
                        } else {
                            format!("{}", e)
                        };
                        TValue::Str(state.intern_str(&msg))
                    }
                    _ => final_err,
                };
                (false, vec![result_val], None)
            }
        }
    };

    // 判断协程是否已结束（return/error）
    let co_finished = matches!(
        co_context.borrow().status,
        ThreadStatus::OK | ThreadStatus::Error
    );

    // 恢复调用者上下文
    restore_caller_context(state, caller_ctx);
    // 恢复 n_ccalls (协程 yield/error 可能导致 n_ccalls 不准确)
    state.n_ccalls = saved_n_ccalls;
    state.n_ny_calls = saved_n_ny_calls;
    // 恢复调用者的 force_noyield_close（协程内的设置不泄漏到调用者）
    state.force_noyield_close = saved_force_noyield_close;
    // 协程结束时清理 pcall_protection_stack（移除协程内部 push 的 PcallProtection）
    if co_finished {
        state
            .pcall_protection_stack
            .truncate(saved_pcall_protection_len);
    }

    // 把 Closed upvalue 值同步回父栈，协程结束则恢复 Open
    if is_first_resume {
        sync_upvals_back(state, &open_upvals, co_finished, true);
    } else {
        // 后续 resume：从 upval_origins 恢复 open_upvals 信息，
        // yield 时同步 Closed 值回父栈（保持 Closed）；结束时恢复 Open。
        let origins = co_context.borrow().upval_origins.clone();
        let dead_upvals: Vec<OpenUpvalInfo> = origins
            .into_iter()
            .map(|(uv_ref, idx)| OpenUpvalInfo {
                uv_ref,
                original_stack_index: idx,
            })
            .collect();
        sync_upvals_back(state, &dead_upvals, co_finished, true);
    }

    // 推送结果
    if let Some((co_stack, result_base, n)) = co_stack_info.take() {
        // Return 分支: 从取出的协程栈中直接 push 返回值，避免 clone 大量数据
        push_resume_results_from_stack(state, a, nresults, success, co_stack, result_base, n);
    } else {
        push_resume_results(state, a, nresults, success, result_values);
    }

    Ok(())
}

/// 首次 resume — 从协程体函数（LClosure）初始化 VM 状态
fn setup_first_resume(
    state: &mut LuaState,
    thread: &LuaThread,
    resume_args: &[TValue],
) -> Result<(), VmError> {
    let func = match thread.function.as_ref() {
        Some(f) => (**f).clone(),
        None => {
            return Err(VmError::RuntimeError(
                "coroutine has no body function".to_string(),
            ));
        }
    };
    let nargs = resume_args.len();

    if let TValue::LClosure(closure) = &func {
        // Lua 函数: 从 proto 加载执行上下文 — Rc::clone O(1) 替代 Vec 深拷贝
        state.code = Rc::clone(&closure.proto.code);
        state.constants = Rc::clone(&closure.proto.constants);
        state.upval_descs = Rc::clone(&closure.proto.upvalues);
        state.protos = closure.proto.protos.clone();
        state.base = 1; // closure 在 stack[0]，寄存器从 stack[1] 开始
        state.pc = 0;
        state.num_params = closure.proto.num_params;
        state.is_vararg = closure.proto.is_vararg();
        state.proto_flag = closure.proto.flag;
        state.nextraargs = 0;
        state.closure_upvals = closure.upvals.borrow().clone();
        state.open_upval = None;
        state.tbc_list = None;
        state.call_stack = Vec::new();
        state.hook_old_pc = 0;

        // 设置栈: stack[0] = closure, stack[1..1+nargs] = resume_args
        state.stack = Vec::new();
        state.stack.push(TValue::LClosure(closure.clone()));
        for arg in resume_args {
            state.stack.push(arg.clone());
        }

        let nfixparams = closure.proto.num_params as usize;
        let fsize = closure.proto.max_stack_size as usize;

        if closure.proto.is_vararg() {
            // vararg 函数: 截断到实际参数末尾，VARARGPREP 会处理变参
            state.stack.truncate(1 + nargs);
            // 填充不足的固定参数为 nil
            for i in nargs..nfixparams {
                while state.stack.len() <= 1 + i {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
                state.stack[1 + i] = TValue::Nil(NilKind::Strict);
            }
        } else {
            // 非 vararg 函数: 扩展到 fsize
            let frame_end = 1 + fsize;
            while state.stack.len() < frame_end {
                state.stack.push(TValue::Nil(NilKind::Strict));
            }
            for i in nargs..nfixparams {
                state.stack[1 + i] = TValue::Nil(NilKind::Strict);
            }
        }

        // 推入初始 CallInfoEntry — 对应 C 中协程的 base CallInfo
        // 记录协程主函数的信息，使 traceback/getinfo 能正确显示最外层帧
        // caller_proto 设为协程主函数的 proto，让 compute_caller_info 能提取 source
        state.call_info = vec![crate::state::CallInfoEntry {
            caller_proto: Some(Rc::clone(&closure.proto)),
            is_c: false,
            closure: Some(closure.clone()),
            base: 1,
            saved_pc: 0,
            name: String::new(),
            namewhat: String::new(),
            proto_flag: closure.proto.flag,
            nextraargs: 0,
            is_tailcall: false,
        }];
    } else if is_callable(&func) {
        // C 函数 (LightUserData/CClosure/LCFn/BuiltinFn) 或带 __call 元方法的 Table:
        // 创建 CALL + RETURN 序列
        // 栈布局: stack[0] = func, stack[1] = func (寄存器 0), stack[2..] = 参数
        // CALL 0 nargs+1 0 — 调用寄存器 0 的函数, MULTRET
        // RETURN 0 0 — 返回所有结果 (MULTRET)
        use crate::opcodes::{create_abck, OpCode};
        let call_inst = create_abck(OpCode::CALL, 0, (nargs + 1) as i32, 0, 0);
        let return_inst = create_abck(OpCode::RETURN, 0, 0, 0, 0);
        state.code = Rc::new(vec![call_inst, return_inst]);
        state.constants = Rc::new(Vec::new());
        state.upval_descs = Rc::new(Vec::new());
        state.protos = Rc::new(Vec::new());
        state.base = 1;
        state.pc = 0;
        state.num_params = nargs as u8;
        state.is_vararg = false;
        state.proto_flag = 0;
        state.nextraargs = 0;
        state.closure_upvals = Vec::new();
        state.open_upval = None;
        state.tbc_list = None;
        state.call_stack = Vec::new();
        state.call_info = Vec::new();
        state.hook_old_pc = 0;

        // 设置栈: stack[0] = func, stack[1] = func (寄存器 0), stack[2..] = args
        state.stack = Vec::new();
        state.stack.push(func.clone()); // stack[0] = func (base-1)
        state.stack.push(func.clone()); // stack[1] = func (寄存器 0)
        for arg in resume_args {
            state.stack.push(arg.clone());
        }
    } else {
        return Err(VmError::RuntimeError(
            "coroutine body must be a function".to_string(),
        ));
    }
    state.top = state.stack.len();

    // 标记为已开始
    thread.context.borrow_mut().started = true;

    Ok(())
}

/// 后续 resume — 从 ThreadContext 恢复并推送 resume 参数作为 yield 的"返回值"
fn setup_subsequent_resume(
    state: &mut LuaState,
    co_context: &Rc<RefCell<ThreadContext>>,
    resume_args: &[TValue],
) -> Result<(), VmError> {
    let ctx = co_context.borrow();

    state.code = ctx.saved_code.clone();
    state.constants = ctx.saved_constants.clone();
    state.upval_descs = ctx.saved_upval_descs.clone();
    state.protos = ctx.saved_protos.clone();
    state.base = ctx.saved_base;
    state.pc = ctx.saved_pc;
    state.num_params = ctx.saved_num_params;
    state.is_vararg = ctx.saved_is_vararg;
    state.proto_flag = ctx.saved_proto_flag;
    state.nextraargs = ctx.saved_nextraargs;
    state.closure_upvals = ctx.saved_closure_upvals.clone();
    state.open_upvals = ctx.saved_open_upvals.clone();
    state.open_upval = ctx.saved_open_upval;
    state.tbc_list = ctx.saved_tbc_list;
    state.call_stack = ctx.saved_call_stack.clone();
    state.stack = ctx.saved_stack.clone();
    state.top = ctx.saved_top;
    state.hook_old_pc = ctx.saved_hook_old_pc;
    // 恢复协程的 pcall_protection_stack（yield 时保存到 ThreadContext）
    state
        .pcall_protection_stack
        .extend(ctx.saved_pcall_protection_stack.iter().cloned());
    // 恢复 close_error_status（yield 时保存到 ThreadContext）
    state.close_error_status = ctx.saved_close_error_status.clone();
    // 恢复协程的 call_info（yield 时保存到 ThreadContext）
    state.call_info = ctx.saved_call_info.clone();
    // pop 掉 yield 时保留的 C 函数 CallInfoEntry
    // 对应 C 中 yield 的 C 函数返回后 ci 被正常 pop
    // （Rust 中 yield 通过 Err(Yield) 返回，op_call 跳过了 pop，这里补偿）
    if state.call_info.last().map(|e| e.is_c).unwrap_or(false) {
        state.call_info.pop();
    }

    let yield_nresults = ctx.saved_yield_nresults;
    let yield_origins = ctx.yield_upval_origins.clone();
    drop(ctx); // 释放 borrow

    // 同步 yield 时关闭的 upvalue 回协程栈，恢复 Open
    // （协程内部修改栈值时，Open upvalue 能自动反映最新值）
    if !yield_origins.is_empty() {
        sync_yield_upvals_back(state, &yield_origins);
    }

    // 推送 resume 参数作为 yield 的"返回值"
    // state.stack 已被 call_yield 截断到 `a`，所以 stack.len() = a
    let stack_base = state.stack.len();
    for arg in resume_args {
        state.stack.push(arg.clone());
    }
    // 根据 yield 的 nresults 调整
    if yield_nresults >= 0 {
        // 固定数量: 填充 nil 或截断
        while (state.stack.len() - stack_base) < yield_nresults as usize {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
        state.stack.truncate(stack_base + yield_nresults as usize);
    }
    // nresults < 0 (MULTRET): 保留所有参数
    state.top = state.stack.len();

    Ok(())
}

// ============================================================================
// coroutine.yield(...) — 对应 C 的 lua_coyield
// ============================================================================

fn call_yield(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    // 检查是否在协程中
    let current_thread = match &state.current_thread {
        Some(ctx) => ctx.clone(),
        None => {
            return Err(VmError::RuntimeError(
                "attempt to yield from outside a coroutine".to_string(),
            ));
        }
    };
    // 检查是否可 yield（无非可 yield 的 C 函数调用在栈上）
    if state.n_ny_calls > 0 {
        return Err(VmError::RuntimeError(
            "attempt to yield across a C-call boundary".to_string(),
        ));
    }

    // 收集 yield 值
    let yield_values: Vec<TValue> = (0..nargs)
        .map(|i| {
            let idx = a + 1 + i;
            if idx < state.stack.len() {
                state.stack[idx].clone()
            } else {
                TValue::Nil(NilKind::Strict)
            }
        })
        .collect();

    // 截断栈到 `a`（移除 yield 函数和参数）
    state.stack.truncate(a);
    state.top = a;

    // 保存 yield 的 nresults 到 ThreadContext（恢复时用于调整 resume 参数）
    current_thread.borrow_mut().saved_yield_nresults = nresults;

    // 返回 Yield 错误 — execute_loop 会转换为 Ok(VmResult::Yield)
    Err(VmError::Yield(yield_values))
}

// ============================================================================
// coroutine.wrap(f) — 对应 C 的 lua_cowrap
// ============================================================================

fn call_wrap(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'wrap' (function expected)".to_string(),
        ));
    }
    let func = get_arg(state, a, 0);
    if !is_callable(&func) {
        return Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'wrap' (function expected, got {})",
            func.ty()
        )));
    }
    // 创建协程
    let context = Rc::new(RefCell::new(ThreadContext::default()));
    context.borrow_mut().status = ThreadStatus::Suspended;
    let thread = LuaThread {
        stack: Vec::new(),
        status: ThreadStatus::Suspended,
        function: Some(Box::new(func)),
        is_main: false,
        context,
    };
    // 收集开 upvalue 信息并保存到 ThreadContext（不关闭！）
    // 首次 resume 时根据同栈/跨栈决定用最新值还是 saved_value 关闭
    // 这样支持 `A = coroutine.wrap(function() ... A() ... end)` 的自引用模式
    // （A 在 call_wrap 时还是旧值，在 call_wrap_fn 首次调用时才被赋值为 wrap RustClosure）
    let pending = collect_wrap_upvals_info(&thread, state);
    let creator_ptr = state
        .current_thread
        .as_ref()
        .map(|c| Rc::as_ptr(c) as usize)
        .unwrap_or(0);
    {
        let mut ctx = thread.context.borrow_mut();
        ctx.wrap_creator_thread_ptr = creator_ptr;
        ctx.pending_wrap_upvals = pending;
    }
    // 创建 RustClosure，upvalues[0] 持有协程 Thread
    // RustClosure 可被 GC 跟踪（state.rs::mark_tvalue 遍历 upvalues），
    // 协程死亡时将 upvalues[0] 置为 nil，替代原 state.wrap_coros[idx] = None
    let wrap_closure = crate::objects::RustClosure {
        func: call_wrap_fn,
        name: c"wrap".as_ptr() as *const u8,
        upvalues: Rc::new(RefCell::new(vec![TValue::Thread(Rc::new(thread))])),
    };
    push_single_result(state, a, nresults, TValue::RustClosure(Rc::new(wrap_closure)));
    Ok(())
}

/// coroutine.wrap 返回的函数被调用时 — 恢复协程
/// 与 call_resume 的区别:
/// - 无 success flag（直接返回值或抛错）
/// - 出错时抛出错误而非返回 false + msg
///
/// 由 op_call 的 `TValue::RustClosure(_)` 分支派发到此函数。
/// RustClosure 的 upvalues[0] 持有协程 Thread；协程死亡时设置为 nil，
/// 后续调用检测到 nil 报 "cannot resume dead coroutine" 错误。
fn call_wrap_fn(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 从 state.stack[a] 取 RustClosure → upvalues[0] 取 Thread
    let rc = match state.stack.get(a) {
        Some(TValue::RustClosure(rc)) => rc.clone(),
        _ => {
            return Err(VmError::RuntimeError(
                "coroutine.wrap: invalid closure".to_string(),
            ));
        }
    };
    let thread = {
        let upvals = rc.upvalues.borrow();
        match upvals.get(0) {
            Some(TValue::Thread(t)) => t.clone(),
            _ => {
                return Err(VmError::RuntimeError(
                    "cannot resume dead coroutine".to_string(),
                ));
            }
        }
    };

    // 检查状态
    let co_status = thread.context.borrow().status;
    match co_status {
        ThreadStatus::Suspended => {}
        ThreadStatus::Normal => {
            return Err(VmError::RuntimeError(
                "cannot resume non-suspended coroutine".to_string(),
            ));
        }
        ThreadStatus::OK | ThreadStatus::Error => {
            return Err(VmError::RuntimeError(
                "cannot resume dead coroutine".to_string(),
            ));
        }
    }

    // 收集所有参数作为 resume 参数（无 thread 参数需要跳过）
    let resume_args: Vec<TValue> = (0..nargs).map(|i| state.stack[a + 1 + i].clone()).collect();

    // 收集开 upvalue 信息（在 save_caller_context 之前，state.stack 仍是父栈）
    // 首次 resume 时从 ThreadContext 取出 pending_wrap_upvals（call_wrap 时保存），
    // 根据同栈/跨栈决定关闭值：
    //   同栈: 从 state.stack 读最新值（支持变量在 call_wrap 后被重新赋值，如自引用 wrap）
    //   跨栈: state.stack 不是原始父栈，用 call_wrap 时保存的 saved_value
    let is_first_resume = !thread.context.borrow().started;
    let same_stack: bool;
    let open_upvals: Vec<OpenUpvalInfo> = if is_first_resume {
        let (pending, creator_ptr) = {
            let mut ctx = thread.context.borrow_mut();
            let p = std::mem::take(&mut ctx.pending_wrap_upvals);
            let c = ctx.wrap_creator_thread_ptr;
            (p, c)
        };
        let caller_ptr = state
            .current_thread
            .as_ref()
            .map(|c| Rc::as_ptr(c) as usize)
            .unwrap_or(0);
        same_stack = creator_ptr == caller_ptr;
        let mut open_upvals = Vec::new();
        let mut origins: Vec<(UpValRef, usize)> = Vec::new();
        for (uv_ref, orig_idx, saved_val) in pending {
            let val = if same_stack {
                state
                    .stack
                    .get(orig_idx)
                    .cloned()
                    .unwrap_or_else(|| saved_val.clone())
            } else {
                saved_val.clone()
            };
            close_upval_by_ref(state, &uv_ref, val);
            open_upvals.push(OpenUpvalInfo {
                uv_ref: uv_ref.clone(),
                original_stack_index: orig_idx,
            });
            origins.push((uv_ref, orig_idx));
        }
        thread.context.borrow_mut().upval_origins = origins;
        open_upvals
    } else {
        same_stack = true;
        Vec::new()
    };

    // 保存调用者上下文
    let mut caller_ctx = save_caller_context(state);
    // 暂存调用者栈到 state.caller_gc_stacks — 协程执行期间 GC 需要看到调用者栈
    // 中的 wrap table 引用，否则内层协程会被误判为不可达（big.lua 嵌套 wrap 场景）
    state
        .caller_gc_stacks
        .push(std::mem::take(&mut caller_ctx.stack));
    // 保存 n_ccalls (协程执行期间可能修改,退出时恢复)
    let saved_n_ccalls = state.n_ccalls;
    // 递增 n_ccalls 防止协程嵌套过深导致 Rust 栈溢出
    // (wrap 调用不经过 op_call 的 n_ccalls 递增路径，需要在此手动递增)
    state.n_ccalls = state.n_ccalls.saturating_add(1);
    if state.n_ccalls >= crate::state::LUAI_MAXCCALLS {
        state.n_ccalls = saved_n_ccalls;
        caller_ctx.stack = state.caller_gc_stacks.pop().unwrap_or_default();
        restore_caller_context(state, caller_ctx);
        return Err(VmError::RuntimeError("C stack overflow".to_string()));
    }
    // 保存 n_ny_calls 并重置为 0（协程初始状态是可 yield 的）
    let saved_n_ny_calls = state.n_ny_calls;
    state.n_ny_calls = 0;
    // 保存 pcall_protection_stack 长度（协程内部 push 的 PcallProtection 不应泄漏到外部）
    let saved_pcall_protection_len = state.pcall_protection_stack.len();

    // 设置协程上下文
    let co_context = thread.context.clone();

    let setup_result = if is_first_resume {
        setup_first_resume(state, &thread, &resume_args)
    } else {
        setup_subsequent_resume(state, &co_context, &resume_args)
    };

    if let Err(e) = setup_result {
        caller_ctx.stack = state.caller_gc_stacks.pop().unwrap_or_default();
        restore_caller_context(state, caller_ctx);
        state.n_ccalls = saved_n_ccalls;
        state.n_ny_calls = saved_n_ny_calls;
        return Err(e);
    }

    // 从 ThreadContext 恢复协程的 hook 设置到 state
    {
        let ctx = co_context.borrow();
        state.hook_func = ctx.saved_hook_func.clone();
        state.hook_mask = ctx.saved_hook_mask;
        state.hook_count = ctx.saved_hook_count;
        state.current_hook_count = ctx.saved_current_hook_count;
        state.allowhook = ctx.saved_allowhook;
    }

    // 设置 current_thread 和状态
    state.current_thread = Some(co_context.clone());
    co_context.borrow_mut().status = ThreadStatus::Normal;

    // 首次 resume 时触发 call hook（对应 C Lua 的 luaD_hook(L, LUA_HOOKCALL, -1, 0, 0)）
    if is_first_resume && state.hook_mask & 1 != 0 && state.hook_func.is_some() {
        VmExecutor::call_hook(state, "call", -1, None, 0, 0)?;
    }

    // 执行
    let exec_result = VmExecutor::execute_with_state(state);

    // 处理结果
    let (result_values, is_dead, error_val) = match exec_result {
        Ok(VmResult::Yield { values }) => {
            // 关闭 yield 出来的闭包的 Open upvalue（协程栈还有效时）
            let yield_origins = close_yield_upvals(&values, state);
            {
                let mut ctx = co_context.borrow_mut();
                ctx.saved_code = std::mem::take(&mut state.code);
                ctx.saved_constants = std::mem::take(&mut state.constants);
                ctx.saved_upval_descs = std::mem::take(&mut state.upval_descs);
                ctx.saved_protos = std::mem::take(&mut state.protos);
                ctx.saved_base = state.base;
                // C 函数 __close (如 coroutine.yield 作为 __close) yield 时，
                // state.pc 指向 OP_RETURN/OP_CLOSE（非 CALL 指令），不应 +1。
                // 此时 PcallProtection.saved_pc == state.pc（都指向 OP_RETURN/OP_CLOSE）。
                // Lua __close yield 时，state.pc 指向 __close 中的 CALL 指令，
                // saved_pc 指向 OP_RETURN/OP_CLOSE，二者不同，需要 +1 跳过 CALL。
                let is_c_close_yield = state.pcall_protection_stack.last().map_or(false, |t| {
                    t.is_close_continuation && t.saved_filled && t.saved_pc == state.pc
                });
                ctx.saved_pc = if is_c_close_yield {
                    state.pc
                } else {
                    state.pc + 1
                };
                ctx.saved_top = state.top;
                ctx.saved_num_params = state.num_params;
                ctx.saved_is_vararg = state.is_vararg;
                ctx.saved_proto_flag = state.proto_flag;
                ctx.saved_nextraargs = state.nextraargs;
                ctx.saved_closure_upvals = std::mem::take(&mut state.closure_upvals);
                ctx.saved_open_upvals = std::mem::take(&mut state.open_upvals);
                ctx.saved_open_upval = state.open_upval;
                ctx.saved_tbc_list = state.tbc_list;
                ctx.saved_call_stack = std::mem::take(&mut state.call_stack);
                ctx.saved_stack = std::mem::take(&mut state.stack);
                ctx.yield_upval_origins = yield_origins;
                ctx.saved_hook_old_pc = state.hook_old_pc;
                ctx.saved_hook_func = state.hook_func.take();
                ctx.saved_hook_mask = state.hook_mask;
                ctx.saved_hook_count = state.hook_count;
                ctx.saved_current_hook_count = state.current_hook_count;
                ctx.saved_allowhook = state.allowhook;
                ctx.saved_pcall_protection_stack =
                    state.pcall_protection_stack[saved_pcall_protection_len..].to_vec();
                // 保存 close_error_status（close continuation 的 pending error）
                ctx.saved_close_error_status = state.close_error_status.take();
                ctx.status = ThreadStatus::Suspended;
            }
            // yield 时从 state 中移除协程的 PcallProtection（避免污染调用者 state）
            state
                .pcall_protection_stack
                .truncate(saved_pcall_protection_len);
            (values, false, None)
        }
        Ok(VmResult::Return {
            nresults: ret_n,
            result_base,
        }) => {
            let return_values: Vec<TValue> = (0..ret_n)
                .map(|i| {
                    if result_base + i < state.stack.len() {
                        state.stack[result_base + i].clone()
                    } else {
                        TValue::Nil(NilKind::Strict)
                    }
                })
                .collect();
            {
                let mut ctx = co_context.borrow_mut();
                ctx.status = ThreadStatus::OK;
                ctx.started = true;
            }
            (return_values, true, None)
        }
        Ok(_) => {
            co_context.borrow_mut().status = ThreadStatus::OK;
            (Vec::new(), true, None)
        }
        Err(e) => {
            // 保存 error 状态到 ctx（必须在 TBC 关闭之前保存）
            {
                let mut ctx = co_context.borrow_mut();
                ctx.status = ThreadStatus::Error;
                ctx.saved_call_info = state.call_info.clone();
                let save_end = state.base.min(state.stack.len());
                ctx.saved_stack = state.stack[..save_end].to_vec();
                ctx.saved_base = state.base;
                ctx.saved_pc = state.pc.wrapping_add(1);
            }
            // 关闭协程的 TBC 变量，对应 C Lua 中 luaD_closeprotected -> luaF_close
            let close_level = state.base;
            if close_level > 0 && close_level <= state.stack.len() {
                state.stack[close_level - 1] = TValue::Nil(NilKind::Strict);
            }
            let _ = crate::func::close(state, close_level, 1, 0);
            // 保留原始错误值（非字符串错误如 error(foo) 应原样传播），
            // 而非格式化为字符串丢失 TValue 类型
            let err_val = state.last_error_value.take().unwrap_or_else(|| {
                let msg = if !state.last_error_msg.is_empty() {
                    state.last_error_msg.clone()
                } else {
                    format!("{}", e)
                };
                TValue::Str(state.intern_str(&msg))
            });
            (Vec::new(), true, Some(err_val))
        }
    };

    // 协程结束则将 RustClosure 的 upvalues[0] 置为 nil
    // （替代原 state.wrap_coros[idx] = None；后续调用会检测 nil 报 "dead coroutine"）
    if is_dead {
        let mut upvals = rc.upvalues.borrow_mut();
        if upvals.len() > 0 {
            upvals[0] = TValue::Nil(NilKind::Strict);
        }
    }

    // 恢复调用者上下文
    caller_ctx.stack = state.caller_gc_stacks.pop().unwrap_or_default();
    restore_caller_context(state, caller_ctx);
    // 恢复 n_ccalls (协程 yield/error 可能导致 n_ccalls 不准确)
    state.n_ccalls = saved_n_ccalls;
    state.n_ny_calls = saved_n_ny_calls;
    // 协程结束时清理 pcall_protection_stack（移除协程内部 push 的 PcallProtection）
    if is_dead {
        state
            .pcall_protection_stack
            .truncate(saved_pcall_protection_len);
    }

    // 把 Closed upvalue 值同步回父栈，协程结束则恢复 Open
    // 同栈时写回 state.stack（原始父栈）；跨栈时跳过写回（父栈不可访问），仅恢复 Open
    if is_first_resume {
        sync_upvals_back(state, &open_upvals, is_dead, same_stack);
    } else {
        // 后续 resume：从 upval_origins 恢复 open_upvals 信息，
        // yield 时同步 Closed 值回父栈（保持 Closed）；结束时恢复 Open。
        let origins = co_context.borrow().upval_origins.clone();
        let dead_upvals: Vec<OpenUpvalInfo> = origins
            .into_iter()
            .map(|(uv_ref, idx)| OpenUpvalInfo {
                uv_ref,
                original_stack_index: idx,
            })
            .collect();
        sync_upvals_back(state, &dead_upvals, is_dead, same_stack);
    }

    // 出错时抛出错误（wrap 语义：不返回 false+err，而是直接抛错）
    // 字符串错误用 RuntimeError，非字符串错误用 RuntimeErrorValue 保留原始 TValue
    if let Some(err_val) = error_val {
        return Err(match err_val {
            TValue::Str(s) => VmError::RuntimeError(s.as_str().to_string()),
            other => VmError::RuntimeErrorValue(other),
        });
    }

    // 推送结果（无 success flag）
    state.stack.truncate(a);
    if nresults != 0 {
        for v in result_values {
            state.stack.push(v);
        }
        if nresults > 0 {
            let current = state.stack.len() - a;
            if current > nresults as usize {
                state.stack.truncate(a + nresults as usize);
            } else {
                while (state.stack.len() - a) < nresults as usize {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
            }
        }
    }
    state.top = state.stack.len();

    Ok(())
}

// ============================================================================
// 打开 Coroutine 库 — 对应 C 的 luaopen_coroutine
// ============================================================================

pub fn open_coroutine_lib(state: &mut LuaState) {
    let mut lib = crate::table::Table::new();

    // 注册 BuiltinFn 的辅助闭包：用函数指针 + 名字注册到表
    // (state 作为参数传入，避免闭包捕获 state 导致借用冲突)
    let register = |lib: &mut crate::table::Table,
                    state: &LuaState,
                    name: &'static std::ffi::CStr,
                    func: crate::objects::BuiltinFnPtr| {
        let key = TValue::Str(state.intern_str(name.to_str().unwrap_or("")));
        let name_ptr = name.as_ptr() as *const u8;
        lib.set(key, TValue::BuiltinFn(BuiltinFn { func, name: name_ptr }));
    };

    register(&mut lib, state, c"create", call_create);
    register(&mut lib, state, c"isyieldable", call_isyieldable);
    register(&mut lib, state, c"resume", call_resume);
    register(&mut lib, state, c"running", call_running);
    register(&mut lib, state, c"status", call_status);
    register(&mut lib, state, c"wrap", call_wrap);
    register(&mut lib, state, c"yield", call_yield);
    register(&mut lib, state, c"close", call_close);

    let key = TValue::Str(state.intern_str("coroutine"));
    state.globals.set(key, TValue::Table(lib));
}
