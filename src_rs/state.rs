use crate::objects::{Instruction, LClosure, LuaType, NilKind, Proto, TValue, UpVal, UpvalDesc};
use crate::strings::{LuaString, StringTable};
use crate::table::Table;
use crate::gc::{GCObjectHeader, GCState};
use crate::execute::{VmExecutor, VmResult, VmError};
use std::rc::Rc;

const EOFMARK: &str = "<eof>";

pub const ERR_RUN: i32 = 2;
pub const ERR_SYNTAX: i32 = 3;
pub const MULT_RET: i32 = -1;

pub const LUA_MINSTACK: usize = 20;
pub const BASIC_STACK_SIZE: usize = 2 * LUA_MINSTACK;
pub const EXTRA_STACK: usize = 5;

pub const MIN_STACK: usize = LUA_MINSTACK;

// ============================================================================
// LuaState — 合并 VmState + LuaState 的所有字段
// ============================================================================

pub struct LuaState {
    // 执行上下文（原 VmState）
    pub constants: Vec<TValue>,
    pub code: Vec<Instruction>,
    pub upval_descs: Vec<UpvalDesc>,
    pub protos: Vec<Proto>,
    pub base: usize,
    pub pc: usize,
    pub trap: bool,
    pub num_params: u8,
    pub is_vararg: bool,
    pub closure_upvals: Vec<UpVal>,
    pub open_upval: Option<usize>,
    pub tbc_list: Option<usize>,
    pub twups_linked: bool,
    pub is_in_twups: bool,

    // 公用字段
    pub stack: Vec<TValue>,
    pub gc: Rc<GCState>,

    // 高层 API 字段（原 LuaState）
    pub globals: Table,
    pub registry: Table,
    pub string_table: StringTable,
}

// ============================================================================
// 构造
// ============================================================================

impl LuaState {
    /// 对应 C 的 lua_newstate → stack_init + resetCI
    ///
    /// stack_init: 预分配 BASIC_STACK_SIZE + EXTRA_STACK 个槽位容量
    /// L->stack_last = stack + BASIC_STACK_SIZE
    /// resetCI: ci->func = stack[0], ci->top = stack[0] + 1 + LUA_MINSTACK
    /// L->top = stack + 1  (函数入口 nil 在位索引 0)
    ///
    /// 验证: gettop() 必须返回 1（函数入口槽）
    pub fn new() -> Self {
        let gc = Rc::new(GCState::default_incremental());
        let mut registry = Table::new();
        let globals = Table::new();
        registry.set(
            TValue::Integer(2),
            TValue::Table(globals.clone()),
        );

        let mut stack = Self::init_stack();

        LuaState {
            constants: Vec::new(),
            code: Vec::new(),
            upval_descs: Vec::new(),
            protos: Vec::new(),
            base: 0,
            pc: 0,
            trap: false,
            num_params: 0,
            is_vararg: false,
            closure_upvals: Vec::new(),
            open_upval: None,
            tbc_list: None,
            twups_linked: false,
            is_in_twups: false,
            stack,
            gc,
            globals,
            registry,
            string_table: StringTable::new(),
        }
    }

    /// 初始化栈: 对应 C 的 stack_init
    /// 分配 BASIC_STACK_SIZE + EXTRA_STACK 容量，推入函数入口 nil
    /// stack[0] = nil (函数入口, ci->func)
    /// top = stack + 1 (1 个元素在用)
    fn init_stack() -> Vec<TValue> {
        let mut stack = Vec::with_capacity(BASIC_STACK_SIZE + EXTRA_STACK);
        stack.push(TValue::Nil(NilKind::Strict));
        stack
    }

    /// 使用已有的 GCState 创建 LuaState
    pub fn with_gc(gc: Rc<GCState>) -> Self {
        let mut registry = Table::new();
        let globals = Table::new();
        registry.set(
            TValue::Integer(2),
            TValue::Table(globals.clone()),
        );

        let mut state = LuaState {
            constants: Vec::new(),
            code: Vec::new(),
            upval_descs: Vec::new(),
            protos: Vec::new(),
            base: 0,
            pc: 0,
            trap: false,
            num_params: 0,
            is_vararg: false,
            closure_upvals: Vec::new(),
            open_upval: None,
            tbc_list: None,
            twups_linked: false,
            is_in_twups: false,
            stack: Self::init_stack(),
            gc,
            globals,
            registry,
            string_table: StringTable::new(),
        };
        state
    }

    /// 执行 Lua 字节码 (顶层主函数)
    /// base=0: stack[0] 兼作函数入口和寄存器 0
    pub fn execute(&mut self, proto: &Proto) -> Result<VmResult, VmError> {
        if self.stack.is_empty() {
            self.stack.push(TValue::Nil(NilKind::Strict));
        }
        VmExecutor::execute(proto, 0, std::mem::take(&mut self.stack), self.gc.clone())
    }

    /// 从 Proto 构建执行上下文（原 VmState::new）
    ///
    /// 函数帧布局: stack[base-1] = 函数入口, stack[base+0..base+N] = 寄存器/参数
    /// 当 base=0 时，stack[0] 兼作函数入口和寄存器 0（主函数场景）
    pub fn from_proto(proto: &Proto, base: usize, mut stack: Vec<TValue>, gc: Rc<GCState>) -> Self {
        // 防御: 当 base > 0 时确保函数入口槽 stack[base-1] 存在
        if base > 0 {
            while stack.len() < base {
                stack.push(TValue::Nil(NilKind::Strict));
            }
        }
        LuaState {
            constants: proto.constants.clone(),
            code: proto.code.clone(),
            upval_descs: proto.upvalues.clone(),
            protos: proto.protos.clone(),
            base,
            pc: 0,
            trap: false,
            num_params: proto.num_params,
            is_vararg: proto.is_vararg(),
            closure_upvals: Vec::new(),
            open_upval: None,
            tbc_list: None,
            twups_linked: false,
            is_in_twups: false,
            stack,
            gc,
            globals: Table::new(),
            registry: Table::new(),
            string_table: StringTable::new(),
        }
    }
}

impl Default for LuaState {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 字符串工具
// ============================================================================

fn str_to_ls(table: &StringTable, s: &str) -> LuaString {
    crate::strings::new_lstr(table, s)
}

fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    if f == 0.0 {
        return "0.0".to_string();
    }
    let s = format!("{:.15}", f);
    let s = s.trim_end_matches('0');
    if s.ends_with('.') {
        format!("{}0", s)
    } else {
        s.to_string()
    }
}

// ============================================================================
// 高层 API 方法（原 LuaState）
// ============================================================================

impl LuaState {
    // ====== Stack ======

    pub fn gettop(&self) -> usize {
        self.stack.len()
    }

    pub fn settop(&mut self, idx: usize) {
        if idx < self.stack.len() {
            self.stack.truncate(idx);
        } else {
            self.stack.resize(idx, TValue::Nil(NilKind::Strict));
        }
    }

    pub fn pop(&mut self, n: usize) {
        let new_len = self.stack.len().saturating_sub(n);
        self.stack.truncate(new_len);
    }

    /// 对应 C 的 lua_absindex:
    ///   return (idx > 0 || is_pseudo(idx)) ? idx : cast_int(L->top - L->ci->func) + idx + 1
    /// 其中 L->top - L->ci->func 等价于 stack.len()（函数帧内有效元素数）
    pub fn abs_index(&self, idx: isize) -> usize {
        let len = self.stack.len() as isize;
        if idx > 0 {
            idx as usize
        } else {
            let abs = len + idx + 1;
            if abs > 0 {
                abs as usize
            } else {
                0
            }
        }
    }

    pub fn rotate(&mut self, idx: isize, n: isize) {
        let abs = self.abs_index(idx);
        if abs == 0 || abs > self.stack.len() {
            return;
        }
        if n > 0 {
            for _ in 0..n {
                let val = self.stack.remove(abs - 1);
                self.stack.push(val);
            }
        } else {
            let count = (-n) as usize;
            for _ in 0..count {
                let val = self.stack.pop().unwrap();
                self.stack.insert(abs - 1, val);
            }
        }
    }

    pub fn copy(&mut self, from_idx: isize, to_idx: isize) {
        let from = self.abs_index(from_idx);
        if from > 0 && from <= self.stack.len() {
            let val = self.stack[from - 1].clone();
            let to = self.abs_index(to_idx);
            if to > 0 {
                if to > self.stack.len() {
                    self.stack.resize(to, TValue::Nil(NilKind::Strict));
                }
                self.stack[to - 1] = val;
            }
        }
    }

    // ====== Push ======

    pub fn push_nil(&mut self) {
        self.stack.push(TValue::Nil(NilKind::Strict));
    }

    pub fn push_boolean(&mut self, b: bool) {
        self.stack.push(TValue::Boolean(b));
    }

    pub fn push_integer(&mut self, n: i64) {
        self.stack.push(TValue::Integer(n));
    }

    pub fn push_float(&mut self, n: f64) {
        self.stack.push(TValue::Float(n));
    }

    pub fn push_string(&mut self, s: &str) {
        let ls = str_to_ls(&self.string_table, s);
        self.stack.push(TValue::Str(ls));
    }

    pub fn push_lstring(&mut self, s: &[u8]) {
        let text = String::from_utf8_lossy(s).into_owned();
        let ls = str_to_ls(&self.string_table, &text);
        self.stack.push(TValue::Str(ls));
    }

    pub fn push_value(&mut self, val: TValue) {
        self.stack.push(val);
    }

    pub fn push_light_userdata(&mut self, p: *mut std::ffi::c_void) {
        self.stack.push(TValue::LightUserData(p));
    }

    pub fn push_fstring(&mut self, fmt: &str) {
        self.push_string(fmt);
    }

    pub fn push_lua_value(&mut self, val: &TValue) {
        self.stack.push(val.clone());
    }

    // ====== Access / Type ======

    pub fn obj_at(&self, idx: isize) -> Option<&TValue> {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            Some(&self.stack[abs - 1])
        } else {
            None
        }
    }

    pub fn obj_at_mut(&mut self, idx: isize) -> Option<&mut TValue> {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            Some(&mut self.stack[abs - 1])
        } else {
            None
        }
    }

    pub fn get_type(&self, idx: isize) -> LuaType {
        self.obj_at(idx).map(|v| v.ty()).unwrap_or(LuaType::Nil)
    }

    pub fn typename(&self, tp: LuaType) -> &'static str {
        match tp {
            LuaType::Nil => "nil",
            LuaType::Boolean => "boolean",
            LuaType::LightUserData => "lightuserdata",
            LuaType::Number => "number",
            LuaType::String => "string",
            LuaType::Table => "table",
            LuaType::Function => "function",
            LuaType::UserData => "userdata",
            LuaType::Thread => "thread",
        }
    }

    pub fn to_integer(&self, idx: isize) -> Option<i64> {
        match self.obj_at(idx) {
            Some(TValue::Integer(i)) => Some(*i),
            Some(TValue::Float(f)) => crate::vm::float_to_integer(*f, crate::vm::F2IMode::Eq),
            Some(TValue::Str(s)) => s.as_str().parse::<i64>().ok(),
            _ => None,
        }
    }

    pub fn to_number(&self, idx: isize) -> Option<f64> {
        match self.obj_at(idx) {
            Some(TValue::Integer(i)) => Some(*i as f64),
            Some(TValue::Float(f)) => Some(*f),
            Some(TValue::Str(s)) => s.as_str().parse::<f64>().ok(),
            _ => None,
        }
    }

    pub fn to_boolean(&self, idx: isize) -> bool {
        !matches!(self.obj_at(idx), Some(TValue::Nil(_)) | Some(TValue::Boolean(false)))
    }

    pub fn to_string(&self, idx: isize) -> Option<String> {
        match self.obj_at(idx) {
            Some(TValue::Str(s)) => Some(s.as_str().to_string()),
            Some(TValue::Integer(i)) => Some(i.to_string()),
            Some(TValue::Float(f)) => Some(format_float(*f)),
            _ => None,
        }
    }

    pub fn to_lstring(&self, idx: isize) -> Option<(String, usize)> {
        match self.obj_at(idx) {
            Some(TValue::Str(s)) => {
                let text = s.as_str().to_string();
                let len = s.len();
                Some((text, len))
            }
            _ => None,
        }
    }

    pub fn to_userdata(&self, idx: isize) -> *mut std::ffi::c_void {
        match self.obj_at(idx) {
            Some(TValue::LightUserData(p)) => *p,
            _ => std::ptr::null_mut(),
        }
    }

    // ====== Globals ======

    pub fn get_global(&mut self, name: &str) -> LuaType {
        let key = TValue::Str(str_to_ls(&self.string_table, name));
        match self.globals.get(&key) {
            Some(val) => {
                let ty = val.ty();
                self.stack.push(val.clone());
                ty
            }
            None => {
                self.stack.push(TValue::Nil(NilKind::Strict));
                LuaType::Nil
            }
        }
    }

    pub fn set_global(&mut self, name: &str) {
        let key = TValue::Str(str_to_ls(&self.string_table, name));
        if let Some(val) = self.stack.pop() {
            self.globals.set(key, val);
        }
    }

    pub fn set_field(&mut self, idx: isize, key_name: &str) {
        let abs = self.abs_index(idx);
        let val = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let key = TValue::Str(str_to_ls(&self.string_table, key_name));
        if abs > 0 && abs <= self.stack.len() {
            let tbl = &mut self.stack[abs - 1];
            if let TValue::Table(ref mut t) = tbl {
                t.set(key, val);
            }
        }
    }

    pub fn get_field(&mut self, idx: isize, key_name: &str) -> LuaType {
        let abs = self.abs_index(idx);
        let key = TValue::Str(str_to_ls(&self.string_table, key_name));
        if abs > 0 && abs <= self.stack.len() {
            let val = if let TValue::Table(ref t) = &self.stack[abs - 1] {
                t.get(&key).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
            } else {
                TValue::Nil(NilKind::Strict)
            };
            let ty = val.ty();
            self.stack.push(val);
            ty
        } else {
            self.stack.push(TValue::Nil(NilKind::Strict));
            LuaType::Nil
        }
    }

    // ====== Table ======

    pub fn create_table(&mut self, narr: usize, nrec: usize) {
        let t = Table::with_capacity(narr, nrec);
        self.stack.push(TValue::Table(t));
    }

    pub fn new_table(&mut self) {
        let t = Table::new();
        self.stack.push(TValue::Table(t));
    }

    pub fn raw_get_i(&mut self, idx: isize, i: i64) -> LuaType {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            let val = if let TValue::Table(ref t) = &self.stack[abs - 1] {
                let tkey = TValue::Integer(i);
                t.get(&tkey).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
            } else {
                TValue::Nil(NilKind::Strict)
            };
            let ty = val.ty();
            self.stack.push(val);
            ty
        } else {
            self.stack.push(TValue::Nil(NilKind::Strict));
            LuaType::Nil
        }
    }

    pub fn raw_set_i(&mut self, idx: isize, i: i64) {
        let abs = self.abs_index(idx);
        let val = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        if abs > 0 && abs <= self.stack.len() {
            if let TValue::Table(ref mut t) = self.stack[abs - 1] {
                t.set_int(i, val);
            }
        }
    }

    pub fn raw_get(&mut self, idx: isize) -> LuaType {
        let key = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            let val = if let TValue::Table(ref t) = &self.stack[abs - 1] {
                t.get(&key).cloned().unwrap_or(TValue::Nil(NilKind::Strict))
            } else {
                TValue::Nil(NilKind::Strict)
            };
            let ty = val.ty();
            self.stack.push(val);
            ty
        } else {
            self.stack.push(TValue::Nil(NilKind::Strict));
            LuaType::Nil
        }
    }

    pub fn raw_set(&mut self, idx: isize) {
        let val = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let key = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            if let TValue::Table(ref mut t) = self.stack[abs - 1] {
                t.set(key, val);
            }
        }
    }

    // ====== Len ======

    pub fn len(&self, idx: isize) -> usize {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            match &self.stack[abs - 1] {
                TValue::Table(t) => t.len() as usize,
                TValue::Str(s) => s.len(),
                TValue::Integer(_) | TValue::Float(_) => 0,
                _ => 0,
            }
        } else {
            0
        }
    }

    // ====== Check stack ======

    pub fn check_stack(&mut self, extra: usize) -> bool {
        let needed = self.stack.len() + extra;
        if needed > self.stack.capacity() {
            self.stack.reserve(extra);
        }
        true
    }

    // ====== Garbage Collection ======

    pub fn gc_stop(&self) {}

    pub fn gc_restart(&self) {}

    pub fn gc_gen(&self) {}

    // ====== Diagnostics ======

    pub fn warning(&mut self, _msg: &str, _tocont: bool) {}

    pub fn check_version(&self) {}

    // ====== Call Meta ======

    pub fn call_meta(&self, _idx: isize, _event: &str) -> bool {
        false
    }

    pub fn traceback(&mut self, msg: &str, _level: usize) {
        let trace = format!("stack traceback:\n\t...\n{}", msg);
        self.push_string(&trace);
    }

    pub fn error(&mut self, msg: &str) -> String {
        msg.to_string()
    }

    // ====== Push C Function ======

    pub fn push_rust_fn(&mut self, _f: fn(&mut LuaState) -> i32, tag: usize) {
        self.push_light_userdata(tag as *mut std::ffi::c_void);
    }

    // ====== Load Code ======

    pub fn load_buffer(&mut self, code: &str, chunk_name: &str) -> i32 {
        match crate::compiler::compile(code, chunk_name) {
            Ok(proto) => {
                let closure = LClosure {
                    gc_header: GCObjectHeader::new(),
                    proto,
                    upvals: Vec::new(),
                };
                self.stack.push(TValue::LClosure(closure));
                0
            }
            Err(err_msg) => {
                self.push_string(&err_msg);
                ERR_SYNTAX
            }
        }
    }

    pub fn load_file(&mut self, fname: Option<&str>) -> i32 {
        if let Some(name) = fname {
            match std::fs::read_to_string(name) {
                Ok(content) => self.load_buffer(&content, name),
                Err(_) => {
                    self.push_fstring(&format!("cannot open {}: No such file or directory", name));
                    ERR_RUN
                }
            }
        } else {
            let mut buf = String::new();
            match std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf) {
                Ok(_) => self.load_buffer(&buf, "=stdin"),
                Err(_) => {
                    self.push_string("cannot read from stdin");
                    ERR_RUN
                }
            }
        }
    }

    // ====== pcall ======

    pub fn pcall(&mut self, nargs: usize, nresults: i32, _errfunc: isize) -> i32 {
        let total = self.gettop();
        let func_idx = total.saturating_sub(nargs + 1);
        if func_idx >= total {
            return ERR_RUN;
        }

        let func_val = self.stack[func_idx].clone();
        match func_val {
            TValue::LClosure(closure) => {
                let args_start = func_idx + 1;
                let args_end = total;
                let nargs_actual = args_end.saturating_sub(args_start);

                // 对应 C 的 luaD_precall → 函数帧布局:
                // stack[0] = 函数入口 nil (ci->func)
                // stack[1..1+nargs] = 参数 (ci->u.l.base = ci->func + 1)
                let mut exec_stack: Vec<TValue> = Vec::with_capacity(1 + nargs_actual + MIN_STACK);
                exec_stack.push(TValue::Nil(NilKind::Strict));
                exec_stack.extend_from_slice(&self.stack[args_start..args_end]);

                let upval_val = UpVal::Closed {
                    value: Box::new(TValue::Table(self.globals.clone())),
                };
                let closure_upvals = vec![upval_val];

                let mut exec_state = LuaState::from_proto(
                    &closure.proto,
                    1,
                    exec_stack,
                    self.gc.clone(),
                );
                exec_state.closure_upvals = closure_upvals;

                match VmExecutor::execute_with_state(&mut exec_state) {
                    Ok(VmResult::Return(_n)) => {
                        self.stack.truncate(func_idx);
                        let actual_n = if nresults == MULT_RET {
                            1
                        } else if nresults <= 0 {
                            0
                        } else {
                            (1).min(nresults as usize)
                        };
                        for _ in 0..actual_n {
                            self.push_nil();
                        }
                        return 0;
                    }
                    Ok(_) => {
                        self.stack.truncate(func_idx);
                        return 0;
                    }
                    Err(e) => {
                        self.stack.truncate(func_idx);
                        self.push_string(&format!("{}", e));
                        return ERR_RUN;
                    }
                }
            }
            TValue::LCFn(_f) => {
                self.stack.truncate(func_idx);
                self.push_string("C functions not supported in pure Rust VM");
                ERR_RUN
            }
            TValue::CClosure(_) => {
                self.stack.truncate(func_idx);
                self.push_string("C closures not supported in pure Rust VM");
                ERR_RUN
            }
            TValue::LightUserData(tag) => {
                let tag_val = tag as usize;
                if tag_val == 1 {
                    let args_start = func_idx + 1;
                    let args_end = total;
                    let mut s = String::new();
                    for i in args_start..args_end {
                        if i > args_start { s.push('\t'); }
                        if let Some(ts) = self.to_string(i as isize) {
                            s.push_str(&ts);
                        }
                    }
                    self.stack.truncate(func_idx);
                    println!("{}", s);
                    return 0;
                }
                self.stack.truncate(func_idx);
                self.push_string(&format!("attempt to call a non-function value (tag={})", tag_val));
                ERR_RUN
            }
            _ => {
                self.stack.truncate(func_idx);
                self.push_string(&format!(
                    "attempt to call a {} value",
                    self.typename(func_val.ty())
                ));
                ERR_RUN
            }
        }
    }

    // ====== Open Libs ======

    pub fn open_selected_libs(&mut self, _mask: i32, _ignored: i32) {
        let arg_table = Table::new();
        self.globals.set(
            TValue::Str(str_to_ls(&self.string_table, "arg")),
            TValue::Table(arg_table),
        );
        
        self.globals.set(
            TValue::Str(str_to_ls(&self.string_table, "print")),
            TValue::LightUserData(1_usize as *mut std::ffi::c_void),
        );
    }

    // ====== Hook ======

    pub fn set_hook(&mut self, _hook: Option<(usize, usize)>, _mask: i32, _count: i32) {}

    // ====== String Helpers ======

    pub fn intern_str(&self, s: &str) -> LuaString {
        str_to_ls(&self.string_table, s)
    }

    pub fn intern(&self, s: &str) -> LuaString {
        str_to_ls(&self.string_table, s)
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stack_init_matches_cpp() {
        // 验证 stack_init: 参考 lstate.cpp L158-169
        let state = LuaState::new();
        // L->top = stack + 1 → gettop() 必须返回 1
        assert_eq!(state.gettop(), 1, "stack length must be 1 (function entry slot)");
        // stack[0] 必须是函数入口 nil
        assert!(matches!(state.stack[0], TValue::Nil(NilKind::Strict)));
        // 容量 = BASIC_STACK_SIZE + EXTRA_STACK
        assert_eq!(state.stack.capacity(), BASIC_STACK_SIZE + EXTRA_STACK);
    }

    #[test]
    fn test_stack_init_from_proto() {
        // 验证 from_proto 的栈初始化: base > 0 时必须保证函数入口槽
        let proto = Proto {
            num_params: 0, flag: 0, max_stack_size: 10,
            size_upvalues: 0, size_k: 0, size_code: 0, size_line_info: 0,
            size_p: 0, size_loc_vars: 0, size_abs_line_info: 0,
            line_defined: 0, last_line_defined: 0,
            constants: vec![],
            code: vec![],
            protos: vec![],
            upvalues: vec![],
            line_info: vec![],
            abs_line_info: vec![],
            loc_vars: vec![],
            source: None,
        };
        let gc = Rc::new(GCState::default_incremental());

        // case 1: base=0, empty stack → main function scenario
        let state = LuaState::from_proto(&proto, 0, vec![], gc.clone());
        assert_eq!(state.base, 0);
        assert_eq!(state.stack.len(), 0, "base=0 allows empty stack (function entry IS register 0)");

        // case 2: base=1, empty stack → called function scenario
        let state = LuaState::from_proto(&proto, 1, vec![], gc.clone());
        assert_eq!(state.base, 1);
        assert!(state.stack.len() >= 1, "base>0 must have stack[base-1] = stack[0] as function entry");
        assert!(matches!(state.stack[0], TValue::Nil(NilKind::Strict)));

        // case 3: base=1, stack with args → called function
        let state = LuaState::from_proto(&proto, 1, vec![
            TValue::Nil(NilKind::Strict),
            TValue::Integer(42),
        ], gc.clone());
        assert_eq!(state.base, 1);
        assert_eq!(state.stack.len(), 2);
        assert!(matches!(state.stack[0], TValue::Nil(NilKind::Strict)));
        assert_eq!(state.stack[1], TValue::Integer(42));
    }

    #[test]
    fn test_stack_init_with_gc() {
        let gc = Rc::new(GCState::default_incremental());
        let state = LuaState::with_gc(gc);
        assert_eq!(state.gettop(), 1, "with_gc must also init stack");
        assert_eq!(state.stack.capacity(), BASIC_STACK_SIZE + EXTRA_STACK);
    }

    #[test]
    fn test_stack_init_default() {
        let state = LuaState::default();
        assert_eq!(state.gettop(), 1, "Default must init stack via new()");
    }
}