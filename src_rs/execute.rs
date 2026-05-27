//! Lua 虚拟机主解释器循环 (纯 Rust 重写)
//!
//! 对应 C 源码: lvm.cpp 中的 luaV_execute 函数
//!
//! ## 设计原则
//! - 使用 Rust `match` 替代 C 的 `switch` + `goto`
//! - `LuaState` 结构体封装所有解释器状态，替代 C 的局部变量 + 宏
//! - 使用 `Result` 传播错误，替代 C 的 longjmp 错误处理
//! - 操作码处理用独立方法，提高可读性和可测试性
//! - 使用 Rust 的 trait 和方法传递代替 C 宏
//!
//! ## 规约驱动开发 (spec-driven-tdd)
//! 每个公开函数都包含规约注释。

use crate::objects::{Instruction, LClosure, NilKind, Proto, TValue, UpVal};
use crate::opcodes::{self, OpCode};
use crate::table::Table;
use crate::tm::{
    TagMethod, TagMethodError,
    try_bin_tm, try_bin_assoc_tm, DefaultMetatables,
};
use crate::vm::{to_number_ns, to_integer_ns, F2IMode, shiftl, is_false, objlen,
    concat_stack, equal, less_than, less_equal, raw_equal, float_to_integer,
    modulus, modulus_float, idiv};
use crate::state::LuaState;
use crate::gc::GCState;
use std::rc::Rc;

// ============================================================================
// VmResult / VmError
// ============================================================================

#[derive(Debug)]
pub enum VmResult {
    Return(usize),
    TailCall { proto: Proto, base: usize },
    Call { proto: Proto, base: usize, num_results: i32 },
    Done,
}

#[derive(Debug, PartialEq, Eq)]
pub enum VmError {
    DivisionByZero,
    ModuloByZero,
    TypeError(String),
    StackOverflow,
    IllegalOpcode(u8),
    RuntimeError(String),
    MetaMethodNotImplemented(String),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::DivisionByZero => write!(f, "attempt to divide by zero"),
            VmError::ModuloByZero => write!(f, "attempt to perform 'n%0'"),
            VmError::TypeError(msg) => write!(f, "type error: {}", msg),
            VmError::StackOverflow => write!(f, "stack overflow"),
            VmError::IllegalOpcode(op) => write!(f, "illegal opcode: {}", op),
            VmError::RuntimeError(msg) => write!(f, "runtime error: {}", msg),
            VmError::MetaMethodNotImplemented(name) => write!(f, "metamethod '{}' not implemented", name),
        }
    }
}

impl std::error::Error for VmError {}

#[derive(Clone)]
struct CallFrame {
    code: Vec<Instruction>,
    constants: Vec<TValue>,
    upval_descs: Vec<crate::objects::UpvalDesc>,
    protos: Vec<Proto>,
    base: usize,
    return_pc: usize,
    return_base: usize,
    num_results: usize,
    num_params: u8,
    is_vararg: bool,
    closure_upvals: Vec<UpVal>,
    tbc_list: Option<usize>,
    open_upval: Option<usize>,
}

// ============================================================================
// VmExecutor
// ============================================================================

pub struct VmExecutor;

impl VmExecutor {
    pub fn execute(proto: &Proto, base: usize, stack: Vec<TValue>, gc: Rc<GCState>) -> Result<VmResult, VmError> {
        let mut state = LuaState::from_proto(proto, base, stack, gc);
        Self::execute_loop(&mut state)
    }

    pub fn execute_with_state(state: &mut LuaState) -> Result<VmResult, VmError> {
        Self::execute_loop(state)
    }

    pub fn execute_loop(state: &mut LuaState) -> Result<VmResult, VmError> {
        let mut call_stack: Vec<CallFrame> = Vec::new();

        loop {
            if state.pc >= state.code.len() {
                if let Some(frame) = call_stack.pop() {
                    state.code = frame.code;
                    state.constants = frame.constants;
                    state.upval_descs = frame.upval_descs;
                    state.protos = frame.protos;
                    state.base = frame.base;
                    state.pc = frame.return_pc;
                    state.num_params = frame.num_params;
                    state.is_vararg = frame.is_vararg;
                    state.closure_upvals = frame.closure_upvals;
                    state.tbc_list = frame.tbc_list;
                    state.open_upval = frame.open_upval;
                    continue;
                }
                return Ok(VmResult::Return(0));
            }

            let inst = state.code[state.pc];
            let op = opcodes::get_opcode(inst);
            // eprintln!("DEBUG EXEC: pc={}, op={:?}({}), A={}, B={}, C={}, stack.len={}, base={}", 
            //     state.pc, op, inst, opcodes::getarg_a(inst), opcodes::getarg_b(inst), 
            //     opcodes::getarg_c(inst), state.stack.len(), state.base);

            match op {
                OpCode::MOVE => Self::op_move(state, inst),
                OpCode::LOADI => Self::op_loadi(state, inst),
                OpCode::LOADF => Self::op_loadf(state, inst),
                OpCode::LOADK => Self::op_loadk(state, inst),
                OpCode::LOADKX => Self::op_loadkx(state, inst),
                OpCode::LOADFALSE => Self::op_loadfalse(state, inst),
                OpCode::LFALSESKIP => Self::op_lfalseskip(state, inst),
                OpCode::LOADTRUE => Self::op_loadtrue(state, inst),
                OpCode::LOADNIL => Self::op_loadnil(state, inst),
                OpCode::GETUPVAL => Self::op_getupval(state, inst),
                OpCode::SETUPVAL => Self::op_setupval(state, inst),
                OpCode::GETTABUP => Self::op_gettabup(state, inst),
                OpCode::GETTABLE => Self::op_gettable(state, inst),
                OpCode::GETI => Self::op_geti(state, inst),
                OpCode::GETFIELD => Self::op_getfield(state, inst),
                OpCode::SETTABUP => Self::op_settabup(state, inst),
                OpCode::SETTABLE => Self::op_settable(state, inst),
                OpCode::SETI => Self::op_seti(state, inst),
                OpCode::SETFIELD => Self::op_setfield(state, inst),
                OpCode::NEWTABLE => Self::op_newtable(state, inst),
                OpCode::SELF => Self::op_self(state, inst),
                OpCode::ADDI => Self::op_addi(state, inst),
                OpCode::ADDK => Self::op_addk(state, inst),
                OpCode::SUBK => Self::op_subk(state, inst),
                OpCode::MULK => Self::op_mulk(state, inst),
                OpCode::MODK => Self::op_modk(state, inst),
                OpCode::POWK => Self::op_powk(state, inst),
                OpCode::DIVK => Self::op_divk(state, inst),
                OpCode::IDIVK => Self::op_idivk(state, inst),
                OpCode::BANDK => Self::op_bandk(state, inst),
                OpCode::BORK => Self::op_bork(state, inst),
                OpCode::BXORK => Self::op_bxork(state, inst),
                OpCode::SHLI => Self::op_shli(state, inst),
                OpCode::SHRI => Self::op_shri(state, inst),
                OpCode::ADD => Self::op_add(state, inst),
                OpCode::SUB => Self::op_sub(state, inst),
                OpCode::MUL => Self::op_mul(state, inst),
                OpCode::MOD => Self::op_mod(state, inst),
                OpCode::POW => Self::op_pow(state, inst),
                OpCode::DIV => Self::op_div(state, inst),
                OpCode::IDIV => Self::op_idiv(state, inst),
                OpCode::BAND => Self::op_band(state, inst),
                OpCode::BOR => Self::op_bor(state, inst),
                OpCode::BXOR => Self::op_bxor(state, inst),
                OpCode::SHL => Self::op_shl(state, inst),
                OpCode::SHR => Self::op_shr(state, inst),
                OpCode::MMBIN => Self::op_mmbin(state, inst),
                OpCode::MMBINI => Self::op_mmbini(state, inst),
                OpCode::MMBINK => Self::op_mmbink(state, inst),
                OpCode::UNM => Self::op_unm(state, inst),
                OpCode::BNOT => Self::op_bnot(state, inst),
                OpCode::NOT => Self::op_not(state, inst),
                OpCode::LEN => Self::op_len(state, inst),
                OpCode::CONCAT => Self::op_concat(state, inst),
                OpCode::CLOSE => Self::op_close(state, inst),
                OpCode::TBC => Self::op_tbc(state, inst),
                OpCode::JMP => Self::op_jmp(state, inst),
                OpCode::EQ => Self::op_eq(state, inst),
                OpCode::LT => Self::op_lt(state, inst),
                OpCode::LE => Self::op_le(state, inst),
                OpCode::EQK => Self::op_eqk(state, inst),
                OpCode::EQI => Self::op_eqi(state, inst),
                OpCode::LTI => Self::op_lti(state, inst),
                OpCode::LEI => Self::op_lei(state, inst),
                OpCode::GTI => Self::op_gti(state, inst),
                OpCode::GEI => Self::op_gei(state, inst),
                OpCode::TEST => Self::op_test(state, inst),
                OpCode::TESTSET => Self::op_testset(state, inst),
                OpCode::CALL => {
                    let a = Self::ra(state, inst);
                    let b = opcodes::getarg_b(inst) as usize;
                    let c = opcodes::getarg_c(inst) as i32;
                    let func_val = Self::read_stack(state, a).clone();
                    match func_val {
                        TValue::LClosure(closure) => {
                            let nargs = if b == 0 { state.stack.len().saturating_sub(a + 1) } else { b.saturating_sub(1) };
                            let nresults = if c == 1 { 0 } else { (c - 1) as usize };
                            let fsize = closure.proto.max_stack_size as usize;
                            let nfixparams = closure.proto.num_params as usize;

                            call_stack.push(CallFrame {
                                code: std::mem::take(&mut state.code),
                                constants: std::mem::take(&mut state.constants),
                                upval_descs: std::mem::take(&mut state.upval_descs),
                                protos: std::mem::take(&mut state.protos),
                                base: state.base,
                                return_pc: state.pc + 1,
                                return_base: a,
                                num_results: nresults,
                                num_params: state.num_params,
                                is_vararg: state.is_vararg,
                                closure_upvals: std::mem::take(&mut state.closure_upvals),
                                tbc_list: state.tbc_list.take(),
                                open_upval: state.open_upval.take(),
                            });

                            state.code = closure.proto.code.clone();
                            state.constants = closure.proto.constants.clone();
                            state.upval_descs = closure.proto.upvalues.clone();
                            state.protos = closure.proto.protos.clone();
                            state.base = a + 1;
                            state.pc = 0;
                            state.num_params = closure.proto.num_params;
                            state.is_vararg = closure.proto.is_vararg();
                            state.closure_upvals = Vec::new();
                            state.tbc_list = None;
                            state.open_upval = None;

                            let frame_end = a + 1 + fsize;
                            while state.stack.len() < frame_end {
                                state.stack.push(TValue::Nil(NilKind::Strict));
                            }
                            for i in nargs..nfixparams {
                                state.stack[a + 1 + i] = TValue::Nil(NilKind::Strict);
                            }
                            Ok(())
                        }
                        TValue::LightUserData(tag) => {
                            let tag_val = tag as usize;
                            if tag_val == 1 {
                                let mut s = String::new();
                                let nargs = if b == 0 { state.stack.len().saturating_sub(a + 1) } else { b.saturating_sub(1) };
                                for i in 0..nargs {
                                    if i > 0 { s.push('\t'); }
                                    let val = Self::read_stack(state, a + 1 + i);
                                    match val {
                                        TValue::Nil(_) => s.push_str("nil"),
                                        TValue::Boolean(bv) => s.push_str(if *bv { "true" } else { "false" }),
                                        TValue::Integer(n) => s.push_str(&n.to_string()),
                                        TValue::Float(n) => {
                                            if n.is_nan() { s.push_str("nan"); }
                                            else if n.is_infinite() { s.push_str(if *n > 0.0 { "inf" } else { "-inf" }); }
                                            else { s.push_str(&n.to_string()); }
                                        }
                                        TValue::Str(lst) => s.push_str(&lst.as_str()),
                                        TValue::Table(_) => s.push_str("table: 0x0"),
                                        TValue::LClosure(_) | TValue::LCFn(_) | TValue::CClosure(_) => s.push_str("function: 0x0"),
                                        _ => s.push_str(&format!("{:?}", val)),
                                    }
                                }
                                println!("{}", s);
                            }
                            state.pc += 1;
                            Ok(())
                        }
                        _ => {
                            state.pc += 1;
                            Ok(())
                        }
                    }
                }
                OpCode::TAILCALL => {
                    let a = Self::ra(state, inst);
                    let func_val = Self::read_stack(state, a).clone();
                    match func_val {
                        TValue::LClosure(closure) => {
                            let nargs_total = state.stack.len().saturating_sub(a);
                            let fsize = closure.proto.max_stack_size as usize;
                            let nfixparams = closure.proto.num_params as usize;
                            let nargs = nargs_total.saturating_sub(1);
                            let func_slot = state.base.saturating_sub(1);

                            for i in 0..nargs_total {
                                let src = a + i;
                                let dst = func_slot + i;
                                if dst < state.stack.len() {
                                    state.stack[dst] = std::mem::take(&mut state.stack[src]);
                                }
                            }

                            state.code = closure.proto.code.clone();
                            state.constants = closure.proto.constants.clone();
                            state.upval_descs = closure.proto.upvalues.clone();
                            state.protos = closure.proto.protos.clone();
                            state.pc = 0;
                            state.num_params = closure.proto.num_params;
                            state.is_vararg = closure.proto.is_vararg();
                            state.closure_upvals = Vec::new();
                            state.tbc_list = None;
                            state.open_upval = None;

                            let frame_end = func_slot + 1 + fsize;
                            while state.stack.len() < frame_end {
                                state.stack.push(TValue::Nil(NilKind::Strict));
                            }
                            for i in nargs..nfixparams {
                                state.stack[func_slot + 1 + i] = TValue::Nil(NilKind::Strict);
                            }
                            Ok(())
                        }
                        _ => Ok(())
                    }
                }
                OpCode::RETURN => {
                    let a = Self::ra(state, inst);
                    let n = opcodes::getarg_b(inst) as i32 - 1;
                    let nresults = if n < 0 { state.stack.len().saturating_sub(a) } else { n as usize };

                    if let Some(frame) = call_stack.pop() {
                        let return_base = frame.return_base;
                        let num_results = frame.num_results;
                        let mut results = Vec::new();
                        for i in 0..nresults {
                            if a + i < state.stack.len() {
                                results.push(std::mem::take(&mut state.stack[a + i]));
                            } else {
                                results.push(TValue::Nil(NilKind::Strict));
                            }
                        }
                        state.code = frame.code;
                        state.constants = frame.constants;
                        state.upval_descs = frame.upval_descs;
                        state.protos = frame.protos;
                        state.base = frame.base;
                        state.pc = frame.return_pc;
                        state.num_params = frame.num_params;
                        state.is_vararg = frame.is_vararg;
                        state.closure_upvals = frame.closure_upvals;
                        state.tbc_list = frame.tbc_list;
                        state.open_upval = frame.open_upval;

                        while state.stack.len() <= return_base + num_results.saturating_sub(1) {
                            state.stack.push(TValue::Nil(NilKind::Strict));
                        }
                        let copy_count = results.len().min(num_results);
                        for i in 0..copy_count {
                            state.stack[return_base + i] = std::mem::take(&mut results[i]);
                        }
                        for i in copy_count..num_results {
                            state.stack[return_base + i] = TValue::Nil(NilKind::Strict);
                        }
                        state.stack.truncate(return_base + num_results);
                        Ok(())
                    } else {
                        return Ok(VmResult::Return(nresults));
                    }
                }
                OpCode::RETURN0 => {
                    if let Some(frame) = call_stack.pop() {
                        let return_base = frame.return_base;
                        let num_results = frame.num_results;
                        state.code = frame.code;
                        state.constants = frame.constants;
                        state.upval_descs = frame.upval_descs;
                        state.protos = frame.protos;
                        state.base = frame.base;
                        state.pc = frame.return_pc;
                        state.num_params = frame.num_params;
                        state.is_vararg = frame.is_vararg;
                        state.closure_upvals = frame.closure_upvals;
                        state.tbc_list = frame.tbc_list;
                        state.open_upval = frame.open_upval;
                        while state.stack.len() <= return_base + num_results.saturating_sub(1) {
                            state.stack.push(TValue::Nil(NilKind::Strict));
                        }
                        for i in 0..num_results {
                            state.stack[return_base + i] = TValue::Nil(NilKind::Strict);
                        }
                        state.stack.truncate(return_base + num_results);
                        Ok(())
                    } else {
                        return Ok(VmResult::Return(0));
                    }
                }
                OpCode::RETURN1 => {
                    let a = Self::ra(state, inst);
                    let val = if a < state.stack.len() {
                        std::mem::take(&mut state.stack[a])
                    } else {
                        TValue::Nil(NilKind::Strict)
                    };
                    if let Some(frame) = call_stack.pop() {
                        let return_base = frame.return_base;
                        let num_results = frame.num_results;
                        state.code = frame.code;
                        state.constants = frame.constants;
                        state.upval_descs = frame.upval_descs;
                        state.protos = frame.protos;
                        state.base = frame.base;
                        state.pc = frame.return_pc;
                        state.num_params = frame.num_params;
                        state.is_vararg = frame.is_vararg;
                        state.closure_upvals = frame.closure_upvals;
                        state.tbc_list = frame.tbc_list;
                        state.open_upval = frame.open_upval;
                        while state.stack.len() <= return_base + num_results.saturating_sub(1) {
                            state.stack.push(TValue::Nil(NilKind::Strict));
                        }
                        state.stack[return_base] = val;
                        for i in 1..num_results {
                            state.stack[return_base + i] = TValue::Nil(NilKind::Strict);
                        }
                        state.stack.truncate(return_base + num_results);
                        Ok(())
                    } else {
                        if state.base > 0 && state.base - 1 < state.stack.len() {
                            state.stack[state.base - 1] = val;
                        }
                        return Ok(VmResult::Return(1));
                    }
                }
                OpCode::FORLOOP => Self::op_forloop(state, inst),
                OpCode::FORPREP => Self::op_forprep(state, inst),
                OpCode::TFORPREP => Self::op_tforprep(state, inst),
                OpCode::TFORCALL => {
                    let ra = Self::ra(state, inst);
                    let c = opcodes::getarg_c(inst) as usize;
                    let f = Self::read_stack(state, ra).clone();
                    let s = Self::read_stack(state, ra + 1).clone();
                    let ctrl = Self::read_stack(state, ra + 3).clone();
                    Self::write_stack(state, ra + 3, f);
                    Self::write_stack(state, ra + 4, s);
                    Self::write_stack(state, ra + 5, ctrl);

                    let func_val = Self::read_stack(state, ra + 3).clone();

                    if let TValue::LClosure(closure) = &func_val {
                        let proto_code = closure.proto.code.clone();
                        let proto_constants = closure.proto.constants.clone();
                        let proto_upvals = closure.proto.upvalues.clone();
                        let proto_protos = closure.proto.protos.clone();
                        let proto_num_params = closure.proto.num_params;
                        let proto_is_vararg = closure.proto.is_vararg();
                        let proto_max_stack = closure.proto.max_stack_size;
                        drop(closure);
                        drop(func_val);

                        let nresults = c + 1;
                        let fsize = proto_max_stack as usize;
                        let nfixparams = proto_num_params as usize;
                        let nargs = 2;

                        call_stack.push(CallFrame {
                            code: std::mem::take(&mut state.code),
                            constants: std::mem::take(&mut state.constants),
                            upval_descs: std::mem::take(&mut state.upval_descs),
                            protos: std::mem::take(&mut state.protos),
                            base: state.base,
                            return_pc: state.pc + 1,
                            return_base: ra + 3,
                            num_results: nresults,
                            num_params: state.num_params,
                            is_vararg: state.is_vararg,
                            closure_upvals: std::mem::take(&mut state.closure_upvals),
                            tbc_list: state.tbc_list.take(),
                            open_upval: state.open_upval.take(),
                        });

                        state.code = proto_code;
                        state.constants = proto_constants;
                        state.upval_descs = proto_upvals;
                        state.protos = proto_protos;
                        state.base = ra + 4;
                        state.pc = 0;
                        state.num_params = proto_num_params;
                        state.is_vararg = proto_is_vararg;
                        state.closure_upvals = Vec::new();
                        state.tbc_list = None;
                        state.open_upval = None;

                        let frame_end = ra + 4 + fsize;
                        while state.stack.len() < frame_end {
                            state.stack.push(TValue::Nil(NilKind::Strict));
                        }
                        for i in nargs..nfixparams {
                            state.stack[ra + 4 + i] = TValue::Nil(NilKind::Strict);
                        }
                        Ok(())
                    } else {
                        state.pc += 1;
                        Ok(())
                    }
                }
                OpCode::TFORLOOP => Self::op_tforloop(state, inst),
                OpCode::SETLIST => Self::op_setlist(state, inst),
                OpCode::CLOSURE => Self::op_closure(state, inst),
                OpCode::VARARG => Self::op_vararg(state, inst),
                OpCode::GETVARG => Self::op_getvarg(state, inst),
                OpCode::ERRNNIL => Self::op_errnnil(state, inst),
                OpCode::VARARGPREP => Self::op_varargprep(state, inst),
                OpCode::EXTRAARG => {
                    return Err(VmError::IllegalOpcode(OpCode::EXTRAARG as u8));
                }
            }?;
        }
    }

    // ========================================================================
    // 辅助方法
    // ========================================================================

    fn ra(state: &LuaState, inst: Instruction) -> usize {
        state.base + opcodes::getarg_a(inst) as usize
    }

    fn rb(state: &LuaState, inst: Instruction) -> usize {
        state.base + opcodes::getarg_b(inst) as usize
    }

    fn rc(state: &LuaState, inst: Instruction) -> usize {
        state.base + opcodes::getarg_c(inst) as usize
    }

    fn ensure_stack(state: &mut LuaState, idx: usize) {
        if idx >= state.stack.len() {
            state.stack.resize(idx + 1, TValue::Nil(NilKind::Strict));
        }
    }

    fn read_stack(state: &LuaState, idx: usize) -> &TValue {
        if idx < state.stack.len() { &state.stack[idx] } else { panic!("stack underflow: idx={}, stack.len={}, pc={}, base={}", idx, state.stack.len(), state.pc, state.base); }
    }

    fn write_stack(state: &mut LuaState, idx: usize, val: TValue) {
        Self::ensure_stack(state, idx);
        state.stack[idx] = val;
    }

    #[allow(dead_code)]
    fn push_stack(state: &mut LuaState, val: TValue) -> usize {
        let idx = state.stack.len();
        state.stack.push(val);
        idx
    }

    fn do_conditional_jump(state: &mut LuaState, inst: Instruction, cond: bool) {
        let expected = opcodes::testarg_k(inst);
        if cond == expected {
            // Take the jump
            let next_idx = state.pc + 1;
            if next_idx >= state.code.len() { return; }
            let next_inst = state.code[next_idx];
            let sj = opcodes::getarg_sj(next_inst);
            state.pc = ((state.pc as i32) + sj + 1) as usize;
        } else {
            state.pc += 1;
        }
    }

    // ========================================================================
    // 操作码实现
    // ========================================================================

    fn op_move(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let val = Self::read_stack(state, b).clone();
        Self::write_stack(state, a, val);
        state.pc += 1;
        Ok(())
    }

    fn op_loadi(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let val = opcodes::getarg_sbx(inst) as i64;
        Self::write_stack(state, a, TValue::Integer(val));
        state.pc += 1;
        Ok(())
    }

    fn op_loadf(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let val = opcodes::getarg_sbx(inst) as f64;
        Self::write_stack(state, a, TValue::Float(val));
        state.pc += 1;
        Ok(())
    }

    fn op_loadk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let idx = opcodes::getarg_bx(inst) as usize;
        let val = state.constants[idx].clone();
        Self::write_stack(state, a, val);
        state.pc += 1;
        Ok(())
    }

    fn op_loadkx(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        state.pc += 1;
        let extra = state.code[state.pc];
        let extra_idx = opcodes::getarg_a(extra) as usize;
        let val = state.constants[extra_idx].clone();
        Self::write_stack(state, a, val);
        state.pc += 1;
        Ok(())
    }

    fn op_loadfalse(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        Self::write_stack(state, a, TValue::Boolean(false));
        state.pc += 1;
        Ok(())
    }

    fn op_lfalseskip(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        Self::write_stack(state, a, TValue::Boolean(false));
        state.pc += 2;
        Ok(())
    }

    fn op_loadtrue(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        Self::write_stack(state, a, TValue::Boolean(true));
        state.pc += 1;
        Ok(())
    }

    fn op_loadnil(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst);
        for i in 0..=b {
            Self::write_stack(state, a + i as usize, TValue::Nil(NilKind::Strict));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_getupval(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as usize;
        if b < state.closure_upvals.len() {
            let val = match &state.closure_upvals[b] {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { stack_index, next: _, previous: _ } => {
                    state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
                }
            };
            Self::write_stack(state, a, val);
        }
        state.pc += 1;
        Ok(())
    }

    fn op_setupval(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as usize;
        let val = Self::read_stack(state, a).clone();
        if b < state.closure_upvals.len() {
            match &mut state.closure_upvals[b] {
                UpVal::Closed { value } => {
                    // GC barrier: if closure is black, mark upvalue
                    state.gc.cond_gc();
                    **value = val;
                }
                UpVal::Open { stack_index, next: _, previous: _ } => {
                    if *stack_index < state.stack.len() {
                        state.stack[*stack_index] = val;
                    }
                }
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_gettabup(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as usize;
        let kb_idx = opcodes::getarg_c(inst) as usize;
        let key = state.constants.get(kb_idx).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let upval_val = if b < state.closure_upvals.len() {
            match &state.closure_upvals[b] {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { stack_index, .. } => state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict)),
            }
        } else {
            TValue::Nil(NilKind::Strict)
        };
        let result = Self::table_get(&upval_val, &key);
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_gettable(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let table_val = Self::read_stack(state, b).clone();
        let key = Self::read_stack(state, c).clone();
        let result = Self::table_get(&table_val, &key);
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_geti(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = opcodes::getarg_c(inst) as i64;
        let table_val = Self::read_stack(state, b).clone();
        let key = TValue::Integer(c);
        let result = Self::table_get(&table_val, &key);
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_getfield(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let table_val = Self::read_stack(state, b).clone();
        let key = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let result = Self::table_get(&table_val, &key);
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_settabup(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = opcodes::getarg_a(inst) as usize;
        let b_key = opcodes::getarg_b(inst) as usize;
        let c = opcodes::getarg_c(inst);
        let key = state.constants.get(b_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let val = Self::resolve_val(state, inst, c);
        let upval_val = if a < state.closure_upvals.len() {
            match &state.closure_upvals[a] {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { stack_index, .. } => state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict)),
            }
        } else {
            TValue::Nil(NilKind::Strict)
        };
        let modified = Self::table_set_tv(upval_val, key, val, &state.gc);
        if a < state.closure_upvals.len() {
            match &mut state.closure_upvals[a] {
                UpVal::Closed { value } => **value = modified,
                UpVal::Open { stack_index, .. } => {
                    if *stack_index < state.stack.len() {
                        state.stack[*stack_index] = modified;
                    }
                }
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_settable(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = opcodes::getarg_c(inst);
        let table_val = Self::read_stack(state, a).clone();
        let key = Self::read_stack(state, b).clone();
        let val = Self::resolve_val(state, inst, c);
        let modified = Self::table_set_tv(table_val, key, val, &state.gc);
        Self::write_stack(state, a, modified);
        state.pc += 1;
        Ok(())
    }

    fn op_seti(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as i64;
        let c = opcodes::getarg_c(inst);
        let table_val = Self::read_stack(state, a).clone();
        let val = Self::resolve_val(state, inst, c);
        let modified = Self::table_set_tv(table_val, TValue::Integer(b), val, &state.gc);
        Self::write_stack(state, a, modified);
        state.pc += 1;
        Ok(())
    }

    fn op_setfield(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b_key = opcodes::getarg_b(inst) as usize;
        let c = opcodes::getarg_c(inst);
        let table_val = Self::read_stack(state, a).clone();
        let key = state.constants.get(b_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let val = Self::resolve_val(state, inst, c);
        let modified = Self::table_set_tv(table_val, key, val, &state.gc);
        Self::write_stack(state, a, modified);
        state.pc += 1;
        Ok(())
    }

    fn op_newtable(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_vb(inst) as u32;
        let mut c = opcodes::getarg_vc(inst) as u32;
        if opcodes::testarg_k(inst) {
            let extra = opcodes::getarg_a(state.code[state.pc + 1]);
            c += (extra as u32) * ((1u32 << opcodes::SIZE_VC));
            state.pc += 1;
        }
        let hash_size = if b > 0 { 1u32 << (b - 1) } else { 0 };
        let array_size = c as usize;
        let table = Table::with_capacity(array_size, hash_size as usize);
        let table_id = state.gc.register_object(array_size + hash_size as usize);
        table.gc_header.set_id(table_id);
        Self::write_stack(state, a, TValue::Table(table));
        state.pc += 1;
        Ok(())
    }

    fn op_self(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let key = state.constants.get(opcodes::getarg_c(inst) as usize)
            .cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let obj = Self::read_stack(state, b).clone();
        Self::write_stack(state, a + 1, obj.clone());
        let result = Self::table_get(&obj, &key);
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    // ---- 算术运算 ----

    fn op_addi(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let imm = opcodes::getarg_sc(inst) as i64;
        let val = Self::read_stack(state, b).clone();
        match val {
            TValue::Integer(iv) => {
                Self::write_stack(state, a, TValue::Integer(iv.wrapping_add(imm)));
                state.pc += 1;
            }
            TValue::Float(fv) => {
                Self::write_stack(state, a, TValue::Float(fv + imm as f64));
                state.pc += 1;
            }
            _ => {}
        }
        state.pc += 1;
        Ok(())
    }

    fn op_addk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        let result = Self::arith_binary(&v1, &v2, |a, b| a + b, |a, b| a.wrapping_add(b));
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_subk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        let result = Self::arith_binary(&v1, &v2, |a, b| a - b, |a, b| a.wrapping_sub(b));
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_mulk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        let result = Self::arith_binary(&v1, &v2, |a, b| a * b, |a, b| a.wrapping_mul(b));
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_modk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        let result = Self::arith_mod(&v1, &v2)?;
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_powk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(n1), Some(n2)) = (to_number_ns(v1), to_number_ns(&v2)) {
            Self::write_stack(state, a, TValue::Float(n1.powf(n2)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_divk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(n1), Some(n2)) = (to_number_ns(v1), to_number_ns(&v2)) {
            Self::write_stack(state, a, TValue::Float(n1 / n2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_idivk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        let result = Self::arith_idiv(&v1, &v2)?;
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_bandk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(i1), TValue::Integer(i2)) = (to_integer_ns(v1, F2IMode::Eq), &v2) {
            Self::write_stack(state, a, TValue::Integer(i1 & i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bork(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(i1), TValue::Integer(i2)) = (to_integer_ns(v1, F2IMode::Eq), &v2) {
            Self::write_stack(state, a, TValue::Integer(i1 | i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bxork(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(i1), TValue::Integer(i2)) = (to_integer_ns(v1, F2IMode::Eq), &v2) {
            Self::write_stack(state, a, TValue::Integer(i1 ^ i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_shli(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let ic = opcodes::getarg_c(inst) as i64;
        let v = Self::read_stack(state, b);
        if let Some(ib) = to_integer_ns(v, F2IMode::Eq) {
            Self::write_stack(state, a, TValue::Integer(shiftl(ic, ib)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_shri(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let ic = opcodes::getarg_c(inst) as i64;
        let v = Self::read_stack(state, b);
        if let Some(ib) = to_integer_ns(v, F2IMode::Eq) {
            Self::write_stack(state, a, TValue::Integer(shiftl(ib, -ic)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_add(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        let result = Self::arith_binary(&v1, &v2, |a, b| a + b, |a, b| a.wrapping_add(b));
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_sub(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        let result = Self::arith_binary(&v1, &v2, |a, b| a - b, |a, b| a.wrapping_sub(b));
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_mul(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        let result = Self::arith_binary(&v1, &v2, |a, b| a * b, |a, b| a.wrapping_mul(b));
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_mod(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        let result = Self::arith_mod(&v1, &v2)?;
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_pow(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(n1), Some(n2)) = (to_number_ns(v1), to_number_ns(v2)) {
            Self::write_stack(state, a, TValue::Float(n1.powf(n2)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_div(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(n1), Some(n2)) = (to_number_ns(v1), to_number_ns(v2)) {
            Self::write_stack(state, a, TValue::Float(n1 / n2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_idiv(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        let result = Self::arith_idiv(&v1, &v2)?;
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_band(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(v1, F2IMode::Eq),
            to_integer_ns(v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(i1 & i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bor(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(v1, F2IMode::Eq),
            to_integer_ns(v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(i1 | i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bxor(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(v1, F2IMode::Eq),
            to_integer_ns(v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(i1 ^ i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_shl(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(v1, F2IMode::Eq),
            to_integer_ns(v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(shiftl(i1, i2)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_shr(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(v1, F2IMode::Eq),
            to_integer_ns(v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(shiftl(i1, -i2)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_mmbin(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: ra = RA(i) → A 字段 = 第一操作数栈位置
        // C: rb = vRB(i) → B 字段 = 第二操作数栈位置
        // C: tm = GETARG_C(i) → C 字段 = 元方法事件
        // C: result = RA(pi) → 原始算术指令的 A 字段 (此处简化为 a)
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let p1 = Self::read_stack(state, a);
        let p2 = Self::read_stack(state, b);
        let tm_idx = opcodes::getarg_c(inst) as u8;
        if let Some(tm) = TagMethod::from_u8(tm_idx) {
            let dmt = DefaultMetatables::new();
            let mut call_fn = |_f: &TValue, _args: &[&TValue]| {
                Err(TagMethodError::NoMetamethod(tm))
            };
            match try_bin_tm(p1, p2, tm, &dmt, &mut call_fn) {
                Ok(result) => { Self::write_stack(state, a, result); }
                Err(_) => { return Err(VmError::MetaMethodNotImplemented(tm.name().to_string())); }
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_mmbini(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: ra = RA(i) → A 字段 = 第一操作数
        // C: imm = GETARG_sB(i) = GETARG_B(i) - OFFSET_sC → 有符号立即数
        // C: tm = GETARG_C(i) → C 字段 = 元方法事件
        // C: flip = GETARG_k(i) → k 位 = 翻转标志
        // C: result = RA(pi)
        let a = Self::ra(state, inst);
        let imm = opcodes::getarg_b(inst) - 127;
        let p1 = Self::read_stack(state, a);
        let p2 = TValue::Integer(imm as i64);
        let flip = opcodes::testarg_k(inst);
        let tm_idx = opcodes::getarg_c(inst) as u8;
        if let Some(tm) = TagMethod::from_u8(tm_idx) {
            let dmt = DefaultMetatables::new();
            let mut call_fn = |_f: &TValue, _args: &[&TValue]| {
                Err(TagMethodError::NoMetamethod(tm))
            };
            match try_bin_assoc_tm(p1, &p2, flip, tm, &dmt, &mut call_fn) {
                Ok(result) => { Self::write_stack(state, a, result); }
                Err(_) => { return Err(VmError::MetaMethodNotImplemented(tm.name().to_string())); }
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_mmbink(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: ra = RA(i) → A 字段 = 第一操作数
        // C: imm = KB(i) → 常量 (从 proto 中读取)
        // C: tm = GETARG_C(i) → C 字段 = 元方法事件
        // C: flip = GETARG_k(i) → k 位 = 翻转标志
        // C: result = RA(pi)
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as usize;
        let p1 = Self::read_stack(state, a);
        let p2 = state.constants.get(b)
            .cloned()
            .unwrap_or(TValue::Nil(NilKind::Strict));
        let flip = opcodes::testarg_k(inst);
        let tm_idx = opcodes::getarg_c(inst) as u8;
        if let Some(tm) = TagMethod::from_u8(tm_idx) {
            let dmt = DefaultMetatables::new();
            let mut call_fn = |_f: &TValue, _args: &[&TValue]| {
                Err(TagMethodError::NoMetamethod(tm))
            };
            match try_bin_assoc_tm(p1, &p2, flip, tm, &dmt, &mut call_fn) {
                Ok(result) => { Self::write_stack(state, a, result); }
                Err(_) => { return Err(VmError::MetaMethodNotImplemented(tm.name().to_string())); }
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_unm(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b);
        match v {
            TValue::Integer(i) => Self::write_stack(state, a, TValue::Integer(i.wrapping_neg())),
            TValue::Float(f) => Self::write_stack(state, a, TValue::Float(-f)),
            _ => {
                let dmt = DefaultMetatables::new();
                match try_bin_tm(v, v, TagMethod::Unm, &dmt, &mut |_f, _args| {
                    Err(crate::tm::TagMethodError::NoMetamethod(TagMethod::Unm))
                }) {
                    Ok(result) => { Self::write_stack(state, a, result); }
                    Err(_) => { return Err(VmError::MetaMethodNotImplemented("__unm".to_string())); }
                }
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bnot(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b);
        if let Some(i) = to_integer_ns(v, F2IMode::Eq) {
            Self::write_stack(state, a, TValue::Integer(!i));
        } else {
            let dmt = DefaultMetatables::new();
            match try_bin_tm(v, v, TagMethod::BNot, &dmt, &mut |_f, _args| {
                Err(crate::tm::TagMethodError::NoMetamethod(TagMethod::BNot))
            }) {
                Ok(result) => { Self::write_stack(state, a, result); }
                Err(_) => { return Err(VmError::MetaMethodNotImplemented("__bnot".to_string())); }
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_not(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b);
        let result = if is_false(v) { TValue::Boolean(true) } else { TValue::Boolean(false) };
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_len(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b);
        let dmt = DefaultMetatables::new();
        let mut call_fn = |_f: &TValue, _args: &[&TValue]| {
            Err(TagMethodError::NoMetamethod(TagMethod::Len))
        };
        let result = match objlen(v, Some(&dmt), Some(&mut call_fn)) {
            Ok(Some(val)) => val,
            Ok(None) => TValue::Integer(0),
            Err(_) => TValue::Integer(0),
        };
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_concat(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let c_val = opcodes::getarg_c(inst) as usize;
        let n = if c_val >= a { c_val - a + 1 } else { 1 };
        let mut vals: Vec<TValue> = Vec::with_capacity(n);
        for i in 0..n {
            let val = Self::read_stack(state, a + i).clone();
            vals.push(val);
        }
        let dmt = DefaultMetatables::new();
        match concat_stack(&mut vals, n, &dmt) {
            Ok(()) => {
                let result = vals.into_iter().next()
                    .unwrap_or(TValue::Str(crate::strings::LuaString::Short(
                        std::sync::Arc::new(crate::strings::ShortString { hash: 0, contents: String::new() })
                    )));
                Self::write_stack(state, a, result);
                let top = a + n;
                let size = state.stack.len();
                if top < size {
                    for i in (a + 1)..top {
                        if i < state.stack.len() {
                            state.stack[i] = TValue::Nil(NilKind::Strict);
                        }
                    }
                }
            }
            Err(crate::tm::TagMethodError::ConcatError { .. }) => {
                return Err(VmError::MetaMethodNotImplemented("__concat".to_string()));
            }
            Err(_) => {
                return Err(VmError::MetaMethodNotImplemented("__concat".to_string()));
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_close(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        crate::func::close(state, a, 0, 1);
        state.pc += 1;
        Ok(())
    }

    fn op_tbc(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        crate::func::new_tbc_upval(state, a);
        state.pc += 1;
        Ok(())
    }

    fn op_jmp(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let sj = opcodes::getarg_sj(inst);
        state.pc = ((state.pc as i32) + sj + 1) as usize;
        Ok(())
    }

    // ---- 比较运算 ----

    fn op_eq(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v1 = Self::read_stack(state, a);
        let v2 = Self::read_stack(state, b);
        let dmt = DefaultMetatables::new();
        let mut call_fn = |_f: &TValue, _args: &[&TValue]| {
            Err(TagMethodError::NoMetamethod(TagMethod::Eq))
        };
        let cond = match equal(v1, v2, Some(&dmt), Some(&mut call_fn)) {
            Ok(b) => b,
            Err(_) => false,
        };
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_lt(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v1 = Self::read_stack(state, a);
        let v2 = Self::read_stack(state, b);
        let dmt = DefaultMetatables::new();
        let mut call_fn = |_f: &TValue, _args: &[&TValue]| {
            Err(TagMethodError::NoMetamethod(TagMethod::Lt))
        };
        let cond = match less_than(v1, v2, Some(&dmt), Some(&mut call_fn)) {
            Ok(b) => b,
            Err(_) => false,
        };
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_le(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v1 = Self::read_stack(state, a);
        let v2 = Self::read_stack(state, b);
        let dmt = DefaultMetatables::new();
        let mut call_fn = |_f: &TValue, _args: &[&TValue]| {
            Err(TagMethodError::NoMetamethod(TagMethod::Le))
        };
        let cond = match less_equal(v1, v2, Some(&dmt), Some(&mut call_fn)) {
            Ok(b) => b,
            Err(_) => false,
        };
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_eqk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b_key = opcodes::getarg_b(inst) as usize;
        let v1 = Self::read_stack(state, a);
        let v2 = state.constants.get(b_key).unwrap();
        let cond = raw_equal(v1, v2);
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_eqi(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let im = opcodes::getarg_sbx(inst) as i64;
        let v = Self::read_stack(state, a);
        let cond = match v {
            TValue::Integer(i) => *i == im,
            TValue::Float(f) => (*f - im as f64).abs() < f64::EPSILON,
            _ => false,
        };
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_lti(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let im = opcodes::getarg_sbx(inst) as i64;
        let v = Self::read_stack(state, a);
        let cond = match v {
            TValue::Integer(i) => *i < im,
            TValue::Float(f) => *f < (im as f64),
            _ => false,
        };
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_lei(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let im = opcodes::getarg_sbx(inst) as i64;
        let v = Self::read_stack(state, a);
        let cond = match v {
            TValue::Integer(i) => *i <= im,
            TValue::Float(f) => *f <= (im as f64),
            _ => false,
        };
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_gti(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let im = opcodes::getarg_sbx(inst) as i64;
        let v = Self::read_stack(state, a);
        let cond = match v {
            TValue::Integer(i) => *i > im,
            TValue::Float(f) => *f > (im as f64),
            _ => false,
        };
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_gei(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let im = opcodes::getarg_sbx(inst) as i64;
        let v = Self::read_stack(state, a);
        let cond = match v {
            TValue::Integer(i) => *i >= im,
            TValue::Float(f) => *f >= (im as f64),
            _ => false,
        };
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_test(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let v = Self::read_stack(state, a);
        let cond = !is_false(v);
        let k = opcodes::testarg_k(inst);
        if cond != k {
            state.pc += 1;
        } else {
            let offset = if k {
                opcodes::getarg_sbx(inst)
            } else {
                opcodes::getarg_b(inst)
            };
            state.pc = ((state.pc as i32) + offset + 1) as usize;
        }
        Ok(())
    }

    fn op_testset(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b).clone();
        let cond = !is_false(&v);
        let expected = opcodes::testarg_k(inst);
        if cond == expected {
            Self::write_stack(state, a, v);
            let next_idx = state.pc + 1;
            if next_idx < state.code.len() {
                let next = state.code[next_idx];
                let sj = opcodes::getarg_sj(next);
                state.pc = ((state.pc as i32) + sj + 1) as usize;
            }
        } else {
            state.pc += 1;
        }
        Ok(())
    }

    // ---- 调用 / 返回 ----

    fn op_call(state: &mut LuaState, inst: Instruction) -> Result<Option<VmResult>, VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst);
        let c = opcodes::getarg_c(inst);
        let func_val = Self::read_stack(state, a).clone();
        match func_val {
            TValue::LClosure(closure) => Ok(Some(VmResult::Call {
                proto: closure.proto,
                base: a + 1,
                num_results: c - 1,
            })),
            TValue::LightUserData(tag) => {
                let tag_val = tag as usize;
                if tag_val == 1 {
                    let mut s = String::new();
                    for i in (a + 1)..(a + b as usize) {
                        if i > a + 1 { s.push('\t'); }
                        let val = Self::read_stack(state, i);
                        match val {
                            TValue::Nil(_) => s.push_str("nil"),
                            TValue::Boolean(bv) => s.push_str(if *bv { "true" } else { "false" }),
                            TValue::Integer(n) => s.push_str(&n.to_string()),
                            TValue::Float(n) => {
                                if n.is_nan() { s.push_str("nan"); }
                                else if n.is_infinite() { s.push_str(if *n > 0.0 { "inf" } else { "-inf" }); }
                                else { s.push_str(&n.to_string()); }
                            }
                            TValue::Str(lst) => s.push_str(&lst.as_str()),
                            TValue::Table(_) => s.push_str("table: 0x0"),
                            TValue::LClosure(_) | TValue::LCFn(_) | TValue::CClosure(_) => s.push_str("function: 0x0"),
                            _ => s.push_str(&format!("{:?}", val)),
                        }
                    }
                    println!("{}", s);
                }
                let nresults = if c >= 1 { c - 1 } else { 0 };
                for i in 0..nresults {
                    Self::write_stack(state, a + i as usize, TValue::Nil(NilKind::Strict));
                }
                state.pc += 1;
                Ok(None)
            }
            _ => {
                state.pc += 1;
                Ok(Some(VmResult::Done))
            }
        }
    }

    fn op_tailcall(state: &mut LuaState, inst: Instruction) -> Result<VmResult, VmError> {
        let a = Self::ra(state, inst);
        let func_val = Self::read_stack(state, a).clone();
        match func_val {
            TValue::LClosure(closure) => Ok(VmResult::TailCall {
                proto: closure.proto,
                base: a + 1,
            }),
            _ => Ok(VmResult::Return(0)),
        }
    }

    fn op_return(state: &mut LuaState, inst: Instruction) -> Result<VmResult, VmError> {
        let a = Self::ra(state, inst);
        let n = opcodes::getarg_b(inst) as i32 - 1;
        let nresults = if n < 0 { state.stack.len().saturating_sub(a) as i32 } else { n };
        Ok(VmResult::Return(nresults as usize))
    }

    fn op_return1(state: &mut LuaState, inst: Instruction) -> Result<VmResult, VmError> {
        let a = Self::ra(state, inst);
        let val = Self::read_stack(state, a).clone();
        if state.base > 0 {
            Self::write_stack(state, state.base - 1, val);
        }
        Ok(VmResult::Return(1))
    }

    // ---- 循环 ----

    fn op_forloop(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);

        // Check if this is an integer or float loop
        let count_val = Self::read_stack(state, ra);
        match count_val {
            TValue::Integer(count) => {
                let step = match Self::read_stack(state, ra + 1) {
                    TValue::Integer(s) => *s,
                    _ => { state.pc += 1; return Ok(()); }
                };
                let idx = match Self::read_stack(state, ra + 2) {
                    TValue::Integer(i) => *i,
                    _ => { state.pc += 1; return Ok(()); }
                };

                if *count > 0 {
                    let new_idx = idx.wrapping_add(step);
                    Self::write_stack(state, ra, TValue::Integer(count - 1));
                    Self::write_stack(state, ra + 2, TValue::Integer(new_idx));
                    let bx = opcodes::getarg_sbx(inst);
                    state.pc = ((state.pc as i32) - bx + 1) as usize;
                    return Ok(());
                }
            }
            TValue::Float(limit) => {
                let step = match Self::read_stack(state, ra + 1) {
                    TValue::Float(s) => *s,
                    _ => { state.pc += 1; return Ok(()); }
                };
                let idx = match Self::read_stack(state, ra + 2) {
                    TValue::Float(f) => *f,
                    _ => { state.pc += 1; return Ok(()); }
                };

                let new_idx = idx + step;
                let should_continue = if step > 0.0 { new_idx <= *limit } else { new_idx >= *limit };
                if should_continue {
                    Self::write_stack(state, ra + 2, TValue::Float(new_idx));
                    let bx = opcodes::getarg_sbx(inst);
                    state.pc = ((state.pc as i32) - bx + 1) as usize;
                    return Ok(());
                }
            }
            _ => {}
        }
        state.pc += 1;
        Ok(())
    }

    fn op_forprep(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        // eprintln!("DEBUG FORPREP: ra={}, stack.len={}, base={}, pc={}", ra, state.stack.len(), state.base, state.pc);

        let init_val = Self::read_stack(state, ra).clone();
        let limit_val = Self::read_stack(state, ra + 1).clone();
        let step_val = Self::read_stack(state, ra + 2).clone();

        match (&init_val, &step_val) {
            (TValue::Integer(init_i), TValue::Integer(step_i)) => {
                if *step_i == 0 {
                    return Err(VmError::RuntimeError("for step is zero".into()));
                }
                let limit_i = match &limit_val {
                    TValue::Integer(i) => *i,
                    TValue::Float(f) => {
                        if *step_i < 0 {
                            float_to_integer(*f, F2IMode::Ceil).unwrap_or(*init_i)
                        } else {
                            float_to_integer(*f, F2IMode::Floor).unwrap_or(*init_i)
                        }
                    }
                    _ => { state.pc += 1; return Ok(()); }
                };

                let skip = if *step_i > 0 { *init_i > limit_i } else { *init_i < limit_i };
                if skip {
                    let bx = opcodes::getarg_sbx(inst);
                    state.pc = ((state.pc as i32) + bx + 1) as usize;
                    return Ok(());
                }
                let count: u64 = if *step_i > 0 {
                    let diff = (limit_i as u64).wrapping_sub(*init_i as u64);
                    let step_u = *step_i as u64;
                    if step_u == 1 { diff } else { diff / step_u }
                } else {
                    let diff = (*init_i as u64).wrapping_sub(limit_i as u64);
                    let step_u = ((-(*step_i + 1)) as u64).wrapping_add(1);
                    diff / step_u
                };
                let saved_init = *init_i;
                let saved_step = *step_i;
                Self::write_stack(state, ra, TValue::Integer(count as i64));
                Self::write_stack(state, ra + 1, TValue::Integer(saved_step));
                Self::write_stack(state, ra + 2, TValue::Integer(saved_init));
            }
            _ => {
                let init_f = match &init_val {
                    TValue::Integer(i) => *i as f64,
                    TValue::Float(f) => *f,
                    _ => { state.pc += 1; return Ok(()); }
                };
                let limit_f = match &limit_val {
                    TValue::Integer(i) => *i as f64,
                    TValue::Float(f) => *f,
                    _ => { state.pc += 1; return Ok(()); }
                };
                let step_f = match &step_val {
                    TValue::Integer(i) => *i as f64,
                    TValue::Float(f) => *f,
                    _ => { state.pc += 1; return Ok(()); }
                };

                if step_f == 0.0 { return Err(VmError::RuntimeError("for step is zero".into())); }
                let skip = if step_f > 0.0 { limit_f < init_f } else { init_f < limit_f };
                if skip {
                    let bx = opcodes::getarg_sbx(inst);
                    state.pc = ((state.pc as i32) + bx + 1) as usize;
                    return Ok(());
                }
                Self::write_stack(state, ra, TValue::Float(limit_f));
                Self::write_stack(state, ra + 1, TValue::Float(step_f));
                Self::write_stack(state, ra + 2, TValue::Float(init_f));
            }
        }

        state.pc += 1;
        Ok(())
    }

    fn op_tforprep(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let tmp = Self::read_stack(state, ra + 2).clone();
        let closing = Self::read_stack(state, ra + 3).clone();
        Self::write_stack(state, ra + 3, tmp);
        Self::write_stack(state, ra + 2, closing);
        let bx = opcodes::getarg_sbx(inst);
        state.pc = ((state.pc as i32) + bx + 1) as usize;
        Ok(())
    }

    fn op_tforcall(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let f = Self::read_stack(state, ra).clone();
        let s = Self::read_stack(state, ra + 1).clone();
        let ctrl = Self::read_stack(state, ra + 2).clone();
        Self::write_stack(state, ra + 3, f);
        Self::write_stack(state, ra + 4, s);
        Self::write_stack(state, ra + 5, ctrl);
        state.pc += 1;
        Ok(())
    }

    fn op_tforloop(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let control = Self::read_stack(state, ra + 3);
        match control {
            TValue::Nil(_) => { state.pc += 1; }
            _ => {
                let bx = opcodes::getarg_sbx(inst);
                state.pc = ((state.pc as i32) - bx + 1) as usize;
            }
        }
        Ok(())
    }

    fn op_setlist(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let n = opcodes::getarg_vb(inst) as usize;
        let mut last = opcodes::getarg_vc(inst) as usize;
        if opcodes::testarg_k(inst) {
            let extra = opcodes::getarg_a(state.code[state.pc + 1]);
            last += (extra as usize) * ((1usize << opcodes::SIZE_VC));
            state.pc += 1;
        }

        let n_actual = if n == 0 { state.stack.len().saturating_sub(ra + 1) } else { n };
        last += n_actual;

        let table_val = Self::read_stack(state, ra);
        if let TValue::Table(ref table) = table_val {
            let mut t = table.clone();
            for i in 0..n_actual {
                let val = Self::read_stack(state, ra + 1 + i).clone();
                let pos = last - i - 1;
                t.set_int((pos + 1) as i64, val);
            }
            Self::write_stack(state, ra, TValue::Table(t));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_closure(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let bx = opcodes::getarg_bx(inst) as usize;
        if bx < state.protos.len() {
            let proto = state.protos[bx].clone();
            let closure = LClosure { gc_header: crate::gc::GCObjectHeader::new(), proto, upvals: Vec::new() };
            Self::write_stack(state, ra, TValue::LClosure(closure));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_vararg(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let n = opcodes::getarg_c(inst) as usize;
        let n_actual = if n == 0 { state.stack.len().saturating_sub(ra) } else { n.saturating_sub(1) };
        for i in 0..n_actual {
            let src_idx = state.base + state.num_params as usize + i;
            let val = if src_idx < state.stack.len() { state.stack[src_idx].clone() } else { TValue::Nil(NilKind::Strict) };
            Self::write_stack(state, ra + i, val);
        }
        state.pc += 1;
        Ok(())
    }

    fn op_getvarg(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let c = Self::rc(state, inst);
        let idx = match Self::read_stack(state, c) {
            TValue::Integer(i) => *i as usize,
            _ => 0,
        };
        let src_idx = state.base + state.num_params as usize + idx;
        let val = if src_idx < state.stack.len() { state.stack[src_idx].clone() } else { TValue::Nil(NilKind::Strict) };
        Self::write_stack(state, ra, val);
        state.pc += 1;
        Ok(())
    }

    fn op_errnnil(state: &mut LuaState, _inst: Instruction) -> Result<(), VmError> {
        state.pc += 1;
        Ok(())
    }

    fn op_varargprep(state: &mut LuaState, _inst: Instruction) -> Result<(), VmError> {
        state.pc += 1;
        Ok(())
    }

    // ========================================================================
    // 辅助: 表操作
    // ========================================================================

    fn table_get(table_val: &TValue, key: &TValue) -> TValue {
        match table_val {
            TValue::Table(t) => t.get(key).cloned().unwrap_or(TValue::Nil(NilKind::Strict)),
            _ => TValue::Nil(NilKind::Strict),
        }
    }

    fn table_set_tv(mut table_val: TValue, key: TValue, val: TValue, gc: &GCState) -> TValue {
        let table_id = if let TValue::Table(ref t) = table_val {
            t.gc_header.id()
        } else {
            None
        };

        if let TValue::Table(ref mut t) = table_val {
            t.set(key, val);
        }

        if let Some(tid) = table_id {
            gc.obj_barrier_back(tid, tid);
            gc.barrier_back(tid);
        }

        table_val
    }

    fn resolve_val(state: &LuaState, inst: Instruction, c: i32) -> TValue {
        if opcodes::testarg_k(inst) {
            state.constants.get(c as usize).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
        } else {
            Self::read_stack(state, Self::rc(state, inst)).clone()
        }
    }

    fn arith_binary(
        v1: &TValue, v2: &TValue,
        float_op: fn(f64, f64) -> f64,
        int_op: fn(i64, i64) -> i64,
    ) -> TValue {
        match (v1, v2) {
            (TValue::Integer(i1), TValue::Integer(i2)) => TValue::Integer(int_op(*i1, *i2)),
            _ => {
                if let (Some(n1), Some(n2)) = (to_number_ns(v1), to_number_ns(v2)) {
                    TValue::Float(float_op(n1, n2))
                } else {
                    TValue::Nil(NilKind::Strict)
                }
            }
        }
    }

    fn arith_mod(v1: &TValue, v2: &TValue) -> Result<TValue, VmError> {
        match (v1, v2) {
            (TValue::Integer(i1), TValue::Integer(i2)) => {
                let r = modulus(*i1, *i2).map_err(|_| VmError::ModuloByZero)?;
                Ok(TValue::Integer(r))
            }
            _ => {
                if let (Some(n1), Some(n2)) = (to_number_ns(v1), to_number_ns(v2)) {
                    Ok(TValue::Float(modulus_float(n1, n2)))
                } else {
                    Ok(TValue::Nil(NilKind::Strict))
                }
            }
        }
    }

    fn arith_idiv(v1: &TValue, v2: &TValue) -> Result<TValue, VmError> {
        match (v1, v2) {
            (TValue::Integer(i1), TValue::Integer(i2)) => {
                let r = idiv(*i1, *i2).map_err(|_| VmError::DivisionByZero)?;
                Ok(TValue::Integer(r))
            }
            _ => {
                if let (Some(n1), Some(n2)) = (to_number_ns(v1), to_number_ns(v2)) {
                    Ok(TValue::Float((n1 / n2).floor()))
                } else {
                    Ok(TValue::Nil(NilKind::Strict))
                }
            }
        }
    }
}

// ============================================================================
// format_float
// ============================================================================

fn format_float(f: f64) -> String {
    if f.is_nan() { return "nan".to_string(); }
    if f.is_infinite() { return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() }; }
    if f == 0.0 { return "0.0".to_string(); }
    let s = format!("{:.15}", f);
    let s = s.trim_end_matches('0');
    if s.ends_with('.') { format!("{}0", s) } else { s.to_string() }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::NilKind;
    use crate::strings::StringTable;
    use crate::gc::GCObjectHeader;
    use std::rc::Rc;

    fn make_gc() -> Rc<GCState> {
        Rc::new(GCState::default_incremental())
    }

    fn execute_test(proto: &Proto, base: usize, stack: Vec<TValue>) -> Result<VmResult, VmError> {
        VmExecutor::execute(proto, base, stack, make_gc())
    }

    fn make_proto(code: Vec<Instruction>, constants: Vec<TValue>) -> Proto {
        Proto {
            num_params: 0, flag: 0, max_stack_size: 10,
            size_upvalues: 0, size_k: constants.len() as i32,
            size_code: code.len() as i32, size_line_info: 0,
            size_p: 0, size_loc_vars: 0, size_abs_line_info: 0,
            line_defined: 0, last_line_defined: 0,
            constants, code,
            protos: vec![], upvalues: vec![],
            line_info: vec![], abs_line_info: vec![],
            loc_vars: vec![], source: None,
        }
    }

    #[allow(dead_code)]
    fn make_abck(op: OpCode, a: i32, b: i32, c: i32, k: i32) -> Instruction {
        let is_vabc = opcodes::get_opmode(op) == opcodes::OpMode::IvABC;
        let mut inst = 0u32;
        inst |= (op as u32) << opcodes::POS_OP;
        inst |= (a as u32 & 0xFF) << opcodes::POS_A;
        inst |= (k as u32 & 1) << opcodes::POS_K;
        if is_vabc {
            inst |= (b as u32 & 0x3F) << opcodes::POS_VB;
            inst |= (c as u32 & 0x3FF) << opcodes::POS_VC;
        } else {
            inst |= (b as u32 & 0xFF) << opcodes::POS_B;
            inst |= (c as u32 & 0xFF) << opcodes::POS_C;
        }
        inst
    }

    fn make_asbx(op: OpCode, a: i32, sbx: i32) -> Instruction {
        let mut inst = 0u32;
        inst |= (op as u32) << opcodes::POS_OP;
        inst |= (a as u32 & 0xFF) << opcodes::POS_A;
        let bx = (sbx + opcodes::OFFSET_SBX) as u32;
        inst |= bx << opcodes::POS_BX;
        inst
    }

    #[allow(dead_code)]
    fn make_bx(op: OpCode, a: i32, bx: i32) -> Instruction {
        let mut inst = 0u32;
        inst |= (op as u32) << opcodes::POS_OP;
        inst |= (a as u32 & 0xFF) << opcodes::POS_A;
        inst |= (bx as u32) << opcodes::POS_BX;
        inst
    }

    fn make_abc(op: OpCode, a: i32, b: i32, c: i32) -> Instruction {
        let is_vabc = opcodes::get_opmode(op) == opcodes::OpMode::IvABC;
        let mut inst = 0u32;
        inst |= (op as u32) << opcodes::POS_OP;
        inst |= (a as u32 & 0xFF) << opcodes::POS_A;
        if is_vabc {
            inst |= (b as u32 & 0x3F) << opcodes::POS_VB;
            inst |= (c as u32 & 0x3FF) << opcodes::POS_VC;
        } else {
            inst |= (b as u32 & 0xFF) << opcodes::POS_B;
            inst |= (c as u32 & 0xFF) << opcodes::POS_C;
        }
        inst
    }

    fn default_stack(size: usize) -> Vec<TValue> {
        vec![TValue::Nil(NilKind::Strict); size]
    }

    // ========================================================================
    // 基本操作码测试
    // ========================================================================

    #[test]
    fn test_execute_loadi() {
        let code = vec![make_asbx(OpCode::LOADI, 0, 42)];
        let proto = make_proto(code, vec![]);
        let stack = default_stack(5);
        let result = execute_test(&proto, 0, stack);
        assert!(result.is_ok());
    }

    #[test]
    fn test_execute_move() {
        let code = vec![
            make_asbx(OpCode::LOADI, 1, 99),
            make_abc(OpCode::MOVE, 0, 1, 0),
        ];
        let proto = make_proto(code, vec![]);
        let stack = default_stack(10);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_add() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_asbx(OpCode::LOADI, 1, 20),
            make_abc(OpCode::ADD, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        let stack = default_stack(10);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_not() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0),
            make_abc(OpCode::NOT, 1, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        let stack = default_stack(10);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_newtable() {
        let code = vec![make_abc(OpCode::NEWTABLE, 0, 0, 3)];
        let proto = make_proto(code, vec![]);
        let stack = default_stack(10);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_forprep() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 1),
            make_asbx(OpCode::LOADI, 1, 5),
            make_asbx(OpCode::LOADI, 2, 1),
            make_abc(OpCode::FORPREP, 0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        let stack = default_stack(20);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_concat() {
        let tb = StringTable::new();
        let mut stack = default_stack(10);
        stack[0] = TValue::Str(tb.intern("hello"));
        stack[1] = TValue::Str(tb.intern("world"));

        let code = vec![make_abc(OpCode::CONCAT, 0, 2, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    // ========================================================================
    // VM 错误测试
    // ========================================================================

    #[test]
    fn test_vm_error_display() {
        assert_eq!(format!("{}", VmError::DivisionByZero), "attempt to divide by zero");
        assert_eq!(format!("{}", VmError::ModuloByZero), "attempt to perform 'n%0'");
    }

    #[test]
    fn test_format_float() {
        assert_eq!(format_float(f64::NAN), "nan");
        assert_eq!(format_float(f64::INFINITY), "inf");
        assert_eq!(format_float(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn test_format_float_zero() {
        assert_eq!(format_float(0.0), "0.0");
        assert_eq!(format_float(-0.0), "0.0");
    }

    #[test]
    fn test_format_float_normal() {
        assert_eq!(format_float(42.0), "42.0");
        assert_eq!(format_float(3.5), "3.5");
    }

    // ========================================================================
    // SUB / MUL / DIV / IDIV / MOD / POW 操作码测试
    // ========================================================================

    #[test]
    fn test_execute_sub() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 30),
            make_asbx(OpCode::LOADI, 1, 10),
            make_abc(OpCode::SUB, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mul() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 6),
            make_asbx(OpCode::LOADI, 1, 7),
            make_abc(OpCode::MUL, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_div() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::DIV, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_idiv() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::IDIV, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mod() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::MOD, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_pow() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 2),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::POW, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // UNM / BNOT / BAND / BOR / BXOR / SHL / SHR 操作码测试
    // ========================================================================

    #[test]
    fn test_execute_unm() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 42),
            make_abc(OpCode::UNM, 1, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_bnot() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0),
            make_abc(OpCode::BNOT, 1, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_band() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0b1100),
            make_asbx(OpCode::LOADI, 1, 0b1010),
            make_abc(OpCode::BAND, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_bor() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0b1100),
            make_asbx(OpCode::LOADI, 1, 0b0011),
            make_abc(OpCode::BOR, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_bxor() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0b1100),
            make_asbx(OpCode::LOADI, 1, 0b1010),
            make_abc(OpCode::BXOR, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_shl() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 1),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::SHL, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_shr() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 16),
            make_asbx(OpCode::LOADI, 1, 2),
            make_abc(OpCode::SHR, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // 常量操作码测试 (ADDK, SUBK, MULK, MODK, POWK, DIVK, IDIVK)
    // ========================================================================

    #[test]
    fn test_execute_addk() {
        let constants = vec![TValue::Integer(5)];
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 3),
            make_abck(OpCode::ADDK, 1, 0, 0, 1),
        ];
        let proto = make_proto(code, constants);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_subk() {
        let constants = vec![TValue::Integer(3)];
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_abck(OpCode::SUBK, 1, 0, 0, 1),
        ];
        let proto = make_proto(code, constants);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mulk() {
        let constants = vec![TValue::Integer(4)];
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 3),
            make_abck(OpCode::MULK, 1, 0, 0, 0),
        ];
        let proto = make_proto(code, constants);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_divk() {
        let constants = vec![TValue::Integer(2)];
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_abck(OpCode::DIVK, 1, 0, 0, 0),
        ];
        let proto = make_proto(code, constants);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // LOADK / LOADKX / LOADF / ADDI / SHLI / SHRI 测试
    // ========================================================================

    #[test]
    fn test_execute_loadk() {
        let constants = vec![TValue::Integer(42)];
        let code = vec![make_bx(OpCode::LOADK, 0, 0)];
        let proto = make_proto(code, constants);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_loadf() {
        let code = vec![make_asbx(OpCode::LOADF, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_addi() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_asbx(OpCode::ADDI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // 比较操作码测试 (EQ, LT, LE, EQI, LTI, LEI, GTI, GEI)
    // ========================================================================

    #[test]
    fn test_execute_eq() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 42),
            make_asbx(OpCode::LOADI, 1, 42),
            make_abc(OpCode::EQ, 0, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_lt() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 3),
            make_asbx(OpCode::LOADI, 1, 5),
            make_abc(OpCode::LT, 0, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_le() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 5),
            make_asbx(OpCode::LOADI, 1, 5),
            make_abc(OpCode::LE, 0, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_eqi() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 42),
            make_asbx(OpCode::EQI, 0, 42),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_lti() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 3),
            make_asbx(OpCode::LTI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_lei() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 5),
            make_asbx(OpCode::LEI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_gti() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 7),
            make_asbx(OpCode::GTI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_gei() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 5),
            make_asbx(OpCode::GEI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // 跳转操作码测试 (JMP, TEST, TESTSET)
    // ========================================================================

    #[test]
    fn test_execute_jmp() {
        let code = vec![
            make_bx(OpCode::JMP, 0, 1),
            make_bx(OpCode::RETURN0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_test() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 1),
            make_bx(OpCode::TEST, 0, 1),
            make_bx(OpCode::RETURN0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_testset() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 42),
            make_abc(OpCode::TESTSET, 1, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // 表操作测试 (NEWTABLE, GETTABLE, SETTABLE, GETI, SETI, SELF, LEN)
    // ========================================================================

    #[test]
    fn test_execute_gettable() {
        let code = vec![
            make_abc(OpCode::NEWTABLE, 0, 0, 3),
            make_asbx(OpCode::LOADI, 1, 1),
            make_abc(OpCode::GETTABLE, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_settable() {
        let code = vec![
            make_abc(OpCode::NEWTABLE, 0, 0, 3),
            make_asbx(OpCode::LOADI, 1, 1),
            make_asbx(OpCode::LOADI, 2, 42),
            make_abck(OpCode::SETTABLE, 0, 1, 2, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_geti() {
        let code = vec![
            make_abc(OpCode::NEWTABLE, 0, 0, 3),
            make_abc(OpCode::GETI, 1, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_seti() {
        let code = vec![
            make_abc(OpCode::NEWTABLE, 0, 0, 3),
            make_asbx(OpCode::LOADI, 1, 42),
            make_abck(OpCode::SETI, 0, 1, 1, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_self() {
        let constants = vec![TValue::Nil(NilKind::Strict)];
        let code = vec![
            make_abc(OpCode::NEWTABLE, 0, 0, 3),
            make_abc(OpCode::SELF, 1, 0, 0),
        ];
        let proto = make_proto(code, constants);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_len() {
        let tb = StringTable::new();
        let mut stack = default_stack(10);
        stack[0] = TValue::Str(tb.intern("hello"));

        let code = vec![make_abc(OpCode::LEN, 1, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    // ========================================================================
    // 返回操作码测试 (RETURN, RETURN1, RETURN0)
    // ========================================================================

    #[test]
    fn test_execute_return() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 99),
            make_abc(OpCode::RETURN, 0, 2, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_return1() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 77),
            make_abc(OpCode::RETURN1, 0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_return0() {
        let code = vec![
            make_bx(OpCode::RETURN0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // CALL / TAILCALL 测试
    // ========================================================================

    #[test]
    fn test_execute_call_lua_closure() {
        // Create an inner proto that just returns 0
        let inner_proto = make_proto(vec![make_bx(OpCode::RETURN0, 0, 0)], vec![]);
        let closure = LClosure { gc_header: GCObjectHeader::new(), proto: inner_proto, upvals: vec![] };

        let mut stack = default_stack(10);
        stack[0] = TValue::LClosure(closure);

        let code = vec![make_abck(OpCode::CALL, 0, 0, 1, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_tailcall_lua_closure() {
        let inner_proto = make_proto(vec![make_bx(OpCode::RETURN0, 0, 0)], vec![]);
        let closure = LClosure { gc_header: GCObjectHeader::new(), proto: inner_proto, upvals: vec![] };

        let mut stack = default_stack(10);
        stack[0] = TValue::LClosure(closure);

        let code = vec![make_abck(OpCode::TAILCALL, 0, 0, 1, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    // ========================================================================
    // CLOSURE 操作码测试
    // ========================================================================

    #[test]
    fn test_execute_closure() {
        let inner_proto = make_proto(vec![make_bx(OpCode::RETURN0, 0, 0)], vec![]);
        let code = vec![make_bx(OpCode::CLOSURE, 0, 0)];
        let mut proto = make_proto(code, vec![]);
        proto.protos = vec![inner_proto];
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // SETLIST 操作码测试
    // ========================================================================

    #[test]
    fn test_execute_setlist() {
        let code = vec![
            make_abc(OpCode::NEWTABLE, 0, 0, 0),
            make_asbx(OpCode::LOADI, 1, 10),
            make_asbx(OpCode::LOADI, 2, 20),
            make_asbx(OpCode::LOADI, 3, 30),
            make_abc(OpCode::SETLIST, 0, 3, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    // ========================================================================
    // LOADFALSE / LOADTRUE / LOADNIL 测试
    // ========================================================================

    #[test]
    fn test_execute_loadfalse() {
        let code = vec![make_abc(OpCode::LOADFALSE, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(5)).is_ok());
    }

    #[test]
    fn test_execute_loadtrue() {
        let code = vec![make_abc(OpCode::LOADTRUE, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(5)).is_ok());
    }

    #[test]
    fn test_execute_loadnil() {
        let code = vec![make_abck(OpCode::LOADNIL, 0, 3, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // TFOR 循环操作码测试
    // ========================================================================

    #[test]
    fn test_execute_tforprep() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 1),
            make_asbx(OpCode::LOADI, 1, 2),
            make_asbx(OpCode::LOADI, 2, 3),
            make_abc(OpCode::TFORPREP, 0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_tforcall() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 1),
            make_asbx(OpCode::LOADI, 1, 2),
            make_asbx(OpCode::LOADI, 2, 3),
            make_abc(OpCode::TFORCALL, 0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_tforloop() {
        let code = vec![
            make_asbx(OpCode::LOADI, 3, 0),
            make_abc(OpCode::TFORLOOP, 0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    // ========================================================================
    // VARARG / ERRNNIL / VARARGPREP 测试
    // ========================================================================

    #[test]
    fn test_execute_vararg() {
        let code = vec![make_abc(OpCode::VARARG, 0, 1, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_errnnil() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 42),
            make_bx(OpCode::ERRNNIL, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_varargprep() {
        let code = vec![
            make_abc(OpCode::VARARGPREP, 0, 3, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_ok());
    }

    // ========================================================================
    // MMBIN / MMBINI / MMBINK / CLOSE / TBC 桩测试
    // ========================================================================

    #[test]
    fn test_execute_mmbin() {
        // C=255 (超出 TagMethod 范围), 使 TM 查找被跳过
        let code = vec![make_abc(OpCode::MMBIN, 0, 0, 255)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mmbini() {
        let code = vec![make_abc(OpCode::MMBINI, 0, 0, 255)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mmbink() {
        let code = vec![make_abc(OpCode::MMBINK, 0, 0, 255)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_close() {
        let code = vec![make_abc(OpCode::CLOSE, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_tbc() {
        let code = vec![make_abc(OpCode::TBC, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // LFALSESKIP / GETVARG 测试
    // ========================================================================

    #[test]
    fn test_execute_lfalseskip() {
        let code = vec![
            make_abc(OpCode::LFALSESKIP, 0, 0, 0),
            make_bx(OpCode::RETURN0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_getvarg() {
        let code = vec![
            make_asbx(OpCode::LOADI, 1, 0),
            make_abc(OpCode::GETVARG, 0, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // VmError 更多测试
    // ========================================================================

    #[test]
    fn test_vm_error_all_displays() {
        assert_eq!(format!("{}", VmError::TypeError("bad type".into())), "type error: bad type");
        assert_eq!(format!("{}", VmError::StackOverflow), "stack overflow");
        assert_eq!(format!("{}", VmError::IllegalOpcode(99)), "illegal opcode: 99");
        assert_eq!(format!("{}", VmError::RuntimeError("boom".into())), "runtime error: boom");
    }

    #[test]
    fn test_vm_error_debug() {
        let e = VmError::DivisionByZero;
        assert_eq!(format!("{:?}", e), "DivisionByZero");
        let e = VmError::StackOverflow;
        assert_eq!(format!("{:?}", e), "StackOverflow");
    }

    // ========================================================================
    // VmResult 测试
    // ========================================================================

    #[test]
    fn test_vm_result_done() {
        let proto = make_proto(vec![], vec![]);
        let result = execute_test(&proto, 0, default_stack(10)).unwrap();
        assert!(matches!(result, VmResult::Return(0)));
    }

    // ========================================================================
    // 整数溢出测试 (ADD wrapping)
    // ========================================================================

    #[test]
    fn test_execute_add_overflow() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, -1),
            make_asbx(OpCode::LOADI, 1, 1),
            make_abc(OpCode::ADD, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_forprep_zero_step_error() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 1),
            make_asbx(OpCode::LOADI, 1, 5),
            make_asbx(OpCode::LOADI, 2, 0),
            make_abc(OpCode::FORPREP, 0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(20)).is_err());
    }
}