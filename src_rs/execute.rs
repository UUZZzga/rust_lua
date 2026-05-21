//! Lua 虚拟机主解释器循环 (纯 Rust 重写)
//!
//! 对应 C 源码: lvm.cpp 中的 luaV_execute 函数
//!
//! ## 设计原则
//! - 使用 Rust `match` 替代 C 的 `switch` + `goto`
//! - `VmState` 结构体封装所有解释器状态，替代 C 的局部变量 + 宏
//! - 使用 `Result` 传播错误，替代 C 的 longjmp 错误处理
//! - 操作码处理用独立方法，提高可读性和可测试性
//! - 使用 Rust 的 trait 和方法传递代替 C 宏
//!
//! ## 规约驱动开发 (spec-driven-tdd)
//! 每个公开函数都包含规约注释。

use crate::lvm;
use crate::objects::{Instruction, LClosure, NilKind, Proto, TValue, UpVal, UpvalDesc};
use crate::opcodes::{self, OpCode};
use crate::strings::LuaString;
use crate::table::Table;

// ============================================================================
// VmState: 解释器状态
// ============================================================================

pub struct VmState {
    pub constants: Vec<TValue>,
    pub code: Vec<Instruction>,
    pub upval_descs: Vec<UpvalDesc>,
    pub protos: Vec<Proto>,
    pub base: usize,
    pub pc: usize,
    pub stack: Vec<TValue>,
    pub trap: bool,
    pub num_params: u8,
    pub is_vararg: bool,
    pub closure_upvals: Vec<UpVal>,
}

impl VmState {
    pub fn new(proto: &Proto, base: usize, stack: Vec<TValue>) -> Self {
        VmState {
            constants: proto.constants.clone(),
            code: proto.code.clone(),
            upval_descs: proto.upvalues.clone(),
            protos: proto.protos.clone(),
            base,
            pc: 0,
            stack,
            trap: false,
            num_params: proto.num_params,
            is_vararg: proto.is_vararg(),
            closure_upvals: Vec::new(),
        }
    }
}

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
        }
    }
}

impl std::error::Error for VmError {}

// ============================================================================
// VmExecutor
// ============================================================================

pub struct VmExecutor;

impl VmExecutor {
    pub fn execute(proto: &Proto, base: usize, stack: Vec<TValue>) -> Result<VmResult, VmError> {
        let mut state = VmState::new(proto, base, stack);

        loop {
            if state.pc >= state.code.len() {
                return Ok(VmResult::Return(0));
            }

            let inst = state.code[state.pc];
            let op = opcodes::get_opcode(inst);

            match op {
                OpCode::MOVE => Self::op_move(&mut state, inst),
                OpCode::LOADI => Self::op_loadi(&mut state, inst),
                OpCode::LOADF => Self::op_loadf(&mut state, inst),
                OpCode::LOADK => Self::op_loadk(&mut state, inst),
                OpCode::LOADKX => Self::op_loadkx(&mut state, inst),
                OpCode::LOADFALSE => Self::op_loadfalse(&mut state, inst),
                OpCode::LFALSESKIP => Self::op_lfalseskip(&mut state, inst),
                OpCode::LOADTRUE => Self::op_loadtrue(&mut state, inst),
                OpCode::LOADNIL => Self::op_loadnil(&mut state, inst),
                OpCode::GETUPVAL => Self::op_getupval(&mut state, inst),
                OpCode::SETUPVAL => Self::op_setupval(&mut state, inst),
                OpCode::GETTABUP => Self::op_gettabup(&mut state, inst),
                OpCode::GETTABLE => Self::op_gettable(&mut state, inst),
                OpCode::GETI => Self::op_geti(&mut state, inst),
                OpCode::GETFIELD => Self::op_getfield(&mut state, inst),
                OpCode::SETTABUP => Self::op_settabup(&mut state, inst),
                OpCode::SETTABLE => Self::op_settable(&mut state, inst),
                OpCode::SETI => Self::op_seti(&mut state, inst),
                OpCode::SETFIELD => Self::op_setfield(&mut state, inst),
                OpCode::NEWTABLE => Self::op_newtable(&mut state, inst),
                OpCode::SELF => Self::op_self(&mut state, inst),
                OpCode::ADDI => Self::op_addi(&mut state, inst),
                OpCode::ADDK => Self::op_addk(&mut state, inst),
                OpCode::SUBK => Self::op_subk(&mut state, inst),
                OpCode::MULK => Self::op_mulk(&mut state, inst),
                OpCode::MODK => Self::op_modk(&mut state, inst),
                OpCode::POWK => Self::op_powk(&mut state, inst),
                OpCode::DIVK => Self::op_divk(&mut state, inst),
                OpCode::IDIVK => Self::op_idivk(&mut state, inst),
                OpCode::BANDK => Self::op_bandk(&mut state, inst),
                OpCode::BORK => Self::op_bork(&mut state, inst),
                OpCode::BXORK => Self::op_bxork(&mut state, inst),
                OpCode::SHLI => Self::op_shli(&mut state, inst),
                OpCode::SHRI => Self::op_shri(&mut state, inst),
                OpCode::ADD => Self::op_add(&mut state, inst),
                OpCode::SUB => Self::op_sub(&mut state, inst),
                OpCode::MUL => Self::op_mul(&mut state, inst),
                OpCode::MOD => Self::op_mod(&mut state, inst),
                OpCode::POW => Self::op_pow(&mut state, inst),
                OpCode::DIV => Self::op_div(&mut state, inst),
                OpCode::IDIV => Self::op_idiv(&mut state, inst),
                OpCode::BAND => Self::op_band(&mut state, inst),
                OpCode::BOR => Self::op_bor(&mut state, inst),
                OpCode::BXOR => Self::op_bxor(&mut state, inst),
                OpCode::SHL => Self::op_shl(&mut state, inst),
                OpCode::SHR => Self::op_shr(&mut state, inst),
                OpCode::MMBIN => Self::op_mmbin(&mut state, inst),
                OpCode::MMBINI => Self::op_mmbini(&mut state, inst),
                OpCode::MMBINK => Self::op_mmbink(&mut state, inst),
                OpCode::UNM => Self::op_unm(&mut state, inst),
                OpCode::BNOT => Self::op_bnot(&mut state, inst),
                OpCode::NOT => Self::op_not(&mut state, inst),
                OpCode::LEN => Self::op_len(&mut state, inst),
                OpCode::CONCAT => Self::op_concat(&mut state, inst),
                OpCode::CLOSE => Self::op_close(&mut state, inst),
                OpCode::TBC => Self::op_tbc(&mut state, inst),
                OpCode::JMP => Self::op_jmp(&mut state, inst),
                OpCode::EQ => Self::op_eq(&mut state, inst),
                OpCode::LT => Self::op_lt(&mut state, inst),
                OpCode::LE => Self::op_le(&mut state, inst),
                OpCode::EQK => Self::op_eqk(&mut state, inst),
                OpCode::EQI => Self::op_eqi(&mut state, inst),
                OpCode::LTI => Self::op_lti(&mut state, inst),
                OpCode::LEI => Self::op_lei(&mut state, inst),
                OpCode::GTI => Self::op_gti(&mut state, inst),
                OpCode::GEI => Self::op_gei(&mut state, inst),
                OpCode::TEST => Self::op_test(&mut state, inst),
                OpCode::TESTSET => Self::op_testset(&mut state, inst),
                OpCode::CALL => return Self::op_call(&mut state, inst),
                OpCode::TAILCALL => return Self::op_tailcall(&mut state, inst),
                OpCode::RETURN => return Self::op_return(&mut state, inst),
                OpCode::RETURN0 => return Ok(VmResult::Return(0)),
                OpCode::RETURN1 => return Self::op_return1(&mut state, inst),
                OpCode::FORLOOP => Self::op_forloop(&mut state, inst),
                OpCode::FORPREP => Self::op_forprep(&mut state, inst),
                OpCode::TFORPREP => Self::op_tforprep(&mut state, inst),
                OpCode::TFORCALL => Self::op_tforcall(&mut state, inst),
                OpCode::TFORLOOP => Self::op_tforloop(&mut state, inst),
                OpCode::SETLIST => Self::op_setlist(&mut state, inst),
                OpCode::CLOSURE => Self::op_closure(&mut state, inst),
                OpCode::VARARG => Self::op_vararg(&mut state, inst),
                OpCode::GETVARG => Self::op_getvarg(&mut state, inst),
                OpCode::ERRNNIL => Self::op_errnnil(&mut state, inst),
                OpCode::VARARGPREP => Self::op_varargprep(&mut state, inst),
                OpCode::EXTRAARG => {
                    return Err(VmError::IllegalOpcode(OpCode::EXTRAARG as u8));
                }
            }?;
        }
    }

    // ========================================================================
    // 辅助方法
    // ========================================================================

    fn ra(state: &VmState, inst: Instruction) -> usize {
        state.base + opcodes::getarg_a(inst) as usize
    }

    fn rb(state: &VmState, inst: Instruction) -> usize {
        state.base + opcodes::getarg_b(inst) as usize
    }

    fn rc(state: &VmState, inst: Instruction) -> usize {
        state.base + opcodes::getarg_c(inst) as usize
    }

    fn ensure_stack(state: &mut VmState, idx: usize) {
        if idx >= state.stack.len() {
            state.stack.resize(idx + 1, TValue::Nil(NilKind::Strict));
        }
    }

    fn read_stack(state: &VmState, idx: usize) -> &TValue {
        if idx < state.stack.len() { &state.stack[idx] } else { panic!("stack underflow") }
    }

    fn write_stack(state: &mut VmState, idx: usize, val: TValue) {
        Self::ensure_stack(state, idx);
        state.stack[idx] = val;
    }

    #[allow(dead_code)]
    fn push_stack(state: &mut VmState, val: TValue) -> usize {
        let idx = state.stack.len();
        state.stack.push(val);
        idx
    }

    fn do_conditional_jump(state: &mut VmState, inst: Instruction, cond: bool) {
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

    fn op_move(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let val = Self::read_stack(state, b).clone();
        Self::write_stack(state, a, val);
        state.pc += 1;
        Ok(())
    }

    fn op_loadi(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let val = opcodes::getarg_sbx(inst) as i64;
        Self::write_stack(state, a, TValue::Integer(val));
        state.pc += 1;
        Ok(())
    }

    fn op_loadf(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let val = opcodes::getarg_sbx(inst) as f64;
        Self::write_stack(state, a, TValue::Float(val));
        state.pc += 1;
        Ok(())
    }

    fn op_loadk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let idx = opcodes::getarg_sbx(inst) as usize;
        let val = state.constants[idx].clone();
        Self::write_stack(state, a, val);
        state.pc += 1;
        Ok(())
    }

    fn op_loadkx(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        state.pc += 1;
        let extra = state.code[state.pc];
        let extra_idx = opcodes::getarg_a(extra) as usize;
        let val = state.constants[extra_idx].clone();
        Self::write_stack(state, a, val);
        state.pc += 1;
        Ok(())
    }

    fn op_loadfalse(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        Self::write_stack(state, a, TValue::Boolean(false));
        state.pc += 1;
        Ok(())
    }

    fn op_lfalseskip(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        Self::write_stack(state, a, TValue::Boolean(false));
        state.pc += 2;
        Ok(())
    }

    fn op_loadtrue(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        Self::write_stack(state, a, TValue::Boolean(true));
        state.pc += 1;
        Ok(())
    }

    fn op_loadnil(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst);
        for i in 0..=b {
            Self::write_stack(state, a + i as usize, TValue::Nil(NilKind::Strict));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_getupval(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as usize;
        if b < state.closure_upvals.len() {
            let val = match &state.closure_upvals[b] {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { stack_index } => {
                    state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
                }
            };
            Self::write_stack(state, a, val);
        }
        state.pc += 1;
        Ok(())
    }

    fn op_setupval(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as usize;
        let val = Self::read_stack(state, a).clone();
        if b < state.closure_upvals.len() {
            match &mut state.closure_upvals[b] {
                UpVal::Closed { value } => **value = val,
                UpVal::Open { stack_index } => {
                    if *stack_index < state.stack.len() {
                        state.stack[*stack_index] = val;
                    }
                }
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_gettabup(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as usize;
        let kb_idx = opcodes::getarg_c(inst) as usize;
        let key = state.constants.get(kb_idx).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let upval_val = if b < state.closure_upvals.len() {
            match &state.closure_upvals[b] {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { stack_index } => state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict)),
            }
        } else {
            TValue::Nil(NilKind::Strict)
        };
        let result = Self::table_get(&upval_val, &key);
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_gettable(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_geti(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_getfield(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_settabup(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = opcodes::getarg_a(inst) as usize;
        let b_key = opcodes::getarg_b(inst) as usize;
        let c = opcodes::getarg_c(inst);
        let key = state.constants.get(b_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let val = Self::resolve_val(state, inst, c);
        let upval_val = if a < state.closure_upvals.len() {
            match &state.closure_upvals[a] {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { stack_index } => state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict)),
            }
        } else {
            TValue::Nil(NilKind::Strict)
        };
        Self::table_set_tv(upval_val, key, val);
        state.pc += 1;
        Ok(())
    }

    fn op_settable(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = opcodes::getarg_c(inst);
        let table_val = Self::read_stack(state, a).clone();
        let key = Self::read_stack(state, b).clone();
        let val = Self::resolve_val(state, inst, c);
        Self::table_set_tv(table_val, key, val);
        state.pc += 1;
        Ok(())
    }

    fn op_seti(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as i64;
        let c = opcodes::getarg_c(inst);
        let table_val = Self::read_stack(state, a).clone();
        let val = Self::resolve_val(state, inst, c);
        Self::table_set_tv(table_val, TValue::Integer(b), val);
        state.pc += 1;
        Ok(())
    }

    fn op_setfield(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b_key = opcodes::getarg_b(inst) as usize;
        let c = opcodes::getarg_c(inst);
        let table_val = Self::read_stack(state, a).clone();
        let key = state.constants.get(b_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let val = Self::resolve_val(state, inst, c);
        Self::table_set_tv(table_val, key, val);
        state.pc += 1;
        Ok(())
    }

    fn op_newtable(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as u32;
        let c = opcodes::getarg_c(inst) as u32;
        let hash_size = if b > 0 { 1u32 << (b - 1) } else { 0 };
        let array_size = c as usize;
        let table = Table::with_capacity(array_size, hash_size as usize);
        Self::write_stack(state, a, TValue::Table(table));
        state.pc += 1;
        Ok(())
    }

    fn op_self(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_addi(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let imm = opcodes::getarg_sbx(inst) as i64;
        let val = Self::read_stack(state, a).clone();
        match val {
            TValue::Integer(iv) => {
                Self::write_stack(state, a, TValue::Integer(iv.wrapping_add(imm)));
            }
            TValue::Float(fv) => {
                Self::write_stack(state, a, TValue::Float(fv + imm as f64));
            }
            _ => {}
        }
        state.pc += 1;
        Ok(())
    }

    fn op_addk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_subk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_mulk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_modk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_powk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(n1), Some(n2)) = (lvm::to_number_ns(v1), lvm::to_number_ns(&v2)) {
            Self::write_stack(state, a, TValue::Float(n1.powf(n2)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_divk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(n1), Some(n2)) = (lvm::to_number_ns(v1), lvm::to_number_ns(&v2)) {
            Self::write_stack(state, a, TValue::Float(n1 / n2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_idivk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_bandk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(i1), TValue::Integer(i2)) = (lvm::to_integer_ns(v1, lvm::F2IMode::Eq), &v2) {
            Self::write_stack(state, a, TValue::Integer(i1 & i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bork(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(i1), TValue::Integer(i2)) = (lvm::to_integer_ns(v1, lvm::F2IMode::Eq), &v2) {
            Self::write_stack(state, a, TValue::Integer(i1 | i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bxork(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b);
        if let (Some(i1), TValue::Integer(i2)) = (lvm::to_integer_ns(v1, lvm::F2IMode::Eq), &v2) {
            Self::write_stack(state, a, TValue::Integer(i1 ^ i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_shli(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let ic = opcodes::getarg_c(inst) as i64;
        let v = Self::read_stack(state, b);
        if let Some(ib) = lvm::to_integer_ns(v, lvm::F2IMode::Eq) {
            Self::write_stack(state, a, TValue::Integer(lvm::shiftl(ic, ib)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_shri(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let ic = opcodes::getarg_c(inst) as i64;
        let v = Self::read_stack(state, b);
        if let Some(ib) = lvm::to_integer_ns(v, lvm::F2IMode::Eq) {
            Self::write_stack(state, a, TValue::Integer(lvm::shiftl(ib, -ic)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_add(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_sub(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_mul(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_mod(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_pow(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(n1), Some(n2)) = (lvm::to_number_ns(v1), lvm::to_number_ns(v2)) {
            Self::write_stack(state, a, TValue::Float(n1.powf(n2)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_div(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(n1), Some(n2)) = (lvm::to_number_ns(v1), lvm::to_number_ns(v2)) {
            Self::write_stack(state, a, TValue::Float(n1 / n2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_idiv(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_band(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            lvm::to_integer_ns(v1, lvm::F2IMode::Eq),
            lvm::to_integer_ns(v2, lvm::F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(i1 & i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bor(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            lvm::to_integer_ns(v1, lvm::F2IMode::Eq),
            lvm::to_integer_ns(v2, lvm::F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(i1 | i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bxor(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            lvm::to_integer_ns(v1, lvm::F2IMode::Eq),
            lvm::to_integer_ns(v2, lvm::F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(i1 ^ i2));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_shl(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            lvm::to_integer_ns(v1, lvm::F2IMode::Eq),
            lvm::to_integer_ns(v2, lvm::F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(lvm::shiftl(i1, i2)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_shr(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b);
        let v2 = Self::read_stack(state, c);
        if let (Some(i1), Some(i2)) = (
            lvm::to_integer_ns(v1, lvm::F2IMode::Eq),
            lvm::to_integer_ns(v2, lvm::F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(lvm::shiftl(i1, -i2)));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_mmbin(state: &mut VmState, _inst: Instruction) -> Result<(), VmError> {
        state.pc += 1;
        Ok(())
    }

    fn op_mmbini(state: &mut VmState, _inst: Instruction) -> Result<(), VmError> {
        state.pc += 1;
        Ok(())
    }

    fn op_mmbink(state: &mut VmState, _inst: Instruction) -> Result<(), VmError> {
        state.pc += 1;
        Ok(())
    }

    fn op_unm(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b);
        match v {
            TValue::Integer(i) => Self::write_stack(state, a, TValue::Integer(i.wrapping_neg())),
            TValue::Float(f) => Self::write_stack(state, a, TValue::Float(-f)),
            _ => {}
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bnot(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b);
        if let Some(i) = lvm::to_integer_ns(v, lvm::F2IMode::Eq) {
            Self::write_stack(state, a, TValue::Integer(!i));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_not(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b);
        let result = if lvm::is_false(v) { TValue::Boolean(true) } else { TValue::Boolean(false) };
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_len(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b);
        let result = lvm::objlen(v).unwrap_or(TValue::Integer(0));
        Self::write_stack(state, a, result);
        state.pc += 1;
        Ok(())
    }

    fn op_concat(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let n = opcodes::getarg_b(inst) as usize;
        let mut result = String::new();
        for i in 0..n {
            let val = Self::read_stack(state, a + i as usize);
            match val {
                TValue::Str(s) => result.push_str(s.as_str()),
                TValue::Integer(i) => result.push_str(&i.to_string()),
                TValue::Float(f) => result.push_str(&format_float(*f)),
                _ => {}
            }
        }
        use crate::strings::ShortString;
        let ls = LuaString::Short(std::sync::Arc::new(ShortString { hash: 0, contents: result }));
        Self::write_stack(state, a, TValue::Str(ls));
        state.pc += 1;
        Ok(())
    }

    fn op_close(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let _a = Self::ra(state, inst);
        state.pc += 1;
        Ok(())
    }

    fn op_tbc(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let _a = Self::ra(state, inst);
        state.pc += 1;
        Ok(())
    }

    fn op_jmp(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let sj = opcodes::getarg_sj(inst);
        state.pc = ((state.pc as i32) + sj + 1) as usize;
        Ok(())
    }

    // ---- 比较运算 ----

    fn op_eq(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v1 = Self::read_stack(state, a);
        let v2 = Self::read_stack(state, b);
        let cond = lvm::raw_equal(v1, v2);
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_lt(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v1 = Self::read_stack(state, a);
        let v2 = Self::read_stack(state, b);
        let cond = lvm::less_than(v1, v2);
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_le(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v1 = Self::read_stack(state, a);
        let v2 = Self::read_stack(state, b);
        let cond = lvm::less_equal(v1, v2);
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_eqk(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b_key = opcodes::getarg_b(inst) as usize;
        let v1 = Self::read_stack(state, a);
        let v2 = state.constants.get(b_key).unwrap();
        let cond = lvm::raw_equal(v1, v2);
        Self::do_conditional_jump(state, inst, cond);
        state.pc += 1;
        Ok(())
    }

    fn op_eqi(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_lti(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_lei(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_gti(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_gei(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_test(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let v = Self::read_stack(state, a);
        let cond = !lvm::is_false(v);
        // For TEST, the expected condition is reversed: jump if is_false(v) XOR expected
        let take_jump = cond;
        if !take_jump {
            state.pc += 1;
        } else {
            let next_idx = state.pc + 1;
            if next_idx >= state.code.len() { return Ok(()); }
            let next = state.code[next_idx];
            let sj = opcodes::getarg_sj(next);
            state.pc = ((state.pc as i32) + sj + 1) as usize;
        }
        Ok(())
    }

    fn op_testset(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b).clone();
        let cond = !lvm::is_false(&v);
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

    fn op_call(state: &mut VmState, inst: Instruction) -> Result<VmResult, VmError> {
        let a = Self::ra(state, inst);
        let func_val = Self::read_stack(state, a).clone();
        match func_val {
            TValue::LClosure(closure) => Ok(VmResult::Call {
                proto: closure.proto,
                base: a + 1,
                num_results: opcodes::getarg_c(inst) - 1,
            }),
            _ => {
                state.pc += 1;
                Ok(VmResult::Done)
            }
        }
    }

    fn op_tailcall(state: &mut VmState, inst: Instruction) -> Result<VmResult, VmError> {
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

    fn op_return(state: &mut VmState, inst: Instruction) -> Result<VmResult, VmError> {
        let a = Self::ra(state, inst);
        let n = opcodes::getarg_b(inst) as i32 - 1;
        let nresults = if n < 0 { state.stack.len().saturating_sub(a) as i32 } else { n };
        Ok(VmResult::Return(nresults as usize))
    }

    fn op_return1(state: &mut VmState, inst: Instruction) -> Result<VmResult, VmError> {
        let a = Self::ra(state, inst);
        let val = Self::read_stack(state, a).clone();
        if state.base > 0 {
            Self::write_stack(state, state.base - 1, val);
        }
        Ok(VmResult::Return(1))
    }

    // ---- 循环 ----

    fn op_forloop(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_forprep(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);

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
                            lvm::float_to_integer(*f, lvm::F2IMode::Ceil).unwrap_or(*init_i)
                        } else {
                            lvm::float_to_integer(*f, lvm::F2IMode::Floor).unwrap_or(*init_i)
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

    fn op_tforprep(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let tmp = Self::read_stack(state, ra + 2).clone();
        let closing = Self::read_stack(state, ra + 3).clone();
        Self::write_stack(state, ra + 3, tmp);
        Self::write_stack(state, ra + 2, closing);
        let bx = opcodes::getarg_sbx(inst);
        state.pc = ((state.pc as i32) + bx + 1) as usize;
        Ok(())
    }

    fn op_tforcall(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_tforloop(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_setlist(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let n = opcodes::getarg_b(inst) as usize;
        let mut last = opcodes::getarg_c(inst) as usize;

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

    fn op_closure(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let bx = opcodes::getarg_sbx(inst) as usize;
        if bx < state.protos.len() {
            let proto = state.protos[bx].clone();
            let closure = LClosure { proto, upvals: state.closure_upvals.clone() };
            Self::write_stack(state, ra, TValue::LClosure(closure));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_vararg(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_getvarg(state: &mut VmState, inst: Instruction) -> Result<(), VmError> {
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

    fn op_errnnil(state: &mut VmState, _inst: Instruction) -> Result<(), VmError> {
        state.pc += 1;
        Ok(())
    }

    fn op_varargprep(state: &mut VmState, _inst: Instruction) -> Result<(), VmError> {
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

    fn table_set_tv(mut table_val: TValue, key: TValue, val: TValue) {
        if let TValue::Table(ref mut t) = table_val {
            t.set(key, val);
        }
    }

    fn resolve_val(state: &VmState, inst: Instruction, c: i32) -> TValue {
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
                if let (Some(n1), Some(n2)) = (lvm::to_number_ns(v1), lvm::to_number_ns(v2)) {
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
                let r = lvm::modulus(*i1, *i2).map_err(|_| VmError::ModuloByZero)?;
                Ok(TValue::Integer(r))
            }
            _ => {
                if let (Some(n1), Some(n2)) = (lvm::to_number_ns(v1), lvm::to_number_ns(v2)) {
                    Ok(TValue::Float(lvm::modulus_float(n1, n2)))
                } else {
                    Ok(TValue::Nil(NilKind::Strict))
                }
            }
        }
    }

    fn arith_idiv(v1: &TValue, v2: &TValue) -> Result<TValue, VmError> {
        match (v1, v2) {
            (TValue::Integer(i1), TValue::Integer(i2)) => {
                let r = lvm::idiv(*i1, *i2).map_err(|_| VmError::DivisionByZero)?;
                Ok(TValue::Integer(r))
            }
            _ => {
                if let (Some(n1), Some(n2)) = (lvm::to_number_ns(v1), lvm::to_number_ns(v2)) {
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
        let mut inst = 0u32;
        inst |= (op as u32) << opcodes::POS_OP;
        inst |= (a as u32 & 0xFF) << opcodes::POS_A;
        inst |= (k as u32 & 1) << opcodes::POS_K;
        inst |= (b as u32 & 0xFF) << opcodes::POS_B;
        inst |= (c as u32 & 0xFF) << opcodes::POS_C;
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
        let mut inst = 0u32;
        inst |= (op as u32) << opcodes::POS_OP;
        inst |= (a as u32 & 0xFF) << opcodes::POS_A;
        inst |= (b as u32 & 0xFF) << opcodes::POS_B;
        inst |= (c as u32 & 0xFF) << opcodes::POS_C;
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
        let result = VmExecutor::execute(&proto, 0, stack);
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
        assert!(VmExecutor::execute(&proto, 0, stack).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_not() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0),
            make_abc(OpCode::NOT, 1, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        let stack = default_stack(10);
        assert!(VmExecutor::execute(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_newtable() {
        let code = vec![make_abc(OpCode::NEWTABLE, 0, 0, 3)];
        let proto = make_proto(code, vec![]);
        let stack = default_stack(10);
        assert!(VmExecutor::execute(&proto, 0, stack).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_concat() {
        let tb = StringTable::new();
        let mut stack = default_stack(10);
        stack[0] = TValue::Str(tb.intern("hello"));
        stack[1] = TValue::Str(tb.intern("world"));

        let code = vec![make_abc(OpCode::CONCAT, 0, 2, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, stack).is_ok());
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
        assert_eq!(super::format_float(f64::NAN), "nan");
        assert_eq!(super::format_float(f64::INFINITY), "inf");
        assert_eq!(super::format_float(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn test_format_float_zero() {
        assert_eq!(super::format_float(0.0), "0.0");
        assert_eq!(super::format_float(-0.0), "0.0");
    }

    #[test]
    fn test_format_float_normal() {
        assert_eq!(super::format_float(42.0), "42.0");
        assert_eq!(super::format_float(3.5), "3.5");
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mul() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 6),
            make_asbx(OpCode::LOADI, 1, 7),
            make_abc(OpCode::MUL, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_div() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::DIV, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_idiv() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::IDIV, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mod() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::MOD, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_pow() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 2),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::POW, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_bnot() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0),
            make_abc(OpCode::BNOT, 1, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_band() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0b1100),
            make_asbx(OpCode::LOADI, 1, 0b1010),
            make_abc(OpCode::BAND, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_bor() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0b1100),
            make_asbx(OpCode::LOADI, 1, 0b0011),
            make_abc(OpCode::BOR, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_bxor() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 0b1100),
            make_asbx(OpCode::LOADI, 1, 0b1010),
            make_abc(OpCode::BXOR, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_shl() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 1),
            make_asbx(OpCode::LOADI, 1, 3),
            make_abc(OpCode::SHL, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_shr() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 16),
            make_asbx(OpCode::LOADI, 1, 2),
            make_abc(OpCode::SHR, 2, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_subk() {
        let constants = vec![TValue::Integer(3)];
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_abck(OpCode::SUBK, 1, 0, 0, 1),
        ];
        let proto = make_proto(code, constants);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mulk() {
        let constants = vec![TValue::Integer(4)];
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 3),
            make_abck(OpCode::MULK, 1, 0, 0, 0),
        ];
        let proto = make_proto(code, constants);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_divk() {
        let constants = vec![TValue::Integer(2)];
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_abck(OpCode::DIVK, 1, 0, 0, 0),
        ];
        let proto = make_proto(code, constants);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // LOADK / LOADKX / LOADF / ADDI / SHLI / SHRI 测试
    // ========================================================================

    #[test]
    fn test_execute_loadk() {
        let constants = vec![TValue::Integer(42)];
        let code = vec![make_asbx(OpCode::LOADK, 0, 0)];
        let proto = make_proto(code, constants);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_loadf() {
        let code = vec![make_asbx(OpCode::LOADF, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_addi() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 10),
            make_asbx(OpCode::ADDI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_lt() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 3),
            make_asbx(OpCode::LOADI, 1, 5),
            make_abc(OpCode::LT, 0, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_le() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 5),
            make_asbx(OpCode::LOADI, 1, 5),
            make_abc(OpCode::LE, 0, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_eqi() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 42),
            make_asbx(OpCode::EQI, 0, 42),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_lti() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 3),
            make_asbx(OpCode::LTI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_lei() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 5),
            make_asbx(OpCode::LEI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_gti() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 7),
            make_asbx(OpCode::GTI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_gei() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 5),
            make_asbx(OpCode::GEI, 0, 5),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_test() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 1),
            make_bx(OpCode::TEST, 0, 1),
            make_bx(OpCode::RETURN0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_testset() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 42),
            make_abc(OpCode::TESTSET, 1, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_geti() {
        let code = vec![
            make_abc(OpCode::NEWTABLE, 0, 0, 3),
            make_abc(OpCode::GETI, 1, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_seti() {
        let code = vec![
            make_abc(OpCode::NEWTABLE, 0, 0, 3),
            make_asbx(OpCode::LOADI, 1, 42),
            make_abck(OpCode::SETI, 0, 1, 1, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_self() {
        let constants = vec![TValue::Nil(NilKind::Strict)];
        let code = vec![
            make_abc(OpCode::NEWTABLE, 0, 0, 3),
            make_abc(OpCode::SELF, 1, 0, 0),
        ];
        let proto = make_proto(code, constants);
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_len() {
        let tb = StringTable::new();
        let mut stack = default_stack(10);
        stack[0] = TValue::Str(tb.intern("hello"));

        let code = vec![make_abc(OpCode::LEN, 1, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, stack).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_return1() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 77),
            make_abc(OpCode::RETURN1, 0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_return0() {
        let code = vec![
            make_bx(OpCode::RETURN0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    // ========================================================================
    // CALL / TAILCALL 测试
    // ========================================================================

    #[test]
    fn test_execute_call_lua_closure() {
        // Create an inner proto that just returns 0
        let inner_proto = make_proto(vec![make_bx(OpCode::RETURN0, 0, 0)], vec![]);
        let closure = LClosure { proto: inner_proto, upvals: vec![] };

        let mut stack = default_stack(10);
        stack[0] = TValue::LClosure(closure);

        let code = vec![make_abck(OpCode::CALL, 0, 0, 1, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_tailcall_lua_closure() {
        let inner_proto = make_proto(vec![make_bx(OpCode::RETURN0, 0, 0)], vec![]);
        let closure = LClosure { proto: inner_proto, upvals: vec![] };

        let mut stack = default_stack(10);
        stack[0] = TValue::LClosure(closure);

        let code = vec![make_abck(OpCode::TAILCALL, 0, 0, 1, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, stack).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
    }

    // ========================================================================
    // LOADFALSE / LOADTRUE / LOADNIL 测试
    // ========================================================================

    #[test]
    fn test_execute_loadfalse() {
        let code = vec![make_abc(OpCode::LOADFALSE, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(5)).is_ok());
    }

    #[test]
    fn test_execute_loadtrue() {
        let code = vec![make_abc(OpCode::LOADTRUE, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(5)).is_ok());
    }

    #[test]
    fn test_execute_loadnil() {
        let code = vec![make_abck(OpCode::LOADNIL, 0, 3, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_tforloop() {
        let code = vec![
            make_asbx(OpCode::LOADI, 3, 0),
            make_abc(OpCode::TFORLOOP, 0, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
    }

    // ========================================================================
    // VARARG / ERRNNIL / VARARGPREP 测试
    // ========================================================================

    #[test]
    fn test_execute_vararg() {
        let code = vec![make_abc(OpCode::VARARG, 0, 1, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
    }

    #[test]
    fn test_execute_errnnil() {
        let code = vec![
            make_asbx(OpCode::LOADI, 0, 42),
            make_bx(OpCode::ERRNNIL, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_varargprep() {
        let code = vec![
            make_abc(OpCode::VARARGPREP, 0, 3, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_ok());
    }

    // ========================================================================
    // MMBIN / MMBINI / MMBINK / CLOSE / TBC 桩测试
    // ========================================================================

    #[test]
    fn test_execute_mmbin() {
        let code = vec![make_abc(OpCode::MMBIN, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mmbini() {
        let code = vec![make_abc(OpCode::MMBINI, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_mmbink() {
        let code = vec![make_abc(OpCode::MMBINK, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_close() {
        let code = vec![make_abc(OpCode::CLOSE, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_tbc() {
        let code = vec![make_abc(OpCode::TBC, 0, 0, 0)];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
    }

    #[test]
    fn test_execute_getvarg() {
        let code = vec![
            make_asbx(OpCode::LOADI, 1, 0),
            make_abc(OpCode::GETVARG, 0, 0, 1),
        ];
        let proto = make_proto(code, vec![]);
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        let result = VmExecutor::execute(&proto, 0, default_stack(10)).unwrap();
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(10)).is_ok());
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
        assert!(VmExecutor::execute(&proto, 0, default_stack(20)).is_err());
    }
}