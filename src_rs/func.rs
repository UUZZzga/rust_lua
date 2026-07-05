use crate::objects::*;
use crate::state::LuaState;
use crate::execute::VmError;
use std::cell::RefCell;
use std::rc::Rc;

pub fn new_proto() -> Proto {
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
        constants: Vec::new(),
        code: Vec::new(),
        protos: Vec::new(),
        upvalues: Vec::new(),
        line_info: Vec::new(),
        abs_line_info: Vec::new(),
        loc_vars: Vec::new(),
        source: None,
    }
}

pub fn proto_size(p: &Proto) -> usize {
    let mut size = std::mem::size_of::<Proto>();
    size += p.code.len() * std::mem::size_of::<Instruction>();
    for c in &p.constants {
        size += tvalue_size(c);
    }
    for sub in &p.protos {
        size += proto_size(sub);
    }
    size += p.upvalues.len() * std::mem::size_of::<UpvalDesc>();
    size += p.line_info.len() * std::mem::size_of::<i8>();
    size += p.abs_line_info.len() * std::mem::size_of::<AbsLineInfo>();
    size += p.loc_vars.len() * std::mem::size_of::<LocVar>();
    size
}

fn tvalue_size(v: &TValue) -> usize {
    match v {
        TValue::Str(_s) => std::mem::size_of_val(v),
        _ => std::mem::size_of_val(v),
    }
}

pub fn new_c_closure(state: &mut LuaState, _nupvals: usize) -> usize {
    let idx = state.closure_upvals.len();
    state.closure_upvals.push(Rc::new(RefCell::new(UpVal::Closed {
        value: Box::new(TValue::Nil(NilKind::Strict)),
    })));
    idx
}

pub fn new_l_closure(state: &mut LuaState, nupvals: usize) -> usize {
    let idx = state.closure_upvals.len();
    for _ in 0..nupvals {
        state.closure_upvals.push(Rc::new(RefCell::new(UpVal::Closed {
            value: Box::new(TValue::Nil(NilKind::Strict)),
        })));
    }
    idx
}

pub fn init_upvals(
    _state: &mut LuaState,
    _closure_start: usize,
    _proto: &Proto,
) {
}

pub fn find_upval(state: &mut LuaState, level: usize) -> usize {
    if !state.is_in_twups {
        state.twups_linked = true;
    }
    let mut prev: Option<usize> = None;
    let mut current = state.open_upval;
    while let Some(uv_idx) = current {
        let uv_level = {
            let uv_ref = state.closure_upvals[uv_idx].borrow();
            match &*uv_ref {
                UpVal::Open { stack_index, .. } => Some(*stack_index),
                UpVal::Closed { .. } => None,
            }
        };
        if uv_level.is_none() {
            // Closed upvalue: skip (shouldn't be in open list, but be safe)
            current = {
                let uv_ref = state.closure_upvals[uv_idx].borrow();
                match &*uv_ref {
                    UpVal::Open { next, .. } => *next,
                    _ => None,
                }
            };
            continue;
        }
        let uv_level = uv_level.unwrap();
        if uv_level < level {
            break;
        }
        if uv_level == level {
            return uv_idx;
        }
        prev = Some(uv_idx);
        current = {
            let uv_ref = state.closure_upvals[uv_idx].borrow();
            match &*uv_ref {
                UpVal::Open { next, .. } => *next,
                _ => None,
            }
        };
    }
    new_upval(state, level, prev)
}

fn new_upval(state: &mut LuaState, level: usize, prev: Option<usize>) -> usize {
    let uv_idx = state.closure_upvals.len();
    let mut next: Option<usize> = None;
    match prev {
        Some(p_idx) => {
            {
                let p_ref = state.closure_upvals[p_idx].borrow();
                if let UpVal::Open { next: p_next, .. } = &*p_ref {
                    next = *p_next;
                }
            }
            {
                let mut p_ref = state.closure_upvals[p_idx].borrow_mut();
                if let UpVal::Open { ref mut next, .. } = &mut *p_ref {
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
        let mut n_ref = state.closure_upvals[n_idx].borrow_mut();
        if let UpVal::Open { ref mut previous, .. } = &mut *n_ref {
            *previous = Some(uv_idx);
        }
    }
    state.closure_upvals.push(Rc::new(RefCell::new(UpVal::Open {
        stack_index: level,
        next,
        previous: prev,
        tbc: false,
    })));
    uv_idx
}

pub fn close_upval(state: &mut LuaState, uv_idx: usize) {
    state.gc.cond_gc();
    let val = {
        let uv_ref = state.closure_upvals[uv_idx].borrow();
        match &*uv_ref {
            UpVal::Open { stack_index, .. } => {
                state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
            }
            UpVal::Closed { value } => (**value).clone(),
        }
    };
    // GC barrier: when upvalue is closed, mark the value
    if let Some(gc_id) = crate::vm::gc_id_of_tvalue(&val) {
        state.gc.mark_object(gc_id);
    }
    unlink_upval(state, uv_idx);
    *state.closure_upvals[uv_idx].borrow_mut() = UpVal::Closed {
        value: Box::new(val),
    };
}

fn unlink_upval(state: &mut LuaState, uv_idx: usize) {
    let (prev, nxt) = {
        let uv_ref = state.closure_upvals[uv_idx].borrow();
        match &*uv_ref {
            UpVal::Open { previous, next, .. } => (*previous, *next),
            _ => return,
        }
    };
    match prev {
        Some(p_idx) => {
            let mut p_ref = state.closure_upvals[p_idx].borrow_mut();
            if let UpVal::Open { ref mut next, .. } = &mut *p_ref {
                *next = nxt;
            }
        }
        None => {
            state.open_upval = nxt;
        }
    }
    if let Some(n_idx) = nxt {
        let mut n_ref = state.closure_upvals[n_idx].borrow_mut();
        if let UpVal::Open { ref mut previous, .. } = &mut *n_ref {
            *previous = prev;
        }
    }
}

pub fn close(state: &mut LuaState, level: usize, status: i32, yy: i32) -> Result<(), VmError> {
    // force_noyield_close: coroutine.close() 关闭自身时设置（对应 C Lua 的 lua_closethread(co, L)
    // 中 co == L 场景）。C Lua 会立即调用 luaF_close(L, L->stack, LUA_OK, 1) 关闭所有 TBC 变量，
    // 并通过 luaD_throwbaselevel 抛到 base level。我们的实现未完整支持此语义，改为设置标志，
    // 让 OP_RETURN 的 func::close 使用不可 yield 模式 (yy=0)，使 __close 中的 yield 失败
    // （对应 C Lua 中 nny > 0 时 yield 报错的场景）。
    let yy = if state.force_noyield_close { 0 } else { yy };

    // 收集所有 should_close 的 upvalue（按 open_upval 链表顺序，stack_index 降序）
    // open_upval 链表是按 stack_index 降序，对应 Lua 5.5 的关闭顺序
    let mut to_close: Vec<usize> = Vec::new();
    let mut current = state.open_upval;
    while let Some(uv_idx) = current {
        if uv_idx >= state.closure_upvals.len() {
            break;
        }
        let (should_close, next, stack_idx) = {
            let uv_ref = state.closure_upvals[uv_idx].borrow();
            match &*uv_ref {
                UpVal::Open { stack_index, next, .. } => (*stack_index >= level, *next, *stack_index),
                UpVal::Closed { .. } => (false, None, 0),
            }
        };
        if should_close {
            to_close.push(uv_idx);
        }
        current = next;
    }

    // 没有需要关闭的 upvalue: 直接返回，不修改错误状态
    // (避免 status!=0 但无 TBC 变量时用 Nil 覆盖原有错误)
    if to_close.is_empty() {
        state.twups_linked = false;
        return Ok(());
    }

    // 对每个 should_close 的 upvalue，按顺序处理
    // 对 TBC upvalue，先调用 __close metamethod，再 close_upval
    // 错误传播: __close 出错时，错误值传递给下一个 __close 的 err 参数
    let mut current_status = status;
    let mut current_err: TValue = if status != 0 {
        state.last_error_value.clone().unwrap_or(TValue::Nil(NilKind::Strict))
    } else {
        TValue::Nil(NilKind::Strict)
    };
    let mut has_error = status != 0;

    for uv_idx in to_close {
        let is_tbc = {
            let uv_ref = state.closure_upvals[uv_idx].borrow();
            matches!(&*uv_ref, UpVal::Open { tbc: true, .. })
        };
        let (stack_idx, tbc_flag) = {
            let uv_ref = state.closure_upvals[uv_idx].borrow();
            if let UpVal::Open { stack_index, tbc, .. } = &*uv_ref {
                (*stack_index, *tbc)
            } else {
                (0, false)
            }
        };
        if is_tbc {
            // TBC upvalue: 读取栈上的值（在 close_upval 之前，因为 close_upval 会改为 Closed）
            let val = {
                let uv_ref = state.closure_upvals[uv_idx].borrow();
                if let UpVal::Open { stack_index, .. } = &*uv_ref {
                    state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
                } else {
                    TValue::Nil(NilKind::Strict)
                }
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
                            VmError::RuntimeError(s) => TValue::Str(state.intern_str(&s)),
                            other => TValue::Str(state.intern_str(&format!("{}", other))),
                        };
                        has_error = true;
                        current_status = 1;  // 错误状态
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
            TValue::Str(s) => s.as_str().to_string(),
            _ => format!("{}", current_err),
        };
        state.last_error_msg = msg;
    }
    state.twups_linked = false;
    if has_error {
        // __close 出错: 返回错误以中断调用者的执行（对应 C 的 luaD_throw）
        // state.last_error_value 已包含最终错误值，调用者可通过它获取原始 TValue
        // 字符串错误用 RuntimeError，非字符串错误用 RuntimeErrorValue 保留原始 TValue
        Err(match &current_err {
            TValue::Str(s) => VmError::RuntimeError(s.as_str().to_string()),
            _ => VmError::RuntimeErrorValue(current_err.clone()),
        })
    } else {
        Ok(())
    }
}

pub fn new_tbc_upval(state: &mut LuaState, level: usize) -> Result<Option<usize>, VmError> {
    // 对应 C 的 luaF_newtbcupval: 检查 __close 元方法，复用或创建 open upvalue，然后标记 tbc
    let val = state.stack.get(level).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
    // C 的 luaF_newtbcupval: l_isfalse 检查，跳过 nil/false
    if val.is_false() {
        return Ok(None);  // false/nil 不需要关闭
    }
    // 对应 C 的 checkclosemth: 检查 __close 元方法是否存在
    let has_close = crate::tm::get_tm_by_obj(&val, crate::tm::TagMethod::Close, &state.dmt).is_some();
    if !has_close {
        // 获取变量名 — 对应 C 的 luaG_findlocal(L, L->ci, idx, NULL)
        let varname = get_var_name_at(state, level).unwrap_or_else(|| "?".to_string());
        return Err(VmError::RuntimeError(format!(
            "variable '{}' got a non-closable value", varname
        )));
    }
    // TBC upvalue 复用 open_upval 链表（通过 find_upval 加入），用 tbc 字段标记
    let uv_idx = find_upval(state, level);
    {
        let mut uv_ref = state.closure_upvals[uv_idx].borrow_mut();
        if let UpVal::Open { ref mut tbc, .. } = &mut *uv_ref {
            *tbc = true;
        }
    }
    // 更新 tbc_list 指向最新的 TBC upvalue（用于 pop_tbc_list 等检查）
    state.tbc_list = Some(uv_idx);
    Ok(Some(uv_idx))
}

/// 获取指定栈位置对应的局部变量名 — 对应 C 的 luaG_findlocal + luaG_getlocalname
/// `reg` 是绝对栈位置 (对应 C 的 StkId level)，需要转换为相对于函数的局部变量编号
fn get_var_name_at(state: &LuaState, reg: usize) -> Option<String> {
    if state.base == 0 || state.base > state.stack.len() {
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
                        return Some(name.as_str().to_string());
                    }
                    return None;
                }
            }
        }
    }
    None
}

pub fn pop_tbc_list(state: &mut LuaState, level: usize) {
    // 简化: tbc_list 不再是链表，只清除 head 的 tbc 标志（如果 stack_index >= level）
    let head = match state.tbc_list {
        Some(h) => h,
        None => return,
    };
    let should_pop = {
        let head_ref = state.closure_upvals[head].borrow();
        if let UpVal::Open { stack_index, .. } = &*head_ref {
            *stack_index >= level
        } else {
            false
        }
    };
    if !should_pop {
        return;
    }
    // 清除 tbc 标志
    {
        let mut head_ref = state.closure_upvals[head].borrow_mut();
        if let UpVal::Open { ref mut tbc, .. } = &mut *head_ref {
            *tbc = false;
        }
    }
    state.tbc_list = None;
}

pub fn get_local_name(_proto: &Proto, _local_number: usize, _pc: usize) -> Option<&str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LuaState;

    fn make_vm_state() -> LuaState {
        LuaState {
            pc: 0,
            stack: Vec::new(),
            top: 0,
            base: 0,
            closure_upvals: Vec::new(),
            open_upval: None,
            tbc_list: None,
            twups_linked: false,
            is_in_twups: false,
            constants: Vec::new(),
            code: Vec::new(),
            upval_descs: Vec::new(),
            protos: Vec::new(),
            trap: false,
            num_params: 0,
            is_vararg: false,
            proto_flag: 0,
            nextraargs: 0,
            gc: std::rc::Rc::new(crate::gc::GCState::default_incremental()),
            globals: crate::table::Table::new(),
            registry: crate::table::Table::new(),
            string_table: crate::strings::StringTable::new(),
            api_func_base: 0,
            n_ccalls: 0,
            dmt: crate::tm::DefaultMetatables::new(),
            stdout: Box::new(std::io::stdout()),
            io_output: None,
            file_handles: std::collections::HashMap::new(),
            popen_handles: std::collections::HashSet::new(),
            io_input_handle: None,
            io_output_handle: None,
            global_state: std::rc::Rc::new(crate::state::GlobalState { gcstopem: false }),
            ci: None,
            call_info: Vec::new(),
            last_traceback: String::new(),
            last_error_msg: String::new(),
            last_c_function: None,
            math_random_state: None,
            hook_func: None,
            hook_mask: 0,
            hook_count: 0,
            hook_old_pc: 0,
            current_hook_count: 0,
            allowhook: true,
            n_ny_calls: 0,
            last_error_value: None,
            pending_yield: None,
            main_thread: LuaThread {
                stack: Vec::new(),
                status: ThreadStatus::OK,
                function: None,
                is_main: true,
                context: Rc::new(RefCell::new(ThreadContext::default())),
            },
            call_stack: Vec::new(),
            current_thread: None,
            wrap_coros: Vec::new(),
            caller_gc_stacks: Vec::new(),
            pcall_protection_stack: Vec::new(),
            weak_tables: Vec::new(),
            concat_gc_counter: std::cell::Cell::new(0),
            concat_gc_interval: std::cell::Cell::new(4096),
            finobj_list: Vec::new(),
            ud_finobj_list: Vec::new(),
            transferinfo_ftransfer: 0,
            transferinfo_ntransfer: 0,
            pending_return_adjust: None,
            last_error_call_info: None,
            last_close_frame: None,
            close_error_status: None,
            force_noyield_close: false,
            error_no_prefix: false,
        }
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
    fn test_proto_size_of_empty_proto() {
        let p = new_proto();
        let size = proto_size(&p);
        let base = std::mem::size_of::<Proto>();
        assert!(size >= base);
    }

    #[test]
    fn test_proto_size_includes_code_and_constants() {
        let mut p = new_proto();
        p.code = vec![1, 2, 3];
        p.constants = vec![TValue::Integer(42)];
        let empty_size = proto_size(&new_proto());
        let filled_size = proto_size(&p);
        assert!(filled_size > empty_size);
    }

    #[test]
    fn test_new_c_closure_creates_closure() {
        let mut state = make_vm_state();
        let _idx = new_c_closure(&mut state, 2);
        assert!(state.closure_upvals.len() > 0);
    }

    #[test]
    fn test_new_l_closure_creates_closure_with_upvals() {
        let mut state = make_vm_state();
        let idx = new_l_closure(&mut state, 3);
        let end = state.closure_upvals.len();
        assert!(idx < end);
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
        assert!(state.closure_upvals[uv].borrow().is_open());
        close_upval(&mut state, uv);
        let uv_ref = state.closure_upvals[uv].borrow();
        match &*uv_ref {
            UpVal::Closed { value } => assert_eq!(**value, TValue::Integer(42)),
            _ => panic!("expected Closed"),
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
        state.stack = vec![TValue::Integer(10), TValue::Integer(20), TValue::Integer(30)];
        let _uv0 = find_upval(&mut state, 0);
        let _uv1 = find_upval(&mut state, 1);
        let _uv2 = find_upval(&mut state, 2);
        close(&mut state, 1, 0, 0);
        assert_eq!(state.open_upval, Some(0));
    }

    #[test]
    fn test_new_tbc_upval_creates_tbc_entry() {
        let mut state = make_vm_state();
        // 创建带 __close 元方法的 Table
        let close_key = TValue::Str(state.intern_str("__close"));
        let mt = crate::table::Table::new();
        mt.set(close_key, TValue::Integer(0));
        let obj = crate::table::Table::new();
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

    #[test]
    fn test_pop_tbc_list_removes_entry() {
        let mut state = make_vm_state();
        // 创建带 __close 元方法的 Table
        let close_key = TValue::Str(state.intern_str("__close"));
        let mt = crate::table::Table::new();
        mt.set(close_key, TValue::Integer(0));
        let obj = crate::table::Table::new();
        obj.set_metatable(Some(mt));
        state.stack = vec![TValue::Table(obj)];
        let _uv = new_tbc_upval(&mut state, 0).expect("closable value should succeed");
        pop_tbc_list(&mut state, 0);
        assert_eq!(state.tbc_list, None);
    }
}