use crate::execute::VmError;
use crate::objects::*;
use crate::state::LuaState;
use crate::strings::lua_string_as_str;
use std::cell::RefCell;
use std::rc::Rc;

pub fn new_proto<'a>() -> Proto<'a> {
    Proto {
        num_params: 0,
        flag: 0,
        max_stack_size: 0,
        size_upvalues: 0,
        size_k: 0,
        size_code: 0,
        size_line_info: 0,
        size_p: 0,
        size_loc_vars: 0,
        size_abs_line_info: 0,
        line_defined: 0,
        last_line_defined: 0,
        constants: Rc::new(Vec::with_capacity(8)),
        code: Rc::new(Vec::with_capacity(8)),
        protos: Rc::new(Vec::new()),
        upvalues: Rc::new(Vec::with_capacity(2)),
        line_info: Vec::with_capacity(8),
        abs_line_info: Vec::new(),
        loc_vars: Vec::new(),
        source: None,
    }
}

/// 取槽位中的 upvalue 引用；槽位空闲（未分配，或已因 upvalue 关闭而回收）时返回 None。
#[inline]
pub fn upval_slot<'s, 'a>(state: &'s LuaState<'a>, uv_idx: usize) -> Option<&'s UpValRef<'a>> {
    state.open_upvals.get(uv_idx).and_then(|slot| slot.as_ref())
}

/// 按 Rc 指针查找 arena 中的槽位索引。
pub fn find_upval_slot<'a>(state: &LuaState<'a>, uv_ref: &UpValRef<'a>) -> Option<usize> {
    let ptr = Rc::as_ptr(uv_ref) as usize;
    state
        .open_upvals
        .iter()
        .position(|r| r.as_ref().map_or(false, |x| Rc::as_ptr(x) as usize == ptr))
}

/// 把 upvalue 引用登记进 arena，返回其槽位索引（已登记则直接返回既有索引）。
///
/// 协程 yield/resume 会重新打开此前关闭的 upvalue，而其槽位可能已被关闭路径回收；
/// 此时必须重新分配槽位，否则该 upvalue 会变成「Open 但不在 arena / 不在 open_upval 链表」，
/// 后续 close 遍历将漏掉它。
pub fn register_upval_slot<'a>(state: &mut LuaState<'a>, uv_ref: &UpValRef<'a>) -> usize {
    if let Some(idx) = find_upval_slot(state, uv_ref) {
        return idx;
    }
    let idx = match state.free_upvals.pop() {
        Some(i) => i,
        None => state.open_upvals.len(),
    };
    if idx < state.open_upvals.len() {
        debug_assert!(
            state.open_upvals[idx].is_none(),
            "reused upvalue slot must be free"
        );
        state.open_upvals[idx] = Some(uv_ref.clone());
    } else {
        state.open_upvals.push(Some(uv_ref.clone()));
    }
    idx
}

pub fn find_upval(state: &mut LuaState, level: usize) -> usize {
    if !state.is_in_twups {
        state.g_mut().twups_linked = true;
    }
    let mut prev: Option<usize> = None;
    let mut current = state.open_upval;
    while let Some(uv_idx) = current {
        let (uv_level, next) = match upval_slot(state, uv_idx) {
            Some(uv_ref) => match &*uv_ref.borrow() {
                UpVal::Open {
                    stack_index, next, ..
                } => (Some(*stack_index), *next),
                UpVal::Closed { .. } => (None, None),
            },
            None => (None, None),
        };
        let uv_level = match uv_level {
            Some(l) => l,
            None => {
                // Closed 或已回收槽位不应出现在链表里；防御性终止
                current = next;
                continue;
            }
        };
        if uv_level < level {
            break;
        }
        if uv_level == level {
            return uv_idx;
        }
        prev = Some(uv_idx);
        current = next;
    }
    new_upval(state, level, prev)
}

fn new_upval(state: &mut LuaState, level: usize, prev: Option<usize>) -> usize {
    // 优先复用已回收槽位（closed upvalue 关闭时归还），否则追加。
    // 复用安全：被释放的槽位不再出现在 open_upval 链表中。
    let uv_idx = match state.free_upvals.pop() {
        Some(idx) => idx,
        None => state.open_upvals.len(),
    };
    let mut next: Option<usize> = None;
    match prev {
        Some(p_idx) => {
            {
                let p_ref = upval_slot(state, p_idx).expect("prev upvalue slot must be live");
                if let UpVal::Open { next: p_next, .. } = &*p_ref.borrow() {
                    next = *p_next;
                }
            }
            {
                let p_ref = upval_slot(state, p_idx).expect("prev upvalue slot must be live");
                if let UpVal::Open { ref mut next, .. } = &mut *p_ref.borrow_mut() {
                    *next = Some(uv_idx);
                }
            }
        }
        None => {
            next = state.open_upval;
            state.open_upval = Some(uv_idx);
        }
    }
    if let Some(n_idx) = next {
        let n_ref = upval_slot(state, n_idx).expect("next upvalue slot must be live");
        if let UpVal::Open {
            ref mut previous, ..
        } = &mut *n_ref.borrow_mut()
        {
            *previous = Some(uv_idx);
        }
    }
    let new_slot = Rc::new(RefCell::new(UpVal::Open {
        stack_index: level,
        next,
        previous: prev,
        tbc: false,
    }));
    if uv_idx < state.open_upvals.len() {
        debug_assert!(
            state.open_upvals[uv_idx].is_none(),
            "reused upvalue slot must be free"
        );
        state.open_upvals[uv_idx] = Some(new_slot);
    } else {
        state.open_upvals.push(Some(new_slot));
    }
    uv_idx
}

pub fn close_upval(state: &mut LuaState, uv_idx: usize) {
    unsafe { state.gc_mut().cond_gc() };
    // 槽位空闲（重复关闭 / 陈旧索引）→ 无操作
    let uv_ref = match upval_slot(state, uv_idx) {
        Some(r) => r.clone(),
        None => return,
    };
    let val = {
        let uv_ref = uv_ref.borrow();
        match &*uv_ref {
            UpVal::Open { stack_index, .. } => state
                .stack
                .get(*stack_index)
                .cloned()
                .unwrap_or(TValue::Nil(NilKind::Strict)),
            UpVal::Closed { value } => value.clone(),
        }
    };
    // GC barrier: when upvalue is closed, mark the value
    if let Some(gc_id) = crate::vm::gc_id_of_tvalue(&val) {
        unsafe { state.gc_mut().mark_object(gc_id) };
    }
    unlink_upval(state, uv_idx);
    // 关闭值写入共享 cell：持有该 upvalue 的闭包可见 Closed 值
    *uv_ref.borrow_mut() = UpVal::Closed { value: val };
    // 归还 arena 强引用与槽位 —— 关闭后的 upvalue 仅由引用它的闭包维持存活。
    // 对应 C 的 luaF_close：closed upvalue 不再是 openupval 链表节点，内存由 GC 回收。
    state.open_upvals[uv_idx] = None;
    state.free_upvals.push(uv_idx);
}

pub fn unlink_upval<'a>(state: &mut LuaState<'a>, uv_idx: usize) {
    let (prev, nxt) = {
        let uv_ref = match upval_slot(state, uv_idx) {
            Some(r) => r,
            None => return,
        };
        match &*uv_ref.borrow() {
            UpVal::Open { previous, next, .. } => (*previous, *next),
            _ => return,
        }
    };
    match prev {
        Some(p_idx) => {
            if let Some(p_ref) = upval_slot(state, p_idx) {
                if let UpVal::Open { ref mut next, .. } = &mut *p_ref.borrow_mut() {
                    *next = nxt;
                }
            }
        }
        None => {
            state.open_upval = nxt;
        }
    }
    if let Some(n_idx) = nxt {
        if let Some(n_ref) = upval_slot(state, n_idx) {
            if let UpVal::Open {
                ref mut previous, ..
            } = &mut *n_ref.borrow_mut()
            {
                *previous = prev;
            }
        }
    }
}

pub fn close<'a>(
    state: &mut LuaState<'a>,
    level: usize,
    status: i32,
    yy: i32,
) -> Result<(), VmError<'a>> {
    // force_noyield_close: coroutine.close() 关闭自身时设置（对应 C Lua 的 lua_closethread(co, L)
    // 中 co == L 场景）。C Lua 会立即调用 luaF_close(L, L->stack, LUA_OK, 1) 关闭所有 TBC 变量，
    // 并通过 luaD_throwbaselevel 抛到 base level。我们的实现未完整支持此语义，改为设置标志，
    // 让 OP_RETURN 的 func::close 使用不可 yield 模式 (yy=0)，使 __close 中的 yield 失败
    // （对应 C Lua 中 nny > 0 时 yield 报错的场景）。
    let yy = if state.force_noyield_close { 0 } else { yy };

    // 快速路径 (无 TBC 上值): 就地关闭, 避免 to_close Vec 分配。
    // 先第一趟检查是否有 TBC — 有 TBC 时必须走慢路径 (保持 __close yield 时
    // 剩余 upvalue 不提前关闭的顺序语义)。
    let mut current = state.open_upval;
    let mut found_tbc = false;
    while let Some(uv_idx) = current {
        let (should_close, next, is_tbc) = match upval_slot(state, uv_idx) {
            Some(uv_ref) => {
                let uv_ref = uv_ref.borrow();
                match &*uv_ref {
                    UpVal::Open {
                        stack_index,
                        next,
                        tbc,
                        ..
                    } => (*stack_index >= level, *next, *tbc),
                    UpVal::Closed { .. } => (false, None, false),
                }
            }
            None => (false, None, false),
        };
        if should_close && is_tbc {
            found_tbc = true;
        }
        current = next;
    }

    if !found_tbc {
        // 无 TBC: 第二趟就地关闭 (对应 C 的 luaF_close 从链表头摘取关闭)
        let mut current = state.open_upval;
        while let Some(uv_idx) = current {
            let (should_close, next) = match upval_slot(state, uv_idx) {
                Some(uv_ref) => {
                    let uv_ref = uv_ref.borrow();
                    match &*uv_ref {
                        UpVal::Open {
                            stack_index, next, ..
                        } => (*stack_index >= level, *next),
                        UpVal::Closed { .. } => (false, None),
                    }
                }
                None => (false, None),
            };
            if should_close {
                close_upval(state, uv_idx);
            }
            current = next;
        }
        // 直接返回，不修改错误状态
        // (避免 status!=0 但无 TBC 变量时用 Nil 覆盖原有错误)
        state.g_mut().twups_linked = false;
        return Ok(());
    }

    // 慢路径: 存在 TBC 上值 — 重新收集并完整处理
    // (保持 __close 元方法调用顺序与错误传播语义)
    let mut to_close: Vec<usize> = Vec::new();
    let mut current = state.open_upval;
    while let Some(uv_idx) = current {
        let (should_close, next, _stack_idx) = match upval_slot(state, uv_idx) {
            Some(uv_ref) => {
                let uv_ref = uv_ref.borrow();
                match &*uv_ref {
                    UpVal::Open {
                        stack_index, next, ..
                    } => (*stack_index >= level, *next, *stack_index),
                    UpVal::Closed { .. } => (false, None, 0),
                }
            }
            None => (false, None, 0),
        };
        if should_close {
            to_close.push(uv_idx);
        }
        current = next;
    }

    // 没有需要关闭的 upvalue: 直接返回
    if to_close.is_empty() {
        state.g_mut().twups_linked = false;
        return Ok(());
    }

    // 对每个 should_close 的 upvalue，按顺序处理
    // 对 TBC upvalue，先调用 __close metamethod，再 close_upval
    // 错误传播: __close 出错时，错误值传递给下一个 __close 的 err 参数
    let mut current_err: TValue = if status != 0 {
        state
            .last_error_value
            .clone()
            .unwrap_or(TValue::Nil(NilKind::Strict))
    } else {
        TValue::Nil(NilKind::Strict)
    };
    let mut has_error = status != 0;

    for uv_idx in to_close {
        let is_tbc = match upval_slot(state, uv_idx) {
            Some(uv_ref) => matches!(&*uv_ref.borrow(), UpVal::Open { tbc: true, .. }),
            None => false,
        };
        if is_tbc {
            // TBC upvalue: 读取栈上的值（在 close_upval 之前，因为 close_upval 会改为 Closed）
            let val = match upval_slot(state, uv_idx) {
                Some(uv_ref) => {
                    let uv_ref = uv_ref.borrow();
                    if let UpVal::Open { stack_index, .. } = &*uv_ref {
                        state
                            .stack
                            .get(*stack_index)
                            .cloned()
                            .unwrap_or(TValue::Nil(NilKind::Strict))
                    } else {
                        TValue::Nil(NilKind::Strict)
                    }
                }
                None => TValue::Nil(NilKind::Strict),
            };
            // 先 close_upval（从 open 链表移除），再调用 __close
            // 对应 C Lua 的 luaF_close: unlinkupval 先于 callclosemethod
            // 这样 yield 后重新执行 close 时，不会再次处理已关闭的 upvalue（幂等）
            close_upval(state, uv_idx);
            // 只对非 nil 的值调用 __close
            if !matches!(val, TValue::Nil(_)) {
                // 清空 last_error_value 以便检测 __close 是否出错
                state.last_error_value = None;
                state.last_error_msg.clear();
                // 调用 __close(val, err?) — 无错误时只传 1 个参数 (对应 C 的 errobj=NULL)
                let err_ref = if has_error { Some(&current_err) } else { None };
                match crate::tm::call_close_method(state, &val, err_ref, yy != 0) {
                    Ok(_) => {
                        // __close 成功: 不改变错误状态
                    }
                    Err(VmError::Yield(values)) => {
                        // __close yield: 传播 yield，不继续处理剩余 upvalue
                        // close_upval 已执行，upvalue 已从 open 链表移除
                        // 恢复 last_error_value（被 line 280 清除），供 close_yield 处理使用
                        // 对应 C Lua 的 CIST_RECST 保存的错误状态跨 yield 保留
                        if has_error {
                            state.last_error_value = Some(current_err.clone());
                        }
                        return Err(VmError::Yield(values));
                    }
                    Err(e) => {
                        // __close 出错: 从返回的 VmError 提取错误值，更新 current_err
                        // (pcall 已清除 last_error_value，不能从 state 读取)
                        current_err = match e {
                            VmError::RuntimeErrorValue(val) => val,
                            VmError::RuntimeError(s) => state.intern_str(&s),
                            other => state.intern_str(&format!("{}", other)),
                        };
                        has_error = true;
                    }
                }
            }
        } else {
            close_upval(state, uv_idx);
        }
    }
    // 如果 close 过程中有错误，设置 state.last_error_value 供调用者检查
    if has_error {
        state.last_error_value = Some(current_err.clone());
        // 同时设置 last_error_msg（用于 close_suspended_coroutine 检测错误）
        let msg = match &current_err {
            s @ (TValue::LongStr(_) | TValue::ShortStr(_)) => lua_string_as_str(s).to_string(),
            _ => format!("{}", current_err),
        };
        state.last_error_msg = msg;
    }
    state.g_mut().twups_linked = false;
    if has_error {
        // __close 出错: 返回错误以中断调用者的执行（对应 C 的 luaD_throw）
        // state.last_error_value 已包含最终错误值，调用者可通过它获取原始 TValue
        // 字符串错误用 RuntimeError，非字符串错误用 RuntimeErrorValue 保留原始 TValue
        Err(match &current_err {
            s @ (TValue::LongStr(_) | TValue::ShortStr(_)) => {
                VmError::RuntimeError(lua_string_as_str(s).to_string())
            }
            _ => VmError::RuntimeErrorValue(current_err.clone()),
        })
    } else {
        Ok(())
    }
}

pub fn new_tbc_upval<'a>(
    state: &mut LuaState<'a>,
    level: usize,
) -> Result<Option<usize>, VmError<'a>> {
    // 对应 C 的 luaF_newtbcupval: 检查 __close 元方法，复用或创建 open upvalue，然后标记 tbc
    let val = state
        .stack
        .get(level)
        .cloned()
        .unwrap_or(TValue::Nil(NilKind::Strict));
    // C 的 luaF_newtbcupval: l_isfalse 检查，跳过 nil/false
    if val.is_false() {
        return Ok(None); // false/nil 不需要关闭
    }
    // 对应 C 的 checkclosemth: 检查 __close 元方法是否存在
    let has_close = crate::tm::get_tm_by_obj(
        &val,
        crate::tm::TagMethod::Close,
        &state.dmt,
        &state.tmnames,
    )
    .is_some();
    if !has_close {
        // 获取变量名 — 对应 C 的 luaG_findlocal(L, L->ci, idx, NULL)
        let varname = get_var_name_at(state, level).unwrap_or_else(|| "?".to_string());
        return Err(VmError::RuntimeError(format!(
            "variable '{}' got a non-closable value",
            varname
        )));
    }
    // TBC upvalue 复用 open_upval 链表（通过 find_upval 加入），用 tbc 字段标记
    let uv_idx = find_upval(state, level);
    if let Some(uv_ref) = upval_slot(state, uv_idx) {
        if let UpVal::Open { ref mut tbc, .. } = &mut *uv_ref.borrow_mut() {
            *tbc = true;
        }
    }
    // 更新 tbc_list 指向最新的 TBC upvalue
    state.tbc_list = Some(uv_idx);
    Ok(Some(uv_idx))
}

/// 获取指定栈位置对应的局部变量名 — 对应 C 的 luaG_findlocal + luaG_getlocalname
/// `reg` 是绝对栈位置 (对应 C 的 StkId level)，需要转换为相对于函数的局部变量编号
fn get_var_name_at(state: &LuaState, reg: usize) -> Option<String> {
    if state.base == 0 || state.base > state.top {
        return None;
    }
    if let TValue::LClosure(closure) = &state.stack[state.base - 1] {
        let proto = &closure.proto;
        let pc = state.pc;
        // C: idx = level - ci->func.p; Rust: func 在 state.base - 1
        // 所以 local_number = reg - (state.base - 1) = reg - state.base + 1
        let local_number = reg.wrapping_sub(state.base - 1);
        if local_number == 0 {
            return None;
        }
        let mut n = local_number as i32;
        for loc_var in &proto.loc_vars {
            if (loc_var.start_pc as usize) <= pc && pc < (loc_var.end_pc as usize) {
                n -= 1;
                if n == 0 {
                    if let Some(ref name) = loc_var.varname {
                        return Some(lua_string_as_str(name).to_string());
                    }
                    return None;
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LuaState;

    fn make_vm_state() -> LuaState<'static> {
        LuaState::default()
    }

    #[test]
    fn test_new_proto_creates_empty_proto() {
        let p = new_proto();
        assert_eq!(p.num_params, 0);
        assert_eq!(p.flag, 0);
        assert_eq!(p.max_stack_size, 0);
        assert!(p.code.is_empty());
        assert!(p.constants.is_empty());
        assert!(p.protos.is_empty());
        assert!(p.upvalues.is_empty());
        assert!(p.line_info.is_empty());
        assert!(p.abs_line_info.is_empty());
        assert!(p.loc_vars.is_empty());
        assert_eq!(p.line_defined, 0);
        assert_eq!(p.last_line_defined, 0);
    }

    #[test]
    fn test_find_upval_finds_existing_open_upval() {
        let mut state = make_vm_state();
        state.stack = vec![TValue::Integer(1), TValue::Integer(2), TValue::Integer(3)];
        let uv = find_upval(&mut state, 1);
        assert_eq!(uv, 0);
        let found = find_upval(&mut state, 1);
        assert_eq!(found, 0);
    }

    #[test]
    fn test_find_upval_creates_new_upval_if_not_found() {
        let mut state = make_vm_state();
        state.stack = vec![TValue::Integer(1), TValue::Integer(2)];
        let uv = find_upval(&mut state, 0);
        assert_eq!(uv, 0);
        let uv2 = find_upval(&mut state, 1);
        assert_eq!(uv2, 1);
    }

    #[test]
    fn test_close_upval_closes_open_upval() {
        let mut state = make_vm_state();
        state.stack = vec![TValue::Integer(42)];
        let uv = find_upval(&mut state, 0);
        let uv_ref = state.open_upvals[uv].clone().expect("slot holds upvalue");
        assert!(uv_ref.borrow().is_open());
        close_upval(&mut state, uv);
        // 关闭后的值写入共享 cell，闭包侧可见
        match &*uv_ref.borrow() {
            UpVal::Closed { value } => assert_eq!(*value, TValue::Integer(42)),
            _ => panic!("expected Closed"),
        }
        // arena 不再强引用已关闭的 upvalue：槽位被回收
        assert!(state.open_upvals[uv].is_none());
        assert_eq!(state.free_upvals, vec![uv]);
    }

    /// 回归：反复创建并关闭 upvalue 不得使 arena 无界增长
    /// （此前 closed upvalue 永久驻留 `open_upvals`，其内存与 Closed 值从不释放）
    #[test]
    fn test_closed_upvals_do_not_grow_arena() {
        let mut state = make_vm_state();
        state.stack = vec![TValue::Integer(0)];
        for round in 0..100 {
            state.stack[0] = TValue::Integer(round);
            let uv = find_upval(&mut state, 0);
            let _ = close(&mut state, 0, 0, 0);
            assert!(
                state.open_upvals.len() <= 1,
                "arena grew to {} slots at round {}",
                state.open_upvals.len(),
                round
            );
            assert!(state.open_upvals[uv].is_none());
        }
    }

    #[test]
    fn test_unlink_upval_removes_from_list() {
        let mut state = make_vm_state();
        state.stack = vec![TValue::Integer(1), TValue::Integer(2), TValue::Integer(3)];
        let _uv0 = find_upval(&mut state, 0);
        let uv1 = find_upval(&mut state, 1);
        let uv2 = find_upval(&mut state, 2);
        assert_eq!(state.open_upval, Some(uv2));
        close_upval(&mut state, uv2);
        assert_eq!(state.open_upval, Some(uv1));
    }

    #[test]
    fn test_close_closes_all_upvals_down_to_level() {
        let mut state = make_vm_state();
        state.stack = vec![
            TValue::Integer(10),
            TValue::Integer(20),
            TValue::Integer(30),
        ];
        let _uv0 = find_upval(&mut state, 0);
        let _uv1 = find_upval(&mut state, 1);
        let _uv2 = find_upval(&mut state, 2);
        let _ = close(&mut state, 1, 0, 0);
        assert_eq!(state.open_upval, Some(0));
    }

    #[test]
    fn test_new_tbc_upval_creates_tbc_entry() {
        let alloc = crate::mem::TestAlloc::new();
        let mut state = make_vm_state();
        // 创建带 __close 元方法的 Table
        let close_key = state.intern_str("__close");
        let mt = Table::new_in(alloc.slot());
        mt.set(close_key, TValue::Integer(0));
        let obj = Table::new_in(alloc.slot());
        obj.set_metatable(Some(mt));
        state.stack = vec![TValue::Table(obj)];
        let uv = new_tbc_upval(&mut state, 0).expect("closable value should succeed");
        assert!(uv.is_some());
        assert_eq!(state.tbc_list, uv);
    }
    #[test]
    fn test_new_tbc_upval_rejects_non_closable() {
        let mut state = make_vm_state();
        state.stack = vec![TValue::Integer(100)];
        // Integer 没有 __close 元方法，应返回 Err
        assert!(new_tbc_upval(&mut state, 0).is_err());
    }

    #[test]
    fn test_new_tbc_upval_skips_false() {
        let mut state = make_vm_state();
        state.stack = vec![TValue::Boolean(false)];
        // false/nil 不需要关闭，应返回 Ok(None)
        let uv = new_tbc_upval(&mut state, 0).expect("false should succeed");
        assert!(uv.is_none());
    }
}
