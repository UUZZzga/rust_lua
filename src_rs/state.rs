use crate::debug::runerror;
use crate::objects::{Instruction, LClosure, LuaType, NilKind, Proto, TValue, UpVal, UpvalDesc, UpValRef};
use crate::strings::{LuaString, StringTable};
use crate::table::Table;
use crate::gc::{GCObjectHeader, GCState};
use crate::execute::{VmExecutor, VmResult, VmError};
use crate::tm::DefaultMetatables;
use std::rc::Rc;
use std::cell::RefCell;
use std::io::{Read, Write};

const EOFMARK: &str = "<eof>";

pub const ERR_RUN: i32 = 2;
pub const ERR_SYNTAX: i32 = 3;
pub const ERR_FILE: i32 = 6;  // LUA_ERRERR + 1, used by luaL_loadfilex
pub const MULT_RET: i32 = -1;

const LUA_SIGNATURE: &[u8] = b"\x1bLua";
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

pub const LUA_MINSTACK: usize = 20;
pub const BASIC_STACK_SIZE: usize = 2 * LUA_MINSTACK;
pub const EXTRA_STACK: usize = 5;

pub const MIN_STACK: usize = LUA_MINSTACK;
pub const LUAI_MAXSTACK: usize = 1000000;
pub const LUAI_MAXCCALLS: u32 = 200;
pub const STACKERRSPACE: usize = 200;
pub const MAXSTACK_BYSIZET: usize = (usize::max_value() / std::mem::size_of::<TValue>()) - STACKERRSPACE ;
pub const MAXSTACK: usize = if LUAI_MAXSTACK < MAXSTACK_BYSIZET {LUAI_MAXSTACK} else {MAXSTACK_BYSIZET};
pub const ERRORSTACKSIZE: usize = MAXSTACK + STACKERRSPACE;

pub struct GlobalState {
    pub gcstopem: bool,
}

pub struct LuaFunctionCallInfo {
    pub savedpc: Instruction,
    pub trap: bool,
    pub nextraargs: i32,
}

pub enum CallInfoU {
    LuaFunction(LuaFunctionCallInfo),
    CFunction(),
}

pub struct CallInfo {
    pub previous: Option<Box<CallInfo>>,
    pub top: usize,
    pub func: usize,
    pub u: CallInfoU,
}

// ============================================================================
// LuaState — 合并 VmState + LuaState 的所有字段
// ============================================================================

pub struct LuaState {
    // 执行上下文（原 VmState）
    pub constants: Vec<TValue>,
    pub code: Vec<Instruction>,
    pub upval_descs: Vec<UpvalDesc>,
    pub protos: Vec<Proto>,
    pub top: usize,
    pub base: usize,
    pub pc: usize,
    pub trap: bool,
    pub num_params: u8,
    pub is_vararg: bool,
    /// 当前执行函数原型的 flag（PF_VAHID / PF_VATAB / PF_FIXED）
    pub proto_flag: u8,
    /// PF_VAHID 模式下隐藏变参的数量（对应 C 的 ci->u.l.nextraargs）
    pub nextraargs: i32,
    pub closure_upvals: Vec<UpValRef>,
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
    pub dmt: DefaultMetatables,
    pub stdout: Box<dyn Write>,
    pub global_state: Rc<GlobalState>,
    pub ci: Option<Box<CallInfo>>,
    /// 调用栈信息，用于构建堆栈回溯 — 对应 C 的 CallInfo 链表
    /// 每个元素是 (source, line, function_name)
    pub call_info: Vec<CallInfoEntry>,
    /// 最后一次错误的堆栈回溯字符串
    pub last_traceback: String,
    /// 最后一次错误的格式化消息（含 source:line 前缀）
    pub last_error_msg: String,
    /// 当前正在调用的 C 函数名（用于 traceback）— None 表示不在 C 函数中
    pub last_c_function: Option<String>,
    /// 数学库随机数生成器状态 — 对应 C 的 RanState (math.random/randomseed)
    pub math_random_state: Option<Box<crate::stdlib::math_lib::RandState>>,
}

/// 调用栈条目 — 用于堆栈回溯
#[derive(Clone)]
pub struct CallInfoEntry {
    pub source: String,
    pub line: i32,
    pub name: String,
    pub is_c: bool,
}

fn G(l: &LuaState) -> &GlobalState {
    &l.global_state
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

        let stack = Self::init_stack();
        let top = stack.len();

        LuaState {
            constants: Vec::new(),
            code: Vec::new(),
            upval_descs: Vec::new(),
            protos: Vec::new(),
            top,
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
            dmt: DefaultMetatables::new(),
            stdout: Box::new(std::io::stdout()),
            global_state: Rc::new(GlobalState { gcstopem: false }),
            ci: None,
            call_info: Vec::new(),
            last_traceback: String::new(),
            last_error_msg: String::new(),
            last_c_function: None,
            math_random_state: None,
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

    fn condmovestack(&mut self, _pre: usize, _pos: usize) {
        // Rust 版本: Vec 自行管理内存，无需在栈重分配时修正指针
        // C 版本的 condmovestack 仅在 hardstacktests 配置下做额外检查
    }

    pub fn checkstackaux(&mut self, n: usize, pre: usize, pos: usize) {
        if self.stack.len() - self.top <= n {
            let _ = self.growstack(n, true);
        } else {
            self.condmovestack(pre, pos);
        }
    }

    pub fn checkstack(&mut self, n: usize) {
        self.checkstackaux(n, 0, 0);
    }

    /// 对应 C 的 luaD_growstack
    /// 尝试将栈增长至少 n 个元素。raiseerror=true 时报告错误，否则返回错误。
    pub fn growstack(&mut self, n: usize, raiseerror: bool) -> Result<(), VmError> {
        let size = self.stack.len();
        if size > MAXSTACK {
            // 栈已超过最大值，线程正在使用为错误保留的额外空间
            debug_assert_eq!(size, ERRORSTACKSIZE);
            if raiseerror {
                // 对应 C 的 luaD_errerr (栈错误发生在消息处理器内)
                // 简化: 直接返回 StackError
            }
            return Err(VmError::StackError);
        } else if n < MAXSTACK {
            let mut newsize = size + (size >> 1);  /* tentative new size (size * 1.5) */
            let needed = self.top + n;
            if newsize > MAXSTACK {
                newsize = MAXSTACK;
            }
            if newsize < needed {
                newsize = needed;
            }
            if newsize <= MAXSTACK {
                return self.reallocstack(newsize, raiseerror);
            }
        }
        /* else stack overflow */
        /* add extra size to be able to handle the error message */
        self.reallocstack(ERRORSTACKSIZE, raiseerror)?;
        if raiseerror {
            runerror(self, "stack overflow", &[]);
        }
        Err(VmError::StackOverflow)
    }

    /// 对应 C 的 luaD_reallocstack
    /// Rust 版本: Vec 自行管理内存，relstack/correctstack 为空操作
    pub fn reallocstack(&mut self, newsize: usize, _raiseerror: bool) -> Result<(), VmError> {
        let oldsize = self.stack.len();
        debug_assert!(newsize <= MAXSTACK || newsize == ERRORSTACKSIZE);
        // relstack: Rust 中无需将指针转为偏移量 (Vec 自行管理内存)
        // G(self).gcstopem = true: 简化，不停止紧急 GC
        // 扩展栈到 newsize + EXTRA_STACK，新位置填 nil
        let target = newsize + EXTRA_STACK;
        if target > oldsize {
            self.stack.resize(target, TValue::Nil(NilKind::Strict));
        }
        // correctstack: Rust 中无需修正指针 (使用索引而非指针)
        Ok(())
    }

    /// 对应 C 的 relstack: 将指针转为偏移量
    /// Rust 版本: 无操作 (Vec 自行管理内存，使用索引)
    fn relstack(&mut self) {
        // no-op in Rust
    }

    /// 对应 C 的 correctstack: 将偏移量转回指针
    /// Rust 版本: 无操作 (Vec 自行管理内存，使用索引)
    fn correctstack(&mut self) {
        // no-op in Rust
    }

    /// 使用已有的 GCState 创建 LuaState
    pub fn with_gc(gc: Rc<GCState>) -> Self {
        let mut registry = Table::new();
        let globals = Table::new();
        registry.set(
            TValue::Integer(2),
            TValue::Table(globals.clone()),
        );

        let stack = Self::init_stack();
        let top = stack.len();

        let state = LuaState {
            constants: Vec::new(),
            code: Vec::new(),
            upval_descs: Vec::new(),
            protos: Vec::new(),
            top,
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
            dmt: DefaultMetatables::new(),
            stdout: Box::new(std::io::stdout()),
            global_state: Rc::new(GlobalState { gcstopem: false }),
            ci: None,
            call_info: Vec::new(),
            last_traceback: String::new(),
            last_error_msg: String::new(),
            last_c_function: None,
            math_random_state: None,
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
        let top = stack.len();
        LuaState {
            constants: proto.constants.clone(),
            code: proto.code.clone(),
            upval_descs: proto.upvalues.clone(),
            protos: proto.protos.clone(),
            top,
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
            dmt: DefaultMetatables::new(),
            stdout: Box::new(std::io::stdout()),
            global_state: Rc::new(GlobalState { gcstopem: false }),
            ci: None,
            call_info: Vec::new(),
            last_traceback: String::new(),
            last_error_msg: String::new(),
            last_c_function: None,
            math_random_state: None,
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

    /// 对应 C 的 lua_remove：删除指定索引处的元素，上方元素下移。
    pub fn remove(&mut self, idx: isize) {
        let abs = self.abs_index(idx);
        if abs == 0 || abs > self.stack.len() {
            return;
        }
        self.stack.remove(abs - 1);
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

    /// 返回指定栈位置值的类型名 — 对应 C 的 luaL_typename
    pub fn typename_at(&self, idx: isize) -> &'static str {
        self.typename(self.get_type(idx))
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

    /// 构建堆栈回溯字符串 — 对应 C 的 luaL_traceback
    ///
    /// 格式:
    /// ```
    /// msg
    /// stack traceback:
    ///         [C]: in global 'assert'
    ///         (command line):1: in main chunk
    ///         [C]: in ?
    /// ```
    pub fn traceback_string(&self, msg: &str, _level: usize) -> String {
        let mut result = String::new();
        if !msg.is_empty() {
            result.push_str(msg);
            result.push('\n');
        }
        result.push_str("stack traceback:");
        // 从 call_info 构建回溯
        if self.call_info.is_empty() {
            // 没有调用信息，使用简化的回溯
            result.push_str("\n\t[C]: in ?");
        } else {
            for entry in &self.call_info {
                result.push('\n');
                result.push('\t');
                if entry.is_c {
                    result.push_str("[C]: in ");
                    result.push_str(&entry.name);
                } else {
                    if entry.line > 0 {
                        result.push_str(&format!("{}:{}: in ", entry.source, entry.line));
                    } else {
                        result.push_str(&format!("{}: in ", entry.source));
                    }
                    result.push_str(&entry.name);
                }
            }
            // 最后添加 [C]: in ?
            result.push_str("\n\t[C]: in ?");
        }
        result
    }

    /// 推入调用栈条目 — 用于堆栈回溯
    pub fn push_call_info(&mut self, entry: CallInfoEntry) {
        self.call_info.push(entry);
    }

    /// 弹出调用栈条目
    pub fn pop_call_info(&mut self) {
        self.call_info.pop();
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
        match crate::compiler::compile(self, code, chunk_name) {
            Ok(proto) => {
                // 创建主闭包，设置 _ENV 上值为全局表
                // 对应 C 的 luaU_undump + closureupvalue(L, proto, 0) = _ENV
                let nup = proto.size_upvalues as usize;
                let mut upvals: Vec<UpValRef> = Vec::with_capacity(nup.max(1));
                // 第一个上值是 _ENV，指向全局表
                upvals.push(Rc::new(RefCell::new(UpVal::Closed {
                    value: Box::new(TValue::Table(self.globals.clone())),
                })));
                // 填充剩余上值（如果有）
                for _ in 1..nup {
                    upvals.push(Rc::new(RefCell::new(UpVal::Closed {
                        value: Box::new(TValue::Nil(NilKind::Strict)),
                    })));
                }
                let closure = LClosure {
                    gc_header: GCObjectHeader::new(),
                    proto,
                    upvals,
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

    /// 对应 C 的 `luaL_loadfilex`：从文件或 stdin 加载 Lua 代码。
    ///
    /// - `filename` 为 `Some(path)` 时读取文件；为 `None` 时读取 stdin。
    /// - `mode` 用于控制是否允许文本/二进制块（当前仅文本块可实际加载）。
    ///
    /// 成功时返回 0，并将主闭包压入栈顶；失败时压入错误信息并返回错误码。
    pub fn load_filex(&mut self, filename: Option<&str>, mode: Option<&str>) -> i32 {
        let chunk_name = filename
            .map(|f| format!("@{}", f))
            .unwrap_or_else(|| "=stdin".to_string());

        let mut bytes = match filename {
            Some(name) => match std::fs::read(name) {
                Ok(b) => b,
                Err(err) => {
                    self.push_fstring(&format!("cannot open {}: {}", name, err));
                    return ERR_FILE;
                }
            },
            None => {
                let mut buf = Vec::new();
                if let Err(err) = std::io::stdin().read_to_end(&mut buf) {
                    self.push_fstring(&format!("cannot read stdin: {}", err));
                    return ERR_FILE;
                }
                buf
            }
        };

        self.load_bytes(&mut bytes, &chunk_name, mode)
    }

    pub fn load_file(&mut self, fname: Option<&str>) -> i32 {
        self.load_filex(fname, None)
    }

    /// 从已读取的字节数组加载 Lua 代码。处理 BOM、shebang、编码与二进制签名。
    fn load_bytes(&mut self, bytes: &mut [u8], chunk_name: &str, mode: Option<&str>) -> i32 {
        let after_bom = skip_bom_mut(bytes);
        let (skipped_comment, first, rest, rest_start) = skip_comment(after_bom);

        let is_binary = first == Some(LUA_SIGNATURE[0]);

        if is_binary && !mode_allows_binary(mode) {
            self.push_string("attempt to load a binary chunk (mode is 'text')");
            return ERR_SYNTAX;
        }
        if !is_binary && !mode_allows_text(mode) {
            self.push_string("attempt to load a text chunk (mode is 'binary')");
            return ERR_SYNTAX;
        }
        if is_binary {
            self.push_string("attempt to load a binary chunk");
            return ERR_SYNTAX;
        }
        let rest =
        if skipped_comment {
            if let Some(rest_start) = rest_start {
                after_bom[rest_start - 1] = b'\n';
                &after_bom[(rest_start - 1)..]
            } else {
                rest
            }
        } else {
            rest
        };
        let source = decode_source_bytes(&rest);
        self.load_buffer(&source, chunk_name)
    }

    /// 对应 C 的 getCcalls: 获取 C 调用嵌套数 (低 16 位)
    fn get_ccalls(&self) -> u32 {
        self.n_ccalls & 0xffff
    }

    /// 对应 C 的 ccall: 调用函数 (无错误保护)
    /// Rust 版本: 简化实现，委托给 pcall 并忽略错误
    fn ccall(&mut self, nargs: usize, n_results: i32, inc: u32) {
        self.n_ccalls = self.n_ccalls.saturating_add(inc);
        if self.get_ccalls() >= LUAI_MAXCCALLS {
            // 对应 C 的 checkstackp + luaE_checkcstack
            // 简化: 仅检查栈空间
            self.checkstack(0);
            if self.get_ccalls() >= LUAI_MAXCCALLS {
                runerror(self, "C stack overflow", &[]);
            }
        }
        // 委托给 pcall 执行实际调用 (忽略错误)
        let _ = self.pcall(nargs, n_results, 0);
        self.n_ccalls = self.n_ccalls.saturating_sub(inc);
    }

    pub fn call(&mut self, nargs: usize, nresults: i32) {
        self.ccall(nargs, nresults, 1)
    }

    pub fn call_no_yield(&mut self, nargs: usize, nresults: i32) {
        // nyci = 0x10000 | 1 (C: lstate.h)
        let nyci: u32 = 0x10000 | 1;
        self.ccall(nargs, nresults, nyci)
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
                let saved_pc = self.pc;
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
                // 关键: 将闭包的上值转移到 state，供 GETUPVAL/SETUPVAL 使用
                self.closure_upvals = closure.upvals;
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
                self.pc = saved_pc;
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
                        // 优先使用 build_traceback 格式化的错误消息（含 source:line 前缀）
                        if !self.last_error_msg.is_empty() {
                            let msg = self.last_error_msg.clone();
                            self.last_error_msg.clear();
                            self.push_string(&msg);
                        } else {
                            match e {
                                VmError::RuntimeError(str) => self.push_string(&str),
                                _ => self.push_string(&format!("{}", e)),
                            }
                        }
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
                let nargs = self.stack.len().saturating_sub(func_idx + 1);

                // 基础库函数派发 (标签 1-19, 22)
                // 对应原 C 源码 lbaselib.cpp 的各个函数
                // 注意: ipairsaux (迭代器) 只在 TFORCALL 中调用, 不在此处理
                if crate::stdlib::base_lib::is_base_tag(tag_val) {
                    match crate::stdlib::base_lib::call_base_function(
                        tag_val, self, func_idx, nargs, nresults,
                    ) {
                        Ok(()) => return 0,
                        Err(crate::execute::VmError::RuntimeError(msg)) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&msg);
                            return ERR_RUN;
                        }
                        Err(e) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&format!("{}", e));
                            return ERR_RUN;
                        }
                    }
                }
                // 数学库函数 (标签 200-299)
                if crate::stdlib::math_lib::is_math_tag(tag_val) {
                    match crate::stdlib::math_lib::call_math_function(
                        tag_val, self, func_idx, nargs, nresults,
                    ) {
                        Ok(()) => return 0,
                        Err(crate::execute::VmError::RuntimeError(msg)) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&msg);
                            return ERR_RUN;
                        }
                        Err(e) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&format!("{}", e));
                            return ERR_RUN;
                        }
                    }
                }
                // UTF-8 库函数 (标签 300-309)
                if crate::stdlib::utf8_lib::is_utf8_tag(tag_val) {
                    match crate::stdlib::utf8_lib::call_utf8_function(
                        tag_val, self, func_idx, nargs, nresults,
                    ) {
                        Ok(()) => return 0,
                        Err(crate::execute::VmError::RuntimeError(msg)) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&msg);
                            return ERR_RUN;
                        }
                        Err(e) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&format!("{}", e));
                            return ERR_RUN;
                        }
                    }
                }
                // Table 库函数 (标签 400-409)
                if crate::stdlib::table_lib::is_table_tag(tag_val) {
                    match crate::stdlib::table_lib::call_table_function(
                        tag_val, self, func_idx, nargs, nresults,
                    ) {
                        Ok(()) => return 0,
                        Err(crate::execute::VmError::RuntimeError(msg)) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&msg);
                            return ERR_RUN;
                        }
                        Err(e) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&format!("{}", e));
                            return ERR_RUN;
                        }
                    }
                }
                // 字符串库函数 (标签 100+)
                if tag_val >= 100 {
                    match crate::stdlib::string_lib::call_string_function(
                        tag_val, self, func_idx, nargs, nresults,
                    ) {
                        Ok(()) => return 0,
                        Err(crate::execute::VmError::RuntimeError(msg)) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&msg);
                            return ERR_RUN;
                        }
                        Err(e) => {
                            self.stack.truncate(func_idx);
                            self.push_string(&format!("{}", e));
                            return ERR_RUN;
                        }
                    }
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

        // 打开基础库 (注册 print, type, pcall, error, setmetatable, getmetatable,
        // tonumber, tostring, assert, select, rawequal, rawlen, rawget, rawset,
        // next, ipairs, pairs, xpcall, warn, _G, _VERSION 等全局函数)
        crate::stdlib::base_lib::open_base_lib(self);

        // 打开字符串库 (创建字符串元表)
        crate::stdlib::string_lib::open_string_lib(self);

        // 打开数学库 (注册 math 全局表, 包含 abs/sin/cos/random 等)
        crate::stdlib::math_lib::open_math_lib(self);

        // 打开 UTF-8 库 (注册 utf8 全局表, 包含 offset/codepoint/char/len/codes 等)
        crate::stdlib::utf8_lib::open_utf8_lib(self);

        // 打开 Table 库 (注册 table 全局表, 包含 concat/unpack/pack/insert/remove 等)
        crate::stdlib::table_lib::open_table_lib(self);
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
// load_file 辅助函数
// ============================================================================

/// 跳过 UTF-8 BOM（EF BB BF）。若 BOM 不完整则保留原字节，与 C 的 `skipBOM` 行为一致。
macro_rules! skip_bom_fn {
    // 入口：$mut 可以是空或 `mut`
    ($name:ident, $($mut:tt)?) => {
        pub fn $name(bytes: & $($mut)? [u8]) -> & $($mut)? [u8] {
            const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
            if bytes.starts_with(BOM) {
                let n = BOM.len();
                // 用内部辅助宏根据 $mut 展开不同的切片方式
                skip_bom_fn!(@subslice bytes, $($mut)?, n)
            } else {
                bytes
            }
        }
    };
    // 不可变：普通索引
    (@subslice $bytes:ident, , $n:ident) => {
        & $bytes[$n..]
    };
    // 可变：split_at_mut
    (@subslice $bytes:ident, mut, $n:ident) => {
        $bytes.split_at_mut($n).1
    };
}

// 生成两个函数，无需重复写 starts_with 和 if 逻辑
skip_bom_fn!(skip_bom,);
skip_bom_fn!(skip_bom_mut, mut);

/// 跳过可选的首行注释（以 '#' 开头的 shebang/Unix exec 行）。
///
/// 返回三元组：`(是否跳过了首行, 首字符, 首字符之后的剩余字节)`。
/// 与 C 的 `skipcomment` 一致：`first` 是从流中读取出来的字符，
/// `rest` 包含 `first`，`load_bytes` 不需要再把 `first` 放回缓冲区。
fn skip_comment(bytes: &[u8]) -> (bool, Option<u8>, &[u8], Option<usize>) {
    if bytes.first() == Some(&b'#') {
        let mut pos = 1;
        while pos < bytes.len() && bytes[pos] != b'\n' {
            pos += 1;
        }
        // 同时消费换行符本身，与 C 的 `skipcomment` 一致。
        if pos < bytes.len() && bytes[pos] == b'\n' {
            pos += 1;
        }
        // first 是注释后的第一个字符；rest 是该字符之后的字节
        let first = bytes.get(pos).copied();
        let rest_start = (pos).min(bytes.len());
        (true, first, &bytes[rest_start..], Some(rest_start))
    } else {
        // first 是第一个字符；rest 是该字符之后的字节
        let first = bytes.first().copied();
        let rest = if bytes.is_empty() { &[] } else { &bytes[0..] };
        (false, first, rest, None)
    }
}

/// 判断 `mode` 是否允许文本块。
fn mode_allows_text(mode: Option<&str>) -> bool {
    match mode {
        None => true,
        Some(m) => m.contains('t') || (!m.contains('b') && !m.contains('t')),
    }
}

/// 判断 `mode` 是否允许二进制块。
fn mode_allows_binary(mode: Option<&str>) -> bool {
    match mode {
        None => true,
        Some(m) => m.contains('b'),
    }
}

/// 将源码字节解码为 Rust `String`。
///
/// Lua 源码本质上是字节流。若字节序列是合法 UTF-8，则直接解码；
/// 否则按 ISO-8859-1 逐字节映射为对应 Unicode 码点。这样既能正确处理
/// 常见的 UTF-8 文件，也能处理 `tests_lua/strings.lua` 等 ISO-8859 文件。
fn decode_source_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
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

    // ------------------------------------------------------------------------
    // load_file 编码与文件加载测试
    // ------------------------------------------------------------------------

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("lua_rs_load_file_test_{}_{}", name, std::process::id()));
        p
    }

    fn write_tmp(name: &str, content: &[u8]) -> std::path::PathBuf {
        let path = tmp_path(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_skip_bom() {
        assert_eq!(skip_bom(b"\xef\xbb\xbfhello"), b"hello");
        assert_eq!(skip_bom(b"\xef\xbbhello"), b"\xef\xbbhello");
        assert_eq!(skip_bom(b"hello"), b"hello");
    }

    #[test]
    fn test_skip_comment() {
        let (skipped, first, rest, _) = skip_comment(b"#!/bin/lua\nprint(1)");
        assert!(skipped);
        assert_eq!(first, Some(b'p'));
        assert_eq!(rest, b"print(1)");

        let (skipped, first, rest, _) = skip_comment(b"-- no shebang\nreturn");
        assert!(!skipped);
        assert_eq!(first, Some(b'-'));
        assert_eq!(rest, b"-- no shebang\nreturn");

        let (skipped, first, rest, _) = skip_comment(b"#only shebang");
        assert!(skipped);
        assert_eq!(first, None);
        assert!(rest.is_empty());
    }

    #[test]
    fn test_decode_source_bytes_utf8() {
        let bytes = "local x = 1 -- 中文".as_bytes();
        assert_eq!(decode_source_bytes(bytes), "local x = 1 -- 中文");
    }

    #[test]
    fn test_decode_source_bytes_iso8859() {
        // ISO-8859-1 字节：á é í
        // 在 Rust String 中每个字节被映射为对应 Unicode 码点，UTF-8 编码后长度会变化，
        // 因此这里验证字符数量与码点值保持一致。
        let bytes: Vec<u8> = vec![0xe1, 0xe9, 0xed];
        let s = decode_source_bytes(&bytes);
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars.len(), 3);
        assert_eq!(chars[0] as u32, 0xe1);
        assert_eq!(chars[1] as u32, 0xe9);
        assert_eq!(chars[2] as u32, 0xed);
    }

    #[test]
    fn test_load_file_decodes_iso8859_strings() {
        let mut state = LuaState::new();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests_lua/strings.lua");
        let status = state.load_file(Some(path));
        assert_eq!(status, 0, "load_file should succeed: {:?}", state.to_string(-1));
        assert!(
            matches!(state.stack.last(), Some(TValue::LClosure(_))),
            "stack top should be a closure"
        );
    }

    #[test]
    fn test_load_file_skips_shebang_all() {
        let mut state = LuaState::new();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests_lua/all.lua");
        let status = state.load_file(Some(path));
        assert_eq!(status, 0, "load_file should succeed: {:?}", state.to_string(-1));
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
    }

    #[test]
    fn test_load_file_missing() {
        let mut state = LuaState::new();
        let status = state.load_file(Some("/nonexistent/path/file.lua"));
        assert_eq!(status, ERR_FILE);
        let msg = state.to_string(-1).unwrap_or_default();
        assert!(msg.contains("cannot open"), "unexpected error: {}", msg);
    }

    #[test]
    fn test_load_file_binary_signature() {
        let mut content = b"\x1bLua\x55".to_vec();
        content.extend_from_slice(&[0; 10]);
        let path = write_tmp("binary", &content);
        let mut state = LuaState::new();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(status, ERR_SYNTAX);
        let msg = state.to_string(-1).unwrap_or_default();
        assert!(msg.contains("binary chunk"), "unexpected error: {}", msg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_mode_text_rejects_binary() {
        let mut content = b"\x1bLua\x55".to_vec();
        content.extend_from_slice(&[0; 10]);
        let path = write_tmp("bin_text_mode", &content);
        let mut state = LuaState::new();
        let status = state.load_filex(Some(path.to_str().unwrap()), Some("t"));
        assert_eq!(status, ERR_SYNTAX);
        let msg = state.to_string(-1).unwrap_or_default();
        assert!(msg.contains("mode is 'text'"), "unexpected error: {}", msg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_mode_binary_rejects_text() {
        let path = write_tmp("text_bin_mode", b"return 42\n");
        let mut state = LuaState::new();
        let status = state.load_filex(Some(path.to_str().unwrap()), Some("b"));
        assert_eq!(status, ERR_SYNTAX);
        let msg = state.to_string(-1).unwrap_or_default();
        assert!(msg.contains("mode is 'binary'"), "unexpected error: {}", msg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_bom() {
        let path = write_tmp("bom", b"\xef\xbb\xbfreturn 42\n");
        let mut state = LuaState::new();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(status, 0, "load_file should succeed: {:?}", state.to_string(-1));
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_shebang_only() {
        let path = write_tmp("shebang_only", b"#!/usr/bin/env lua\n");
        let mut state = LuaState::new();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(status, 0, "empty shebang file should load: {:?}", state.to_string(-1));
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_latin1_in_string_literal() {
        // ISO-8859-1 字节直接出现在字符串字面量中
        let mut bytes: Vec<u8> = b"local s = \"".to_vec();
        bytes.extend_from_slice(&[0xe1, 0xe9, 0xed]);
        bytes.extend_from_slice(b"\"\nreturn #s\n");
        let path = write_tmp("latin1_str", &bytes);
        let mut state = LuaState::new();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(status, 0, "latin1 string literal should load: {:?}", state.to_string(-1));
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_shebang_with_latin1() {
        // 首行是 shebang，后续包含 ISO-8859-1 字节
        let mut bytes: Vec<u8> = b"#!/bin/lua\nlocal s = \"".to_vec();
        bytes.extend_from_slice(&[0xc1, 0xc9, 0xcd]);
        bytes.extend_from_slice(b"\"\n");
        let path = write_tmp("shebang_latin1", &bytes);
        let mut state = LuaState::new();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(status, 0, "shebang + latin1 should load: {:?}", state.to_string(-1));
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
        let _ = std::fs::remove_file(&path);
    }
}