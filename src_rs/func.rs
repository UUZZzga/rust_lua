use crate::objects::*;
use crate::state::LuaState;

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
    state.closure_upvals.push(UpVal::Closed {
        value: Box::new(TValue::Nil(NilKind::Strict)),
    });
    idx
}

pub fn new_l_closure(state: &mut LuaState, nupvals: usize) -> usize {
    let idx = state.closure_upvals.len();
    for _ in 0..nupvals {
        state.closure_upvals.push(UpVal::Closed {
            value: Box::new(TValue::Nil(NilKind::Strict)),
        });
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
        let uv = &state.closure_upvals[uv_idx];
        let uv_level = match uv {
            UpVal::Open { stack_index, .. } => *stack_index,
            UpVal::Closed { .. } => {
                current = match uv {
                    UpVal::Open { next, .. } => *next,
                    _ => None,
                };
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
        current = match uv {
            UpVal::Open { next, .. } => *next,
            _ => None,
        };
    }
    new_upval(state, level, prev)
}

fn new_upval(state: &mut LuaState, level: usize, prev: Option<usize>) -> usize {
    let uv_idx = state.closure_upvals.len();
    let mut next: Option<usize> = None;
    match prev {
        Some(p_idx) => {
            if let UpVal::Open { next: p_next, .. } = &state.closure_upvals[p_idx] {
                next = *p_next;
            }
            if let UpVal::Open { ref mut next, .. } = state.closure_upvals[p_idx] {
                *next = Some(uv_idx);
            }
        }
        None => {
            next = state.open_upval;
            state.open_upval = Some(uv_idx);
        }
    }
    if let Some(n_idx) = next {
        if let UpVal::Open { ref mut previous, .. } = state.closure_upvals[n_idx] {
            *previous = Some(uv_idx);
        }
    }
    state.closure_upvals.push(UpVal::Open {
        stack_index: level,
        next,
        previous: prev,
    });
    uv_idx
}

pub fn close_upval(state: &mut LuaState, uv_idx: usize) {
    state.gc.cond_gc();
    let val = match &state.closure_upvals[uv_idx] {
        UpVal::Open { stack_index, .. } => {
            state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
        }
        UpVal::Closed { value } => (**value).clone(),
    };
    // GC barrier: when upvalue is closed, mark the value
    if let Some(gc_id) = crate::vm::gc_id_of_tvalue(&val) {
        state.gc.mark_object(gc_id);
    }
    unlink_upval(state, uv_idx);
    state.closure_upvals[uv_idx] = UpVal::Closed {
        value: Box::new(val),
    };
}

fn unlink_upval(state: &mut LuaState, uv_idx: usize) {
    let (prev, nxt) = match &state.closure_upvals[uv_idx] {
        UpVal::Open {
            previous,
            next,
            ..
        } => (*previous, *next),
        _ => return,
    };
    match prev {
        Some(p_idx) => {
            if let UpVal::Open { ref mut next, .. } = state.closure_upvals[p_idx] {
                *next = nxt;
            }
        }
        None => {
            state.open_upval = nxt;
        }
    }
    if let Some(n_idx) = nxt {
        if let UpVal::Open { ref mut previous, .. } = state.closure_upvals[n_idx] {
            *previous = prev;
        }
    }
}

pub fn close(state: &mut LuaState, level: usize, _status: i32, _nresults: i32) {
    let mut current = state.open_upval;
    while let Some(uv_idx) = current {
        let should_close = match &state.closure_upvals[uv_idx] {
            UpVal::Open { stack_index, .. } => *stack_index >= level,
            UpVal::Closed { .. } => false,
        };
        current = match &state.closure_upvals[uv_idx] {
            UpVal::Open { next, .. } => *next,
            _ => None,
        };
        if should_close {
            close_upval(state, uv_idx);
        }
        if state.open_upval.is_none() {
            break;
        }
    }
    state.twups_linked = false;
}

pub fn new_tbc_upval(state: &mut LuaState, level: usize) -> Option<usize> {
    let uv_idx = state.closure_upvals.len();
    state.closure_upvals.push(UpVal::Open {
        stack_index: level,
        next: None,
        previous: None,
    });
    if let Some(head) = state.tbc_list {
        if let UpVal::Open { ref mut next, .. } = state.closure_upvals[head] {
            *next = Some(uv_idx);
        }
        if let UpVal::Open { ref mut previous, .. } = state.closure_upvals[uv_idx] {
            *previous = Some(head);
        }
    }
    state.tbc_list = Some(uv_idx);
    Some(uv_idx)
}

pub fn pop_tbc_list(state: &mut LuaState, level: usize) {
    let head = match state.tbc_list {
        Some(h) => h,
        None => return,
    };
    if let UpVal::Open { stack_index, .. } = &state.closure_upvals[head] {
        if *stack_index < level {
            return;
        }
    }
    let new_head = match &state.closure_upvals[head] {
        UpVal::Open { previous, .. } => *previous,
        _ => None,
    };
    if let Some(h) = state.tbc_list {
        if let UpVal::Open { ref mut next, .. } = state.closure_upvals[h] {
            *next = None;
        }
    }
    state.tbc_list = new_head;
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
        assert!(state.closure_upvals[uv].is_open());
        close_upval(&mut state, uv);
        match &state.closure_upvals[uv] {
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
        state.stack = vec![TValue::Integer(100)];
        let uv = new_tbc_upval(&mut state, 0);
        assert!(uv.is_some());
        assert_eq!(state.tbc_list, uv);
    }

    #[test]
    fn test_pop_tbc_list_removes_entry() {
        let mut state = make_vm_state();
        state.stack = vec![TValue::Integer(100)];
        let _uv = new_tbc_upval(&mut state, 0);
        pop_tbc_list(&mut state, 0);
        assert_eq!(state.tbc_list, None);
    }
}