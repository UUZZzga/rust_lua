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
    /// 当前执行函数原型的 flag（PF_VAHID / PF_VATAB / PF_FIXED）
    pub proto_flag: u8,
    /// PF_VAHID 模式下隐藏变参的数量（对应 C 的 ci->u.l.nextraargs）
    pub nextraargs: i32,
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

    // C API 导出层使用：当前 C 函数帧的 func 位置（0-based 栈索引）。
    // C API 的正索引相对于此位置；Lua 代码路径不使用此字段。
    // 0 表示栈底（主线程初始状态）。
    pub api_func_base: usize,
    // C 函数调用嵌套计数（对应 C 的 L->nCcalls），用于检测 C 栈溢出
    pub n_ccalls: u32,
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
            proto_flag: 0,
            nextraargs: 0,
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
            api_func_base: 0,
            n_ccalls: 0,
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
            proto_flag: 0,
            nextraargs: 0,
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
            api_func_base: 0,
            n_ccalls: 0,
        };
        state
    }

    /// 执行 Lua 字节码 (顶层主函数)
    /// base=0: stack[0] 兼作函数入口和寄存器 0
    pub fn execute(&mut self, proto: &Proto) -> Result<VmResult, VmError> {
        if self.stack.is_empty() {
            self.stack.push(TValue::Nil(NilKind::Strict));
        }
        let fsize = proto.max_stack_size as usize;
        self.code = proto.code.clone();
        self.constants = proto.constants.clone();
        self.upval_descs = proto.upvalues.clone();
        self.protos = proto.protos.clone();
        self.base = 0;
        self.pc = 0;
        self.num_params = proto.num_params;
        self.is_vararg = proto.is_vararg();
        self.proto_flag = proto.flag;
        self.nextraargs = 0;
        self.closure_upvals = Vec::new();
        self.tbc_list = None;
        self.open_upval = None;

        while self.stack.len() < fsize {
            self.stack.push(TValue::Nil(NilKind::Strict));
        }
        VmExecutor::execute_loop(self)
    }

    /// 从 Proto 构建执行上下文（原 VmState::new）
    ///
    /// 函数帧布局: stack[base-1] = 函数入口, stack[base+0..base+N] = 寄存器/参数
    /// 当 base=0 时，stack[0] 兼作函数入口和寄存器 0（主函数场景）
    pub fn from_proto(proto: &Proto, base: usize, mut stack: Vec<TValue>, gc: Rc<GCState>) -> Self {
        if base > 0 {
            while stack.len() < base {
                stack.push(TValue::Nil(NilKind::Strict));
            }
        }
        let needed = base + proto.max_stack_size as usize;
        while stack.len() < needed {
            stack.push(TValue::Nil(NilKind::Strict));
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
            proto_flag: proto.flag,
            nextraargs: 0,
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
            api_func_base: 0,
            n_ccalls: 0,
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

pub fn str_to_ls(table: &StringTable, s: &str) -> LuaString {
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
        // func_idx 是函数在栈中的 0-based 绝对索引。
        // 栈布局: [... | func | arg1 | arg2 | ... | top]
        // func_idx = stack.len() - nargs - 1
        let func_idx = self.stack.len().saturating_sub(nargs + 1);
        if func_idx >= self.stack.len() {
            return ERR_RUN;
        }

        let func_val = self.stack[func_idx].clone();
        match func_val {
            TValue::LClosure(closure) => {
                let nargs_actual = self.stack.len().saturating_sub(func_idx + 1);
                let fsize = closure.proto.max_stack_size as usize;
                let nfixparams = closure.proto.num_params as usize;
                let proto_is_vararg = closure.proto.is_vararg();

                let saved_code = std::mem::take(&mut self.code);
                let saved_constants = std::mem::take(&mut self.constants);
                let saved_upval_descs = std::mem::take(&mut self.upval_descs);
                let saved_protos = std::mem::take(&mut self.protos);
                let saved_base = self.base;
                let saved_num_params = self.num_params;
                let saved_is_vararg = self.is_vararg;
                let saved_proto_flag = self.proto_flag;
                let saved_nextraargs = self.nextraargs;
                let saved_closure_upvals = std::mem::take(&mut self.closure_upvals);
                let saved_tbc_list = self.tbc_list.take();
                let saved_open_upval = self.open_upval.take();

                self.code = closure.proto.code.clone();
                self.constants = closure.proto.constants.clone();
                self.upval_descs = closure.proto.upvalues.clone();
                self.protos = closure.proto.protos.clone();
                self.base = func_idx + 1;
                self.pc = 0;
                self.num_params = closure.proto.num_params;
                self.is_vararg = closure.proto.is_vararg();
                self.proto_flag = closure.proto.flag;
                self.nextraargs = 0;
                let upval_val = UpVal::Closed {
                    value: Box::new(TValue::Table(self.globals.clone())),
                };
                self.closure_upvals = vec![upval_val];
                self.tbc_list = None;
                self.open_upval = None;

                if proto_is_vararg {
                    // vararg 函数: 截断栈到实际参数末尾，VARARGPREP 会处理
                    self.stack.truncate(func_idx + 1 + nargs_actual);
                    for i in nargs_actual..nfixparams {
                        let idx = func_idx + 1 + i;
                        while self.stack.len() <= idx {
                            self.stack.push(TValue::Nil(NilKind::Strict));
                        }
                        self.stack[idx] = TValue::Nil(NilKind::Strict);
                    }
                } else {
                    let frame_end = func_idx + 1 + fsize;
                    while self.stack.len() < frame_end {
                        self.stack.push(TValue::Nil(NilKind::Strict));
                    }
                    for i in nargs_actual..nfixparams {
                        self.stack[func_idx + 1 + i] = TValue::Nil(NilKind::Strict);
                    }
                }

                let result = VmExecutor::execute_loop(self);

                self.code = saved_code;
                self.constants = saved_constants;
                self.upval_descs = saved_upval_descs;
                self.protos = saved_protos;
                self.base = saved_base;
                self.pc = 0;
                self.num_params = saved_num_params;
                self.is_vararg = saved_is_vararg;
                self.proto_flag = saved_proto_flag;
                self.nextraargs = saved_nextraargs;
                self.closure_upvals = saved_closure_upvals;
                self.tbc_list = saved_tbc_list;
                self.open_upval = saved_open_upval;

                match result {
                    Ok(VmResult::Return { nresults: nret, result_base }) => {
                        let expected = if nresults == MULT_RET {
                            nret
                        } else if nresults <= 0 {
                            0
                        } else {
                            (nret).min(nresults as usize)
                        };

                        // 从 result_base 位置取结果（可能是 VARARGPREP 调整后的 base）
                        let mut tmp_results = Vec::new();
                        for i in 0..nret {
                            if result_base + i < self.stack.len() {
                                tmp_results.push(std::mem::take(&mut self.stack[result_base + i]));
                            } else {
                                tmp_results.push(TValue::Nil(NilKind::Strict));
                            }
                        }
                        self.stack.truncate(func_idx);
                        for i in 0..expected {
                            if i < tmp_results.len() {
                                self.stack.push(std::mem::take(&mut tmp_results[i]));
                            } else {
                                self.stack.push(TValue::Nil(NilKind::Strict));
                            }
                        }
                        0
                    }
                    Ok(_) => {
                        self.stack.truncate(func_idx);
                        0
                    }
                    Err(e) => {
                        self.stack.truncate(func_idx);
                        self.push_string(&format!("{}", e));
                        ERR_RUN
                    }
                }
            }
            TValue::LCFn(lcf) => {
                Self::pcall_c_function(self, func_idx, nresults, lcf.func)
            }
            TValue::CClosure(cc) => {
                Self::pcall_c_function(self, func_idx, nresults, cc.f)
            }
            TValue::LightUserData(tag) => {
                let tag_val = tag as usize;
                if tag_val == 1 {
                    let args_start = func_idx + 1;
                    let args_end = self.stack.len();
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

    /// 从 pcall 调用 C 函数（轻量 C 函数或 C 闭包）。
    ///
    /// 对应 C 的 precallC + luaD_poscall：
    /// 1. 设置 api_func_base = func_idx，确保栈空间，调用 f(L)
    /// 2. 把栈顶 n 个结果移动到 func_idx 位置
    fn pcall_c_function(
        &mut self,
        func_idx: usize,
        nresults: i32,
        f: unsafe extern "C" fn(*mut std::ffi::c_void) -> i32,
    ) -> i32 {
        use std::ffi::c_void;

        // precallC: 设置 api_func_base，确保栈空间
        let saved_api_base = self.api_func_base;
        self.api_func_base = func_idx;
        self.n_ccalls = self.n_ccalls.saturating_add(1);

        let needed_top = self.stack.len() + MIN_STACK;
        while self.stack.len() < needed_top {
            self.stack.push(TValue::Nil(NilKind::Strict));
        }

        // 调用 C 函数: n = f(L)
        let ptr: *mut LuaState = self;
        let n = unsafe { f(ptr as *mut c_void) };

        // poscall: 把栈顶 n 个结果移动到 func_idx 位置
        let top = self.stack.len();
        let n = n as usize;
        let first_result = top.saturating_sub(n);

        // 恢复 api_func_base 和 n_ccalls
        self.api_func_base = saved_api_base;
        self.n_ccalls = self.n_ccalls.saturating_sub(1);

        // 计算期望结果数
        let expected = if nresults == MULT_RET {
            n
        } else if nresults <= 0 {
            0
        } else {
            n.min(nresults as usize)
        };

        // 把结果复制到临时 Vec，避免覆盖问题
        let results: Vec<TValue> = (0..n)
            .map(|i| self.stack[first_result + i].clone())
            .collect();

        // 截断到 func_idx，然后推入 expected 个结果
        self.stack.truncate(func_idx);
        for i in 0..expected {
            if i < results.len() {
                self.stack.push(results[i].clone());
            } else {
                self.stack.push(TValue::Nil(NilKind::Strict));
            }
        }
        0
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
        assert_eq!(state.stack.len(), 10, "base=0 with max_stack_size=10 must allocate 10 register slots");

        // case 2: base=1, empty stack → called function scenario
        let state = LuaState::from_proto(&proto, 1, vec![], gc.clone());
        assert_eq!(state.base, 1);
        assert_eq!(state.stack.len(), 11, "base=1 with max_stack_size=10 must allocate 1+10=11 slots");
        assert!(matches!(state.stack[0], TValue::Nil(NilKind::Strict)));

        // case 3: base=1, stack with args → called function
        let state = LuaState::from_proto(&proto, 1, vec![
            TValue::Nil(NilKind::Strict),
            TValue::Integer(42),
        ], gc.clone());
        assert_eq!(state.base, 1);
        assert_eq!(state.stack.len(), 11, "base=1 with max_stack_size=10 must allocate 1+10=11 slots");
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