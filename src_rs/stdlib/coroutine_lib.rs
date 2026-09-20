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
use crate::objects::{
    BuiltinFn, LuaThread, NilKind, TValue, Table, ThreadContext, ThreadStatus, UpVal, UpValRef,
    UpValVec,
};
use crate::state::{ExecState, LuaState};
use std::cell::RefCell;
use std::rc::Rc;

// ============================================================================
// 栈操作辅助函数
// ============================================================================

fn get_arg(state: &LuaState, a: usize, idx: usize) -> TValue {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.exec.stack.len() {
        return TValue::Nil(NilKind::Strict);
    }
    state.exec.stack[stack_idx].clone()
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
    state.exec.stack.truncate(a);
    if nresults == 0 {
        return;
    }
    state.exec.stack.push(TValue::Boolean(success));
    for v in values {
        state.exec.stack.push(v);
    }
    if nresults > 0 {
        let current = state.exec.stack.len() - a;
        if current > nresults as usize {
            state.exec.stack.truncate(a + nresults as usize);
        } else {
            while (state.exec.stack.len() - a) < nresults as usize {
                state.exec.stack.push(TValue::Nil(NilKind::Strict));
            }
        }
    }
    state.exec.top = state.exec.stack.len();
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
    state.exec.stack.truncate(a);
    if nresults == 0 {
        return;
    }
    state.exec.stack.push(TValue::Boolean(success));
    if nresults > 0 {
        // 固定结果数: 只需 nresults-1 个返回值，避免 push 全部 n 个值导致 OOM
        let nvals = (nresults as usize).saturating_sub(1);
        for i in 0..nvals {
            let val = if i < n && result_base + i < co_stack.len() {
                std::mem::take(&mut co_stack[result_base + i])
            } else {
                TValue::Nil(NilKind::Strict)
            };
            state.exec.stack.push(val);
        }
        // 不足则补 nil
        while (state.exec.stack.len() - a) < nresults as usize {
            state.exec.stack.push(TValue::Nil(NilKind::Strict));
        }
    } else {
        // LUA_MULTRET: push 全部返回值
        for i in 0..n {
            let val = if result_base + i < co_stack.len() {
                std::mem::take(&mut co_stack[result_base + i])
            } else {
                TValue::Nil(NilKind::Strict)
            };
            state.exec.stack.push(val);
        }
    }
    // co_stack 在此 drop，释放协程栈内存
    state.exec.top = state.exec.stack.len();
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
// 执行上下文交换 — 协程切换的核心
// ============================================================================
//
// 协程的整个执行状态 (ExecState) 存放在 ThreadContext.exec (Box<ExecState>)，
// 运行中的协程状态在 LuaState.exec。resume/yield 只需交换两个 Box 指针 (O(1))，
// 替代原先 ~30 字段逐个 save/restore。caller 的 exec 随 swap 进入 ctx.exec，
// 挂起的协程 exec 随 swap 进入 state.exec，语义与 C Lua 的每线程 lua_State 一致。

/// 把 LuaState.exec（当前运行协程）与 ThreadContext.exec（挂起协程）整体交换。
/// 调用后 state.exec = 协程的 exec，ctx.exec = 调用者的 exec。
#[inline]
fn swap_exec(state: &mut LuaState, ctx: &Rc<RefCell<ThreadContext>>) {
    let mut borrowed = ctx.borrow_mut();
    std::mem::swap(&mut state.exec, &mut borrowed.exec);
}

/// 首次 resume: 用全新构建的协程 ExecState 替换 state.exec，
/// 并把调用者的 exec 存入 ThreadContext.exec。
fn install_fresh_exec(state: &mut LuaState, ctx: &Rc<RefCell<ThreadContext>>, co_exec: ExecState) {
    let caller_exec = std::mem::replace(&mut state.exec, Box::new(co_exec));
    ctx.borrow_mut().exec = caller_exec;
}

/// resume 进入协程后的公共初始化（swap 之后调用）:
/// 按调用者的计数重置协程的 C 栈保护计数，保证跨多次 resume 不累积。
#[inline]
fn init_coroutine_guards(state: &mut LuaState, saved_n_ccalls: u32) {
    state.exec.n_ccalls = saved_n_ccalls.saturating_add(1);
    state.exec.n_ny_calls = 0;
    state.exec.force_noyield_close = false;
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
                let mut visited = std::collections::HashSet::with_hasher(
                    crate::objects::FxBuildHasher::default(),
                );
                collect_and_close_upvals(
                    &closure.upvals.borrow(),
                    state,
                    &mut result,
                    &mut visited,
                );
            } else {
                // C 函数体 (如 pcall): 函数本身没有 upvalue，但其参数中可能有 LClosure
                // 这些 LClosure 的 Open upvalue 仍指向父栈，需要转为 Closed
                // 参数在 call_resume 中通过 state.exec.stack[a+2..] 访问（resume_args 之前）
                // 但此时还未 save_caller_context，state.exec.stack 仍是父栈
                // 直接扫描栈上的 LClosure 参数
                let mut visited = std::collections::HashSet::with_hasher(
                    crate::objects::FxBuildHasher::default(),
                );
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
    visited: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
) {
    let mut visited_tables =
        std::collections::HashSet::with_hasher(crate::objects::FxBuildHasher::default());
    // 先 clone 栈上的 LClosure/Table 引用（避免遍历时借用 state.exec.stack）
    let closures: Vec<Rc<RefCell<UpValVec>>> = state.exec
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
    let tables: Vec<Table> = state.exec
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
    if let Some(uv_idx) = state.exec
        .open_upvals
        .iter()
        .position(|r| Rc::as_ptr(r) as usize == ptr)
    {
        crate::func::unlink_upval(state, uv_idx);
    }
    *uv_ref.borrow_mut() = UpVal::Closed {
        value: val,
    };
}

/// 递归收集并关闭所有可达的 Open upvalue
/// 当 upvalue 的值是 LClosure 时，递归处理该闭包的 upvalue
/// 当 upvalue 的值是 Table 时，递归扫描 Table（包括元表）中的 LClosure
fn collect_and_close_upvals(
    upvals: &[Rc<RefCell<UpVal>>],
    state: &mut LuaState,
    result: &mut Vec<OpenUpvalInfo>,
    visited: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
) {
    let mut visited_tables =
        std::collections::HashSet::with_hasher(crate::objects::FxBuildHasher::default());
    collect_and_close_upvals_impl(upvals, state, result, visited, &mut visited_tables);
}

fn collect_and_close_upvals_impl(
    upvals: &[Rc<RefCell<UpVal>>],
    state: &mut LuaState,
    result: &mut Vec<OpenUpvalInfo>,
    visited: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
    visited_tables: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
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
                    let val = state.exec
                        .stack
                        .get(original_idx)
                        .cloned()
                        .unwrap_or(TValue::Nil(NilKind::Strict));
                    (true, original_idx, val)
                }
                UpVal::Closed { value } => (false, 0, value.clone()),
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
    visited: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
    visited_tables: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
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
/// 避免协程执行期间 state.exec.stack 被替换后 upvalue 失效
pub fn close_hook_upvals(hook: &TValue, state: &mut LuaState) {
    if let TValue::LClosure(closure) = hook {
        let mut result = Vec::new();
        let mut visited =
            std::collections::HashSet::with_hasher(crate::objects::FxBuildHasher::default());
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
        let mut visited =
            std::collections::HashSet::with_hasher(crate::objects::FxBuildHasher::default());
        if let TValue::LClosure(closure) = boxed_func.as_ref() {
            collect_open_upvals_recursive(
                &closure.upvals.borrow(),
                state,
                &mut result,
                &mut visited,
            );
        } else {
            // C 函数体: 扫描栈上的 LClosure 和 Table 参数
            let mut visited_tables =
                std::collections::HashSet::with_hasher(crate::objects::FxBuildHasher::default());
            for v in state.exec.stack.iter() {
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
    visited: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
) {
    let mut visited_tables =
        std::collections::HashSet::with_hasher(crate::objects::FxBuildHasher::default());
    collect_open_upvals_recursive_impl(upvals, state, result, visited, &mut visited_tables);
}

fn collect_open_upvals_recursive_impl(
    upvals: &[Rc<RefCell<UpVal>>],
    state: &LuaState,
    result: &mut Vec<(UpValRef, usize, TValue)>,
    visited: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
    visited_tables: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
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
                    let val = state.exec
                        .stack
                        .get(original_idx)
                        .cloned()
                        .unwrap_or(TValue::Nil(NilKind::Strict));
                    (true, original_idx, val)
                }
                UpVal::Closed { value } => (false, 0, value.clone()),
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
    visited: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
    visited_tables: &mut std::collections::HashSet<usize, crate::objects::FxBuildHasher>,
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
                UpVal::Closed { value } => value.clone(),
                UpVal::Open { .. } => continue, // 已是 Open，跳过
            }
        };
        // 写入父栈原位置（跨栈时跳过：state.exec.stack 不是原始父栈）
        if write_back && info.original_stack_index < state.exec.stack.len() {
            state.exec.stack[info.original_stack_index] = latest_val.clone();
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
                if let Some(uv_idx) = state.exec
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
/// 在 saved_stack = take(state.exec.stack) 之前调用（state.exec.stack 仍是协程栈）
/// 返回 (uv_ref, original_stack_index) 列表，供 resume 时同步回协程栈
fn close_yield_upvals(yield_values: &[TValue], state: &mut LuaState) -> Vec<(UpValRef, usize)> {
    // 快速路径: yield 值不含 LClosure/Table 且 open_upval 链为空时，
    // 无需分配 visited HashSet 也无需遍历链表，直接返回空（coroutine bench 热路径）
    let has_container = yield_values
        .iter()
        .any(|v| matches!(v, TValue::LClosure(_) | TValue::Table(_)));
    if !has_container && state.exec.open_upval.is_none() {
        return Vec::new();
    }
    let mut result_info: Vec<OpenUpvalInfo> = Vec::new();
    let mut visited =
        std::collections::HashSet::with_hasher(crate::objects::FxBuildHasher::default());
    let mut visited_tables =
        std::collections::HashSet::with_hasher(crate::objects::FxBuildHasher::default());
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
    // 遍历 state.exec.open_upval 链表，关闭所有剩余的 open upvalue。
    // 这些 upvalue 指向协程栈，yield 后 state.exec.stack 切回主线程栈，
    // 若不关闭，外部持有的闭包（前一次 yield 传出但不在本次 yield 值中）
    // 会通过 Open upvalue 访问主线程栈的错误位置。
    // 跳过 TBC upvalue：它们只在协程内部通过 func::close 访问，
    // yield 后协程栈被保存到 ThreadContext，resume 时恢复，TBC upvalue 仍指向正确位置。
    // 若关闭 TBC upvalue，sync_yield_upvals_back 恢复 Open 时会丢失 tbc 标记，
    // 导致后续 close 不调用 __close。
    // 先收集要关闭的 uv_idx（遍历链表时不能修改链表），再逐个 unlink + close
    let mut to_close: Vec<(usize, usize)> = Vec::new(); // (uv_idx, stack_index)
    let mut current = state.exec.open_upval;
    while let Some(uv_idx) = current {
        if uv_idx >= state.exec.open_upvals.len() {
            break;
        }
        let uv_ref = state.exec.open_upvals[uv_idx].clone();
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
        let val = state.exec
            .stack
            .get(stack_index)
            .cloned()
            .unwrap_or(TValue::Nil(NilKind::Strict));
        let uv_ref = state.exec.open_upvals[uv_idx].clone();
        crate::func::unlink_upval(state, uv_idx);
        *uv_ref.borrow_mut() = UpVal::Closed {
            value: val,
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
        let uv = state.exec.open_upvals[uv_idx].borrow();
        match &*uv {
            UpVal::Open { stack_index, .. } => *stack_index,
            _ => return,
        }
    };
    let mut prev: Option<usize> = None;
    let mut current = state.exec.open_upval;
    while let Some(idx) = current {
        if idx == uv_idx {
            return; // 已在链表中
        }
        let (cur_level, next) = {
            let uv = state.exec.open_upvals[idx].borrow();
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
        let mut uv = state.exec.open_upvals[uv_idx].borrow_mut();
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
            let mut p = state.exec.open_upvals[p_idx].borrow_mut();
            if let UpVal::Open { ref mut next, .. } = &mut *p {
                *next = Some(uv_idx);
            }
        }
        None => {
            state.exec.open_upval = Some(uv_idx);
        }
    }
    if let Some(n_idx) = next_node {
        let mut n = state.exec.open_upvals[n_idx].borrow_mut();
        if let UpVal::Open {
            ref mut previous, ..
        } = &mut *n
        {
            *previous = Some(uv_idx);
        }
    }
}

/// resume 时把 yield 时关闭的 upvalue 的 Closed 值同步回协程栈，并恢复 Open
/// 在 setup_subsequent_resume 恢复 state.exec.stack 之后调用
fn sync_yield_upvals_back(state: &mut LuaState, origins: &[(UpValRef, usize)]) {
    for (uv_ref, stack_index) in origins {
        let val = {
            let uv = uv_ref.borrow();
            match &*uv {
                UpVal::Closed { value } => value.clone(),
                UpVal::Open { .. } => continue, // 已是 Open，跳过
            }
        };
        // 写回协程栈
        if *stack_index < state.exec.stack.len() {
            state.exec.stack[*stack_index] = val;
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
        if let Some(uv_idx) = state.exec
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
        context: context.clone(),
        c_state: std::cell::Cell::new(std::ptr::null_mut()),
    };
    let thread_rc = Rc::new(thread);
    // 设置 thread_ref，让 coroutine.running() 能返回同一对象
    context.borrow_mut().thread_ref = Rc::downgrade(&thread_rc);
    push_single_result(state, a, nresults, TValue::Thread(thread_rc));
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
                let is_current = state.exec
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
        state.exec.force_noyield_close = true;
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
        let in_coroutine = state.exec.current_thread.is_some();
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
            let is_current = state.exec
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
    // 交换: caller exec -> ctx.exec, 协程 exec -> state.exec (O(1))
    let co_context = thread.context.clone();
    swap_exec(state, &co_context);

    // 设置 current_thread 和状态
    state.exec.current_thread = Some(co_context.clone());
    co_context.borrow_mut().status = ThreadStatus::Normal;

    // 保存 close 前的 last_error_msg 状态（用于检测 __close 是否出错）
    let saved_err_msg = state.last_error_msg.clone();
    let saved_err_value = state.last_error_value.take();
    state.last_error_msg.clear();

    // 调用 close 关闭所有 TBC upvalue（运行 __close metamethod）
    // close 函数内部对 TBC upvalue 调用 call_close_method，使用 pcall 处理错误
    // status=0 表示正常关闭（err 参数为 nil）
    crate::func::close(state, state.exec.base, 0, 1).ok();

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

    // 交换回调用者；协程已结束，清空其 exec（释放挂起栈内存）
    {
        let mut ctx = co_context.borrow_mut();
        std::mem::swap(&mut state.exec, &mut ctx.exec);
        ctx.exec = Box::new(ExecState::default());
        ctx.status = ThreadStatus::OK;
        ctx.error_msg = None;
    }

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
        state.exec.current_thread.is_some() && state.exec.n_ny_calls == 0
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
    let (thread_val, ismain) = match &state.exec.current_thread {
        Some(ctx) => {
            // 在协程中 — 返回该协程的原始 LuaThread 对象（通过 thread_ref）
            // 这样 coroutine.running() 每次返回同一对象，table 查找才能正确工作
            let thread_rc = ctx.borrow().thread_ref.upgrade();
            match thread_rc {
                Some(rc) => (TValue::Thread(rc), false),
                None => {
                    // thread_ref 已失效（不应发生），回退到创建临时对象
                    let thread = LuaThread {
                        stack: Vec::new(),
                        status: ctx.borrow().status,
                        function: None,
                        is_main: false,
                        context: ctx.clone(),
                        c_state: std::cell::Cell::new(std::ptr::null_mut()),
                    };
                    (TValue::Thread(Rc::new(thread)), false)
                }
            }
        }
        None => {
            // 主线程 — 返回 main_thread + true
            (TValue::Thread(Rc::new(state.main_thread.clone())), true)
        }
    };

    state.exec.stack.truncate(a);
    if nresults >= 1 {
        state.exec.stack.push(thread_val);
        if nresults >= 2 {
            state.exec.stack.push(TValue::Boolean(ismain));
        }
        let current = state.exec.stack.len() - a;
        for _ in current..nresults as usize {
            state.exec.stack.push(TValue::Nil(NilKind::Strict));
        }
    } else if nresults < 0 {
        // MULTRET
        state.exec.stack.push(thread_val);
        state.exec.stack.push(TValue::Boolean(ismain));
    }
    state.exec.top = state.exec.stack.len();
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
            .map(|i| state.exec.stack[a + 2 + i].clone())
            .collect()
    } else {
        Vec::new()
    };

    // 收集开 upvalue 信息（在 save_caller_context 之前，state.exec.stack 仍是父栈）
    let open_upvals = close_open_upvals(&thread, state);

    // 保存调用者的 C 栈保护计数（协程执行期间可能修改；协程 exec 与调用者 exec
    // 随 swap 交换，退出时调用者原值自动恢复，无需显式还原）
    let saved_n_ccalls = state.exec.n_ccalls;

    // 设置协程上下文
    let co_context = thread.context.clone();
    let is_first_resume = !co_context.borrow().started;

    // setup 内部完成 exec 交换：调用者 exec -> ctx.exec，协程 exec -> state.exec (O(1))
    let setup_result = if is_first_resume {
        // 首次 resume — 从协程体函数初始化
        setup_first_resume(state, &thread, &resume_args)
    } else {
        // 后续 resume — 交换回协程 exec 并推送 resume 参数
        setup_subsequent_resume(state, &co_context, &resume_args)
    };

    if let Err(e) = setup_result {
        // setup 失败（仅首次 resume 的"body 不是函数"路径）：未发生交换，
        // state.exec 仍是调用者的，直接返回
        return Err(e);
    }

    // 首次 resume setup 成功后立即标记 started=true，
    // 这样协程 yield 后的后续 resume 会走 setup_subsequent_resume 而非重新初始化
    if is_first_resume {
        co_context.borrow_mut().started = true;
    }

    // 按调用者计数初始化协程的 C 栈保护（跨多次 resume 不累积）。
    // 协程的 hook 已随 swap 就位（首次 resume 由 setup_first_resume 从旧 ctx.exec
    // 保留 pre-hook，后续 resume 由上次 yield 保存的 exec 带来）。
    init_coroutine_guards(state, saved_n_ccalls);
    if state.exec.n_ccalls >= crate::state::LUAI_MAXCCALLS {
        // C 栈溢出：交换回调用者 exec
        swap_exec(state, &co_context);
        return push_resume_error(state, a, nresults, "C stack overflow");
    }

    // 设置 current_thread 和状态
    state.exec.current_thread = Some(co_context.clone());
    co_context.borrow_mut().status = ThreadStatus::Normal;

    // 首次 resume 时触发 call hook（对应 C Lua 的 luaD_hook(L, LUA_HOOKCALL, -1, 0, 0)）
    if is_first_resume && state.exec.hook_mask & 1 != 0 && state.exec.hook_func.is_some() {
        VmExecutor::call_hook(state, "call", -1, None, 0, 0)?;
    }

    // 调用 execute_loop
    let exec_result = VmExecutor::execute_with_state(state);

    // 处理结果
    // co_stack_info: Return 分支取出协程整个栈，避免 clone 大量返回值导致 OOM
    // (stack, result_base, n) — 从取出的栈的 [result_base, result_base+n) 读取返回值
    // 每个分支内部完成 swap 交换回调用者 exec，之后 state.exec = 调用者。
    let (success, result_values, mut co_stack_info): (
        bool,
        Vec<TValue>,
        Option<(Vec<TValue>, usize, usize)>,
    ) = match exec_result {
        Ok(VmResult::Yield { values }) => {
            // 关闭 yield 出来的闭包的 Open upvalue（协程栈还有效时）
            let yield_origins = close_yield_upvals(&values, state);
            // C 函数 __close (如 coroutine.yield 作为 __close) yield 时，
            // state.exec.pc 指向 OP_RETURN/OP_CLOSE（非 CALL 指令），不应 +1。
            // 此时 PcallProtection.saved_pc == state.exec.pc（都指向 OP_RETURN/OP_CLOSE）。
            // Lua __close yield 时，state.exec.pc 指向 __close 中的 CALL 指令，
            // saved_pc 指向 OP_RETURN/OP_CLOSE，二者不同，需要 +1 跳过 CALL。
            let is_c_close_yield = state.exec.pcall_protection_stack.last().map_or(false, |t| {
                t.is_close_continuation && t.saved_filled && t.saved_pc == state.exec.pc
            });
            state.exec.pc = if is_c_close_yield {
                state.exec.pc
            } else {
                state.exec.pc + 1
            };
            // 保存 yield 关闭的 upvalue 来源（resume 时同步回协程栈）
            co_context.borrow_mut().yield_upval_origins = yield_origins;
            // 交换回调用者（协程 exec 连同 pc/pcall 保护等进入 ctx.exec）
            swap_exec(state, &co_context);
            co_context.borrow_mut().status = ThreadStatus::Suspended;
            (true, values, None)
        }
        Ok(VmResult::Return {
            nresults: ret_n,
            result_base,
        }) => {
            // 协程返回 — 取出协程整个栈(用 mem::take 避免分配新 Vec)
            // 返回值在 stack[result_base..result_base+ret_n]，后续从取出的栈中读取
            let co_stack = std::mem::take(&mut state.exec.stack);
            {
                let mut ctx = co_context.borrow_mut();
                ctx.status = ThreadStatus::OK;
                ctx.started = true;
                // 交换回调用者（协程 exec 进入 ctx.exec）
                std::mem::swap(&mut state.exec, &mut ctx.exec);
                // 协程结束，清空 call_info（对应 C 中协程 dead 后 ci 链为空）
                ctx.exec.call_info.clear();
            }
            (true, Vec::new(), Some((co_stack, result_base, ret_n)))
        }
        Ok(_) => {
            let mut ctx = co_context.borrow_mut();
            ctx.status = ThreadStatus::OK;
            // 交换回调用者
            std::mem::swap(&mut state.exec, &mut ctx.exec);
            ctx.exec.call_info.clear();
            (true, Vec::new(), None)
        }
        Err(e) => {
            // 保存 error 状态快照（pre-close 的调用栈，供 debug.traceback(co) 使用）。
            // 必须在 TBC 关闭之前保存（TBC 关闭会修改 stack[base-1]）。
            // 只克隆到 base 的部分栈（build_traceback_from_thread 只需要
            // ctx.exec.stack[ctx.exec.base-1] 处的 LClosure），避免克隆整个栈导致 OOM。
            let (err_call_info, err_stack, err_base, err_pc) = {
                let save_end = state.exec.base.min(state.exec.stack.len());
                (
                    std::mem::take(&mut state.exec.call_info),
                    state.exec.stack[..save_end].to_vec(),
                    state.exec.base,
                    state.exec.pc.wrapping_add(1),
                )
            };
            // 第二次 resume 时 pcall 的保护已丢失（state.pcall 的 saved 状态是局部变量，
            // yield 后被销毁）。需要在此手动关闭协程（foo）的 TBC 变量，
            // 对应 C Lua 中 pcall 错误时 luaD_closeprotected -> luaF_close 的行为。
            let close_level = state.exec.base;
            if close_level > 0 && close_level <= state.exec.stack.len() {
                // 设为 nil 让 debug.getinfo 返回 "C"（对应 pcall 的 C 函数帧）
                state.exec.stack[close_level - 1] = TValue::Nil(NilKind::Strict);
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
                        let func_ptr = bf.raw_func() as *const () as usize;
                        func_ptr == crate::stdlib::base_lib::call_pcall as *const () as usize
                            || func_ptr
                                == crate::stdlib::base_lib::call_xpcall as *const () as usize
                    } else {
                        false
                    }
                })
                .unwrap_or(false);

            if body_is_protective {
                // pcall/xpcall 捕获了错误，协程正常返回 (false, err)
                // resume 返回 (true, false, err)
                {
                    let mut ctx = co_context.borrow_mut();
                    // 交换回调用者（协程 exec 进入 ctx.exec）
                    std::mem::swap(&mut state.exec, &mut ctx.exec);
                    ctx.exec.call_info = err_call_info;
                    ctx.exec.stack = err_stack;
                    ctx.exec.base = err_base;
                    ctx.exec.pc = err_pc;
                    ctx.status = ThreadStatus::OK;
                    ctx.error_msg = None;
                }
                (true, vec![TValue::Boolean(false), final_err], None)
            } else {
                // 协程错误，resume 返回 (false, err)
                {
                    let mut ctx = co_context.borrow_mut();
                    // 交换回调用者（协程 exec 进入 ctx.exec，保留错误状态）
                    std::mem::swap(&mut state.exec, &mut ctx.exec);
                    ctx.exec.call_info = err_call_info;
                    ctx.exec.stack = err_stack;
                    ctx.exec.base = err_base;
                    ctx.exec.pc = err_pc;
                    ctx.status = ThreadStatus::Error;
                    ctx.error_msg = Some(final_err.clone());
                }
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

    // 各分支已完成 swap 交换回调用者 exec；调用者的 n_ccalls/n_ny_calls/
    // force_noyield_close/pcall_protection_stack 均随 swap 自动恢复。

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

    let mut co = ExecState::default();

    if let TValue::LClosure(closure) = &func {
        // Lua 函数: 从 proto 加载执行上下文 — Rc::clone O(1) 替代 Vec 深拷贝
        co.code = Rc::clone(&closure.proto.code);
        co.constants = Rc::clone(&closure.proto.constants);
        co.upval_descs = Rc::clone(&closure.proto.upvalues);
        co.protos = closure.proto.protos.clone();
        co.base = 1; // closure 在 stack[0]，寄存器从 stack[1] 开始
        co.pc = 0;
        co.num_params = closure.proto.num_params;
        co.is_vararg = closure.proto.is_vararg();
        co.proto_flag = closure.proto.flag;
        co.nextraargs = 0;
        co.closure_upvals = Rc::clone(&closure.upvals);
        co.open_upval = None;
        co.tbc_list = None;
        co.call_stack = Vec::new();
        co.hook_old_pc = 0;

        // 设置栈: stack[0] = closure, stack[1..1+nargs] = resume_args
        co.stack = Vec::new();
        co.stack.push(TValue::LClosure(closure.clone()));
        for arg in resume_args {
            co.stack.push(arg.clone());
        }

        let nfixparams = closure.proto.num_params as usize;
        let fsize = closure.proto.max_stack_size as usize;

        if closure.proto.is_vararg() {
            // vararg 函数: 截断到实际参数末尾，VARARGPREP 会处理变参
            co.stack.truncate(1 + nargs);
            // 填充不足的固定参数为 nil
            for i in nargs..nfixparams {
                while co.stack.len() <= 1 + i {
                    co.stack.push(TValue::Nil(NilKind::Strict));
                }
                co.stack[1 + i] = TValue::Nil(NilKind::Strict);
            }
        } else {
            // 非 vararg 函数: 扩展到 fsize
            let frame_end = 1 + fsize;
            while co.stack.len() < frame_end {
                co.stack.push(TValue::Nil(NilKind::Strict));
            }
            for i in nargs..nfixparams {
                co.stack[1 + i] = TValue::Nil(NilKind::Strict);
            }
        }

        // 推入初始 CallInfoEntry — 对应 C 中协程的 base CallInfo
        // 记录协程主函数的信息，使 traceback/getinfo 能正确显示最外层帧
        // perf: caller_proto 延迟计算, 从 co.stack[0] (= base-1) 获取主函数 proto
        co.call_info = vec![crate::state::CallInfoEntry {
            is_c: false,
            closure: Some(closure.clone()),
            base: 1,
            saved_pc: 0,
            name: None,
            namewhat: "",
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
        co.code = Rc::new(vec![call_inst, return_inst]);
        co.constants = Rc::new(Vec::new());
        co.upval_descs = Rc::new(Vec::new());
        co.protos = Rc::new(Vec::new());
        co.base = 1;
        co.pc = 0;
        co.num_params = nargs as u8;
        co.is_vararg = false;
        co.proto_flag = 0;
        co.nextraargs = 0;
        co.closure_upvals = Rc::new(RefCell::new(UpValVec::new()));
        co.open_upval = None;
        co.tbc_list = None;
        co.call_stack = Vec::new();
        co.call_info = Vec::new();
        co.hook_old_pc = 0;

        // 设置栈: stack[0] = func, stack[1] = func (寄存器 0), stack[2..] = args
        co.stack = Vec::new();
        co.stack.push(func.clone()); // stack[0] = func (base-1)
        co.stack.push(func.clone()); // stack[1] = func (寄存器 0)
        for arg in resume_args {
            co.stack.push(arg.clone());
        }
    } else {
        return Err(VmError::RuntimeError(
            "coroutine body must be a function".to_string(),
        ));
    }
    co.top = co.stack.len();

    // 保留 first resume 前由 debug.sethook(co, ...) 设置的 hook（存于旧 ctx.exec）
    let pre_hook = {
        let ctx = thread.context.borrow();
        (
            ctx.exec.hook_func.clone(),
            ctx.exec.hook_mask,
            ctx.exec.hook_count,
            ctx.exec.current_hook_count,
            ctx.exec.hook_old_pc,
            ctx.exec.allowhook,
        )
    };
    co.hook_func = pre_hook.0;
    co.hook_mask = pre_hook.1;
    co.hook_count = pre_hook.2;
    co.current_hook_count = pre_hook.3;
    co.hook_old_pc = pre_hook.4;
    co.allowhook = pre_hook.5;

    // 安装: 调用者 exec -> ThreadContext.exec, co exec -> state.exec
    install_fresh_exec(state, &thread.context, co);

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
    // 交换: caller exec -> ctx.exec, 协程 exec -> state.exec (O(1) 指针交换)
    swap_exec(state, co_context);

    // pop 掉 yield 时保留的 C 函数 CallInfoEntry
    // 对应 C 中 yield 的 C 函数返回后 ci 被正常 pop
    // （Rust 中 yield 通过 Err(Yield) 返回，op_call 跳过了 pop，这里补偿）
    if state.exec.call_info.last().map(|e| e.is_c).unwrap_or(false) {
        state.exec.call_info.pop();
    }

    let yield_nresults = state.exec.saved_yield_nresults;
    let yield_origins = std::mem::take(&mut co_context.borrow_mut().yield_upval_origins);

    // 同步 yield 时关闭的 upvalue 回协程栈，恢复 Open
    // （协程内部修改栈值时，Open upvalue 能自动反映最新值）
    if !yield_origins.is_empty() {
        sync_yield_upvals_back(state, &yield_origins);
    }

    // 推送 resume 参数作为 yield 的"返回值"
    // state.exec.stack 已被 call_yield 截断到 `a`，所以 stack.len() = a
    let stack_base = state.exec.stack.len();
    for arg in resume_args {
        state.exec.stack.push(arg.clone());
    }
    // 根据 yield 的 nresults 调整
    if yield_nresults >= 0 {
        // 固定数量: 填充 nil 或截断
        while (state.exec.stack.len() - stack_base) < yield_nresults as usize {
            state.exec.stack.push(TValue::Nil(NilKind::Strict));
        }
        state.exec.stack.truncate(stack_base + yield_nresults as usize);
    }
    // nresults < 0 (MULTRET): 保留所有参数
    state.exec.top = state.exec.stack.len();

    Ok(())
}

// ============================================================================
// coroutine.yield(...) — 对应 C 的 lua_coyield
// ============================================================================

fn call_yield(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    // 检查是否在协程中
    if state.exec.current_thread.is_none() {
        return Err(VmError::RuntimeError(
            "attempt to yield from outside a coroutine".to_string(),
        ));
    }
    // 检查是否可 yield（无非可 yield 的 C 函数调用在栈上）
    if state.exec.n_ny_calls > 0 {
        return Err(VmError::RuntimeError(
            "attempt to yield across a C-call boundary".to_string(),
        ));
    }

    // 收集 yield 值
    let yield_values: Vec<TValue> = (0..nargs)
        .map(|i| {
            let idx = a + 1 + i;
            if idx < state.exec.stack.len() {
                state.exec.stack[idx].clone()
            } else {
                TValue::Nil(NilKind::Strict)
            }
        })
        .collect();
    // 截断栈到 `a`（移除 yield 函数和参数）
    state.exec.stack.truncate(a);
    state.exec.top = a;

    // 保存 yield 的 nresults 到协程 exec（恢复时用于调整 resume 参数，
    // yield 时随 exec 交换到 ThreadContext.exec）
    state.exec.saved_yield_nresults = nresults;

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
        context: context.clone(),
        c_state: std::cell::Cell::new(std::ptr::null_mut()),
    };
    let thread_rc = Rc::new(thread);
    // 设置 thread_ref，让 coroutine.running() 能返回同一对象
    context.borrow_mut().thread_ref = Rc::downgrade(&thread_rc);
    // 收集开 upvalue 信息并保存到 ThreadContext（不关闭！）
    // 首次 resume 时根据同栈/跨栈决定用最新值还是 saved_value 关闭
    // 这样支持 `A = coroutine.wrap(function() ... A() ... end)` 的自引用模式
    // （A 在 call_wrap 时还是旧值，在 call_wrap_fn 首次调用时才被赋值为 wrap RustClosure）
    let pending = collect_wrap_upvals_info(&thread_rc, state);
    let creator_ptr = state.exec
        .current_thread
        .as_ref()
        .map(|c| Rc::as_ptr(c) as usize)
        .unwrap_or(0);
    {
        let mut ctx = thread_rc.context.borrow_mut();
        ctx.wrap_creator_thread_ptr = creator_ptr;
        ctx.pending_wrap_upvals = pending;
    }
    // 创建 RustClosure，upvalues[0] 持有协程 Thread
    // RustClosure 可被 GC 跟踪（state.rs::mark_tvalue 遍历 upvalues），
    // 协程死亡时将 upvalues[0] 置为 nil，替代原 state.wrap_coros[idx] = None
    let wrap_closure = crate::objects::RustClosure {
        func: call_wrap_fn,
        name: c"wrap".as_ptr() as *const u8,
        upvalues: Rc::new(RefCell::new(vec![TValue::Thread(thread_rc)])),
    };
    push_single_result(
        state,
        a,
        nresults,
        TValue::RustClosure(Rc::new(wrap_closure)),
    );
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
    // 从 state.exec.stack[a] 取 RustClosure → upvalues[0] 取 Thread
    let rc = match state.exec.stack.get(a) {
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
    let resume_args: Vec<TValue> = (0..nargs).map(|i| state.exec.stack[a + 1 + i].clone()).collect();

    // 收集开 upvalue 信息（在 save_caller_context 之前，state.exec.stack 仍是父栈）
    // 首次 resume 时从 ThreadContext 取出 pending_wrap_upvals（call_wrap 时保存），
    // 根据同栈/跨栈决定关闭值：
    //   同栈: 从 state.exec.stack 读最新值（支持变量在 call_wrap 后被重新赋值，如自引用 wrap）
    //   跨栈: state.exec.stack 不是原始父栈，用 call_wrap 时保存的 saved_value
    let is_first_resume = !thread.context.borrow().started;
    let same_stack: bool;
    let open_upvals: Vec<OpenUpvalInfo> = if is_first_resume {
        let (pending, creator_ptr) = {
            let mut ctx = thread.context.borrow_mut();
            let p = std::mem::take(&mut ctx.pending_wrap_upvals);
            let c = ctx.wrap_creator_thread_ptr;
            (p, c)
        };
        let caller_ptr = state.exec
            .current_thread
            .as_ref()
            .map(|c| Rc::as_ptr(c) as usize)
            .unwrap_or(0);
        same_stack = creator_ptr == caller_ptr;
        let mut open_upvals = Vec::new();
        let mut origins: Vec<(UpValRef, usize)> = Vec::new();
        for (uv_ref, orig_idx, saved_val) in pending {
            let val = if same_stack {
                state.exec
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

    // 保存调用者的 C 栈保护计数（swap 时随调用者 exec 进出 ctx.exec 自动恢复）
    let saved_n_ccalls = state.exec.n_ccalls;
    // 暂存调用者栈到 state.caller_gc_stacks — 协程执行期间 GC 需要看到调用者栈
    // 中的 wrap table 引用，否则内层协程会被误判为不可达（big.lua 嵌套 wrap 场景）
    state
        .caller_gc_stacks
        .push(std::mem::take(&mut state.exec.stack));

    // 设置协程上下文
    let co_context = thread.context.clone();

    // setup 内部完成 exec 交换：调用者 exec -> ctx.exec，协程 exec -> state.exec (O(1))
    let setup_result = if is_first_resume {
        setup_first_resume(state, &thread, &resume_args)
    } else {
        setup_subsequent_resume(state, &co_context, &resume_args)
    };

    if let Err(e) = setup_result {
        // setup 失败（仅首次 resume 的"body 不是函数"路径）：未发生交换，
        // state.exec 仍是调用者的（栈为空），恢复调用者栈
        state.exec.stack = state.caller_gc_stacks.pop().unwrap_or_default();
        return Err(e);
    }

    // 首次 resume setup 成功后立即标记 started=true，
    // 这样协程 yield 后的后续 resume 会走 setup_subsequent_resume 而非重新初始化
    if is_first_resume {
        co_context.borrow_mut().started = true;
    }

    // 按调用者计数初始化协程的 C 栈保护
    // (wrap 调用不经过 op_call 的 n_ccalls 递增路径，在此手动递增)
    init_coroutine_guards(state, saved_n_ccalls);
    if state.exec.n_ccalls >= crate::state::LUAI_MAXCCALLS {
        // C 栈溢出：交换回调用者 exec 并恢复调用者栈
        swap_exec(state, &co_context);
        state.exec.stack = state.caller_gc_stacks.pop().unwrap_or_default();
        return Err(VmError::RuntimeError("C stack overflow".to_string()));
    }

    // 设置 current_thread 和状态
    state.exec.current_thread = Some(co_context.clone());
    co_context.borrow_mut().status = ThreadStatus::Normal;

    // 首次 resume 时触发 call hook（对应 C Lua 的 luaD_hook(L, LUA_HOOKCALL, -1, 0, 0)）
    if is_first_resume && state.exec.hook_mask & 1 != 0 && state.exec.hook_func.is_some() {
        VmExecutor::call_hook(state, "call", -1, None, 0, 0)?;
    }

    // 执行
    let exec_result = VmExecutor::execute_with_state(state);

    // 处理结果
    let (result_values, is_dead, error_val) = match exec_result {
        Ok(VmResult::Yield { values }) => {
            // 关闭 yield 出来的闭包的 Open upvalue（协程栈还有效时）
            let yield_origins = close_yield_upvals(&values, state);
            // C 函数 __close (如 coroutine.yield 作为 __close) yield 时，
            // state.exec.pc 指向 OP_RETURN/OP_CLOSE（非 CALL 指令），不应 +1。
            // 此时 PcallProtection.saved_pc == state.exec.pc（都指向 OP_RETURN/OP_CLOSE）。
            // Lua __close yield 时，state.exec.pc 指向 __close 中的 CALL 指令，
            // saved_pc 指向 OP_RETURN/OP_CLOSE，二者不同，需要 +1 跳过 CALL。
            let is_c_close_yield = state.exec.pcall_protection_stack.last().map_or(false, |t| {
                t.is_close_continuation && t.saved_filled && t.saved_pc == state.exec.pc
            });
            state.exec.pc = if is_c_close_yield {
                state.exec.pc
            } else {
                state.exec.pc + 1
            };
            // 保存 yield 关闭的 upvalue 来源（resume 时同步回协程栈）
            co_context.borrow_mut().yield_upval_origins = yield_origins;
            // 交换回调用者（协程 exec 连同 pc/pcall 保护等进入 ctx.exec）
            swap_exec(state, &co_context);
            co_context.borrow_mut().status = ThreadStatus::Suspended;
            (values, false, None)
        }
        Ok(VmResult::Return {
            nresults: ret_n,
            result_base,
        }) => {
            let return_values: Vec<TValue> = (0..ret_n)
                .map(|i| {
                    if result_base + i < state.exec.stack.len() {
                        state.exec.stack[result_base + i].clone()
                    } else {
                        TValue::Nil(NilKind::Strict)
                    }
                })
                .collect();
            {
                let mut ctx = co_context.borrow_mut();
                ctx.status = ThreadStatus::OK;
                ctx.started = true;
                // 交换回调用者（协程 exec 进入 ctx.exec）
                std::mem::swap(&mut state.exec, &mut ctx.exec);
            }
            (return_values, true, None)
        }
        Ok(_) => {
            {
                let mut ctx = co_context.borrow_mut();
                ctx.status = ThreadStatus::OK;
                // 交换回调用者
                std::mem::swap(&mut state.exec, &mut ctx.exec);
            }
            (Vec::new(), true, None)
        }
        Err(e) => {
            // 保存 error 状态到 ctx（必须在 TBC 关闭之前保存）
            let (err_call_info, err_stack, err_base, err_pc) = {
                let save_end = state.exec.base.min(state.exec.stack.len());
                (
                    std::mem::take(&mut state.exec.call_info),
                    state.exec.stack[..save_end].to_vec(),
                    state.exec.base,
                    state.exec.pc.wrapping_add(1),
                )
            };
            // 关闭协程的 TBC 变量，对应 C Lua 中 luaD_closeprotected -> luaF_close
            let close_level = state.exec.base;
            if close_level > 0 && close_level <= state.exec.stack.len() {
                state.exec.stack[close_level - 1] = TValue::Nil(NilKind::Strict);
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
            {
                let mut ctx = co_context.borrow_mut();
                // 交换回调用者（协程 exec 进入 ctx.exec，保留错误状态）
                std::mem::swap(&mut state.exec, &mut ctx.exec);
                ctx.exec.call_info = err_call_info;
                ctx.exec.stack = err_stack;
                ctx.exec.base = err_base;
                ctx.exec.pc = err_pc;
                ctx.status = ThreadStatus::Error;
                ctx.error_msg = Some(err_val.clone());
            }
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

    // 各分支已完成 swap 交换回调用者 exec；恢复调用者栈
    state.exec.stack = state.caller_gc_stacks.pop().unwrap_or_default();

    // 把 Closed upvalue 值同步回父栈，协程结束则恢复 Open
    // 同栈时写回 state.exec.stack（原始父栈）；跨栈时跳过写回（父栈不可访问），仅恢复 Open
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
    state.exec.stack.truncate(a);
    if nresults != 0 {
        for v in result_values {
            state.exec.stack.push(v);
        }
        if nresults > 0 {
            let current = state.exec.stack.len() - a;
            if current > nresults as usize {
                state.exec.stack.truncate(a + nresults as usize);
            } else {
                while (state.exec.stack.len() - a) < nresults as usize {
                    state.exec.stack.push(TValue::Nil(NilKind::Strict));
                }
            }
        }
    }
    state.exec.top = state.exec.stack.len();

    Ok(())
}

// ============================================================================
// C API lua_resume 的实现 — 供 capi.rs::lua_resume 调用
// ============================================================================
//
// 与 Lua 层 call_resume 的区别：
// - NL 是独立的 LuaState（由 lua_newthread 创建），不需要 save/restore caller context
// - 函数从 NL 栈获取（由 lua_xmove 移入），而非从 thread.function 获取
// - 结果直接放回 NL 栈，由调用方通过 lua_xmove 取回

/// C API lua_resume 的核心实现。
///
/// 从 `state.exec.current_thread` 获取 ThreadContext，根据 started 标志判断首次/后续 resume。
/// 首次 resume 时从栈取函数（栈布局: [nil, func, arg1, ..., argN]），创建临时 LuaThread
/// 复用 setup_first_resume 逻辑。后续 resume 直接调用 setup_subsequent_resume。
///
/// 返回 (status, nresults)，status 为 LUA_OK/LUA_YIELD/LUA_ERRRUN，
/// nresults 为结果数（已放在 state.exec.stack 上）。
#[cfg(not(feature = "cmp_c"))]
pub fn c_api_resume(state: &mut LuaState, nargs: usize) -> Result<(i32, usize), VmError> {
    let co_context = match state.exec.current_thread.clone() {
        Some(ctx) => ctx,
        None => {
            return Err(VmError::RuntimeError(
                "lua_resume: not a C API thread".to_string(),
            ));
        }
    };

    // 检查协程状态
    let co_status = co_context.borrow().status;
    match co_status {
        ThreadStatus::Suspended => {}
        ThreadStatus::Normal => {
            return Ok((
                crate::capi::LUA_ERRRUN,
                push_error(state, "cannot resume non-suspended coroutine"),
            ));
        }
        ThreadStatus::OK | ThreadStatus::Error => {
            return Ok((
                crate::capi::LUA_ERRRUN,
                push_error(state, "cannot resume dead coroutine"),
            ));
        }
    }

    let is_first_resume = !co_context.borrow().started;
    let saved_n_ccalls = state.exec.n_ccalls;
    let saved_n_ny_calls = state.exec.n_ny_calls;
    // 准备 setup（首次/后续）
    let setup_result = if is_first_resume {
        // 首次 resume: 从 NL 栈取函数和参数
        // 栈布局: [nil, func, arg1, ..., argN]
        let stack_len = state.exec.stack.len();
        if stack_len < nargs + 1 {
            state.exec.n_ny_calls = saved_n_ny_calls;
            return Err(VmError::RuntimeError(
                "lua_resume: not enough values on stack".to_string(),
            ));
        }
        let func_idx = stack_len - nargs - 1;
        let func = state.exec.stack[func_idx].clone();
        let resume_args: Vec<TValue> = state.exec.stack[func_idx + 1..].to_vec();

        // 创建临时 LuaThread（共享 context），让 setup_first_resume 能取到 function
        let temp_thread = LuaThread {
            stack: Vec::new(),
            status: ThreadStatus::Suspended,
            function: Some(Box::new(func)),
            is_main: false,
            context: co_context.clone(),
            c_state: std::cell::Cell::new(std::ptr::null_mut()),
        };
        setup_first_resume(state, &temp_thread, &resume_args)
    } else {
        // 后续 resume: 收集栈顶 nargs 个值作为 resume 参数
        let stack_len = state.exec.stack.len();
        let start = if stack_len >= nargs {
            stack_len - nargs
        } else {
            stack_len
        };
        let resume_args: Vec<TValue> = state.exec.stack[start..].to_vec();
        setup_subsequent_resume(state, &co_context, &resume_args)
    };

    if let Err(e) = setup_result {
        state.exec.n_ccalls = saved_n_ccalls;
        state.exec.n_ny_calls = saved_n_ny_calls;
        return Err(e);
    }

    // 首次 resume setup 成功后立即标记 started=true，
    // 这样协程 yield 后的后续 resume 会走 setup_subsequent_resume 而非重新初始化
    if is_first_resume {
        co_context.borrow_mut().started = true;
    }

    // 按调用者计数初始化协程的 C 栈保护（跨多次 resume 不累积）
    init_coroutine_guards(state, saved_n_ccalls);
    if state.exec.n_ccalls >= crate::state::LUAI_MAXCCALLS {
        // C 栈溢出：协程 exec 放回 ctx.exec（保持挂起），恢复 NL 计数器
        co_context.borrow_mut().exec = std::mem::take(&mut state.exec);
        state.exec.current_thread = Some(co_context.clone());
        state.exec.n_ccalls = saved_n_ccalls;
        state.exec.n_ny_calls = saved_n_ny_calls;
        return Ok((
            crate::capi::LUA_ERRRUN,
            push_error(state, "C stack overflow"),
        ));
    }

    // 设置 current_thread 和状态为 Normal
    state.exec.current_thread = Some(co_context.clone());
    co_context.borrow_mut().status = ThreadStatus::Normal;

    // 执行
    let exec_result = VmExecutor::execute_with_state(state);

    // 处理结果
    let (status, nresults) = match exec_result {
        Ok(VmResult::Yield { values }) => {
            // yield: 保存 VM 状态到 ThreadContext（整体移动 ExecState）
            let n = values.len();
            // C 函数 __close yield 时 pc 不应 +1（见 call_resume 同处说明）
            let is_c_close_yield = state.exec.pcall_protection_stack.last().map_or(false, |t| {
                t.is_close_continuation && t.saved_filled && t.saved_pc == state.exec.pc
            });
            state.exec.pc = if is_c_close_yield {
                state.exec.pc
            } else {
                state.exec.pc + 1
            };
            {
                let mut ctx = co_context.borrow_mut();
                ctx.exec = std::mem::take(&mut state.exec);
                ctx.status = ThreadStatus::Suspended;
            }
            // NL 的 exec 被取空后恢复 current_thread，供下一次 lua_resume 定位协程
            state.exec.current_thread = Some(co_context.clone());
            // push yield 值到 state.exec.stack: [nil, val1, val2, ...]
            state.exec.stack = Vec::with_capacity(n + 1);
            state.exec.stack.push(TValue::Nil(NilKind::Strict));
            for v in values {
                state.exec.stack.push(v);
            }
            state.exec.top = state.exec.stack.len();
            (crate::capi::LUA_YIELD, n)
        }
        Ok(VmResult::Return {
            nresults: ret_n,
            result_base,
        }) => {
            // 协程返回 — 取出返回值，重新设置栈
            let co_stack = std::mem::take(&mut state.exec.stack);
            {
                let mut ctx = co_context.borrow_mut();
                ctx.status = ThreadStatus::OK;
                ctx.started = true;
                // 协程 exec 存入 ctx（dead），NL 恢复默认
                ctx.exec = std::mem::take(&mut state.exec);
                ctx.exec.call_info.clear();
            }
            // NL 的 exec 被取空后恢复 current_thread
            state.exec.current_thread = Some(co_context.clone());
            // push 返回值: [nil, result1, result2, ...]
            state.exec.stack = Vec::with_capacity(ret_n + 1);
            state.exec.stack.push(TValue::Nil(NilKind::Strict));
            for i in 0..ret_n {
                let val = if result_base + i < co_stack.len() {
                    co_stack[result_base + i].clone()
                } else {
                    TValue::Nil(NilKind::Strict)
                };
                state.exec.stack.push(val);
            }
            state.exec.top = state.exec.stack.len();
            (crate::capi::LUA_OK, ret_n)
        }
        Ok(_) => {
            {
                let mut ctx = co_context.borrow_mut();
                ctx.status = ThreadStatus::OK;
                ctx.started = true;
                ctx.exec = std::mem::take(&mut state.exec);
                ctx.exec.call_info.clear();
            }
            state.exec.current_thread = Some(co_context.clone());
            state.exec.stack = vec![TValue::Nil(NilKind::Strict)];
            state.exec.top = 1;
            (crate::capi::LUA_OK, 0)
        }
        Err(e) => {
            // 错误: 保存错误状态到 ctx（pre-close 快照），关闭 TBC 变量
            let (err_call_info, err_stack, err_base, err_pc) = {
                let save_end = state.exec.base.min(state.exec.stack.len());
                (
                    std::mem::take(&mut state.exec.call_info),
                    state.exec.stack[..save_end].to_vec(),
                    state.exec.base,
                    state.exec.pc.wrapping_add(1),
                )
            };
            // 关闭 TBC 变量
            let close_level = state.exec.base;
            if close_level > 0 && close_level <= state.exec.stack.len() {
                state.exec.stack[close_level - 1] = TValue::Nil(NilKind::Strict);
            }
            let _ = crate::func::close(state, close_level, 1, 0);
            // 获取错误值
            let err_val = state.last_error_value.take().unwrap_or_else(|| match &e {
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
            {
                let mut ctx = co_context.borrow_mut();
                ctx.exec = std::mem::take(&mut state.exec);
                ctx.exec.call_info = err_call_info;
                ctx.exec.stack = err_stack;
                ctx.exec.base = err_base;
                ctx.exec.pc = err_pc;
                ctx.status = ThreadStatus::Error;
            }
            state.exec.current_thread = Some(co_context.clone());
            // push 错误消息: [nil, err_msg]
            state.exec.stack = vec![TValue::Nil(NilKind::Strict), err_val];
            state.exec.top = 2;
            (crate::capi::LUA_ERRRUN, 1)
        }
    };

    // 恢复 n_ccalls / n_ny_calls
    state.exec.n_ccalls = saved_n_ccalls;
    state.exec.n_ny_calls = saved_n_ny_calls;

    Ok((status, nresults))
}

/// 把错误消息 push 到 state.exec.stack，返回 nresults (1)
fn push_error(state: &mut LuaState, msg: &str) -> usize {
    state.exec.stack = vec![
        TValue::Nil(NilKind::Strict),
        TValue::Str(state.intern_str(msg)),
    ];
    state.exec.top = 2;
    1
}

// ============================================================================
// 打开 Coroutine 库 — 对应 C 的 luaopen_coroutine
// ============================================================================

pub fn open_coroutine_lib(state: &mut LuaState) {
    let mut lib = Table::new();

    // 注册 BuiltinFn 的辅助闭包：用函数指针 + 名字注册到表
    // (state 作为参数传入，避免闭包捕获 state 导致借用冲突)
    let register = |lib: &mut crate::table::Table,
                    state: &LuaState,
                    name: &'static std::ffi::CStr,
                    func: crate::objects::BuiltinFnPtr| {
        let key = TValue::Str(state.intern_str(name.to_str().unwrap_or("")));
        let name_ptr = name.as_ptr() as *const u8;
        lib.set(
            key,
            TValue::BuiltinFn(BuiltinFn::impure(func, name_ptr)),
        );
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
