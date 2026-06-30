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

use crate::objects::{CallFrame, Instruction, LClosure, NilKind, Proto, TValue, UpVal, UpValRef, LuaType, PF_VAHID, PF_VATAB};
use crate::opcodes::{self, OpCode};
use crate::table::Table;
use crate::tm::{
    TagMethod,
    try_bin_tm, try_bin_assoc_tm, try_bini_tm, try_concat_tm,
    call_order_tm, equal_obj, obj_len,
};
use crate::vm::{to_number_ns, to_integer_ns, F2IMode, shiftl, is_false,
    concat_stack, raw_equal, float_to_integer,
    modulus, modulus_float, idiv};
use crate::state::{LuaState, LUA_MINSTACK, MAX_CALL_CHAIN, LUAI_MAXCCALLS};
use crate::gc::GCState;
use std::rc::Rc;
use std::cell::RefCell;
use std::ffi::c_void;

// ============================================================================
// VmResult / VmError
// ============================================================================

#[derive(Debug)]
pub enum VmResult {
    Return { nresults: usize, result_base: usize },
    TailCall { proto: Proto, base: usize },
    Call { proto: Proto, base: usize, num_results: i32 },
    /// 协程 yield — 携带 yield 的值
    Yield { values: Vec<TValue> },
    Done,
}

#[derive(Debug)]
pub enum VmError {
    DivisionByZero,
    ModuloByZero,
    TypeError(String),
    StackOverflow,
    StackError,
    IllegalOpcode(u8),
    RuntimeError(String),
    /// 非字符串错误值 — error() 传入非字符串参数时使用，保留原始 TValue
    /// 对应 C Lua 中 errfunc 为非字符串时的行为
    RuntimeErrorValue(TValue),
    MetaMethodNotImplemented(String),
    /// 协程 yield 信号 — 携带 yield 的值（非真实错误，由 execute_loop 转换为 VmResult::Yield）
    Yield(Vec<TValue>),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::DivisionByZero => write!(f, "attempt to divide by zero"),
            VmError::ModuloByZero => write!(f, "attempt to perform 'n%0'"),
            VmError::TypeError(msg) => write!(f, "type error: {}", msg),
            VmError::StackOverflow => write!(f, "stack overflow"),
            VmError::StackError => write!(f, "stack error"),
            VmError::IllegalOpcode(op) => write!(f, "illegal opcode: {}", op),
            VmError::RuntimeError(msg) => write!(f, "runtime error: {}", msg),
            VmError::RuntimeErrorValue(_) => write!(f, "runtime error (non-string value)"),
            VmError::MetaMethodNotImplemented(name) => write!(f, "metamethod '{}' not implemented", name),
            VmError::Yield(_) => write!(f, "yield"),
        }
    }
}

impl std::error::Error for VmError {}

// CallFrame 已移至 objects.rs（pub）— 协程上下文需要保存调用栈

// ============================================================================
// 辅助函数 — 用于堆栈回溯
// ============================================================================

/// 对应 C 的 luaO_chunkid：将源名转换为短源名（从字节切片）
fn short_source_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "?".to_string();
    }
    match bytes[0] {
        b'=' => String::from_utf8_lossy(&bytes[1..]).into_owned(),
        b'@' => String::from_utf8_lossy(&bytes[1..]).into_owned(),
        _ => {
            let end = bytes
                .iter()
                .position(|&b| b == b'\n')
                .unwrap_or(bytes.len())
                .min(40);
            let head = String::from_utf8_lossy(&bytes[..end]);
            if bytes.len() > 40 || bytes.iter().any(|&b| b == b'\n') {
                format!("[string \"{}...\"]", head)
            } else {
                format!("[string \"{}\"]", head)
            }
        }
    }
}

/// 对应 C 的 luaO_chunkid：将源名转换为短源名
fn short_source(source: &crate::strings::LuaString) -> String {
    short_source_bytes(source.as_str().as_bytes())
}

/// 格式化函数名 — 对应 C 的 pushfuncname
///
/// 逻辑:
/// 1. 若 namewhat 非空: `"{namewhat} '{name}'"` (如 "global 'foo'", "method 'bar'")
/// 2. 若是 main chunk: "main chunk"
/// 3. 若有闭包信息 (Lua 函数): "function '<src>:<linedefined>'"
/// 4. 否则: "?"
fn format_func_name(namewhat: &str, name: &str, is_main: bool, closure: Option<&LClosure>) -> String {
    if !namewhat.is_empty() {
        format!("{} '{}'", namewhat, name)
    } else if is_main {
        "main chunk".to_string()
    } else if let Some(c) = closure {
        let src = c.proto.source.as_ref()
            .map(|s| short_source(s))
            .unwrap_or_else(|| "?".to_string());
        format!("function <{}:{}>", src, c.proto.line_defined)
    } else {
        "?".to_string()
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

/// 从调用指令的上下文获取函数名和 namewhat
/// 对应 C 的 funcnamefromcode
///
/// 逻辑:
/// 1. 获取 OP_CALL 指令的操作数 A（函数寄存器）
/// 2. 调用 get_obj_name 查找寄存器 A 的值是如何被设置的
fn get_func_name(state: &LuaState, call_pc: usize) -> (String, String) {
    if call_pc >= state.code.len() {
        return (String::new(), String::new());
    }
    // 获取调用者的 proto
    if state.base == 0 || state.base > state.stack.len() {
        return (String::new(), String::new());
    }
    let proto = match &state.stack[state.base - 1] {
        TValue::LClosure(c) => &c.proto,
        _ => return (String::new(), String::new()),
    };

    // 获取 OP_CALL 指令的操作数 A（函数寄存器）
    let call_inst = state.code[call_pc];
    let func_reg = opcodes::getarg_a(call_inst) as usize;

    get_obj_name(proto, call_pc, func_reg)
}

/// 查找设置指定寄存器的指令
/// 对应 C 的 findsetreg
fn find_set_reg(proto: &Proto, lastpc: usize, reg: usize) -> i32 {
    let mut setreg: i32 = -1;
    if lastpc == 0 || lastpc > proto.code.len() {
        return -1;
    }
    // C: if (testMMMode(GET_OPCODE(p->code[lastpc]))) lastpc--;
    // MMBIN/MMBINI/MMBINK 前面的算术指令在 to_integer 失败时未实际执行，
    // 需要跳过它以找到真正设置该寄存器的指令。
    let mut lastpc = lastpc;
    if opcodes::test_mm_mode(opcodes::get_opcode(proto.code[lastpc])) {
        if lastpc == 0 { return -1; }
        lastpc -= 1;
    }
    let mut jmptarget: i32 = 0;  // 0 之前的代码是无条件的
    for pc in 0..lastpc {
        let inst = proto.code[pc];
        let op = opcodes::get_opcode(inst);
        let a = opcodes::getarg_a(inst) as usize;
        let change = match op {
            OpCode::LOADNIL => {
                let b = opcodes::getarg_b(inst) as usize;
                a <= reg && reg <= a + b
            }
            OpCode::TFORCALL => reg >= a + 2,
            OpCode::CALL | OpCode::TAILCALL => reg >= a,
            OpCode::JMP => {
                let b = opcodes::getarg_sj(inst);
                let dest = (pc as i32) + 1 + b;
                if dest <= lastpc as i32 && dest > jmptarget {
                    jmptarget = dest;
                }
                false
            }
            _ => opcodes::test_a_mode(op) && reg == a,
        };
        if change {
            setreg = filter_pc(pc as i32, jmptarget);
        }
    }
    setreg
}

/// 对应 C 的 filterpc：如果 pc 在 jmptarget 之前（条件代码内），返回 -1
fn filter_pc(pc: i32, jmptarget: i32) -> i32 {
    if pc < jmptarget { -1 } else { pc }
}

/// 查找寄存器对应的局部变量名
/// 对应 C 的 luaF_getlocalname
fn get_local_name_at(proto: &Proto, reg: usize, pc: usize) -> Option<String> {
    // reg 是 0-based 寄存器编号，local_number 是 1-based
    let local_number = reg + 1;
    let mut count = local_number;
    for loc_var in &proto.loc_vars {
        if (loc_var.start_pc as usize) > pc {
            break;
        }
        if (loc_var.start_pc as usize) <= pc && pc < (loc_var.end_pc as usize) {
            count -= 1;
            if count == 0 {
                if let Some(ref name) = loc_var.varname {
                    return Some(name.as_str().to_string());
                }
            }
        }
    }
    None
}

/// 获取寄存器值的 name 和 namewhat
/// 对应 C 的 getobjname + basicgetobjname
fn get_obj_name(proto: &Proto, pc: usize, reg: usize) -> (String, String) {
    // 先查找局部变量
    if let Some(name) = get_local_name_at(proto, reg, pc) {
        return (name, "local".to_string());
    }

    // 查找设置 reg 的指令
    let set_pc = find_set_reg(proto, pc, reg);
    if set_pc < 0 || set_pc as usize >= proto.code.len() {
        return (String::new(), String::new());
    }
    let set_pc = set_pc as usize;
    let set_inst = proto.code[set_pc];
    let set_op = opcodes::get_opcode(set_inst);

    match set_op {
        OpCode::MOVE => {
            // MOVE A B: R[A] := R[B]
            let b = opcodes::getarg_b(set_inst) as usize;
            if b < reg {
                // 递归查找 B 的 name
                return get_obj_name(proto, set_pc, b);
            }
            (String::new(), String::new())
        }
        OpCode::GETUPVAL => {
            // GETUPVAL A B: R[A] := Upval[B]
            let b = opcodes::getarg_b(set_inst) as usize;
            if b < proto.upvalues.len() {
                if let Some(ref name) = proto.upvalues[b].name {
                    return (name.as_str().to_string(), "upvalue".to_string());
                }
            }
            (String::new(), String::new())
        }
        OpCode::GETTABUP => {
            // GETTABUP A B C: R[A] := Upval[B][K[C]]
            let c = opcodes::getarg_c(set_inst) as usize;
            let b = opcodes::getarg_b(set_inst) as usize;
            let name = if c < proto.constants.len() {
                match &proto.constants[c] {
                    TValue::Str(s) => s.as_str().to_string(),
                    _ => "?".to_string(),
                }
            } else {
                "?".to_string()
            };
            // 检查上值是否是 _ENV
            let is_env = b < proto.upvalues.len()
                && proto.upvalues[b].name.as_ref().map(|s| s.as_str()) == Some("_ENV");
            let namewhat = if is_env { "global" } else { "field" };
            (name, namewhat.to_string())
        }
        OpCode::GETFIELD => {
            // GETFIELD A B C: R[A] := R[B][K[C]]
            let c = opcodes::getarg_c(set_inst) as usize;
            let b = opcodes::getarg_b(set_inst) as usize;
            let name = if c < proto.constants.len() {
                match &proto.constants[c] {
                    TValue::Str(s) => s.as_str().to_string(),
                    _ => "?".to_string(),
                }
            } else {
                "?".to_string()
            };
            // 检查表寄存器 B 是否是 _ENV
            let is_env = is_env_register(proto, set_pc, b);
            let namewhat = if is_env { "global" } else { "field" };
            (name, namewhat.to_string())
        }
        OpCode::GETTABLE => {
            // GETTABLE A B C: R[A] := R[B][R[C]]
            let b = opcodes::getarg_b(set_inst) as usize;
            // 检查表寄存器 B 是否是 _ENV
            let is_env = is_env_register(proto, set_pc, b);
            let namewhat = if is_env { "global" } else { "field" };
            ("?".to_string(), namewhat.to_string())
        }
        OpCode::GETI => {
            // GETI A B C: R[A] := R[B][R[C]]
            ("integer index".to_string(), "field".to_string())
        }
        OpCode::SELF => {
            // SELF A B C: A+1 := B; A := B[K[C]]
            let c = opcodes::getarg_c(set_inst) as usize;
            let name = if c < proto.constants.len() {
                match &proto.constants[c] {
                    TValue::Str(s) => s.as_str().to_string(),
                    _ => "?".to_string(),
                }
            } else {
                "?".to_string()
            };
            (name, "method".to_string())
        }
        _ => (String::new(), String::new()),
    }
}

/// 检查寄存器 reg 是否是 _ENV（局部变量或上值）
/// 对应 C 的 isEnv
fn is_env_register(proto: &Proto, pc: usize, reg: usize) -> bool {
    // 先查找局部变量
    if let Some(name) = get_local_name_at(proto, reg, pc) {
        return name == "_ENV";
    }
    // 查找设置 reg 的指令
    let set_pc = find_set_reg(proto, pc, reg);
    if set_pc < 0 || set_pc as usize >= proto.code.len() {
        return false;
    }
    let set_inst = proto.code[set_pc as usize];
    let set_op = opcodes::get_opcode(set_inst);
    match set_op {
        OpCode::GETUPVAL => {
            let b = opcodes::getarg_b(set_inst) as usize;
            b < proto.upvalues.len()
                && proto.upvalues[b].name.as_ref().map(|s| s.as_str()) == Some("_ENV")
        }
        _ => false,
    }
}

/// 构造寄存器值的变量信息字符串 — 对应 C 的 varinfo
///
/// C 实现:
/// ```c
/// static const char *varinfo (lua_State *L, const TValue *o) {
///   CallInfo *ci = L->ci;
///   const char *name = NULL;
///   const char *kind = NULL;
///   if (isLua(ci)) {
///     kind = getupvalname(ci, o, &name);
///     if (!kind) {
///       int reg = instack(ci, o);
///       if (reg >= 0)
///         kind = getobjname(ci_func(ci)->p, currentpc(ci), reg, &name);
///     }
///   }
///   return formatvarinfo(L, kind, name);
/// }
/// ```
///
/// Rust 版本直接接收寄存器编号（调用方已知），通过 get_obj_name 获取
/// kind 和 name，返回 " (kind 'name')" 或空字符串。
fn varinfo_str(state: &LuaState, reg: usize) -> String {
    if state.base == 0 || state.base > state.stack.len() {
        return String::new();
    }
    let proto = match &state.stack[state.base - 1] {
        TValue::LClosure(c) => &c.proto,
        _ => return String::new(),
    };
    let (name, kind) = get_obj_name(proto, state.pc, reg);
    if kind.is_empty() {
        String::new()
    } else {
        format!(" ({} '{}')", kind, name)
    }
}

/// 检查错误消息是否已包含 source:line 前缀
/// 用于避免 error()/assert() 等已添加前缀的错误消息被重复添加
/// 匹配模式: "source:line: " 其中 line 是数字
fn has_source_line_prefix(msg: &str) -> bool {
    let bytes = msg.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            // 找到 ":数字"，继续查找数字后的 ": "
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' && j + 1 < bytes.len() && bytes[j + 1] == b' ' {
                return true;
            }
        }
        i += 1;
    }
    false
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
        // call_stack 已提升为 state.call_stack 字段，以支持协程挂起/恢复

        // 调试跟踪：通过环境变量 LUA_VM_TRACE=1 启用
        // LUA_VM_TRACE=2 时额外打印完整栈内容
        let trace_level: u8 = std::env::var("LUA_VM_TRACE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        loop {
            if state.pc >= state.code.len() {
                if let Some(frame) = state.call_stack.pop() {
                    // 同时弹出调用栈信息
                    state.call_info.pop();
                    // 递减 C 调用深度 (对应 op_call 中递增的 n_ccalls)
                    state.n_ccalls = state.n_ccalls.saturating_sub(1);
                    state.code = frame.code;
                    state.constants = frame.constants;
                    state.upval_descs = frame.upval_descs;
                    state.protos = frame.protos;
                    state.base = frame.base;
                    state.pc = frame.return_pc;
                    state.num_params = frame.num_params;
                    state.is_vararg = frame.is_vararg;
                    state.proto_flag = frame.proto_flag;
                    state.nextraargs = frame.nextraargs;
                    state.closure_upvals = frame.closure_upvals;
                    state.tbc_list = frame.tbc_list;
                    state.open_upval = frame.open_upval;
                    // 对应 C 的 rethook: L->oldpc = pcRel(ci->u.l.savedpc, ci_func(ci)->p)
                    // 函数返回时，设置 oldpc 为调用者的 return_pc
                    state.hook_old_pc = state.pc as i32;
                    continue;
                }
                return Ok(VmResult::Return { nresults: 0, result_base: state.base });
            }

            let inst = state.code[state.pc];
            let op = opcodes::get_opcode(inst);

            // 检查 count hook 和 line hook — 对应 C 的 luaG_traceexec
            // VARARGPREP 不触发 hook（对应 C 的 luaG_tracecall 对 vararg 函数返回 0）
            if state.hook_mask & (4 | 8) != 0 && op != OpCode::VARARGPREP { // LUA_MASKLINE=4 | LUA_MASKCOUNT=8
                if state.hook_func.is_some() {
                    // count hook — 对应 C: counthook = (mask & LUA_MASKCOUNT) && (--L->hookcount == 0)
                    let mut counthook = false;
                    if state.hook_mask & 8 != 0 { // LUA_MASKCOUNT
                        state.current_hook_count -= 1;
                        if state.current_hook_count == 0 {
                            counthook = true;
                            state.current_hook_count = state.hook_count; // resethookcount
                        }
                    }
                    // count hook 先触发 — 对应 C: if (counthook) luaD_hook(L, LUA_HOOKCOUNT, -1, 0, 0)
                    if counthook {
                        if state.allowhook {
                            if let Some(last_entry) = state.call_info.last_mut() {
                                last_entry.saved_pc = state.pc;
                            }
                        }
                        Self::call_hook(state, "count", -1, None, 0, 0)?;
                    }
                    // line hook — 对应 C: if (mask & LUA_MASKLINE)
                    if state.hook_mask & 4 != 0 { // LUA_MASKLINE = 4
                        let new_pc = state.pc as i32;
                        // 对应 C: int oldpc = (L->oldpc < p->sizecode) ? L->oldpc : 0;
                        let code_len = state.code.len() as i32;
                        let old_pc = if state.hook_old_pc < code_len {
                            state.hook_old_pc
                        } else {
                            0
                        };
                        // 对应 C: if (npci <= oldpc || changedline(p, oldpc, npci))
                        // 注意: 即使 current_line < 0 (stripped 代码), 也要调用 hook
                        // C 实现中 luaG_getfuncline 返回 -1, luaD_hook 会被调用
                        if new_pc <= old_pc || Self::changed_line(state, old_pc, new_pc) {
                            let current_line = Self::get_current_line(state);
                            // 对应 C: ci->u.l.savedpc = pc; (在 luaG_traceexec 中更新)
                            // 在调用 line hook 前更新 call_info 最后一个条目的 saved_pc,
                            // 让 hook 回调中的 debug.getinfo(2, "l").currentline 返回正确的行号
                            // 注意: 只在 allowhook 为 true 时更新。当 hook 函数执行时,
                            // allowhook 为 false, last_entry 是 hook_entry (代表触发 hook
                            // 的函数), 不应被更新为 hook 函数的 pc。
                            if state.allowhook {
                                if let Some(last_entry) = state.call_info.last_mut() {
                                    last_entry.saved_pc = state.pc;
                                }
                            }
                            Self::call_hook(state, "line", current_line, None, 0, 0)?;
                        }
                        // 对应 C: L->oldpc = npci;
                        state.hook_old_pc = new_pc;
                    }
                }
            }

            // 调试跟踪输出
            if trace_level >= 1 {
                // 检测是否支持 ANSI 颜色
                let use_color = std::env::var("TERM").ok()
                    .map(|t| t != "dumb")
                    .unwrap_or(false);

                // 打印完整代码列表，标记当前 PC
                eprint!("{}", Self::dump_code_with_pc(state, state.pc, use_color));
                
                if trace_level >= 2 {
                    // 打印栈内容
                    eprint!("{}", Self::dump_stack(state));
                }
            }

            let result = match op {
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
                OpCode::CALL => Self::op_call(state, inst),
                OpCode::TAILCALL => Self::op_tailcall(state, inst),
                OpCode::RETURN => match Self::op_return(state, inst) {
                    Ok(Some(vr)) => return Ok(vr),
                    Ok(None) => Ok(()),
                    Err(VmError::Yield(values)) => return Ok(VmResult::Yield { values }),
                    Err(e) => Err(e),
                },
                OpCode::RETURN0 => match Self::op_return0(state, inst) {
                    Ok(Some(vr)) => return Ok(vr),
                    Ok(None) => Ok(()),
                    Err(VmError::Yield(values)) => return Ok(VmResult::Yield { values }),
                    Err(e) => Err(e),
                },
                OpCode::RETURN1 => match Self::op_return1(state, inst) {
                    Ok(Some(vr)) => return Ok(vr),
                    Ok(None) => Ok(()),
                    Err(VmError::Yield(values)) => return Ok(VmResult::Yield { values }),
                    Err(e) => Err(e),
                },
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

                    let mut func_val = Self::read_stack(state, ra + 3).clone();
                    // 可调用表 (带 __call 元方法) 支持 — 对应 op_call 中的 luaT_tryfuncTM
                    // string.gmatch 返回的迭代器是带 __call 的表,这里提取 __call 作为
                    // 实际函数,原表作为 self 参数放到 ra+4,ra+5 保持 ctrl
                    if let TValue::Table(t) = &func_val {
                        if let Some(mt) = t.get_metatable() {
                            let call_key = TValue::Str(state.intern_str("__call"));
                            if let Some(call_fn) = mt.get(&call_key) {
                                let table_clone = func_val.clone();
                                Self::write_stack(state, ra + 4, table_clone);
                                Self::write_stack(state, ra + 3, call_fn.clone());
                                func_val = call_fn;
                            }
                        }
                    }

                    // 检查是否是 coroutine.wrap 返回的 Table（GC 跟踪，可被回收）
                    // wrap Table 的元表只有 WRAP_MARKER，没有 __call，所以上面的 __call 检测不会匹配
                    if let Some(idx) = crate::stdlib::coroutine_lib::get_wrap_idx(&func_val) {
                        let tag = crate::stdlib::coroutine_lib::CORO_WRAP_CALL_BASE + idx;
                        let nresults = (c + 1) as i32;
                        crate::stdlib::coroutine_lib::call_wrap_call(
                            tag, state, ra + 3, 2, nresults,
                        )?;
                        state.pc += 1;
                        Ok(())
                    } else if let TValue::LClosure(closure) = &func_val {
                        let proto_code = closure.proto.code.clone();
                        let proto_constants = closure.proto.constants.clone();
                        let proto_upvals = closure.proto.upvalues.clone();
                        let proto_protos = closure.proto.protos.clone();
                        let proto_num_params = closure.proto.num_params;
                        let proto_is_vararg = closure.proto.is_vararg();
                        let proto_flag = closure.proto.flag;
                        let proto_max_stack = closure.proto.max_stack_size;

                        // 获取调用前的源和行号（用于 traceback）
                        let (caller_source, caller_line) = if state.base > 0 && state.base <= state.stack.len() {
                            if let TValue::LClosure(prev_closure) = &state.stack[state.base - 1] {
                                let src = prev_closure
                                    .proto
                                    .source
                                    .as_ref()
                                    .map(|s| s.as_str().to_string())
                                    .unwrap_or_else(|| "=?".to_string());
                                let line = get_proto_line(&prev_closure.proto, state.pc);
                                (src, line)
                            } else {
                                ("=?".to_string(), -1)
                            }
                        } else {
                            ("=?".to_string(), -1)
                        };

                        let nresults = (c + 1) as i32;
                        let fsize = proto_max_stack as usize;
                        let nfixparams = proto_num_params as usize;
                        let nargs = 2;

                        state.call_stack.push(CallFrame {
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
                            proto_flag: state.proto_flag,
                            nextraargs: state.nextraargs,
                            closure_upvals: std::mem::take(&mut state.closure_upvals),
                            tbc_list: state.tbc_list.take(),
                            open_upval: state.open_upval.take(),
                        });

                        // 推入调用栈信息 — 对应 C 的 funcnamefromcode 返回 "for iterator"
                        state.call_info.push(crate::state::CallInfoEntry {
                            source: caller_source,
                            line: caller_line,
                            name: "for iterator".to_string(),
                            is_c: false,
                            closure: Some(closure.clone()),
                            base: state.base,
                            saved_pc: state.pc,
                            namewhat: "for iterator".to_string(),
                            proto_flag: state.proto_flag,
                            nextraargs: state.nextraargs,
                            is_tailcall: false,
                        });

                        state.code = proto_code;
                        state.constants = proto_constants;
                        state.upval_descs = proto_upvals;
                        state.protos = proto_protos;
                        state.base = ra + 4;
                        state.pc = 0;
                        state.num_params = proto_num_params;
                        state.is_vararg = proto_is_vararg;
                        state.proto_flag = proto_flag;
                        state.nextraargs = 0;
                        // 关键: 将闭包的上值转移到 state，供 GETUPVAL/SETUPVAL 使用
                        state.closure_upvals = closure.upvals.borrow().clone();
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
                    } else if let TValue::LightUserData(tag) = &func_val {
                        // 处理 LightUserData 迭代器 (ipairs/pairs 返回的迭代器)
                        let tag_val = *tag as usize;
                        let nresults = (c + 1) as i32;
                        let nargs = 2; // state 和 control
                        let result = if tag_val == crate::stdlib::base_lib::BASE_IPAIRS_AUX {
                            // ipairs 迭代器 (ipairsaux)
                            crate::stdlib::base_lib::call_ipairs_aux(
                                state, ra + 3, nargs, nresults,
                            )
                        } else if tag_val == crate::stdlib::base_lib::BASE_NEXT_ITER {
                            // pairs 迭代器 (next)
                            crate::stdlib::base_lib::call_next_iter(
                                state, ra + 3, nargs, nresults,
                            )
                        } else if crate::stdlib::base_lib::is_base_tag(tag_val) {
                    crate::stdlib::base_lib::call_base_function(
                        tag_val, state, ra + 3, nargs, nresults,
                    )
                } else if crate::stdlib::math_lib::is_math_tag(tag_val) {
                    crate::stdlib::math_lib::call_math_function(
                        tag_val, state, ra + 3, nargs, nresults,
                    )
                } else if crate::stdlib::utf8_lib::is_utf8_tag(tag_val) {
                    // UTF-8 迭代器 (iter_auxstrict / iter_auxlax)
                    crate::stdlib::utf8_lib::call_utf8_function(
                        tag_val, state, ra + 3, nargs, nresults,
                    )
                } else if crate::stdlib::string_lib::is_string_tag(tag_val) {
                    // 字符串库迭代器 (gmatch_iter)
                    crate::stdlib::string_lib::call_string_function(
                        tag_val, state, ra + 3, nargs, nresults,
                    )
                } else if crate::stdlib::coroutine_lib::is_coro_tag(tag_val) {
                    // Coroutine 库函数（标签 700-709）
                    crate::stdlib::coroutine_lib::call_coro_function(
                        tag_val, state, ra + 3, nargs, nresults,
                    )
                } else if crate::stdlib::coroutine_lib::is_wrap_call_tag(tag_val) {
                    // coroutine.wrap 返回的函数（标签 710+）
                    crate::stdlib::coroutine_lib::call_wrap_call(
                        tag_val, state, ra + 3, nargs, nresults,
                    )
                } else {
                    Ok(())
                };
                        match result {
                            Ok(()) => {
                                // TFORCALL 调用 C 函数迭代器时，adjust_results 可能
                                // 推迟了结果放置（return hook 启用时）。TFORCALL 不经
                                // op_call 路径，需在此完成 adjust，否则 TFORLOOP 读到
                                // 错误的值。
                                state.finish_pending_adjust();
                                state.pc += 1;
                                Ok(())
                            }
                            Err(e) => Err(e),
                        }
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
            };
            match result {
                Ok(()) => {}
                Err(e) => {
                    if let VmError::Yield(values) = e {
                        return Ok(VmResult::Yield { values });
                    }
                    let mut current_error = e;
                    loop {
                        // close continuation error 处理 — 对应 C Lua 的 precover + finishpcallk +
                        // luaF_close 机制（CIST_RECST 保存错误状态后继续关闭剩余 TBC 变量）
                        //
                        // 当 __close 元方法 error 时，error 传播到 execute_loop。
                        // 若栈顶 PP 是 is_close_continuation + saved_filled（即 __close 是被
                        // yield 穿过的 close continuation），则：
                        //   1. pop PP，恢复 close 调用者（如 foo）的执行上下文
                        //   2. 保存 error 值到 close_error_status（模拟 CIST_RECST）
                        //   3. 调用 func::close 继续关闭剩余 TBC 变量
                        //      - 成功：检查 close_error_status，若有 pending error 则 fall through
                        //        到 pcall 处理；否则继续 execute_loop
                        //      - yield：返回 Yield
                        //      - 出错：更新 current_error，fall through 到 pcall 处理
                        if state.pcall_protection_stack.last().map_or(false, |t| t.saved_filled && t.is_close_continuation) {
                            let pp = state.pcall_protection_stack.pop().unwrap();
                            // 恢复 close 调用者的执行上下文 (对应 C 的 L->ci = ci->previous)
                            state.code = pp.saved_code;
                            state.constants = pp.saved_constants;
                            state.upval_descs = pp.saved_upval_descs;
                            state.protos = pp.saved_protos;
                            state.base = pp.saved_base;
                            state.pc = pp.saved_pc;
                            state.num_params = pp.saved_num_params;
                            state.is_vararg = pp.saved_is_vararg;
                            state.proto_flag = pp.saved_proto_flag;
                            state.nextraargs = pp.saved_nextraargs;
                            state.closure_upvals = pp.saved_closure_upvals;
                            state.tbc_list = pp.saved_tbc_list;
                            state.open_upval = pp.saved_open_upval;
                            // 截断栈，移除 __close 函数的帧
                            state.stack.truncate(pp.func_idx);
                            state.top = state.stack.len();
                            // 保存 error 值到 close_error_status（供 func::close 和后续传播使用）
                            // 保留 last_error_value，让 func::close 读取它作为 current_err
                            // （对应 C Lua 的 CIST_RECST 保存错误状态，luaF_close 用 status 读取）
                            if state.last_error_value.is_none() {
                                state.last_error_value = Some(match &current_error {
                                    VmError::RuntimeErrorValue(val) => val.clone(),
                                    VmError::RuntimeError(s) => TValue::Str(state.intern_str(s)),
                                    _ => TValue::Str(state.intern_str(&format!("{}", current_error))),
                                });
                            }
                            let error_val = state.last_error_value.clone().unwrap();
                            state.last_error_msg.clear();
                            state.close_error_status = Some(error_val);
                            // 调用 func::close 继续关闭剩余 TBC 变量
                            // level = state.base（close 调用者的 base），status = 1（error），ynresults = 1
                            match crate::func::close(state, state.base, 1, 1) {
                                Ok(()) => {
                                    if let Some(err) = state.close_error_status.take() {
                                        // 有 pending error，转换回 current_error，fall through 到 pcall 处理
                                        state.last_error_value = Some(err.clone());
                                        current_error = match err {
                                            TValue::Str(s) => VmError::RuntimeError(s.to_string()),
                                            v => VmError::RuntimeErrorValue(v),
                                        };
                                        continue;  // 下一轮迭代处理 pcall
                                    }
                                    // 无 pending error，继续 execute_loop
                                    break;
                                }
                                Err(VmError::Yield(values)) => {
                                    return Ok(VmResult::Yield { values });
                                }
                                Err(e2) => {
                                    // close 出错（非 yield），更新 current_error，fall through 到 pcall
                                    state.close_error_status = None;
                                    current_error = e2;
                                    continue;
                                }
                            }
                        }
                        // 检查 pcall_protection_stack — 对应 C Lua 的 precover + finishpcallk
                        // yield 穿过 pcall/xpcall 后，C 函数栈帧被销毁，但保护状态保留。
                        // 当 inner_func 后续执行 error 时，由 execute_loop 处理 pcall/xpcall 的返回。
                        // 只处理 saved_filled=true 的 PcallProtection（即被 yield 穿过的），
                        // 避免误处理 state.pcall 的 LClosure 分支调用的 execute_loop 中的 error。
                        // 跳过 is_close_continuation 的 PcallProtection：close continuation 的 error
                        // 已在上面的 close continuation 分支处理。
                        if state.pcall_protection_stack.last().map_or(false, |t| t.saved_filled && !t.is_close_continuation) {
                            // 获取 error 值（保留原始 TValue 类型，如 error({s}) 的表）
                            let mut error_val = state.last_error_value.clone()
                                .unwrap_or_else(|| {
                                    match &current_error {
                                        VmError::RuntimeErrorValue(val) => val.clone(),
                                        VmError::RuntimeError(s) => TValue::Str(state.intern_str(s)),
                                        _ => TValue::Str(state.intern_str(&format!("{}", current_error))),
                                    }
                                });
                            state.last_error_value = None;
                            state.last_error_msg.clear();

                            // 关闭 TBC 变量 — 对应 C Lua 的 finishpcallk:
                            // luaF_close(L, func, status, 1)
                            // 当 error 穿过被 yield 的 pcall 时，需要关闭 pcall 保护范围内的
                            // TBC 变量。使用 pcall 调用者（如 foo）的 closure_upvals/tbc_list/open_upval
                            // (从 call_stack 栈顶 CallFrame 获取，保存的是 pcall 调用者的值)。
                            // 若 call_stack 为空（inner_func 直接 error），使用当前 state 的上下文。
                            let pp_func_idx = state.pcall_protection_stack.last().unwrap().func_idx;
                            let saved_ctx = if let Some(frame) = state.call_stack.last().cloned() {
                                let saved_cu = std::mem::replace(&mut state.closure_upvals, frame.closure_upvals.clone());
                                let saved_tl = std::mem::replace(&mut state.tbc_list, frame.tbc_list);
                                let saved_ou = std::mem::replace(&mut state.open_upval, frame.open_upval);
                                Some((saved_cu, saved_tl, saved_ou))
                            } else {
                                None
                            };
                            state.last_error_value = Some(error_val.clone());
                            match crate::func::close(state, pp_func_idx, 1, 1) {
                                Ok(()) => {}
                                Err(VmError::Yield(values)) => {
                                    if let Some((cu, tl, ou)) = saved_ctx {
                                        state.closure_upvals = cu;
                                        state.tbc_list = tl;
                                        state.open_upval = ou;
                                    }
                                    return Ok(VmResult::Yield { values });
                                }
                                Err(_) => {}
                            }
                            // 获取最终 error 值（可能被 __close 更新）
                            if let Some(final_err) = state.last_error_value.take() {
                                error_val = final_err;
                            }
                            state.last_error_msg.clear();

                            // current_results 是当前要传递给 pcall/xpcall 的返回值
                            // 初始为 [error_val]，因为 inner_func 执行了 error
                            let mut current_results: Vec<TValue> = vec![error_val];
                            // is_error 表示当前是否在处理 error（而非成功返回值）
                            let mut is_error = true;

                            // 循环处理所有 PcallProtection（从内到外）
                            // 只处理 saved_filled=true 的 PcallProtection，
                            // 遇到 shield（saved_filled=false，由 state.pcall push）时 break，
                            // 使 error 传播到 state.pcall，而非被外层 PcallProtection 捕获。
                            // 遇到 is_close_continuation 时也 break：close continuation 的 error
                            // 已在上面的分支处理。
                            while let Some(protection) = state.pcall_protection_stack.last() {
                                if !protection.saved_filled { break; }
                                if protection.is_close_continuation { break; }
                                let protection = state.pcall_protection_stack.pop().unwrap();
                                // 恢复 pcall 调用者的执行上下文
                                state.code = protection.saved_code;
                                state.constants = protection.saved_constants;
                                state.upval_descs = protection.saved_upval_descs;
                                state.protos = protection.saved_protos;
                                state.base = protection.saved_base;
                                state.pc = protection.saved_pc;
                                state.num_params = protection.saved_num_params;
                                state.is_vararg = protection.saved_is_vararg;
                                state.proto_flag = protection.saved_proto_flag;
                                state.nextraargs = protection.saved_nextraargs;
                                state.closure_upvals = protection.saved_closure_upvals;
                                state.tbc_list = protection.saved_tbc_list;
                                state.open_upval = protection.saved_open_upval;

                                if is_error {
                                    // 处理 error：pcall/xpcall 捕获 error
                                    match protection.pcall_kind {
                                        crate::state::PcallKind::Pcall => {
                                            // pcall: 返回 (false, error_val)
                                            state.stack.truncate(protection.func_idx);
                                            state.stack.push(TValue::Boolean(false));
                                            state.stack.push(current_results[0].clone());
                                            state.top = protection.func_idx + 2;
                                            current_results = vec![TValue::Boolean(false), current_results[0].clone()];
                                            // pcall 已捕获 error，后续 PcallProtection 处理成功返回值
                                            is_error = false;
                                        }
                                        crate::state::PcallKind::Xpcall { handler } => {
                                            // xpcall: 调用 handler(error_val)，返回 (false, handler_result)
                                            state.stack.truncate(protection.func_idx);
                                            state.stack.push(handler);
                                            state.stack.push(current_results[0].clone());
                                            state.top = protection.func_idx + 2;
                                            let handler_status = state.pcall(1, -1, 0);
                                            let handler_nret = state.stack.len().saturating_sub(protection.func_idx);
                                            let handler_result: Vec<TValue> = if handler_status == 0 {
                                                // handler 成功: 返回 handler 的结果
                                                (0..handler_nret).map(|i| state.stack[protection.func_idx + i].clone()).collect()
                                            } else {
                                                // handler 失败: 返回 "error in error handling"
                                                vec![TValue::Str(state.intern_str("error in error handling"))]
                                            };
                                            // xpcall 返回 (false, handler_result...)
                                            state.stack.truncate(protection.func_idx);
                                            state.stack.push(TValue::Boolean(false));
                                            for r in &handler_result {
                                                state.stack.push(r.clone());
                                            }
                                            state.top = protection.func_idx + 1 + handler_result.len();
                                            current_results = {
                                                let mut v = vec![TValue::Boolean(false)];
                                                v.extend(handler_result);
                                                v
                                            };
                                            is_error = false;
                                        }
                                    }
                                } else {
                                    // 处理成功返回值：pcall/xpcall 返回 (true, results...)
                                    // 先获取当前栈上的 current_results（在恢复 saved_* 状态后，
                                    // current_results 可能已在栈上，但这里用 Vec 传递）
                                    state.stack.truncate(protection.func_idx);
                                    state.stack.push(TValue::Boolean(true));
                                    for r in &current_results {
                                        state.stack.push(r.clone());
                                    }
                                    state.top = protection.func_idx + 1 + current_results.len();
                                    // current_results 更新为 (true, results...)
                                    current_results = {
                                        let mut v = vec![TValue::Boolean(true)];
                                        v.extend(current_results.iter().cloned());
                                        v
                                    };
                                }
                            }
                            // 所有 PcallProtection 处理完毕，清空 call_stack
                            // 对应 C Lua 的 longjmp 跳过所有 CallInfo，回到 pcall 调用前的状态
                            // pcall 处理路径只在协程场景下被触发（PP1.saved_filled=true，即 yield 穿过 pcall），
                            // call_stack 中的帧是被中断的调用帧，pcall 捕获 error 后不再需要
                            state.call_stack.clear();
                            break;
                        }
                        Self::build_traceback(state, &current_error);
                        return Err(current_error);
                    }
                    continue;
                }
            }
        }
    }

    /// 构建堆栈回溯并存储到 state.last_traceback — 对应 C 的 luaL_traceback
    ///
    /// 在错误发生时调用，从当前状态和调用栈构建回溯信息。
    /// 遍历 call_info 中的所有调用帧，构建完整的堆栈回溯。
    /// 同时格式化错误消息（添加 source:line 前缀）并存储到 state.last_error_msg。
    fn build_traceback(state: &mut LuaState, error: &VmError) {
        // LEVELS1/LEVELS2: 对应 C 的 traceback 层数限制
        // 超过 LEVELS1+LEVELS2 帧时，只显示前 LEVELS1 帧和后 LEVELS2 帧，中间用 "..." 跳过
        const LEVELS1: usize = 10;
        const LEVELS2: usize = 11;

        let mut trace = String::from("stack traceback:");

        // 获取当前帧的源、行号和闭包
        let (cur_source, cur_line, cur_closure) = if state.base > 0 && state.base <= state.stack.len() {
            if let TValue::LClosure(closure) = &state.stack[state.base - 1].clone() {
                let src = closure.proto.source.as_ref()
                    .map(|s| short_source(s))
                    .unwrap_or_else(|| "?".to_string());
                let ln = get_proto_line(&closure.proto, state.pc);
                (src, ln, Some(closure.clone()))
            } else {
                ("?".to_string(), -1, None)
            }
        } else {
            ("?".to_string(), -1, None)
        };

        let ci = &state.call_info;
        let ci_len = ci.len();
        // 跳过 call_info 末尾的 C 函数帧 — 它们由 last_c_function 处理
        // （error 时 C 函数帧保留在 call_info 中，供 build_traceback_from_thread 使用）
        let c_chain_len = ci.iter().rev().take_while(|e| e.is_c).count();
        let effective_ci_len = ci_len - c_chain_len;
        let has_lua_frame = cur_closure.is_some();

        // 如果错误由 C 函数引发，先添加 C 函数帧 — 对应 C 的 [C]: in global 'name'
        if let Some(c_func_name) = &state.last_c_function {
            trace.push_str(&format!("\n\t[C]: in global '{}'", c_func_name));
        }

        // 收集所有 Lua 帧信息 (source, line, name_str) — 从内到外
        // 帧 0: 当前函数; 帧 1..=effective_ci_len: 调用者
        // call_info[i] 存储: source/line = 调用者(外层)的信息, name/closure = 被调用者(内层)的信息
        let mut frames: Vec<(String, i32, String)> = Vec::new();

        // 帧 0: 当前函数
        if has_lua_frame {
            let name_str = if effective_ci_len > 0 {
                let last = &ci[effective_ci_len - 1];
                format_func_name(&last.namewhat, &last.name, false, last.closure.as_ref())
            } else {
                "main chunk".to_string()
            };
            frames.push((cur_source.clone(), cur_line, name_str));
        }

        // 帧 1..=effective_ci_len: 调用者帧
        for level in 1..=effective_ci_len {
            let entry = &ci[effective_ci_len - level];
            let src = short_source_bytes(entry.source.as_bytes());
            let line = entry.line;
            let name_str = if level < effective_ci_len {
                // name/namewhat/closure 来自更外层的 call_info 条目
                let outer = &ci[effective_ci_len - 1 - level];
                format_func_name(&outer.namewhat, &outer.name, false, outer.closure.as_ref())
            } else {
                // 最外层是 main chunk
                "main chunk".to_string()
            };
            frames.push((src, line, name_str));
        }

        // 应用 LEVELS1/LEVELS2 限制 — 对应 C 的 limit2show 逻辑
        let total = frames.len();
        let limit2show = if total > LEVELS1 + LEVELS2 {
            Some(LEVELS1)
        } else {
            None
        };

        let mut idx = 0;
        while idx < total {
            if let Some(limit) = limit2show {
                if idx == limit {
                    // 跳过中间层
                    let n = total - idx - LEVELS2;
                    trace.push_str(&format!("\n\t...\t(skipping {} levels)", n));
                    idx = total - LEVELS2;
                    continue;
                }
            }
            let (src, line, name) = &frames[idx];
            if *line > 0 {
                trace.push_str(&format!("\n\t{}:{}: in {}", src, line, name));
            } else {
                trace.push_str(&format!("\n\t{}: in {}", src, name));
            }
            idx += 1;
        }

        // 格式化错误消息（添加 source:line 前缀）— 对应 C 的 luaG_addinfo
        let error_msg = match error {
            VmError::RuntimeError(msg) => msg.clone(),
            VmError::RuntimeErrorValue(val) => format!("{}", val),
            other => format!("{}", other),
        };
        // 只在错误消息尚未包含 source:line 前缀时添加
        // (error()/assert() 等 C 函数已经通过 luaL_where 添加了前缀)
        let prefix = if cur_line > 0 && !has_source_line_prefix(&error_msg) {
            format!("{}:{}: ", cur_source, cur_line)
        } else {
            String::new()
        };
        state.last_error_msg = format!("{}{}", prefix, error_msg);
        // 末尾追加 C 层调用者帧 — 对应 C Lua 中调用主块的 C 函数
        // (如 pcall/docall)，该帧无名称，显示为 [C]: in ?
        trace.push_str("\n\t[C]: in ?");
        state.last_traceback = trace;
    }

    /// 格式化单条指令为 bytecode_dump 格式
    /// 格式: "{pc}\t[-]\t{OP_NAME}\t{operands}"
    pub fn format_instruction(state: &LuaState, inst: Instruction, pc: usize) -> String {
        // 使用 bytecode_dump 的格式: pc [-] instruction
        format!("{}\t[-]\t{}", pc + 1, crate::compiler::bytecode_dump::format_instruction(inst))
    }

    /// 打印完整代码列表，标记当前执行的 PC
    /// 支持 ANSI 颜色高亮（终端）和 <- 标记
    pub fn dump_code_with_pc(state: &LuaState, current_pc: usize, use_color: bool) -> String {
        let mut output = String::new();
        output.push_str(&format!("\n=== code ({} instructions, pc={}) ===\n", state.code.len(), current_pc));

        for (i, &inst) in state.code.iter().enumerate() {
            let inst_str = crate::compiler::bytecode_dump::format_instruction(inst);
            let is_current = i == current_pc;

            if is_current {
                if use_color {
                    // ANSI 黄色高亮 + <- 标记
                    output.push_str(&format!("\x1b[33m{}\t[-]\t{}\t<-\x1b[0m\n", i + 1, inst_str));
                } else {
                    // 纯文本 <- 标记
                    output.push_str(&format!("{}\t[-]\t{}\t<-\n", i + 1, inst_str));
                }
            } else {
                output.push_str(&format!("{}\t[-]\t{}\n", i + 1, inst_str));
            }
        }
        output.push_str("=== end code ===\n");
        output
    }

    /// 打印完整栈内容（调试用）
    pub fn dump_stack(state: &LuaState) -> String {
        let mut output = String::new();
        output.push_str(&format!("\n=== stack (len={}, base={}, pc={}) ===\n", state.stack.len(), state.base, state.pc));
        for (i, val) in state.stack.iter().enumerate() {
            let mut markers = String::new();
            if i == state.base { markers.push_str(" <-- base"); }
            if i == state.base + state.num_params as usize { markers.push_str(" <-- after params"); }
            output.push_str(&format!("  [{:03}] {:<30}{}\n", i, format!("{}", val), markers));
        }
        output.push_str("=== end stack ===\n");
        output
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
        if idx < state.stack.len() {
            &state.stack[idx]
        } else {
            // 打印完整的调试信息
            eprintln!("\n=== STACK UNDERFLOW PANIC ===");
            eprintln!("尝试访问栈索引: {}, 栈大小: {}", idx, state.stack.len());
            eprintln!("当前 PC: {}, Base: {}", state.pc, state.base);

            // 打印完整指令列表，标记当前执行的指令
            let use_color = std::env::var("TERM").ok()
                .map(|t| t != "dumb")
                .unwrap_or(false);
            eprint!("{}", Self::dump_code_with_pc(state, state.pc, use_color));

            // 打印完整栈内容
            eprint!("{}", Self::dump_stack(state));

            // 打印栈帧信息
            eprintln!("\n--- 栈帧信息 ---");
            eprintln!("  Base 寄存器起始: {}", state.base);
            eprintln!("  参数数量: {}", state.num_params);
            eprintln!("  是否可变参数: {}", state.is_vararg);
            eprintln!("  代码长度: {} 条指令", state.code.len());
            
            // 打印 upval 信息
            if !state.closure_upvals.is_empty() {
                eprintln!("\n--- Upval 信息 (共 {} 个) ---", state.closure_upvals.len());
                for (i, upval) in state.closure_upvals.iter().enumerate() {
                    let uv_ref = upval.borrow();
                    match &*uv_ref {
                        UpVal::Closed { value } => {
                            eprintln!("  upval[{}] = Closed({})", i, value);
                        }
                        UpVal::Open { stack_index, .. } => {
                            let val = if *stack_index < state.stack.len() {
                                format!("{}", state.stack[*stack_index])
                            } else {
                                "<invalid>".to_string()
                            };
                            eprintln!("  upval[{}] = Open(stack_index={}, value={})", i, stack_index, val);
                        }
                    }
                }
            }

            panic!("stack underflow: idx={}, stack.len={}, pc={}, base={}",
                   idx, state.stack.len(), state.pc, state.base);
        }
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
            // Take the jump (对应 C 的 donextjump: ni = *pc; pc += GETARG_sJ(ni) + 1)
            // state.pc 是 TEST 位置，JMP 在 state.pc + 1
            let jmp_pc = state.pc + 1;
            if jmp_pc >= state.code.len() {
                state.pc = jmp_pc;  // 越界，跳出循环
                return;
            }
            let jmp_inst = state.code[jmp_pc];
            let sj = opcodes::getarg_sj(jmp_inst);
            state.pc = ((jmp_pc as i32) + sj + 1) as usize;
        } else {
            // Skip the jump (对应 C 的 pc++)
            state.pc += 2;  // 跳过 TEST 和 JMP
        }
    }

    /// 元方法 continuation — 对应 C Lua 的 luaV_finishOp
    ///
    /// yield 穿过元方法后，resume 时元方法返回，由此函数完成被中断的指令
    /// （如 OP_LE 的 do_conditional_jump，OP_MMBIN 的结果放置）。
    ///
    /// 返回:
    /// - Ok(true): 已处理元方法 continuation，调用者应返回 Ok(None) 继续循环
    /// - Ok(false): 非元方法返回，调用者应按正常协程底部返回处理
    /// - Err(e): close 失败等错误
    fn try_finish_metamethod(state: &mut LuaState, result: Option<TValue>) -> Result<bool, VmError> {
        // 检查顶部 PcallProtection 是否为 yield 穿过的元方法
        // saved_call_stack_len == state.call_stack.len() 确保是元方法自身返回
        // (而非元方法调用的函数返回 — 此时 call_stack 仍有该函数的帧)
        let cur_call_stack_len = state.call_stack.len();
        let is_mm = state.pcall_protection_stack.last()
            .map_or(false, |t| t.is_metamethod && t.saved_filled
                && t.saved_call_stack_len == cur_call_stack_len);
        if !is_mm {
            return Ok(false);
        }

        let protection = state.pcall_protection_stack.pop().unwrap();

        // 关闭元方法栈帧的 TBC 变量和开 upvalue
        // (必须在恢复调用者状态之前，因为 close 使用 state.base)
        crate::func::close(state, state.base, 0, 1)?;

        // 提取返回值
        let result_val = result.unwrap_or(TValue::Nil(NilKind::Strict));

        // 恢复调用者的执行上下文 (对应 C 的 L->ci = ci->previous)
        state.code = protection.saved_code;
        state.constants = protection.saved_constants;
        state.upval_descs = protection.saved_upval_descs;
        state.protos = protection.saved_protos;
        state.base = protection.saved_base;
        state.pc = protection.saved_pc;  // 指向被中断的指令
        state.num_params = protection.saved_num_params;
        state.is_vararg = protection.saved_is_vararg;
        state.proto_flag = protection.saved_proto_flag;
        state.nextraargs = protection.saved_nextraargs;
        state.closure_upvals = protection.saved_closure_upvals;
        state.tbc_list = protection.saved_tbc_list;
        state.open_upval = protection.saved_open_upval;

        // 截断栈，移除元方法的帧
        state.stack.truncate(protection.func_idx);

        // 读取被中断的指令 (对应 C 的 luaV_finishOp)
        let inst_opt = if state.pc < state.code.len() {
            Some(state.code[state.pc])
        } else {
            None
        };
        let op_opt = inst_opt.map(|i| opcodes::get_opcode(i));

        // 对于 __newindex (SETTABLE/SETI/SETFIELD/SETTABUP)，没有返回值，
        // 不需要写入结果到 metamethod_res 位置
        let skip_write_res = matches!(
            op_opt,
            Some(OpCode::SETTABLE) | Some(OpCode::SETI) | Some(OpCode::SETFIELD) | Some(OpCode::SETTABUP)
        );

        if !skip_write_res {
            // 将结果放入 res 槽位 (对应 C 的 setobjs2s(L, res, --L->top))
            let res = protection.metamethod_res;
            while state.stack.len() <= res {
                state.stack.push(TValue::Nil(NilKind::Strict));
            }
            state.stack[res] = result_val.clone();
            state.top = state.stack.len();
        }

        // 完成 continuation
        if let Some(inst) = inst_opt {
            let op = op_opt.unwrap();
            match op {
                OpCode::LE | OpCode::LT | OpCode::LEI | OpCode::LTI | OpCode::GTI | OpCode::GEI => {
                    let cond = !result_val.is_false();
                    Self::do_conditional_jump(state, inst, cond);
                }
                OpCode::MMBIN | OpCode::MMBINI | OpCode::MMBINK => {
                    // 结果已在目标寄存器 (metamethod_res = RA(pi))
                    // 跳过 MMBIN 指令 (对应 C 的 ci->u.l.savedpc++)
                    state.pc += 1;
                }
                OpCode::GETTABLE | OpCode::GETI | OpCode::GETFIELD | OpCode::SELF | OpCode::GETTABUP => {
                    // __index 元方法 continuation (对应 C 的 luaV_finishOp):
                    //   setobjs2s(L, base + GETARG_A(inst), --L->top.p);
                    // 即将栈顶结果 (临时位置) 移到 RA 位置
                    let ra = state.base + opcodes::getarg_a(inst) as usize;
                    // 结果在栈顶 (metamethod_res 位置)
                    let result = state.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
                    while state.stack.len() <= ra {
                        state.stack.push(TValue::Nil(NilKind::Strict));
                    }
                    state.stack[ra] = result;
                    state.top = state.stack.len();
                    state.pc += 1;
                }
                OpCode::SETTABLE | OpCode::SETI | OpCode::SETFIELD | OpCode::SETTABUP => {
                    // __newindex 元方法 continuation (对应 C 的 luaV_finishOp default 分支):
                    // 0 个返回值，只需前进 PC
                    state.pc += 1;
                }
                OpCode::CONCAT => {
                    // 对应 C 的 luaV_finishOp 对 OP_CONCAT 的处理:
                    //   StkId top = L->top.p - 1;  // top when 'luaT_tryconcatTM' was called
                    //   int a = GETARG_A(inst);
                    //   int total = cast_int(top - 1 - (base + a));
                    //   setobjs2s(L, top - 2, top);  // put TM result in proper position
                    //   L->top.p = top - 1;
                    //   luaV_concat(L, total);  // concat them (may yield again)
                    // try_finish_metamethod 已将结果放到 res = func_idx - 2 位置
                    // (对应 setobjs2s(L, top - 2, top))
                    // 现在需要: 移除 p2 (L->top.p = top - 1)，然后拼接剩余元素
                    // 注意: a 必须加上 state.base 得到绝对栈位置 (对应 C 的 base + GETARG_A)
                    let a = state.base + opcodes::getarg_a(inst) as usize;
                    let func_idx = protection.func_idx;
                    // 栈状态: [a, a+1, ..., result, p2] (长度 = func_idx)
                    // 截断到 func_idx - 1, 移除 p2 (对应 L->top.p = top - 1)
                    state.stack.truncate(func_idx - 1);
                    state.top = state.stack.len();
                    // 还需拼接的元素数量 (对应 total = top - 1 - (base + a))
                    let total = func_idx - 1 - a;
                    if total > 1 {
                        // 尝试直接拼接 (对应 luaV_concat 的第一步)
                        let mut vals: Vec<TValue> = state.stack[a..a + total].to_vec();
                        match concat_stack(&mut vals, total) {
                            Ok(()) => {
                                for (i, v) in vals.into_iter().enumerate() {
                                    state.stack[a + i] = v;
                                }
                                state.stack.truncate(a + 1);
                                state.top = state.stack.len();
                            }
                            Err(crate::tm::TagMethodError::ConcatError { .. }) => {
                                // 直接拼接失败,调用 __concat 元方法 (可能 yield)
                                // concat_stack 可能已拼接栈顶的 string 序列(在 vals 中),
                                // 必须写回 state.stack 以反映已拼接的结果
                                state.stack.truncate(a);
                                state.stack.extend_from_slice(&vals);
                                state.top = state.stack.len();
                                Self::finish_concat_loop(state, a)?;
                            }
                            Err(_) => {
                                return Err(VmError::RuntimeError("concat error".into()));
                            }
                        }
                    }
                    // 清除剩余槽位
                    while state.stack.len() > a + 1 {
                        state.stack.pop();
                    }
                    state.top = state.stack.len();
                    state.pc += 1;
                }
                _ => {
                    // 未知指令: 前进 PC
                    state.pc += 1;
                }
            }
        }

        Ok(true)
    }

    /// 完成 __close continuation — 对应 C Lua 的 luaV_finishOp 对 OP_RETURN/OP_CLOSE 的处理
    ///
    /// 当 __close 元方法 yield 后，resume 时 __close 函数返回，op_return 检测到
    /// is_close_continuation=true 的 PcallProtection，恢复 close 调用者状态，
    /// 重新执行 OP_RETURN/OP_CLOSE（对应 C 的 savedpc--）。
    ///
    /// 返回 true 表示已处理 continuation，op_return 应返回 Ok(None) 让 execute_loop
    /// 重新执行 OP_RETURN/OP_CLOSE。返回 false 表示不是 close continuation。
    fn finish_close_continuation(state: &mut LuaState) -> Result<bool, VmError> {
        let cur_call_stack_len = state.call_stack.len();
        let is_close_cont = state.pcall_protection_stack.last()
            .map_or(false, |t| t.is_close_continuation && t.saved_filled
                && t.saved_call_stack_len == cur_call_stack_len);
        if !is_close_cont {
            return Ok(false);
        }

        // 关闭 __close 函数栈帧的 TBC 变量和开 upvalue
        // 此时 state.base 是 __close 函数的 base
        // 如果 close 再次 yield，PcallProtection 保留供下次 resume 使用
        // 对应 C Lua 的 luaV_finishOp: savedpc-- 重新执行 OP_RETURN/OP_CLOSE
        match crate::func::close(state, state.base, 0, 1) {
            Ok(()) => {}
            Err(e) => {
                // close yield 或出错: 不 pop PcallProtection, 不恢复状态
                return Err(e);
            }
        }

        // close 成功: pop PcallProtection
        let protection = state.pcall_protection_stack.pop().unwrap();

        // 恢复 close 调用者的执行上下文 (对应 C 的 L->ci = ci->previous)
        state.code = protection.saved_code;
        state.constants = protection.saved_constants;
        state.upval_descs = protection.saved_upval_descs;
        state.protos = protection.saved_protos;
        state.base = protection.saved_base;
        state.pc = protection.saved_pc;  // 指向 OP_RETURN/OP_CLOSE (不 +1, 重新执行)
        state.num_params = protection.saved_num_params;
        state.is_vararg = protection.saved_is_vararg;
        state.proto_flag = protection.saved_proto_flag;
        state.nextraargs = protection.saved_nextraargs;
        state.closure_upvals = protection.saved_closure_upvals;
        state.tbc_list = protection.saved_tbc_list;
        state.open_upval = protection.saved_open_upval;

        // 截断栈，移除 __close 函数的帧
        state.stack.truncate(protection.func_idx);
        state.top = state.stack.len();

        // 检查 close_error_status — 对应 C Lua 的 CIST_RECST 保存的错误状态
        // 当 __close continuation 是被 execute_loop 的 close continuation error 分支
        // 触发的（前一个 __close 出错后 func::close 继续关闭剩余 TBC），close_error_status
        // 保存了 pending error。此时已恢复 close 调用者的状态（state.base = 调用者 base），
        // 需继续 close 调用者的剩余 TBC 变量（对应 C Lua 的 luaF_close 循环）。
        // 如果剩余 __close 再次 yield，重新设置 close_error_status 并返回 Err(Yield)。
        if let Some(err) = state.close_error_status.take() {
            state.last_error_value = Some(err.clone());
            // 继续 close 调用者的剩余 TBC 变量（status=1, yy=1 可 yield）
            // 对应 C Lua 的 finishpcallk: luaF_close(L, func, status, 1)
            match crate::func::close(state, state.base, 1, 1) {
                Ok(()) => {
                    // close 完成: 返回 pending error 给上层 pcall
                    return match err {
                        TValue::Str(s) => Err(VmError::RuntimeError(s.to_string())),
                        v => Err(VmError::RuntimeErrorValue(v)),
                    };
                }
                Err(VmError::Yield(values)) => {
                    // __close 再次 yield: 重新设置 close_error_status，返回 Yield
                    // 对应 C Lua 的 luaF_close 被 yield interrupt，finishpcallk 再次被调用
                    state.close_error_status = Some(err);
                    return Err(VmError::Yield(values));
                }
                Err(e2) => {
                    // __close 出错: 用新 error 替代 pending error
                    state.close_error_status = None;
                    return Err(e2);
                }
            }
        }

        Ok(true)
    }

    /// pcall/xpcall 正常返回 continuation — 对应 C Lua 的 finishpcallk (成功路径)
    ///
    /// yield 穿过 pcall/xpcall 后，C 函数栈帧被销毁，但保护状态保留在
    /// pcall_protection_stack 中 (saved_filled=true)。当被保护的 Lua 函数
    /// 执行 OP_RETURN 返回时，execute_loop 收到 Ok(Some(VmResult::Return { ... }))，
    /// 由此函数检查并处理 pcall/xpcall 的正常返回:
    ///   1. pop PcallProtection
    ///   2. 从 VmResult 取返回值，截断栈到 func_idx
    ///   3. 恢复 pcall 调用者的执行上下文
    ///   4. push true + 返回值，按 nresults 调整栈 (模拟 call_pcall/call_xpcall 成功返回)
    ///   5. 返回 true，execute_loop 继续循环 (pcall 调用者从 CALL 指令之后继续执行)
    ///
    /// 注意: 只处理非 close continuation、非 metamethod 的 PcallProtection。
    /// close continuation 由 finish_close_continuation 处理，
    /// metamethod 由 try_finish_metamethod 处理。
    fn finish_pcall_return(state: &mut LuaState, nret: usize, result_base: usize) -> Result<bool, VmError> {
        let need_finish = state.pcall_protection_stack.last().map_or(false, |t| {
            t.saved_filled && !t.is_close_continuation && !t.is_metamethod
        });
        if !need_finish {
            return Ok(false);
        }

        let protection = state.pcall_protection_stack.pop().unwrap();

        // 从 result_base 取 nret 个返回值
        let mut tmp_results: Vec<TValue> = Vec::with_capacity(nret);
        for i in 0..nret {
            if result_base + i < state.stack.len() {
                tmp_results.push(std::mem::take(&mut state.stack[result_base + i]));
            } else {
                tmp_results.push(TValue::Nil(NilKind::Strict));
            }
        }

        // 截断栈到 func_idx (移除被保护函数的栈帧)
        state.stack.truncate(protection.func_idx);

        // 恢复 pcall 调用者的执行上下文
        state.code = protection.saved_code;
        state.constants = protection.saved_constants;
        state.upval_descs = protection.saved_upval_descs;
        state.protos = protection.saved_protos;
        state.base = protection.saved_base;
        state.pc = protection.saved_pc;  // 指向 CALL pcall 指令之后
        state.num_params = protection.saved_num_params;
        state.is_vararg = protection.saved_is_vararg;
        state.proto_flag = protection.saved_proto_flag;
        state.nextraargs = protection.saved_nextraargs;
        state.closure_upvals = protection.saved_closure_upvals;
        state.tbc_list = protection.saved_tbc_list;
        state.open_upval = protection.saved_open_upval;

        // push true + 返回值，按 nresults 调整栈
        // (模拟 call_pcall/call_xpcall 的成功返回: results = [true] + tmp_results)
        // 直接调整栈 (不使用 push_results/adjust_results，避免 pending_return_adjust 问题)
        let mut results: Vec<TValue> = {
            let mut v = vec![TValue::Boolean(true)];
            v.extend(tmp_results);
            v
        };
        let n = if protection.nresults < 0 {
            results.len()
        } else {
            protection.nresults as usize
        };
        for i in 0..n {
            if i < results.len() {
                state.stack.push(std::mem::take(&mut results[i]));
            } else {
                state.stack.push(TValue::Nil(NilKind::Strict));
            }
        }
        state.top = state.stack.len();

        Ok(true)
    }

    /// 完成 CONCAT 的 __concat 元方法调用后的拼接循环
    /// 对应 op_concat 的 Err 分支循环逻辑 (从 try_concat_tm 之后)
    /// 栈状态: [a, a+1, ..., result] (result 在 top-1 位置)
    /// 尝试直接拼接,如果失败继续调用 try_concat_tm (可能 yield)
    fn finish_concat_loop(state: &mut LuaState, a: usize) -> Result<(), VmError> {
        loop {
            let remaining = state.stack.len() - a;
            if remaining <= 1 {
                break;
            }
            let mut vals: Vec<TValue> = state.stack[a..a + remaining].to_vec();
            match concat_stack(&mut vals, remaining) {
                Ok(()) => {
                    for (i, v) in vals.into_iter().enumerate() {
                        state.stack[a + i] = v;
                    }
                    state.stack.truncate(a + 1);
                    state.top = state.stack.len();
                    break;
                }
                Err(crate::tm::TagMethodError::ConcatError { .. }) => {
                    // 直接拼接失败,调用 __concat 元方法
                    // concat_stack 可能已拼接栈顶的 string 序列(在 vals 中),
                    // 必须写回 state.stack 以反映已拼接的结果,
                    // 否则会错误地对两个 string 调用 __concat 元方法
                    state.stack.truncate(a);
                    state.stack.extend_from_slice(&vals);
                    state.top = state.stack.len();
                    let top = state.stack.len();
                    if top < a + 2 {
                        break;
                    }
                    let p1 = state.stack[top - 2].clone();
                    let p2 = state.stack[top - 1].clone();
                    // try_concat_tm 将结果写入 res (top-2),可能 yield
                    try_concat_tm(state, &p1, &p2, top - 2)?;
                    // 移除 top-1 (p2)
                    state.stack.truncate(top - 1);
                    state.top = state.stack.len();
                    // 继续循环尝试直接拼接
                    continue;
                }
                Err(_) => {
                    return Err(VmError::RuntimeError("concat error".into()));
                }
            }
        }
        Ok(())
    }

    // ========================================================================
    // Line hook 支持 — 对应 C 的 luaG_traceexec
    // ========================================================================

    /// 获取当前指令所在行号 — 对应 C 的 luaG_getfuncline
    fn get_current_line(state: &LuaState) -> i32 {
        if state.base == 0 || state.base > state.stack.len() {
            return -1;
        }
        if let TValue::LClosure(closure) = &state.stack[state.base - 1] {
            let proto = &closure.proto;
            if proto.line_info.is_empty() || state.pc >= proto.line_info.len() {
                return -1;
            }
            // 计算行号: 基础行号 + delta 累加
            let mut base_pc = -1i32;
            let mut base_line = proto.line_defined;
            for abs in &proto.abs_line_info {
                let abs_pc = abs.pc;
                if abs_pc <= state.pc as i32 && abs_pc > base_pc {
                    base_pc = abs_pc;
                    base_line = abs.line;
                }
            }
            let mut line = base_line;
            let mut i = base_pc + 1;
            while i <= state.pc as i32 {
                let delta = proto.line_info[i as usize];
                if delta != i8::MIN {
                    line += delta as i32;
                }
                i += 1;
            }
            line
        } else {
            -1
        }
    }

    /// 检查从 old_pc 到 new_pc 是否发生了行号变化 — 对应 C 的 changedline
    fn changed_line(state: &LuaState, old_pc: i32, new_pc: i32) -> bool {
        if state.base == 0 || state.base > state.stack.len() {
            return false;
        }
        if let TValue::LClosure(closure) = &state.stack[state.base - 1] {
            let proto = &closure.proto;
            if proto.line_info.is_empty() {
                return false;
            }
            // 简化实现: 直接比较两个 pc 对应的行号
            let old_line = Self::get_proto_line_at(proto, old_pc as usize);
            let new_line = Self::get_proto_line_at(proto, new_pc as usize);
            old_line != new_line
        } else {
            false
        }
    }

    /// 获取 proto 在指定 pc 处的行号
    fn get_proto_line_at(proto: &Proto, pc: usize) -> i32 {
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

    /// 调用 hook 函数 — 对应 C 的 luaD_hook(L, event, line)
    ///
    /// 在栈上压入 hook 函数和参数 (event, line)，通过 pcall 调用，
    /// 然后恢复栈。hook 调用期间临时禁用 hook 避免递归。
    /// event 可以是 "line", "call", "return" 等
    ///
    /// `frame_base`: 指定 hook_entry 的 base（触发 hook 的帧的 base）。
    ///   - None: 使用 state.base（适用于 line hook、Lua 函数 call hook、return hook）
    ///   - Some(base): 使用指定 base（适用于 C 函数 call hook，此时 state.base 尚未更新）
    pub fn call_hook(state: &mut LuaState, event: &str, line: i32, frame_base: Option<usize>,
                     ftransfer: i32, ntransfer: i32) -> Result<(), VmError> {
        let hook_fn = match &state.hook_func {
            Some(f) => f.clone(),
            None => return Ok(()),
        };

        // 对应 C 的 luaD_hook: if (hook && L->allowhook)
        if !state.allowhook {
            return Ok(());
        }

        // 设置 transferinfo — 对应 C 的 L->transferinfo
        state.transferinfo_ftransfer = ftransfer;
        state.transferinfo_ntransfer = ntransfer;

        // 保存当前栈顶
        let saved_top = state.stack.len();

        // 对应 C 的 L->allowhook = 0 (防止递归)
        state.allowhook = false;

        // 压入 hook 函数和参数: f(event, line)
        // 对应 C 的 hookf: currentline >= 0 时推入 integer, 否则推入 nil
        state.stack.push(hook_fn.clone());
        let event_str = state.intern_str(event);
        state.stack.push(TValue::Str(event_str));
        if line >= 0 {
            state.stack.push(TValue::Integer(line as i64));
        } else {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }

        // 推入 CallInfoEntry，标记为 hook 调用（对应 C 的 CIST_HOOKED）
        // debug.getinfo(1) 会从 call_info.last() 获取 namewhat
        let hook_closure = match &hook_fn {
            TValue::LClosure(c) => Some(c.clone()),
            _ => None,
        };

        // hook_entry 的 base 指向触发 hook 的帧的 base
        // 这样 debug.getinfo(2) 能通过 state.stack[entry.base - 1] 获取触发帧的闭包
        let hook_base = frame_base.unwrap_or(state.base);

        // 获取触发 hook 的函数的 source 和 pc
        let (caller_source, caller_pc) = if hook_base > 0 && hook_base <= state.stack.len() {
            if let TValue::LClosure(prev_closure) = &state.stack[hook_base - 1] {
                let src = prev_closure
                    .proto
                    .source
                    .as_ref()
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_else(|| "=?".to_string());
                (src, state.pc)
            } else {
                // C 函数或其它类型
                ("=[C]".to_string(), 0)
            }
        } else {
            ("=?".to_string(), 0)
        };

        state.call_info.push(crate::state::CallInfoEntry {
            source: caller_source,
            line: line,
            name: "?".to_string(),
            is_c: false,
            closure: hook_closure,
            base: hook_base,
            saved_pc: caller_pc,
            namewhat: "hook".to_string(),
            proto_flag: state.proto_flag,
            nextraargs: state.nextraargs,
            is_tailcall: false,
        });

        // 调用 hook 函数 (2 个参数, 0 个返回值)
        // 临时保存并清空 call_stack，防止 hook 函数 RETURN0 时 op_return0
        // 弹出错误的 CallFrame（pcall 不推入 call_stack，否则会 pop 掉
        // 触发 hook 的函数调用者帧）。清空后 op_return0 走 else 分支
        // 返回 VmResult::Return，execute_loop 正常返回到 pcall。
        let saved_call_stack = std::mem::take(&mut state.call_stack);
        let status = state.pcall(2, 0, 0);
        state.call_stack = saved_call_stack;

        // 弹出 CallInfoEntry
        state.call_info.pop();

        // 如果出错，从栈上获取错误消息（pcall 把错误消息推入 func_idx 位置）
        let err_msg = if status != 0 {
            // pcall 出错时，错误消息在 saved_top 位置（func_idx）
            let msg = if saved_top < state.stack.len() {
                match &state.stack[saved_top] {
                    TValue::Str(s) => s.as_str().to_string(),
                    _ => String::new(),
                }
            } else {
                String::new()
            };
            Some(msg)
        } else {
            None
        };

        // 恢复栈顶
        state.stack.truncate(saved_top);

        // 对应 C 的 L->allowhook = 1 (恢复 hook)
        state.allowhook = true;

        if let Some(msg) = err_msg {
            let msg = if msg.is_empty() {
                "error in hook function".to_string()
            } else {
                msg
            };
            return Err(VmError::RuntimeError(msg));
        }

        Ok(())
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
            let val = {
                let uv_ref = state.closure_upvals[b].borrow();
                match &*uv_ref {
                    UpVal::Closed { value } => (**value).clone(),
                    UpVal::Open { stack_index, .. } => {
                        state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
                    }
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
            let mut uv_ref = state.closure_upvals[b].borrow_mut();
            match &mut *uv_ref {
                UpVal::Closed { value } => {
                    state.gc.cond_gc();
                    **value = val;
                }
                UpVal::Open { stack_index, .. } => {
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
            let uv_ref = state.closure_upvals[b].borrow();
            match &*uv_ref {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { stack_index, .. } => state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict)),
            }
        } else {
            TValue::Nil(NilKind::Strict)
        };
        let result = Self::table_get(state, &upval_val, &key)?;
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
        let result = Self::table_get(state, &table_val, &key)?;
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
        let result = Self::table_get(state, &table_val, &key)?;
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
        let result = Self::table_get(state, &table_val, &key)?;
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
            let uv_ref = state.closure_upvals[a].borrow();
            match &*uv_ref {
                UpVal::Closed { value } => (**value).clone(),
                UpVal::Open { stack_index, .. } => state.stack.get(*stack_index).cloned().unwrap_or(TValue::Nil(NilKind::Strict)),
            }
        } else {
            TValue::Nil(NilKind::Strict)
        };
        // table_set 通过 Rc<RefCell<TableData>> 的内部可变性修改表，
        // 不需要写回 upval_val
        Self::table_set(state, &upval_val, key, val)?;
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
        Self::table_set(state, &table_val, key, val)?;
        state.pc += 1;
        Ok(())
    }

    fn op_seti(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as i64;
        let c = opcodes::getarg_c(inst);
        let table_val = Self::read_stack(state, a).clone();
        let val = Self::resolve_val(state, inst, c);
        Self::table_set(state, &table_val, TValue::Integer(b), val)?;
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
        Self::table_set(state, &table_val, key, val)?;
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
        }
        // C 总是 pc++ 跳过 extra argument（无论 k 是否为真）
        state.pc += 1;  // skip extra argument
        let hash_size = if b > 0 { 1u32 << (b - 1) } else { 0 };
        let array_size = c as usize;
        state.maybe_collect_gc();
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
        let result = Self::table_get(state, &obj, &key)?;
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
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_binary(&v1, &v2, |a, b| a + b, |a, b| a.wrapping_add(b));
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_subk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_binary(&v1, &v2, |a, b| a - b, |a, b| a.wrapping_sub(b));
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_mulk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_binary(&v1, &v2, |a, b| a * b, |a, b| a.wrapping_mul(b));
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_modk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_mod(&v1, &v2)?;
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_powk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        if let (Some(n1), Some(n2)) = (to_number_ns(&v1), to_number_ns(&v2)) {
            Self::write_stack(state, a, TValue::Float(n1.powf(n2)));
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_divk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        if let (Some(n1), Some(n2)) = (to_number_ns(&v1), to_number_ns(&v2)) {
            Self::write_stack(state, a, TValue::Float(n1 / n2));
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_idivk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_idiv(&v1, &v2)?;
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_bandk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        if let (Some(i1), TValue::Integer(i2)) = (to_integer_ns(&v1, F2IMode::Eq), &v2) {
            Self::write_stack(state, a, TValue::Integer(i1 & i2));
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_bork(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        if let (Some(i1), TValue::Integer(i2)) = (to_integer_ns(&v1, F2IMode::Eq), &v2) {
            Self::write_stack(state, a, TValue::Integer(i1 | i2));
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_bxork(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c_key = opcodes::getarg_c(inst) as usize;
        let v2 = state.constants.get(c_key).cloned().unwrap_or(TValue::Nil(NilKind::Strict));
        let v1 = Self::read_stack(state, b).clone();
        if let (Some(i1), TValue::Integer(i2)) = (to_integer_ns(&v1, F2IMode::Eq), &v2) {
            Self::write_stack(state, a, TValue::Integer(i1 ^ i2));
            state.pc += 2;  // skip MMBINK
        } else {
            state.pc += 1;  // fall through to MMBINK
        }
        Ok(())
    }

    fn op_shli(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        // C 使用 GETARG_sC (有符号常量), 与 C 版本一致
        let ic = opcodes::getarg_sc(inst) as i64;
        let v = Self::read_stack(state, b).clone();
        if let Some(ib) = to_integer_ns(&v, F2IMode::Eq) {
            Self::write_stack(state, a, TValue::Integer(shiftl(ic, ib)));
            state.pc += 2;  // skip MMBINI
        } else {
            state.pc += 1;  // fall through to MMBINI
        }
        Ok(())
    }

    fn op_shri(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        // C 使用 GETARG_sC (有符号常量), 与 C 版本一致
        let ic = opcodes::getarg_sc(inst) as i64;
        let v = Self::read_stack(state, b).clone();
        if let Some(ib) = to_integer_ns(&v, F2IMode::Eq) {
            Self::write_stack(state, a, TValue::Integer(shiftl(ib, -ic)));
            state.pc += 2;  // skip MMBINI
        } else {
            state.pc += 1;  // fall through to MMBINI
        }
        Ok(())
    }

    fn op_add(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: op_arith — if both numbers, compute and pc++ (skip MMBIN); else fall through
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_binary(&v1, &v2, |a, b| a + b, |a, b| a.wrapping_add(b));
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_sub(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_binary(&v1, &v2, |a, b| a - b, |a, b| a.wrapping_sub(b));
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;
        }
        Ok(())
    }

    fn op_mul(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_binary(&v1, &v2, |a, b| a * b, |a, b| a.wrapping_mul(b));
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;
        }
        Ok(())
    }

    fn op_mod(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_mod(&v1, &v2)?;
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_pow(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if let (Some(n1), Some(n2)) = (to_number_ns(&v1), to_number_ns(&v2)) {
            Self::write_stack(state, a, TValue::Float(n1.powf(n2)));
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_div(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if let (Some(n1), Some(n2)) = (to_number_ns(&v1), to_number_ns(&v2)) {
            Self::write_stack(state, a, TValue::Float(n1 / n2));
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_idiv(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if v1.is_number() && v2.is_number() {
            let result = Self::arith_idiv(&v1, &v2)?;
            Self::write_stack(state, a, result);
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_band(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(&v1, F2IMode::Eq),
            to_integer_ns(&v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(i1 & i2));
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_bor(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(&v1, F2IMode::Eq),
            to_integer_ns(&v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(i1 | i2));
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_bxor(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(&v1, F2IMode::Eq),
            to_integer_ns(&v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(i1 ^ i2));
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_shl(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(&v1, F2IMode::Eq),
            to_integer_ns(&v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(shiftl(i1, i2)));
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_shr(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let c = Self::rc(state, inst);
        let v1 = Self::read_stack(state, b).clone();
        let v2 = Self::read_stack(state, c).clone();
        if let (Some(i1), Some(i2)) = (
            to_integer_ns(&v1, F2IMode::Eq),
            to_integer_ns(&v2, F2IMode::Eq),
        ) {
            Self::write_stack(state, a, TValue::Integer(shiftl(i1, -i2)));
            state.pc += 2;  // skip MMBIN
        } else {
            state.pc += 1;  // fall through to MMBIN
        }
        Ok(())
    }

    fn op_mmbin(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: ra = RA(i), rb = vRB(i), tm = GETARG_C(i), result = RA(pi)
        // C: luaT_trybinTM(L, s2v(ra), rb, result, tm)
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let p1 = Self::read_stack(state, a).clone();
        let p2 = Self::read_stack(state, b).clone();
        let tm_idx = opcodes::getarg_c(inst) as u8;

        if let Some(tm) = TagMethod::from_u8(tm_idx) {
            // result = RA(pi), pi = 前一条指令 (原始算术指令)
            let pi = state.code[state.pc - 1];
            let result = Self::ra(state, pi);
            // varinfo_str 需要相对于 base 的寄存器编号，而非绝对栈位置
            let p1_info = varinfo_str(state, opcodes::getarg_a(inst) as usize);
            let p2_info = varinfo_str(state, opcodes::getarg_b(inst) as usize);
            try_bin_tm(state, &p1, &p2, result, tm, p1_info, p2_info)?;
        }
        state.pc += 1;
        Ok(())
    }

    fn op_mmbini(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: ra = RA(i), imm = GETARG_sB(i), tm = GETARG_C(i), flip = GETARG_k(i)
        // C: result = RA(pi)
        // C: luaT_trybiniTM(L, s2v(ra), imm, flip, result, tm)
        let a = Self::ra(state, inst);
        let imm = opcodes::getarg_b(inst) - 127;
        let p1 = Self::read_stack(state, a).clone();
        let flip = opcodes::testarg_k(inst);
        let tm_idx = opcodes::getarg_c(inst) as u8;
        if let Some(tm) = TagMethod::from_u8(tm_idx) {
            let pi = state.code[state.pc - 1];
            let result = Self::ra(state, pi);
            let p1_info = varinfo_str(state, opcodes::getarg_a(inst) as usize);
            try_bini_tm(state, &p1, imm as i64, flip, result, tm, p1_info)?;
        }
        state.pc += 1;
        Ok(())
    }

    fn op_mmbink(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: ra = RA(i), imm = KB(i), tm = GETARG_C(i), flip = GETARG_k(i)
        // C: result = RA(pi)
        // C: luaT_trybinassocTM(L, s2v(ra), imm, flip, result, tm)
        let a = Self::ra(state, inst);
        let b = opcodes::getarg_b(inst) as usize;
        let p1 = Self::read_stack(state, a).clone();
        let p2 = state.constants.get(b)
            .cloned()
            .unwrap_or(TValue::Nil(NilKind::Strict));
        let flip = opcodes::testarg_k(inst);
        let tm_idx = opcodes::getarg_c(inst) as u8;
        if let Some(tm) = TagMethod::from_u8(tm_idx) {
            let pi = state.code[state.pc - 1];
            let result = Self::ra(state, pi);
            let p1_info = varinfo_str(state, opcodes::getarg_a(inst) as usize);
            try_bin_assoc_tm(state, &p1, &p2, flip, result, tm, p1_info, String::new())?;
        }
        state.pc += 1;
        Ok(())
    }

    fn op_unm(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: ra = RA(i), rb = vRB(i)
        // C: if integer: setivalue(s2v(ra), -ib)
        // C: if float: setfltvalue(s2v(ra), -nb)
        // C: else: luaT_trybinTM(L, rb, rb, ra, TM_UNM)
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b).clone();
        match v {
            TValue::Integer(i) => Self::write_stack(state, a, TValue::Integer(i.wrapping_neg())),
            TValue::Float(f) => Self::write_stack(state, a, TValue::Float(-f)),
            _ => {
                // result = ra (与 C 一致)
                // UNM 不是位运算，走 opinterror，不需要变量信息
                try_bin_tm(state, &v, &v, a, TagMethod::Unm, String::new(), String::new())?;
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_bnot(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: ra = RA(i), rb = vRB(i)
        // C: if tointegerns(rb, &ib): setivalue(s2v(ra), ~ib)
        // C: else: luaT_trybinTM(L, rb, rb, ra, TM_BNOT)
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b).clone();
        if let Some(i) = to_integer_ns(&v, F2IMode::Eq) {
            Self::write_stack(state, a, TValue::Integer(!i));
        } else {
            // result = ra (与 C 一致)
            let info = varinfo_str(state, opcodes::getarg_b(inst) as usize);
            try_bin_tm(state, &v, &v, a, TagMethod::BNot, info.clone(), info)?;
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
        // C: StkId ra = RA(i); Protect(luaV_objlen(L, ra, vRB(i)));
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b).clone();
        obj_len(state, a, &v)?;
        state.pc += 1;
        Ok(())
    }

    fn op_concat(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: StkId ra = RA(i); int n = GETARG_B(i);
        //     L->top.p = ra + n; ProtectNT(luaV_concat(L, n));
        let a = Self::ra(state, inst);
        let n = opcodes::getarg_b(inst) as usize;
        // 确保栈上有 n 个值
        while state.stack.len() < a + n {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
        // 设置 top = a + n (标记拼接操作数的结束)
        state.stack.truncate(a + n);

        // 尝试直接拼接 (对应 C 的 luaV_concat)
        // concat_stack 在栈上操作，失败时返回 ConcatError
        // 注意: 即使返回 Err, vals 也可能已被部分拼接 (栈顶的 string 序列),
        // 必须保留 vals 以便写回 state.stack
        let concat_result = {
            let mut vals: Vec<TValue> = state.stack[a..a + n].to_vec();
            match concat_stack(&mut vals, n) {
                Ok(()) => Ok(vals),
                Err(e) => Err((e, vals)),
            }
        };

        match concat_result {
            Ok(vals) => {
                // 拼接成功: 结果在 vals[0]
                let result = vals.into_iter().next().unwrap_or_else(|| {
                    TValue::Str(crate::strings::LuaString::Short(
                        std::sync::Arc::new(crate::strings::ShortString { hash: 0, contents: String::new() })
                    ))
                });
                Self::write_stack(state, a, result);
                // 清除剩余槽位
                for i in 1..n {
                    if a + i < state.stack.len() {
                        state.stack[a + i] = TValue::Nil(NilKind::Strict);
                    }
                }
            }
            Err((_, vals)) => {
                // 拼接失败: 尝试 __concat 元方法
                // 先把 concat_stack 部分拼接的结果写回 state.stack
                // (否则会对已被拼接的 string 重复调用 __concat，报 "attempt to concatenate a string value")
                state.stack.truncate(a);
                state.stack.extend_from_slice(&vals);
                state.top = state.stack.len();
                // C: luaT_tryconcatTM(L) — p1 = top-2, p2 = top-1, res = p1
                // 循环处理，每次处理 2 个值
                loop {
                    let top = state.stack.len();
                    if top < a + 2 {
                        break;
                    }
                    let p1 = state.stack[top - 2].clone();
                    let p2 = state.stack[top - 1].clone();
                    // try_concat_tm 将结果写入 res (top-2)
                    try_concat_tm(state, &p1, &p2, top - 2)?;
                    // 移除 top-1
                    state.stack.truncate(top - 1);
                    state.top = state.stack.len();
                    // 尝试再次直接拼接剩余值
                    let remaining = state.stack.len() - a;
                    if remaining <= 1 {
                        break;
                    }
                    let mut vals: Vec<TValue> = state.stack[a..a + remaining].to_vec();
                    match concat_stack(&mut vals, remaining) {
                        Ok(()) => {
                            // 拼接成功，替换栈上的值
                            for (i, v) in vals.into_iter().enumerate() {
                                state.stack[a + i] = v;
                            }
                            state.stack.truncate(a + 1);
                            break;
                        }
                        Err(crate::tm::TagMethodError::ConcatError { .. }) => {
                            // concat_stack 可能已拼接栈顶的 string 序列(在 vals 中),
                            // 必须写回 state.stack 以反映已拼接的结果,
                            // 否则下次循环会错误地对两个 string 调用 __concat
                            state.stack.truncate(a);
                            state.stack.extend_from_slice(&vals);
                            state.top = state.stack.len();
                            continue;
                        }
                        Err(_) => {
                            return Err(VmError::RuntimeError("concat error".into()));
                        }
                    }
                }
                // 结果在 stack[a]
                // 清除剩余槽位
                while state.stack.len() > a + 1 {
                    state.stack.pop();
                }
            }
            Err(_) => {
                return Err(VmError::RuntimeError("concat error".into()));
            }
        }
        state.pc += 1;
        Ok(())
    }

    fn op_close(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        crate::func::close(state, a, 0, 1)?;
        state.pc += 1;
        Ok(())
    }

    fn op_tbc(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        crate::func::new_tbc_upval(state, a)?;
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
        // C: StkId ra = RA(i); TValue *rb = vRB(i);
        //     Protect(cond = luaV_equalobj(L, s2v(ra), rb));
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v1 = Self::read_stack(state, a).clone();
        let v2 = Self::read_stack(state, b).clone();
        let cond = equal_obj(state, &v1, &v2)?;
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_lt(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: op_order(L, l_lti, LTnum, lessthanothers)
        // lessthanothers: if (string) strcmp; else luaT_callorderTM(L, l, r, TM_LT)
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v1 = Self::read_stack(state, a).clone();
        let v2 = Self::read_stack(state, b).clone();
        let cond = if v1.is_number() && v2.is_number() {
            crate::vm::lt_num(&v1, &v2)
        } else if let (TValue::Str(s1), TValue::Str(s2)) = (&v1, &v2) {
            crate::vm::strcmp(s1, s2) == std::cmp::Ordering::Less
        } else {
            call_order_tm(state, &v1, &v2, TagMethod::Lt)?
        };
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_le(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: op_order(L, l_lei, LEnum, lessequalothers)
        // lessequalothers: if (string) strcmp; else luaT_callorderTM(L, l, r, TM_LE)
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v1 = Self::read_stack(state, a).clone();
        let v2 = Self::read_stack(state, b).clone();
        let cond = if v1.is_number() && v2.is_number() {
            crate::vm::le_num(&v1, &v2)
        } else if let (TValue::Str(s1), TValue::Str(s2)) = (&v1, &v2) {
            crate::vm::strcmp(s1, s2) != std::cmp::Ordering::Greater
        } else {
            call_order_tm(state, &v1, &v2, TagMethod::Le)?
        };
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_eqk(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b_key = opcodes::getarg_b(inst) as usize;
        let v1 = Self::read_stack(state, a);
        let v2 = state.constants.get(b_key).unwrap();
        let cond = raw_equal(v1, v2);
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_eqi(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        // EQI 是 IABC 模式,使用 sB 参数 (有符号 B, 8 位)
        // 对应 C: int im = GETARG_sB(i);
        let im = opcodes::getarg_sb(inst) as i64;
        let v = Self::read_stack(state, a);
        let cond = match v {
            TValue::Integer(i) => *i == im,
            TValue::Float(f) => (*f - im as f64).abs() < f64::EPSILON,
            _ => false,
        };
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_lti(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: op_orderI(L, l_lti, luai_numlt, 0, TM_LT)
        // flip = 0, event = LT → __lt(a, im)
        // C 字段 (isfloat): 原常量是否为浮点数（如 5.0）
        let a = Self::ra(state, inst);
        let im = opcodes::getarg_sb(inst) as i64;
        let isfloat = opcodes::getarg_c(inst) != 0;
        let v = Self::read_stack(state, a).clone();
        let cond = match &v {
            TValue::Integer(i) => *i < im,
            TValue::Float(f) => *f < (im as f64),
            _ => crate::tm::call_orderi_tm(state, &v, im, false, isfloat, TagMethod::Lt)?,
        };
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_lei(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: op_orderI(L, l_lei, luai_numle, 0, TM_LE)
        // flip = 0, event = LE → __le(a, im)
        let a = Self::ra(state, inst);
        let im = opcodes::getarg_sb(inst) as i64;
        let isfloat = opcodes::getarg_c(inst) != 0;
        let v = Self::read_stack(state, a).clone();
        let cond = match &v {
            TValue::Integer(i) => *i <= im,
            TValue::Float(f) => *f <= (im as f64),
            _ => crate::tm::call_orderi_tm(state, &v, im, false, isfloat, TagMethod::Le)?,
        };
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_gti(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: op_orderI(L, l_gti, luai_numgt, 1, TM_LT)
        // flip = 1, event = LT → __lt(im, a)  (a > im 等价于 im < a)
        let a = Self::ra(state, inst);
        let im = opcodes::getarg_sb(inst) as i64;
        let isfloat = opcodes::getarg_c(inst) != 0;
        let v = Self::read_stack(state, a).clone();
        let cond = match &v {
            TValue::Integer(i) => *i > im,
            TValue::Float(f) => *f > (im as f64),
            _ => crate::tm::call_orderi_tm(state, &v, im, true, isfloat, TagMethod::Lt)?,
        };
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_gei(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: op_orderI(L, l_gei, luai_numge, 1, TM_LE)
        // flip = 1, event = LE → __le(im, a)  (a >= im 等价于 im <= a)
        let a = Self::ra(state, inst);
        let im = opcodes::getarg_sb(inst) as i64;
        let isfloat = opcodes::getarg_c(inst) != 0;
        let v = Self::read_stack(state, a).clone();
        let cond = match &v {
            TValue::Integer(i) => *i >= im,
            TValue::Float(f) => *f >= (im as f64),
            _ => crate::tm::call_orderi_tm(state, &v, im, true, isfloat, TagMethod::Le)?,
        };
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_test(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let v = Self::read_stack(state, a);
        let cond = !is_false(v);
        Self::do_conditional_jump(state, inst, cond);
        Ok(())
    }

    fn op_testset(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let b = Self::rb(state, inst);
        let v = Self::read_stack(state, b).clone();
        let cond = !is_false(&v);
        let expected = opcodes::testarg_k(inst);
        if cond == expected {
            // 对应 C: setobj2s(L, ra, rb); donextjump(ci);
            Self::write_stack(state, a, v);
            let jmp_pc = state.pc + 1;
            if jmp_pc < state.code.len() {
                let jmp_inst = state.code[jmp_pc];
                let sj = opcodes::getarg_sj(jmp_inst);
                state.pc = ((jmp_pc as i32) + sj + 1) as usize;
            } else {
                state.pc = jmp_pc;  // 越界，跳出循环
            }
        } else {
            // 对应 C: pc++ (跳过 JMP)
            state.pc += 2;  // 跳过 TESTSET 和 JMP
        }
        Ok(())
    }

    // ---- 调用 / 返回 ----

    fn op_call(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let mut b = opcodes::getarg_b(inst) as usize;
        let c = opcodes::getarg_c(inst) as i32;
        let mut func_val = Self::read_stack(state, a).clone();

        // __call 元方法支持 — 对应 C 的 luaT_tryfuncTM + precall 的 goto retry
        // 循环处理 __call 链:当 __call 元方法本身也是表时,继续查找其 __call,
        // 直到找到真正的可调用对象 (LClosure/LCFn/CClosure/LightUserData)。
        // 对应 C precall 中的 default 分支: func = luaT_tryfuncTM(L, func); goto retry;
        // MAX_CCMT = 0xf (15): 对应 C 的 4 位计数器,超过 15 层时报 "too long"
        let mut chain_len: usize = 0;
        loop {
            // 检查是否是 coroutine.wrap 返回的 Table（GC 跟踪，可被回收）
            if let Some(idx) = crate::stdlib::coroutine_lib::get_wrap_idx(&func_val) {
                let tag = crate::stdlib::coroutine_lib::CORO_WRAP_CALL_BASE + idx;
                let nargs = if b == 0 { state.stack.len().saturating_sub(a + 1) } else { b.saturating_sub(1) };
                let nresults = c - 1;
                crate::stdlib::coroutine_lib::call_wrap_call(
                    tag, state, a, nargs, nresults,
                )?;
                state.pc += 1;
                return Ok(());
            }
            if let TValue::Table(ref t) = func_val {
                let mt_opt = t.get_metatable();
                let call_fn = mt_opt.as_ref().and_then(|mt| {
                    let call_key = TValue::Str(state.intern_str("__call"));
                    mt.get(&call_key)
                });
                if let Some(call_fn) = call_fn {
                    // 对应 C: for (st = L->top; st > func; st--) setobj(st, st-1);
                    // 在位置 a 插入 __call 函数,原 table 和所有参数右移 1 位
                    // 调用变为 __call(original_value, original_args...)
                    state.stack.insert(a, call_fn.clone());
                    // b 增加 1 以反映额外的 self 参数 (MULTRET 时 b=0 不变,
                    // nargs 从 stack.len() 计算,自动包含额外元素)
                    if b > 0 { b += 1; }
                    // 对应 C: if ((status & MAX_CCMT) == MAX_CCMT) luaG_runerror(...)
                    if chain_len >= MAX_CALL_CHAIN {
                        return Err(VmError::RuntimeError(
                            "'__call' chain too long".to_string(),
                        ));
                    }
                    chain_len += 1;
                    func_val = call_fn;
                    continue;  // 对应 C 的 goto retry
                }
                let type_name = state.typename(func_val.ty());
                return Err(VmError::RuntimeError(format!("attempt to call a {} value", type_name)));
            }
            break;
        }

        match func_val {
            TValue::LClosure(closure) => {
                let nargs = if b == 0 { state.stack.len().saturating_sub(a + 1) } else { b.saturating_sub(1) };
                let nresults = c - 1;  // -1 表示 MULTRET (对应 C 的 nresults = GETARG_C(i) - 1)
                let fsize = closure.proto.max_stack_size as usize;
                let nfixparams = closure.proto.num_params as usize;
                let proto_is_vararg = closure.proto.is_vararg();

                // 获取调用前的源和行号（用于 traceback）
                let (caller_source, caller_line) = if state.base > 0 && state.base <= state.stack.len() {
                    if let TValue::LClosure(prev_closure) = &state.stack[state.base - 1] {
                        let src = prev_closure
                            .proto
                            .source
                            .as_ref()
                            .map(|s| s.as_str().to_string())
                            .unwrap_or_else(|| "=?".to_string());
                        let line = get_proto_line(&prev_closure.proto, state.pc);
                        (src, line)
                    } else {
                        ("=?".to_string(), -1)
                    }
                } else {
                    ("=?".to_string(), -1)
                };

                // 获取函数名和 namewhat（对应 C 的 funcnamefromcode）
                let (func_name, func_namewhat) = get_func_name(state, state.pc);

                // 检查 C 调用深度 (对应 C 的 luaE_incCstack / luaE_checkcstack)
                // 每次 Lua 闭包调用递增 n_ccalls,达到 LUAI_MAXCCALLS(200) 时
                // 抛出 "C stack overflow",防止无限递归导致内存耗尽。
                state.n_ccalls = state.n_ccalls.saturating_add(1);
                if state.n_ccalls >= LUAI_MAXCCALLS {
                    state.n_ccalls = state.n_ccalls.saturating_sub(1);
                    return Err(VmError::RuntimeError("C stack overflow".to_string()));
                }

                state.call_stack.push(CallFrame {
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
                    proto_flag: state.proto_flag,
                    nextraargs: state.nextraargs,
                    closure_upvals: std::mem::take(&mut state.closure_upvals),
                    tbc_list: state.tbc_list.take(),
                    open_upval: state.open_upval.take(),
                });

                // 推入调用栈信息（用于 debug.getinfo 和 traceback）
                state.call_info.push(crate::state::CallInfoEntry {
                    source: caller_source,
                    line: caller_line,
                    name: func_name,
                    is_c: false,
                    closure: Some(closure.clone()),
                    base: state.base,
                    saved_pc: state.pc,
                    namewhat: func_namewhat,
                    proto_flag: state.proto_flag,
                    nextraargs: state.nextraargs,
                    is_tailcall: false,
                });

                state.code = closure.proto.code.clone();
                state.constants = closure.proto.constants.clone();
                state.upval_descs = closure.proto.upvalues.clone();
                state.protos = closure.proto.protos.clone();
                state.base = a + 1;
                state.pc = 0;
                state.num_params = closure.proto.num_params;
                state.is_vararg = closure.proto.is_vararg();
                state.proto_flag = closure.proto.flag;
                state.nextraargs = 0;
                // 关键: 将闭包的上值转移到 state，供 GETUPVAL/SETUPVAL 使用
                state.closure_upvals = closure.upvals.borrow().clone();
                state.tbc_list = None;
                state.open_upval = None;
                // 对应 C 的 luaD_hookcall: L->oldpc = 0
                // 新函数的 oldpc 设为 0，第一条指令会触发 hook（因为 0 不是有效 pc）
                state.hook_old_pc = 0;

                if proto_is_vararg {
                    // vararg 函数: 截断栈到实际参数末尾，VARARGPREP 会处理变参并扩展栈
                    // 对应 C 的 L->top = ra + b (OP_CALL 中设置)
                    state.stack.truncate(a + 1 + nargs);
                    // 填充不足的固定参数为 nil
                    for i in nargs..nfixparams {
                        Self::write_stack(state, a + 1 + i, TValue::Nil(NilKind::Strict));
                    }
                    // call hook 对 vararg 函数在 VARARGPREP 中触发
                } else {
                    // 非 vararg 函数: 直接扩展到 fsize
                    let frame_end = a + 1 + fsize;
                    while state.stack.len() < frame_end {
                        state.stack.push(TValue::Nil(NilKind::Strict));
                    }
                    for i in nargs..nfixparams {
                        state.stack[a + 1 + i] = TValue::Nil(NilKind::Strict);
                    }
                    // 对应 C 的 luaG_tracecall -> luaD_hookcall: 触发 call hook
                    if state.hook_mask & 1 != 0 {  // LUA_MASKCALL
                        // ftransfer=1 (参数从 func+1 开始), ntransfer=numparams
                        Self::call_hook(state, "call", -1, None, 1, nfixparams as i32)?;
                    }
                }
                Ok(())
            }
            TValue::LightUserData(tag) => {
                let tag_val = tag as usize;
                let nargs = if b == 0 { state.stack.len().saturating_sub(a + 1) } else { b.saturating_sub(1) };
                let nresults: i32 = if c == 0 { -1 } else { c - 1 };

                // 对应 C 的 precallC: 推入 CallInfoEntry 并在整个 C 函数执行期间保留
                // 这样 debug.getinfo/traceback 能正确看到 C 函数帧
                // 对应 C 的 luaD_precall -> inc_ci 创建新的 CallInfo
                //
                // 性能优化: 不在每次 C 函数调用时调用 get_func_name (对应 C 的
                // funcnamefromcode), 因为它内部调用 find_set_reg 遍历 0..call_pc
                // 的所有指令 (O(n))。C 实现只在 traceback 时才计算函数名。
                // 这里用 tag 映射获取函数名, namewhat 设为 "function"。
                let c_name: String = if crate::stdlib::base_lib::is_base_tag(tag_val) {
                    crate::stdlib::base_lib::base_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::math_lib::is_math_tag(tag_val) {
                    crate::stdlib::math_lib::math_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::utf8_lib::is_utf8_tag(tag_val) {
                    crate::stdlib::utf8_lib::utf8_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::table_lib::is_table_tag(tag_val) {
                    crate::stdlib::table_lib::table_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::debug_lib::is_debug_tag(tag_val) {
                    crate::stdlib::debug_lib::debug_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::os_lib::is_os_tag(tag_val) {
                    crate::stdlib::os_lib::os_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::coroutine_lib::is_coro_tag(tag_val) {
                    crate::stdlib::coroutine_lib::coro_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::string_lib::is_string_tag(tag_val) {
                    crate::stdlib::string_lib::string_function_name(tag_val).map(|s| s.to_string())
                } else {
                    None
                }.unwrap_or_default();
                let c_namewhat = if c_name.is_empty() { String::new() } else { "function".to_string() };
                state.call_info.push(crate::state::CallInfoEntry {
                    source: "=[C]".to_string(),
                    line: -1,
                    name: c_name.clone(),
                    is_c: true,
                    closure: None,
                    base: a + 1,
                    saved_pc: state.pc,
                    namewhat: c_namewhat.clone(),
                    proto_flag: state.proto_flag,
                    nextraargs: state.nextraargs,
                    is_tailcall: false,
                });

                // 对应 C 的 luaG_tracecall -> luaD_hookcall: 触发 call hook
                if state.hook_mask & 1 != 0 {  // LUA_MASKCALL
                    // ftransfer=1 (参数从 func+1 开始), ntransfer=narg
                    Self::call_hook(state, "call", -1, Some(a + 1), 1, nargs as i32)?;
                }

                // 基础库函数派发
                // 对应原 C 源码 lbaselib.cpp 的各个函数
                // 注意: ipairsaux (迭代器) 只在 TFORCALL 中调用, 不在此处理
                // 普通库函数调用（对应 C 的 luaD_call）不递增 n_ny_calls。
                // 只有 luaD_callnoyield 场景（__close/__gc 元方法、lua_call C API、
                // 错误处理等）才递增。pcall/xpcall 是 CIST_YPCALL（yieldable protected）。

                let dispatch_result = if crate::stdlib::base_lib::is_base_tag(tag_val) {
                    crate::stdlib::base_lib::call_base_function(
                        tag_val, state, a, nargs, nresults,
                    )
                } else if crate::stdlib::math_lib::is_math_tag(tag_val) {
                    // 数学库函数（标签 200-299）
                    crate::stdlib::math_lib::call_math_function(
                        tag_val, state, a, nargs, nresults,
                    )
                } else if crate::stdlib::utf8_lib::is_utf8_tag(tag_val) {
                    // UTF-8 库函数（标签 300-309）
                    crate::stdlib::utf8_lib::call_utf8_function(
                        tag_val, state, a, nargs, nresults,
                    )
                } else if crate::stdlib::table_lib::is_table_tag(tag_val) {
                    // Table 库函数（标签 400-409）
                    crate::stdlib::table_lib::call_table_function(
                        tag_val, state, a, nargs, nresults,
                    )
                } else if crate::stdlib::debug_lib::is_debug_tag(tag_val) {
                    // Debug 库函数（标签 500-519）
                    crate::stdlib::debug_lib::call_debug_function(
                        tag_val, state, a, nargs, nresults,
                    )
                } else if crate::stdlib::os_lib::is_os_tag(tag_val) {
                    // OS 库函数（标签 600-609）
                    crate::stdlib::os_lib::call_os_function(
                        tag_val, state, a, nargs, nresults,
                    )
                } else if crate::stdlib::coroutine_lib::is_coro_tag(tag_val) {
                    // Coroutine 库函数（标签 700-709）
                    crate::stdlib::coroutine_lib::call_coro_function(
                        tag_val, state, a, nargs, nresults,
                    )
                } else if crate::stdlib::coroutine_lib::is_wrap_call_tag(tag_val) {
                    // coroutine.wrap 返回的函数（标签 710+）
                    crate::stdlib::coroutine_lib::call_wrap_call(
                        tag_val, state, a, nargs, nresults,
                    )
                } else if tag_val >= 100 {
                    // 字符串库函数（标签 100+）
                    crate::stdlib::string_lib::call_string_function(
                        tag_val, state, a, nargs, nresults,
                    )
                } else {
                    Ok(())
                };

                // （n_ny_calls 在 luaD_call 路径中不递增，无需递减）

                // yield/error 时不弹出 CallInfoEntry —
                // yield: call_resume 的 yield 分支会保存完整的 call_info
                // error: build_traceback_from_thread 依赖 call_info 中的 C 函数帧显示 error 位置
                //        execute.rs 的 build_traceback 会跳过末尾的 C 函数帧（用 last_c_function 处理）
                let is_yield = matches!(&dispatch_result, Err(VmError::Yield(_)));
                let is_error = dispatch_result.is_err();
                if !is_yield && !is_error {
                    state.call_info.pop();
                }

                // 对应 C 的 luaD_poscall -> rethook: 触发 return hook
                // yield 时也触发 return hook（对应 C Lua 中 yield 的 return 事件）
                if state.hook_mask & 2 != 0 {  // LUA_MASKRET
                    // ftransfer = firstres - func; 从 pending_return_adjust 读取结果位置
                    // (C 函数通过 adjust_results 把结果放到栈顶之上并设置了 pending_return_adjust)
                    let (ftransfer, nres) = match state.pending_return_adjust {
                        Some((_, _, n_actual, first_result_pos)) => {
                            ((first_result_pos as i32) - (a as i32), n_actual as i32)
                        }
                        None => (1, 0),
                    };
                    // 保存 pending_return_adjust，防止 hook 函数内部的 C 函数调用覆盖它
                    let saved_pending = state.pending_return_adjust.take();
                    state.call_info.push(crate::state::CallInfoEntry {
                        source: "=[C]".to_string(),
                        line: -1,
                        name: c_name,
                        is_c: true,
                        closure: None,
                        base: a + 1,
                        saved_pc: state.pc,
                        namewhat: c_namewhat,
                        proto_flag: state.proto_flag,
                        nextraargs: state.nextraargs,
                        is_tailcall: false,
                    });
                    Self::call_hook(state, "return", -1, Some(a + 1), ftransfer, nres)?;
                    state.call_info.pop();
                    // 恢复 pending_return_adjust，供 finish_pending_adjust 使用
                    state.pending_return_adjust = saved_pending;
                }

                // 执行待定的返回值调整（return hook 启用时 push_results 延迟了 adjust）
                if !is_yield {
                    state.finish_pending_adjust();
                }

                dispatch_result?;

                // 对应 C 的 rethook: C 函数返回时设置 oldpc
                state.hook_old_pc = state.pc as i32;
                state.pc += 1;
                Ok(())
            }
            TValue::LCFn(lcf) => {
                Self::call_c_function(state, a, b, c, lcf.func)?;
                Ok(())
            }
            TValue::CClosure(cc) => {
                Self::call_c_function(state, a, b, c, cc.f)?;
                Ok(())
            }
            other => {
                // 非可调用对象: 抛出 "attempt to call a {type} value" 错误
                // 对应 C 的 luaG_callerror
                let type_name = state.typename(other.ty());
                Err(VmError::RuntimeError(format!("attempt to call a {} value", type_name)))
            }
        }
    }

    /// 调用 C 函数（轻量 C 函数或 C 闭包），对应 C 的 precallC + luaD_poscall。
    ///
    /// 语义:
    /// 1. precallC: 设置 api_func_base = a，确保栈空间，调用 f(L)
    /// 2. poscall: 把栈顶 n 个结果移动到 a 位置，根据 nresults 调整栈顶
    ///
    /// 参数:
    /// - a: 函数在栈中的位置（0-based）
    /// - b: 指令的 B 操作数（参数数+1，0 表示 MULTRET）
    /// - c: 指令的 C 操作数（结果数+1，0 表示 MULTRET）
    /// - f: C 函数指针
    fn call_c_function(
        state: &mut LuaState,
        a: usize,
        _b: usize,
        c: i32,
        f: unsafe extern "C" fn(*mut c_void) -> i32,
    ) -> Result<(), VmError> {
        // nresults: -1 = MULTRET, >=0 = 固定结果数
        let nresults: i32 = if c == 0 { -1 } else { c - 1 };

        // precallC: 设置 api_func_base，确保栈空间
        let saved_api_base = state.api_func_base;
        state.api_func_base = a;
        state.n_ccalls = state.n_ccalls.saturating_add(1);

        // 确保栈空间 (LUA_MINSTACK)
        let needed_top = state.stack.len() + LUA_MINSTACK;
        while state.stack.len() < needed_top {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }

        // 调用 C 函数: n = f(L)
        // C 函数通过 capi.rs 导出的 API 操作栈，返回结果数 n
        let ptr: *mut LuaState = state;
        let n = unsafe { f(ptr as *mut c_void) };

        // poscall: 把栈顶 n 个结果移动到 a 位置
        let top = state.stack.len();
        let n = n as usize;
        let first_result = top.saturating_sub(n);

        // 恢复 api_func_base 和 n_ccalls
        state.api_func_base = saved_api_base;
        state.n_ccalls = state.n_ccalls.saturating_sub(1);

        // 移动结果到 a 位置（对应 C 的 moveresults）
        if nresults >= 0 {
            // 固定结果数
            let nresults = nresults as usize;
            let copy_count = n.min(nresults);
            // 先把结果复制到临时 Vec，避免覆盖问题
            let results: Vec<TValue> = (0..copy_count)
                .map(|i| state.stack[first_result + i].clone())
                .collect();
            // 确保 a + nresults 在栈范围内
            while state.stack.len() <= a + nresults {
                state.stack.push(TValue::Nil(NilKind::Strict));
            }
            for i in 0..copy_count {
                state.stack[a + i] = results[i].clone();
            }
            for i in copy_count..nresults {
                state.stack[a + i] = TValue::Nil(NilKind::Strict);
            }
            state.stack.truncate(a + nresults);
        } else {
            // MULTRET: 保留所有 n 个结果
            let results: Vec<TValue> = (0..n)
                .map(|i| state.stack[first_result + i].clone())
                .collect();
            while state.stack.len() < a + n {
                state.stack.push(TValue::Nil(NilKind::Strict));
            }
            for i in 0..n {
                state.stack[a + i] = results[i].clone();
            }
            state.stack.truncate(a + n);
        }

        // 对应 C 的 rethook: L->oldpc = pcRel(ci->u.l.savedpc, ci_func(ci)->p)
        // C 函数返回时，设置 oldpc 为 CALL 指令的 pc，这样下一条指令的
        // changedline 检查会正确判断行号是否变化
        state.hook_old_pc = state.pc as i32;
        state.pc += 1;
        Ok(())
    }

    fn op_tailcall(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let mut b = opcodes::getarg_b(inst) as usize;
        let mut func_val = Self::read_stack(state, a).clone();

        // 对应 C 的 OP_TAILCALL: if (TESTARG_k(i)) luaF_closeupval(L, base);
        // K 标志位表示可能有 open upvalues,必须在移动栈数据之前关闭,
        // 否则 open upvalue 指向的栈位置会被覆盖,导致 upvalue 值错误
        // (如 Z combinator 中 a 的 upvalue le 被尾调用的参数覆盖)
        if opcodes::testarg_k(inst) {
            crate::func::close(state, state.base, 0, 0)?;
        }

        // __call 元方法支持 — 对应 C 的 luaT_tryfuncTM + precall 的 goto retry
        // (同 op_call 的处理,循环解析 __call 链,处理嵌套 __call 表)
        // MAX_CCMT = 0xf (15): 对应 C 的 4 位计数器,超过 15 层时报 "too long"
        let mut chain_len: usize = 0;
        loop {
            // 检查是否是 coroutine.wrap 返回的 Table（GC 跟踪，可被回收）
            if let Some(idx) = crate::stdlib::coroutine_lib::get_wrap_idx(&func_val) {
                let tag = crate::stdlib::coroutine_lib::CORO_WRAP_CALL_BASE + idx;
                let nargs = if b == 0 { state.stack.len().saturating_sub(a + 1) } else { b.saturating_sub(1) };
                // tailcall 沿用调用者的 nresults，用 MULTRET (-1) 让 call_wrap_call 处理
                crate::stdlib::coroutine_lib::call_wrap_call(
                    tag, state, a, nargs, -1,
                )?;
                state.pc += 1;
                return Ok(());
            }
            if let TValue::Table(ref t) = func_val {
                let mt_opt = t.get_metatable();
                let call_fn = mt_opt.as_ref().and_then(|mt| {
                    let call_key = TValue::Str(state.intern_str("__call"));
                    mt.get(&call_key)
                });
                if let Some(call_fn) = call_fn {
                    state.stack.insert(a, call_fn.clone());
                    if b > 0 { b += 1; }
                    // 对应 C: if ((status & MAX_CCMT) == MAX_CCMT) luaG_runerror(...)
                    if chain_len >= MAX_CALL_CHAIN {
                        return Err(VmError::RuntimeError(
                            "'__call' chain too long".to_string(),
                        ));
                    }
                    chain_len += 1;
                    func_val = call_fn;
                    continue;
                }
                let type_name = state.typename(func_val.ty());
                return Err(VmError::RuntimeError(format!("attempt to call a {} value", type_name)));
            }
            break;
        }

        match func_val {
            TValue::LClosure(closure) => {
                let nargs_total = state.stack.len().saturating_sub(a);
                let fsize = closure.proto.max_stack_size as usize;
                let nfixparams = closure.proto.num_params as usize;
                let nargs = nargs_total.saturating_sub(1);
                let func_slot = state.base.saturating_sub(1);
                let proto_is_vararg = closure.proto.is_vararg();

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
                state.proto_flag = closure.proto.flag;
                state.nextraargs = 0;
                // 关键: 将闭包的上值转移到 state，供 GETUPVAL/SETUPVAL 使用
                state.closure_upvals = closure.upvals.borrow().clone();
                state.tbc_list = None;
                state.open_upval = None;

                // 对应 C 的 ci->callstatus |= CIST_TAIL
                // 标记当前 CallInfoEntry 为尾调用，供 debug.getinfo(1).istailcall 读取
                if let Some(entry) = state.call_info.last_mut() {
                    entry.is_tailcall = true;
                    entry.closure = Some(closure.clone());
                }
                // 对应 C 的 luaD_hookcall: L->oldpc = 0
                // 新函数的 oldpc 设为 0，第一条指令会触发 line hook
                state.hook_old_pc = 0;

                if proto_is_vararg {
                    // vararg 函数: 截断栈到实际参数末尾，VARARGPREP 会处理
                    state.stack.truncate(func_slot + 1 + nargs);
                    for i in nargs..nfixparams {
                        Self::write_stack(state, func_slot + 1 + i, TValue::Nil(NilKind::Strict));
                    }
                    // call hook 对 vararg 函数在 VARARGPREP 中触发 (tail call 事件)
                } else {
                    let frame_end = func_slot + 1 + fsize;
                    // 对应 C 的 ci->top = ci->func.p + 1 + p->maxstacksize
                    // 截断栈到帧末尾,丢弃尾调用遗留的额外元素
                    // (如 __call 链产生的中间表),防止栈无限增长导致 O(n^2) 卡死
                    if state.stack.len() > frame_end {
                        state.stack.truncate(frame_end);
                    }
                    while state.stack.len() < frame_end {
                        state.stack.push(TValue::Nil(NilKind::Strict));
                    }
                    for i in nargs..nfixparams {
                        state.stack[func_slot + 1 + i] = TValue::Nil(NilKind::Strict);
                    }
                    // 对应 C 的 startfunc -> luaG_tracecall -> luaD_hookcall:
                    // 非 vararg 尾调用触发 "tail call" hook (CIST_TAIL 已设置)
                    if state.hook_mask & 1 != 0 {  // LUA_MASKCALL
                        Self::call_hook(state, "tail call", -1, None, 1, nfixparams as i32)?;
                    }
                }
                Ok(())
            }
            TValue::LCFn(lcf) => {
                // TAILCALL C 函数: 调用后结果放在 a 位置，后续 RETURN 指令处理返回
                Self::call_c_function(state, a, b, 0, lcf.func)?;
                Ok(())
            }
            TValue::CClosure(cc) => {
                Self::call_c_function(state, a, b, 0, cc.f)?;
                Ok(())
            }
            TValue::LightUserData(tag) => {
                // TAILCALL LightUserData 函数 (基础库/字符串库函数)
                // 注意: ipairsaux (迭代器) 只在 TFORCALL 中调用, 不在此处理
                let tag_val = tag as usize;
                let nargs = if b == 0 { state.stack.len().saturating_sub(a + 1) } else { b.saturating_sub(1) };
                if crate::stdlib::base_lib::is_base_tag(tag_val) {
                    crate::stdlib::base_lib::call_base_function(
                        tag_val, state, a, nargs, -1,
                    )?;
                } else if crate::stdlib::math_lib::is_math_tag(tag_val) {
                    // 数学库函数（标签 200-299）
                    crate::stdlib::math_lib::call_math_function(
                        tag_val, state, a, nargs, -1,
                    )?;
                } else if crate::stdlib::utf8_lib::is_utf8_tag(tag_val) {
                    // UTF-8 库函数（标签 300-309）
                    crate::stdlib::utf8_lib::call_utf8_function(
                        tag_val, state, a, nargs, -1,
                    )?;
                } else if crate::stdlib::table_lib::is_table_tag(tag_val) {
                    // Table 库函数（标签 400-409）
                    crate::stdlib::table_lib::call_table_function(
                        tag_val, state, a, nargs, -1,
                    )?;
                } else if crate::stdlib::debug_lib::is_debug_tag(tag_val) {
                    // Debug 库函数（标签 500-519）
                    crate::stdlib::debug_lib::call_debug_function(
                        tag_val, state, a, nargs, -1,
                    )?;
                } else if crate::stdlib::os_lib::is_os_tag(tag_val) {
                    // OS 库函数（标签 600-609）
                    crate::stdlib::os_lib::call_os_function(
                        tag_val, state, a, nargs, -1,
                    )?;
                } else if crate::stdlib::coroutine_lib::is_coro_tag(tag_val) {
                    // Coroutine 库函数（标签 700-709）
                    crate::stdlib::coroutine_lib::call_coro_function(
                        tag_val, state, a, nargs, -1,
                    )?;
                } else if crate::stdlib::coroutine_lib::is_wrap_call_tag(tag_val) {
                    // coroutine.wrap 返回的函数（标签 710+）
                    crate::stdlib::coroutine_lib::call_wrap_call(
                        tag_val, state, a, nargs, -1,
                    )?;
                } else if tag_val >= 100 {
                    crate::stdlib::string_lib::call_string_function(
                        tag_val, state, a, nargs, -1,
                    )?;
                }
                // 对应 C 的 rethook: C 函数返回时设置 oldpc
                state.hook_old_pc = state.pc as i32;
                state.pc += 1;
                Ok(())
            }
            _ => {
                state.pc += 1;
                Ok(())
            }
        }
    }

    fn op_return(state: &mut LuaState, inst: Instruction) -> Result<Option<VmResult>, VmError> {
        let a = Self::ra(state, inst);
        let n = opcodes::getarg_b(inst) as i32 - 1;
        let nresults = if n < 0 { state.stack.len().saturating_sub(a) } else { n as usize };

        // 元方法 continuation 检查 (在 call_stack.pop() 之前)
        // resume 后 call_stack 不为空 (包含调用者帧)，所以必须在 pop 之前检查
        // 对应 C Lua 的 luaV_finishOp + unroll 机制
        {
            let result_val = if a < state.stack.len() {
                Some(state.stack[a].clone())
            } else {
                None
            };
            if Self::try_finish_metamethod(state, result_val)? {
                return Ok(None);  // 元方法 continuation 已处理，继续循环
            }
        }

        // close continuation 检查 (在 close 之前)
        // __close 函数返回时检测 is_close_continuation 的 PcallProtection
        // 恢复 close 调用者的执行上下文，重新执行 OP_RETURN/OP_CLOSE
        // 对应 C Lua 的 luaV_finishOp 对 OP_RETURN/OP_CLOSE 的 savedpc-- 机制
        if Self::finish_close_continuation(state)? {
            return Ok(None);
        }

        // 先关闭 TBC 变量和 upvalues（在 call_stack.pop() 之前）
        // 对应 C 的 OP_RETURN: luaF_close (line 1774) 在 luaD_poscall (line 1781) 之前执行
        // close yield 时不 pop call_stack，resume 后重新执行 OP_RETURN（对应 C 的 savedpc--）
        let close_result = crate::func::close(state, state.base, 0, 1);
        match close_result {
            Ok(()) => {}
            Err(e) => {
                // close yield 或出错: 不 pop call_stack，传播错误
                return Err(e);
            }
        }
        if let Some(frame) = state.call_stack.pop() {
            // 递减 C 调用深度 (对应 op_call 中递增的 n_ccalls)
            state.n_ccalls = state.n_ccalls.saturating_sub(1);
            let return_base = frame.return_base;
            let num_results = frame.num_results;
            // 对应 C 的 rethook: 触发 return hook (在 close 之后, 弹出 call_info 之前)
            // C 的 OP_RETURN: luaF_close 先执行 (可能设置 hook), 然后 luaD_poscall 触发 rethook
            if state.hook_mask & 2 != 0 {  // LUA_MASKRET
                let ftransfer = (a as i32) - (state.base as i32) + 1;
                Self::call_hook(state, "return", -1, None, ftransfer, nresults as i32)?;
            }
            // close 成功: 弹出 call_info (对应 C 的 luaD_poscall: L->ci = L->ci->previous)
            state.call_info.pop();
            // 收集返回值（在 close 之后，对应 C 的 moveresults）
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
            state.proto_flag = frame.proto_flag;
            state.nextraargs = frame.nextraargs;
            state.closure_upvals = frame.closure_upvals;
            state.tbc_list = frame.tbc_list;
            state.open_upval = frame.open_upval;
            // 对应 C 的 rethook: L->oldpc = pcRel(ci->u.l.savedpc, ci_func(ci)->p)
            // C 的 pcRel 宏为 (cast_int((pc) - (p)->code) - 1)，有 -1 偏移
            // state.pc = frame.return_pc = CALL 指令的下一条 (pc+1)
            // 因此 oldpc 应为 (pc+1) - 1 = pc，即 CALL 指令本身的索引
            state.hook_old_pc = state.pc as i32 - 1;

            if num_results >= 0 {
                while state.stack.len() < return_base + num_results as usize {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
            }
            let copy_count = if num_results >= 0 {
                results.len().min(num_results as usize)
            } else {
                results.len()
            };
            for i in 0..copy_count {
                while state.stack.len() <= return_base + i {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
                state.stack[return_base + i] = std::mem::take(&mut results[i]);
            }
            if num_results >= 0 {
                for i in copy_count..num_results as usize {
                    while state.stack.len() <= return_base + i {
                        state.stack.push(TValue::Nil(NilKind::Strict));
                    }
                    state.stack[return_base + i] = TValue::Nil(NilKind::Strict);
                }
            }
            let final_len = if num_results < 0 {
                return_base + results.len()
            } else {
                return_base + num_results as usize
            };
            state.stack.truncate(final_len);
            state.top = state.stack.len();
            Ok(None)
        } else {
            // 正常协程底部函数返回 / pcall 调用的函数返回
            // close 已在 if 分支前执行（幂等，upvalue 已关闭）
            // 对应 C 的 rethook: 触发 return hook (在 close 之后)
            // pcall 调用的 Lua 函数返回时也走此分支，需触发 return hook
            if state.hook_mask & 2 != 0 {  // LUA_MASKRET
                let ftransfer = (a as i32) - (state.base as i32) + 1;
                Self::call_hook(state, "return", -1, None, ftransfer, nresults as i32)?;
            }
            // pcall 正常返回 continuation 检查
            // yield 穿过 pcall 后，pcall 的 C 函数栈帧被销毁，但保护状态保留。
            // 当被保护的 Lua 函数返回时，由此检查处理 pcall 的正常返回。
            if Self::finish_pcall_return(state, nresults, a)? {
                return Ok(None);  // pcall continuation 已处理，继续循环
            }
            Ok(Some(VmResult::Return { nresults, result_base: a }))
        }
    }

    fn op_return0(state: &mut LuaState, _inst: Instruction) -> Result<Option<VmResult>, VmError> {
        // 元方法 continuation 检查 (在 call_stack.pop() 之前)
        // resume 后 call_stack 不为空 (包含调用者帧)，所以必须在 pop 之前检查
        // 对应 C Lua 的 luaV_finishOp + unroll 机制
        if Self::try_finish_metamethod(state, None)? {
            return Ok(None);  // 元方法 continuation 已处理，继续循环
        }
        // close continuation 检查 (在 close 之前)
        // __close 函数返回时检测 is_close_continuation 的 PcallProtection
        // 恢复 close 调用者的执行上下文，重新执行 OP_RETURN/OP_CLOSE
        // 对应 C Lua 的 luaV_finishOp 对 OP_RETURN/OP_CLOSE 的 savedpc-- 机制
        if Self::finish_close_continuation(state)? {
            return Ok(None);  // 重新执行 OP_RETURN/OP_CLOSE
        }
        // 先关闭 TBC 变量和 upvalues（在 call_stack.pop() 之前）
        // 对应 C 的 OP_RETURN: luaF_close (line 1774) 在 luaD_poscall (line 1781) 之前执行
        // close yield 时不 pop call_stack，resume 后重新执行 OP_RETURN（对应 C 的 savedpc--）
        match crate::func::close(state, state.base, 0, 1) {
            Ok(()) => {}
            Err(e) => {
                // close yield 或出错: 不 pop call_stack，传播错误
                return Err(e);
            }
        }
        if let Some(frame) = state.call_stack.pop() {
            // 递减 C 调用深度 (对应 op_call 中递增的 n_ccalls)
            state.n_ccalls = state.n_ccalls.saturating_sub(1);
            let return_base = frame.return_base;
            let num_results = frame.num_results;
            // 对应 C 的 rethook: 触发 return hook (在 close 之后, 弹出 call_info 之前)
            if state.hook_mask & 2 != 0 {  // LUA_MASKRET
                Self::call_hook(state, "return", -1, None, 0, 0)?;
            }
            // close 成功: 弹出 call_info (对应 C 的 luaD_poscall: L->ci = L->ci->previous)
            state.call_info.pop();
            state.code = frame.code;
            state.constants = frame.constants;
            state.upval_descs = frame.upval_descs;
            state.protos = frame.protos;
            state.base = frame.base;
            state.pc = frame.return_pc;
            state.num_params = frame.num_params;
            state.is_vararg = frame.is_vararg;
            state.proto_flag = frame.proto_flag;
            state.nextraargs = frame.nextraargs;
            state.closure_upvals = frame.closure_upvals;
            state.tbc_list = frame.tbc_list;
            state.open_upval = frame.open_upval;
            // 对应 C 的 rethook: L->oldpc = pcRel(ci->u.l.savedpc, ci_func(ci)->p)
            state.hook_old_pc = state.pc as i32 - 1;
            // op_return0 返回 0 个值
            // MULTRET (num_results < 0) 时: 截断到 return_base (0 个结果)
            // 固定数量时: 填充 nil 并截断到 return_base + num_results
            if num_results >= 0 {
                while state.stack.len() < return_base + num_results as usize {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
                for i in 0..num_results as usize {
                    while state.stack.len() <= return_base + i {
                        state.stack.push(TValue::Nil(NilKind::Strict));
                    }
                    state.stack[return_base + i] = TValue::Nil(NilKind::Strict);
                }
                state.stack.truncate(return_base + num_results as usize);
            } else {
                state.stack.truncate(return_base);
            }
            state.top = state.stack.len();
            Ok(None)
        } else {
            // 正常协程底部函数返回 / pcall 调用的函数返回
            // close 已在 if 分支前执行（幂等，upvalue 已关闭）
            // 对应 C 的 rethook: 触发 return hook (在 close 之后)
            if state.hook_mask & 2 != 0 {  // LUA_MASKRET
                Self::call_hook(state, "return", -1, None, 0, 0)?;
            }
            // pcall 正常返回 continuation 检查 (nret=0)
            if Self::finish_pcall_return(state, 0, state.base)? {
                return Ok(None);  // pcall continuation 已处理，继续循环
            }
            Ok(Some(VmResult::Return { nresults: 0, result_base: state.base }))
        }
    }

    fn op_return1(state: &mut LuaState, inst: Instruction) -> Result<Option<VmResult>, VmError> {
        let a = Self::ra(state, inst);
        let val = if a < state.stack.len() {
            std::mem::take(&mut state.stack[a])
        } else {
            TValue::Nil(NilKind::Strict)
        };
        // 元方法 continuation 检查 (在 call_stack.pop() 之前)
        // resume 后 call_stack 不为空 (包含调用者帧)，所以必须在 pop 之前检查
        // 对应 C Lua 的 luaV_finishOp + unroll 机制
        if Self::try_finish_metamethod(state, Some(val.clone()))? {
            return Ok(None);  // 元方法 continuation 已处理，继续循环
        }
        // close continuation 检查 (在 close 之前)
        // __close 函数返回时检测 is_close_continuation 的 PcallProtection
        // 恢复 close 调用者的执行上下文，重新执行 OP_RETURN/OP_CLOSE
        // 对应 C Lua 的 luaV_finishOp 对 OP_RETURN/OP_CLOSE 的 savedpc-- 机制
        if Self::finish_close_continuation(state)? {
            return Ok(None);  // 重新执行 OP_RETURN/OP_CLOSE
        }
        // 先关闭 TBC 变量和 upvalues（在 call_stack.pop() 之前）
        // 对应 C 的 OP_RETURN: luaF_close (line 1774) 在 luaD_poscall (line 1781) 之前执行
        // close yield 时不 pop call_stack，resume 后重新执行 OP_RETURN（对应 C 的 savedpc--）
        match crate::func::close(state, state.base, 0, 1) {
            Ok(()) => {}
            Err(e) => {
                // close yield 或出错: 不 pop call_stack，传播错误
                return Err(e);
            }
        }
        if let Some(frame) = state.call_stack.pop() {
            // 递减 C 调用深度 (对应 op_call 中递增的 n_ccalls)
            state.n_ccalls = state.n_ccalls.saturating_sub(1);
            let return_base = frame.return_base;
            let num_results = frame.num_results;
            // 对应 C 的 rethook: 触发 return hook (在 close 之后, 弹出 call_info 之前)
            if state.hook_mask & 2 != 0 {  // LUA_MASKRET
                let ftransfer = (a as i32) - (state.base as i32) + 1;
                Self::call_hook(state, "return", -1, None, ftransfer, 1)?;
            }
            // close 成功: 弹出 call_info (对应 C 的 luaD_poscall: L->ci = L->ci->previous)
            state.call_info.pop();
            state.code = frame.code;
            state.constants = frame.constants;
            state.upval_descs = frame.upval_descs;
            state.protos = frame.protos;
            state.base = frame.base;
            state.pc = frame.return_pc;
            state.num_params = frame.num_params;
            state.is_vararg = frame.is_vararg;
            state.proto_flag = frame.proto_flag;
            state.nextraargs = frame.nextraargs;
            state.closure_upvals = frame.closure_upvals;
            state.tbc_list = frame.tbc_list;
            state.open_upval = frame.open_upval;
            // 对应 C 的 rethook: L->oldpc = pcRel(ci->u.l.savedpc, ci_func(ci)->p)
            state.hook_old_pc = state.pc as i32 - 1;
            // op_return1 返回 1 个值
            // MULTRET (num_results < 0) 时: 把 val 放到 return_base, 截断到 return_base + 1
            // 固定数量时: 填充 nil 并截断到 return_base + num_results
            if num_results >= 0 {
                while state.stack.len() < return_base + num_results as usize {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
                while state.stack.len() <= return_base {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
                state.stack[return_base] = val;
                for i in 1..num_results as usize {
                    while state.stack.len() <= return_base + i {
                        state.stack.push(TValue::Nil(NilKind::Strict));
                    }
                    state.stack[return_base + i] = TValue::Nil(NilKind::Strict);
                }
                state.stack.truncate(return_base + num_results as usize);
            } else {
                while state.stack.len() <= return_base {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
                state.stack[return_base] = val;
                state.stack.truncate(return_base + 1);
            }
            state.top = state.stack.len();
            Ok(None)
        } else {
            // 正常协程底部函数返回 / pcall 调用的函数返回
            // close 已在 if 分支前执行（幂等，upvalue 已关闭）
            // 对应 C 的 rethook: 触发 return hook (在 close 之后)
            if state.hook_mask & 2 != 0 {  // LUA_MASKRET
                let ftransfer = (a as i32) - (state.base as i32) + 1;
                Self::call_hook(state, "return", -1, None, ftransfer, 1)?;
            }
            // 把返回值放到 base-1（func 位置）
            let result_base = state.base.saturating_sub(1);
            if state.base > 0 && result_base < state.stack.len() {
                state.stack[result_base] = val;
            }
            // pcall 正常返回 continuation 检查 (nret=1)
            if Self::finish_pcall_return(state, 1, result_base)? {
                return Ok(None);  // pcall continuation 已处理，继续循环
            }
            Ok(Some(VmResult::Return { nresults: 1, result_base }))
        }
    }

    // ---- 循环 ----

    fn op_forloop(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);

        // Check if this is an integer or float loop
        let count_val = Self::read_stack(state, ra);
        match count_val {
            TValue::Integer(i) => {
                let count = *i as u64;
                let step = match Self::read_stack(state, ra + 1) {
                    TValue::Integer(s) => *s,
                    _ => { state.pc += 1; return Ok(()); }
                };
                let idx = match Self::read_stack(state, ra + 2) {
                    TValue::Integer(i) => *i,
                    _ => { state.pc += 1; return Ok(()); }
                };

                if count > 0 {
                    Self::write_stack(state, ra, TValue::Integer((count - 1) as i64));
                    let new_idx = (idx as u64).wrapping_add(step as u64) as i64;
                    Self::write_stack(state, ra + 2, TValue::Integer(new_idx));
                    let bx = opcodes::getarg_bx(inst);
                    state.pc = ((state.pc as i32) - bx) as usize;
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
                    let bx = opcodes::getarg_bx(inst);
                    state.pc = ((state.pc as i32) - bx) as usize;
                }
            }
            _ => {}
        }
        state.pc += 1;
        Ok(())
    }

    fn op_forprep(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
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
                        // 对应 C 的 forlimit: float_to_integer 失败时, 根据 float 值的范围处理
                        // 正无穷或过大: 设为 MAXINTEGER; 负无穷或过小: 设为 MININTEGER
                        if *step_i < 0 {
                            float_to_integer(*f, F2IMode::Ceil).unwrap_or_else(|| {
                                if *f > 0.0 { i64::MAX } else { i64::MIN }
                            })
                        } else {
                            float_to_integer(*f, F2IMode::Floor).unwrap_or_else(|| {
                                if *f > 0.0 { i64::MAX } else { i64::MIN }
                            })
                        }
                    }
                    _ => { state.pc += 1; return Ok(()); }
                };

                let skip = if *step_i > 0 { *init_i > limit_i } else { *init_i < limit_i };
                if skip {
                    // C 代码中 vmfetch() 已递增 pc，所以 pc += GETARG_Bx(i) + 1 实际跳到 prep+bx+2。
                    // Rust 中 state.pc 指向当前指令，需要 +2 来达到相同效果（跳过 FORLOOP）。
                    let bx = opcodes::getarg_bx(inst);
                    state.pc = ((state.pc as i32) + bx + 2) as usize;
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

                if step_f == 0.0 {
                    return Err(VmError::RuntimeError("for step is zero".into()));
                }
                let skip = if step_f > 0.0 { limit_f < init_f } else { init_f < limit_f };
                if skip {
                    // 同上：+2 跳过 FORLOOP
                    let bx = opcodes::getarg_bx(inst);
                    state.pc = ((state.pc as i32) + bx + 2) as usize;
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
        let need_tbc = !closing.is_false();
        Self::write_stack(state, ra + 3, tmp);
        Self::write_stack(state, ra + 2, closing);
        if need_tbc {
            crate::func::new_tbc_upval(state, ra + 2)?;
        }
        let bx = opcodes::getarg_bx(inst);
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
        let control = Self::read_stack(state, ra + 3).clone();
        match control {
            TValue::Nil(_) => { state.pc += 1; }
            _ => {
                let bx = opcodes::getarg_bx(inst);
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
                let pos = last - n_actual + i;
                t.set_int((pos + 1) as i64, val);
            }
            Self::write_stack(state, ra, TValue::Table(t));
        }
        state.pc += 1;
        Ok(())
    }

    fn op_closure(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        // C: OP_CLOSURE — 创建新闭包并初始化上值
        // 对应 C 源码:
        //   Proto *p = p->p[bx];
        //   int nup = p->sizeupvalues;
        //   for (i = 0; i < nup; i++) {
        //     if (p->upvalues[i].instack)
        //       upv[i] = luaF_findupval(L, base + p->upvalues[i].idx);
        //     else
        //       upv[i] = cl->upvals[p->upvalues[i].idx];
        //   }
        let ra = Self::ra(state, inst);
        let bx = opcodes::getarg_bx(inst) as usize;
        if bx < state.protos.len() {
            let proto = state.protos[bx].clone();
            let nup = proto.size_upvalues as usize;
            let mut upvals: Vec<UpValRef> = Vec::with_capacity(nup);
            for i in 0..nup {
                if i < proto.upvalues.len() {
                    let desc = &proto.upvalues[i];
                    if desc.in_stack {
                        // 上值来自当前栈帧: 通过 find_upval 创建/查找 Open 上值
                        // 对应 C: upv[i] = luaF_findupval(L, base + p->upvalues[i].idx);
                        let stack_idx = state.base + desc.idx as usize;
                        let uv_idx = crate::func::find_upval(state, stack_idx);
                        upvals.push(state.closure_upvals[uv_idx].clone());
                    } else {
                        // 上值来自外层闭包: 共享同一个 Rc<RefCell<UpVal>>
                        // 对应 C: upv[i] = cl->upvals[p->upvalues[i].idx];
                        let parent_idx = desc.idx as usize;
                        if parent_idx < state.closure_upvals.len() {
                            upvals.push(state.closure_upvals[parent_idx].clone());
                        } else {
                            upvals.push(Rc::new(RefCell::new(UpVal::Closed {
                                value: Box::new(TValue::Nil(NilKind::Strict)),
                            })));
                        }
                    }
                } else {
                    upvals.push(Rc::new(RefCell::new(UpVal::Closed {
                        value: Box::new(TValue::Nil(NilKind::Strict)),
                    })));
                }
            }
            let closure = LClosure { gc_header: crate::gc::GCObjectHeader::new(), proto, upvals: Rc::new(RefCell::new(upvals)) };
            Self::write_stack(state, ra, TValue::LClosure(closure));
        }
        state.pc += 1;
        Ok(())
    }

    /// VARARG: 获取变参列表（对应 C 的 OP_VARARG + luaT_getvarargs）
    ///
    /// 指令格式: A B C k
    /// - A: 目标寄存器起始位置
    /// - C - 1: 需要的结果数（0 = MULTRET，取全部）
    /// - k 位 + B: 如果 k=1，B 是 vararg 表的寄存器偏移；否则无表（PF_VAHID 模式）
    fn op_vararg(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let c = opcodes::getarg_c(inst) as i32;
        let wanted: i32 = c - 1;  // -1 = MULTRET
        let has_vatab = opcodes::testarg_k(inst);
        let vatab = if has_vatab { opcodes::getarg_b(inst) as usize } else { usize::MAX };

        if has_vatab {
            // PF_VATAB: 从 vararg 表取值
            // 表在 state.base + vatab（对应 C 的 ci->func.p + vatab + 1，因为 state.base = func + 1）
            let table_pos = state.base + vatab;
            let table_val = Self::read_stack(state, table_pos).clone();
            let nargs = if let TValue::Table(ref t) = table_val {
                if let Some(TValue::Integer(n)) = t.get(&TValue::Str(state.string_table.intern("n"))) {
                    n as usize
                } else {
                    0
                }
            } else {
                0
            };
            let touse = if wanted < 0 { nargs } else { (wanted as usize).min(nargs) };
            let need_fill = if wanted < 0 { 0 } else { (wanted as usize).saturating_sub(touse) };

            for i in 0..touse {
                let val = if let TValue::Table(ref t) = table_val {
                    t.get_int((i + 1) as i64).unwrap_or(TValue::Nil(NilKind::Strict))
                } else {
                    TValue::Nil(NilKind::Strict)
                };
                Self::write_stack(state, ra + i, val);
            }
            for i in 0..need_fill {
                Self::write_stack(state, ra + touse + i, TValue::Nil(NilKind::Strict));
            }
            if wanted < 0 {
                // MULTRET: 设置 top = ra + nargs
                state.stack.truncate(ra + touse);
            }
        } else {
            // PF_VAHID: 从隐藏变参取值
            let nextra = state.nextraargs as usize;
            // 变参在 state.base - 1 - nextra .. state.base - 2
            let vararg_start = state.base.saturating_sub(1 + nextra);
            let touse = if wanted < 0 { nextra } else { (wanted as usize).min(nextra) };
            let need_fill = if wanted < 0 { 0 } else { (wanted as usize).saturating_sub(touse) };

            for i in 0..touse {
                let val = state.stack[vararg_start + i].clone();
                Self::write_stack(state, ra + i, val);
            }
            for i in 0..need_fill {
                Self::write_stack(state, ra + touse + i, TValue::Nil(NilKind::Strict));
            }
            if wanted < 0 {
                // MULTRET: 设置 top = ra + nextra
                state.stack.truncate(ra + touse);
            }
        }
        state.pc += 1;
        Ok(())
    }

    /// GETVARG: 获取单个变参（对应 C 的 OP_GETVARG + luaT_getvararg）
    ///
    /// 指令格式: A B C
    /// - A: 目标寄存器
    /// - R[C]: 键（整数 n 取第 n 个变参，字符串 "n" 返回变参数量）
    ///
    /// 仅用于 PF_VAHID 模式（PF_VATAB 模式下编译器会生成 GETTABLE）
    fn op_getvarg(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let ra = Self::ra(state, inst);
        let c = Self::rc(state, inst);
        let key = Self::read_stack(state, c).clone();
        let nextra = state.nextraargs;

        let result = match &key {
            TValue::Integer(n) => {
                let n = *n;
                if n >= 1 && (n as usize) <= nextra as usize {
                    // 变参在 state.base - 1 - nextra .. state.base - 2
                    // 第 n 个变参在 state.base - 1 - nextra + (n - 1) = state.base - nextra + n - 2
                    let idx = state.base.saturating_sub(nextra as usize + 1).saturating_add(n as usize - 1);
                    if idx < state.stack.len() {
                        state.stack[idx].clone()
                    } else {
                        TValue::Nil(NilKind::Strict)
                    }
                } else {
                    TValue::Nil(NilKind::Strict)
                }
            }
            TValue::Str(s) => {
                if s.as_str() == "n" {
                    TValue::Integer(nextra as i64)
                } else {
                    TValue::Nil(NilKind::Strict)
                }
            }
            _ => TValue::Nil(NilKind::Strict),
        };
        Self::write_stack(state, ra, result);
        state.pc += 1;
        Ok(())
    }

    /// ERRNNIL: 如果 R[A] 不为 nil，报 "global already defined" 错误
    ///
    /// 指令格式: A Bx
    /// - A: 要检查的寄存器
    /// - Bx: 常量表索引+1（Bx==0 表示索引不可用，用 "?" 作为名字）
    ///
    /// 对应 C 的 OP_ERRNNIL + luaG_errnnil
    fn op_errnnil(state: &mut LuaState, inst: Instruction) -> Result<(), VmError> {
        let a = Self::ra(state, inst);
        let val = Self::read_stack(state, a);
        if !matches!(val, TValue::Nil(_)) {
            let bx = opcodes::getarg_bx(inst) as usize;
            let globalname = if bx > 0 {
                let k_idx = bx - 1;
                if k_idx < state.constants.len() {
                    if let TValue::Str(s) = &state.constants[k_idx] {
                        s.as_str().to_string()
                    } else {
                        "?".to_string()
                    }
                } else {
                    "?".to_string()
                }
            } else {
                "?".to_string()
            };
            return Err(VmError::RuntimeError(format!("global '{}' already defined", globalname)));
        }
        state.pc += 1;
        Ok(())
    }

    /// VARARGPREP: 调整变参函数的栈布局（对应 C 的 luaT_adjustvarargs）
    ///
    /// 两种模式:
    /// - PF_VAHID: 隐藏变参。把 func 和固定参数复制到变参之后，调整 base。
    ///   变参留在原位，通过 state.nextraargs 记录数量。
    /// - PF_VATAB: 建表模式。把变参打包成表，放到固定参数之后的位置。
    ///
    /// 调用前栈布局 (state.base = a + 1, func 在 state.base - 1):
    ///   [func][arg1..argNfix][extra1..extraK]
    ///   ^state.base-1        ^state.base     ^state.base+nfixparams
    ///
    /// totalargs = stack.len() - state.base (即 func 之后的所有参数)
    fn op_varargprep(state: &mut LuaState, _inst: Instruction) -> Result<(), VmError> {
        let flag = state.proto_flag;
        if flag & (PF_VAHID | PF_VATAB) == 0 {
            // 非变参函数，无需调整
            state.pc += 1;
            return Ok(());
        }

        let nfixparams = state.num_params as usize;
        // totalargs = L->top - ci->func - 1 = stack.len() - (base - 1) - 1 = stack.len() - base
        let totalargs = state.stack.len().saturating_sub(state.base);
        let nextra = totalargs.saturating_sub(nfixparams);

        if flag & PF_VATAB != 0 {
            // PF_VATAB: 创建 vararg 表
            // 变参在 state.base + nfixparams .. state.base + nfixparams + nextra
            let vatab_pos = state.base + nfixparams;
            let mut table = Table::new();
            for i in 0..nextra {
                let val = state.stack[vatab_pos + i].clone();
                table.set_int((i + 1) as i64, val);
            }
            // t.n = nextra
            let key_n = state.string_table.intern("n");
            table.set(TValue::Str(key_n), TValue::Integer(nextra as i64));
            // 把表放到 vatab_pos 位置，截断后续
            // nextra=0 时 vatab_pos 可能等于 stack_len，需要先扩展栈
            while state.stack.len() <= vatab_pos {
                state.stack.push(TValue::Nil(NilKind::Strict));
            }
            state.stack[vatab_pos] = TValue::Table(table);
            state.stack.truncate(vatab_pos + 1);
        } else {
            // PF_VAHID: 隐藏变参 (buildhiddenargs)
            state.nextraargs = nextra as i32;
            let func_pos = state.base - 1;
            // 把 func 副本复制到栈顶（变参之后）
            let func_val = state.stack[func_pos].clone();
            state.stack.push(func_val);
            // 把固定参数复制到栈顶，原位置设为 nil
            for i in 0..nfixparams {
                let val = state.stack[state.base + i].clone();
                state.stack.push(val);
                state.stack[state.base + i] = TValue::Nil(NilKind::Strict);
            }
            // 调整 base: ci->func.p += totalargs + 1 → state.base += totalargs + 1
            // 新的 func 在变参之后，变参在新 func 之前
            state.base += totalargs + 1;
            // vararg 参数位置（原固定参数之后）设为 nil
            // 对应 C 的 setnilvalue(s2v(ci->func.p + nfixparams + 1))
            // 此时 state.base 已调整，新 func 在 state.base - 1
            // 原来的 vararg 位置 = 新 func - nextra - 1 = state.base - 1 - nextra - 1
            // 但 C 是在调整前设置 nil，位置是 ci->func.p + nfixparams + 1（旧 func）
            // 旧 func + nfixparams + 1 = 旧 base + nfixparams = 变参起始位置
            // 这个位置在 buildhiddenargs 后已经是变参区域的一部分，不需要设 nil
            // C 代码是在 buildhiddenargs 之后执行 setnilvalue(ci->func.p + nfixparams + 1)
            // 但此时 ci->func.p 已调整，ci->func.p + nfixparams + 1 指向新区域的 vararg 槽
            // 实际上这个 nil 是为了标记 vararg 参数槽为空（供 GC）
            // 在 Rust 中我们不需要 GC 标记，跳过此步
        }

        // 扩展栈到 base + max_stack_size (对应 C 的 ci->top = ci->func.p + 1 + fsize)
        // vararg 函数在 VARARGPREP 完成变参重排后，需要扩展栈以容纳所有寄存器
        if state.base > 0 && state.base <= state.stack.len() {
            if let TValue::LClosure(closure) = &state.stack[state.base - 1] {
                let fsize = closure.proto.max_stack_size as usize;
                let frame_end = state.base + fsize;
                while state.stack.len() < frame_end {
                    state.stack.push(TValue::Nil(NilKind::Strict));
                }
            }
        }

        // 对应 C 的 OP_VARARGPREP -> luaD_hookcall: vararg 函数在 VARARGPREP 后触发 call hook
        // luaD_hookcall 检查 CIST_TAIL 决定事件类型: 尾调用触发 "tail call", 否则 "call"
        if state.hook_mask & 1 != 0 {  // LUA_MASKCALL
            // ftransfer=1 (参数从 func+1 开始), ntransfer=numparams
            let nfixparams = state.num_params as i32;
            let event = if state.call_info.last().map(|e| e.is_tailcall).unwrap_or(false) {
                "tail call"
            } else {
                "call"
            };
            Self::call_hook(state, event, -1, None, 1, nfixparams)?;
        }

        // 对应 C 的 VARARGPREP: L->oldpc = 1; next opcode will be seen as a "new" line
        // 设置 oldpc = 1，这样下一条指令 (pc=1) 会触发 hook (1 <= 1)
        // 注意: 必须在 call hook 之后设置，因为 call hook 内部可能会设置 line hook mask
        if state.hook_mask & 4 != 0 {
            state.hook_old_pc = 1;
        }

        state.pc += 1;
        Ok(())
    }

    // ========================================================================
    // 辅助: 表操作
    // ========================================================================

    fn table_get(state: &mut LuaState, table_val: &TValue, key: &TValue) -> Result<TValue, VmError> {
        // 对应 C Lua 的 luaV_finishget — 用循环代替递归，加 MAXTAGLOOP 限制
        // 防止 __index 链无限循环（如 a.__index = a 导致栈溢出）
        const MAXTAGLOOP: usize = 2000;
        let mut current = table_val.clone();
        for _ in 0..MAXTAGLOOP {
            match &current {
                TValue::Table(t) => {
                    // 先直接查找表
                    if let Some(v) = t.get(key) {
                        if !matches!(v, TValue::Nil(_)) {
                            return Ok(v);
                        }
                    }
                    // 查找 __index 元方法
                    let index_val = t.get_metatable().and_then(|mt| {
                        let index_key = crate::tm::make_tm_tvalue(crate::tm::TagMethod::Index);
                        mt.get(&index_key)
                    });
                    if let Some(index_val) = index_val {
                        match &index_val {
                            TValue::Table(_) => {
                                // __index 是表: 循环
                                current = index_val.clone();
                                continue;
                            }
                            TValue::LClosure(_) | TValue::LCFn(_) | TValue::CClosure(_) | TValue::LightUserData(_) => {
                                // __index 是函数: 调用 __index(table, key) (可能 yield)
                                return Self::call_index_metamethod(state, index_val.clone(), current.clone(), key.clone());
                            }
                            _ => {}
                        }
                    }
                    return Ok(TValue::Nil(NilKind::Strict));
                }
                TValue::Str(_) => {
                    // 字符串类型: 查找字符串元表的 __index
                    if let Some(mt) = state.dmt.get(LuaType::String) {
                        let index_key = crate::tm::make_tm_tvalue(crate::tm::TagMethod::Index);
                        if let Some(index_val) = mt.get(&index_key) {
                            match index_val {
                                TValue::Table(index_table) => {
                                    return Ok(index_table.get(key).unwrap_or(TValue::Nil(NilKind::Strict)));
                                }
                                _ => return Ok(TValue::Nil(NilKind::Strict)),
                            }
                        }
                    }
                    return Ok(TValue::Nil(NilKind::Strict));
                }
                other => {
                    // 非表/字符串值: 查找 __index 元方法 (基本类型如 number/boolean/nil)
                    // 对应 C Lua 的 luaV_finishget: 对非表值调用 getTMbyobj
                    let index_val = crate::tm::get_tm_by_obj(other, crate::tm::TagMethod::Index, &state.dmt);
                    match index_val {
                        Some(f) => match &f {
                            TValue::LClosure(_) | TValue::LCFn(_) | TValue::CClosure(_) | TValue::LightUserData(_) => {
                                // __index 是函数: 调用 __index(obj, key)
                                return Self::call_index_metamethod(state, f, current.clone(), key.clone());
                            }
                            TValue::Table(_) => {
                                // __index 是表: 循环
                                current = f;
                                continue;
                            }
                            _ => {
                                let type_name = state.typename(other.ty());
                                return Err(VmError::RuntimeError(format!("attempt to index a {} value", type_name)));
                            }
                        },
                        None => {
                            let type_name = state.typename(other.ty());
                            return Err(VmError::RuntimeError(format!("attempt to index a {} value", type_name)));
                        }
                    }
                }
            }
        }
        Err(VmError::RuntimeError("'__index' chain too long; possible loop".into()))
    }

    /// 调用 __index 元方法函数: __index(table, key)
    /// 使用 call_tm_res 支持 yield (PcallProtection 机制)
    fn call_index_metamethod(state: &mut LuaState, index_fn: TValue, table: TValue, key: TValue) -> Result<TValue, VmError> {
        // res 设为当前栈顶 (call_tm_res 会 push func/p1/p2 在这之后)
        let res = state.stack.len();
        // 调用 call_tm_res (支持 yield)
        crate::tm::call_tm_res(state, &index_fn, &table, &key, res, crate::tm::TagMethod::Index)?;
        // 成功: 结果在 res 位置，栈长度为 res+1
        let result = state.stack[res].clone();
        // 截断栈，移除临时结果
        state.stack.truncate(res);
        state.top = state.stack.len();
        Ok(result)
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

    /// 设置表字段，支持 `__newindex` 元方法和 yield
    /// 对应 C Lua 的 luaV_finishset
    /// 成功时表已被修改 (通过 Rc<RefCell<Table>> 的内部可变性)
    fn table_set(
        state: &mut LuaState,
        table_val: &TValue,
        key: TValue,
        val: TValue,
    ) -> Result<(), VmError> {
        // 对应 C Lua 的 luaV_finishset — 用循环代替递归，加 MAXTAGLOOP 限制
        // 防止 __newindex 链无限循环（如 a.__newindex = a 导致栈溢出）
        const MAXTAGLOOP: usize = 2000;
        let mut current = table_val.clone();
        for _ in 0..MAXTAGLOOP {
            match &current {
                TValue::Table(t) => {
                    // 先检查 key 是否已在表中 (非 nil)
                    let existing = t.get(&key);
                    let key_exists = existing.as_ref().map_or(false, |v| !matches!(v, TValue::Nil(_)));

                    if key_exists {
                        // key 已存在: 直接设置
                        t.set(key, val);
                        // GC barrier
                        let tid = t.gc_header.id();
                        if let Some(tid) = tid {
                            state.gc.obj_barrier_back(tid, tid);
                            state.gc.barrier_back(tid);
                        }
                        return Ok(());
                    }

                    // key 不存在: 查找 __newindex 元方法
                    let newindex_val = t.get_metatable().and_then(|mt| {
                        let newindex_key = crate::tm::make_tm_tvalue(crate::tm::TagMethod::NewIndex);
                        mt.get(&newindex_key)
                    });
                    if let Some(newindex_val) = newindex_val {
                        match &newindex_val {
                            TValue::LClosure(_) | TValue::LCFn(_) | TValue::CClosure(_) | TValue::LightUserData(_) => {
                                // __newindex 是函数: 调用 __newindex(table, key, val) (可能 yield)
                                crate::tm::call_tm(
                                    state,
                                    &newindex_val,
                                    &current,
                                    &key,
                                    &val,
                                    crate::tm::TagMethod::NewIndex,
                                )?;
                                return Ok(());
                            }
                            TValue::Table(_) => {
                                // __newindex 是表: 循环
                                current = newindex_val.clone();
                                continue;
                            }
                            _ => {
                                // __newindex 不是函数也不是表: 当作无元方法
                            }
                        }
                    }

                    // 没有 __newindex 元方法, 即将插入新键: 对应 C 的 luaH_finishset
                    // 检查 NaN/nil 键 (NaN 永远不等于自身, 故每次都是新键)
                    match &key {
                        TValue::Nil(_) => {
                            return Err(VmError::RuntimeError("table index is nil".to_string()));
                        }
                        TValue::Float(f) if f.is_nan() => {
                            return Err(VmError::RuntimeError("table index is NaN".to_string()));
                        }
                        _ => {}
                    }

                    // 没有 __newindex 元方法: 直接设置
                    t.set(key, val);
                    let tid = t.gc_header.id();
                    if let Some(tid) = tid {
                        state.gc.obj_barrier_back(tid, tid);
                        state.gc.barrier_back(tid);
                    }
                    return Ok(());
                }
                _ => {
                    // 非表值: 查找 __newindex 元方法
                    let newindex_val = crate::tm::get_tm_by_obj(
                        &current,
                        crate::tm::TagMethod::NewIndex,
                        &state.dmt,
                    );
                    match newindex_val {
                        Some(f) => {
                            match &f {
                                TValue::LClosure(_) | TValue::LCFn(_) | TValue::CClosure(_) | TValue::LightUserData(_) => {
                                    // __newindex 是函数: 调用 (可能 yield)
                                    crate::tm::call_tm(
                                        state,
                                        &f,
                                        &current,
                                        &key,
                                        &val,
                                        crate::tm::TagMethod::NewIndex,
                                    )?;
                                    return Ok(());
                                }
                                TValue::Table(_) => {
                                    // __newindex 是表: 循环
                                    current = f;
                                    continue;
                                }
                                _ => {
                                    let type_name = state.typename(current.ty());
                                    return Err(VmError::RuntimeError(format!("attempt to index a {} value", type_name)));
                                }
                            }
                        }
                        None => {
                            let type_name = state.typename(current.ty());
                            return Err(VmError::RuntimeError(format!("attempt to index a {} value", type_name)));
                        }
                    }
                }
            }
        }
        Err(VmError::RuntimeError("'__newindex' chain too long; possible loop".into()))
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
            make_asbx(OpCode::LOADI, 0, 0),
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
        let closure = LClosure { gc_header: GCObjectHeader::new(), proto: inner_proto, upvals: Rc::new(RefCell::new(vec![])) };

        let mut stack = default_stack(10);
        stack[0] = TValue::LClosure(closure);

        let code = vec![make_abck(OpCode::CALL, 0, 0, 1, 0)];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, stack).is_ok());
    }

    #[test]
    fn test_execute_tailcall_lua_closure() {
        let inner_proto = make_proto(vec![make_bx(OpCode::RETURN0, 0, 0)], vec![]);
        let closure = LClosure { gc_header: GCObjectHeader::new(), proto: inner_proto, upvals: Rc::new(RefCell::new(vec![])) };

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
        // R[0] = nil 时，ERRNNIL 不报错
        let code = vec![
            make_abc(OpCode::LOADNIL, 0, 0, 0),
            make_bx(OpCode::ERRNNIL, 0, 0),
        ];
        let proto = make_proto(code, vec![]);
        assert!(execute_test(&proto, 0, default_stack(10)).is_ok());

        // R[0] = 42（非 nil）时，ERRNNIL 应报错
        let code2 = vec![
            make_asbx(OpCode::LOADI, 0, 42),
            make_bx(OpCode::ERRNNIL, 0, 0),
        ];
        let proto2 = make_proto(code2, vec![]);
        assert!(execute_test(&proto2, 0, default_stack(10)).is_err());
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
        assert!(matches!(result, VmResult::Return { nresults: 0, .. }));
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