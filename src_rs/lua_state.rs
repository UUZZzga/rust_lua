use crate::objects::{LClosure, LuaType, NilKind, TValue};
use crate::strings::LuaString;
use crate::strings::StringTable;
use crate::table::Table;
use crate::gc::{GCObjectHeader, GCState};
use crate::execute::{VmExecutor, VmResult};
use std::rc::Rc;

pub const ERR_RUN: i32 = 2;
pub const ERR_SYNTAX: i32 = 3;
pub const MULT_RET: i32 = -1;
pub const MIN_STACK: usize = 20;

pub struct LuaState {
    pub stack: Vec<TValue>,
    pub globals: Table,
    pub registry: Table,
    pub string_table: StringTable,
    pub gc: Rc<GCState>,
}

impl LuaState {
    pub fn new() -> Self {
        let gc = Rc::new(GCState::default_incremental());
        let mut registry = Table::new();
        let globals = Table::new();
        registry.set(
            TValue::Integer(2), // LUA_RIDX_GLOBALS
            TValue::Table(globals.clone()),
        );
        LuaState {
            stack: Vec::with_capacity(MIN_STACK),
            globals,
            registry,
            string_table: StringTable::new(),
            gc,
        }
    }

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

    /// Absolute index → real index (positive)
    pub fn abs_index(&self, idx: isize) -> usize {
        let len = self.stack.len() as isize;
        if idx >= 0 {
            idx as usize
        } else if len + idx >= 0 {
            (len + idx) as usize
        } else {
            0 // invalid
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
        let ls = self.string_table.intern(s);
        self.stack.push(TValue::Str(ls));
    }

    pub fn push_lstring(&mut self, s: &[u8]) {
        let text = String::from_utf8_lossy(s).into_owned();
        let ls = self.string_table.intern(&text);
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
        let key = TValue::Str(self.string_table.intern(name));
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
        let key = TValue::Str(self.string_table.intern(name));
        if let Some(val) = self.stack.pop() {
            self.globals.set(key, val);
        }
    }

    pub fn set_field(&mut self, idx: isize, key_name: &str) {
        let abs = self.abs_index(idx);
        let val = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let key = TValue::Str(self.string_table.intern(key_name));
        if abs > 0 && abs <= self.stack.len() {
            let tbl = &mut self.stack[abs - 1];
            if let TValue::Table(ref mut t) = tbl {
                t.set(key, val);
            }
        }
    }

    pub fn get_field(&mut self, idx: isize, key_name: &str) -> LuaType {
        let abs = self.abs_index(idx);
        let key = TValue::Str(self.string_table.intern(key_name));
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

    pub fn gc_stop(&self) {
        // placeholder: GC always running in incremental mode for now
    }

    pub fn gc_restart(&self) {
        // placeholder
    }

    pub fn gc_gen(&self) {
        // placeholder
    }

    // ====== Diagnostics ======

    pub fn warning(&mut self, _msg: &str, _tocont: bool) {
        // placeholder
    }

    pub fn check_version(&self) {
        // Rust VM is always compatible with itself
    }

    // ====== Call Meta ======

    pub fn call_meta(&self, _idx: isize, _event: &str) -> bool {
        // placeholder: metamethod dispatch not yet implemented in LuaState
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

    /// Push a Rust closure as a 'C function'-like entry on the stack.
    /// Stored as LightUserData for identification; actual dispatch is done
    /// by matching the pointer in pcall.
    pub fn push_rust_fn(&mut self, _f: fn(&mut LuaState) -> i32, tag: usize) {
        self.push_light_userdata(tag as *mut std::ffi::c_void);
    }

    // ====== Load Code (compilation via C bridge, then convert to Proto) ======

    /// Load Lua source code from a string, producing compiled Proto on the stack.
    /// Returns OK (0) or an error code.
    pub fn load_buffer(&mut self, _code: &str, chunk_name: &str) -> i32 {
        // Since the Rust parser is not yet available, this is a stub.
        // The real implementation would compile the source to Proto,
        // then create a Lua closure and push it on the stack.

        // For now, we create a minimal Proto that pushes the right info
        // as error so the user sees appropriate messages.
        let empty_proto = crate::func::new_proto();
        let closure = LClosure {
            gc_header: GCObjectHeader::new(),
            proto: empty_proto,
            upvals: Vec::new(),
        };
        self.stack.push(TValue::LClosure(closure));

        // Actually this won't work for real code. We need the C parser bridge.
        // Return a syntax error indicating compilation is not yet available in pure Rust.
        // For a fully working interpreter, we'd need either:
        // a) A pure Rust parser
        // b) A bridge from C compilation to Rust Proto

        // For now, push a meaningful error:
        self.pop(1); // remove the empty closure
        self.push_fstring(&format!(
            "{}:{}: compilation not yet available in pure Rust VM",
            chunk_name, EOFMARK
        ));
        ERR_SYNTAX
    }

    /// Load Lua source from a file (by name).
    /// Returns OK (0) or an error code.
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
            // stdin
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

    /// Protected call: call the function at (gettop - nargs), with nargs arguments
    /// and expecting nresults return values. The msghandler (if any) is at errfunc.
    ///
    /// Returns OK (0) on success, error code on failure.
    pub fn pcall(&mut self, nargs: usize, nresults: i32, _errfunc: isize) -> i32 {
        let total = self.gettop();
        let func_idx = total.saturating_sub(nargs + 1);
        if func_idx >= total {
            return ERR_RUN;
        }

        let func_val = self.stack[func_idx].clone();
        match func_val {
            TValue::LClosure(closure) => {
                // Extract args
                let args_start = func_idx + 1;
                let args_end = total;
                let args: Vec<TValue> = self.stack[args_start..args_end].to_vec();

                // Build initial stack with args
                let new_stack = args;

                match VmExecutor::execute(&closure.proto, 0, new_stack, self.gc.clone()) {
                    Ok(VmResult::Return(n)) => {
                        // Remove func + args from stack
                        self.stack.truncate(func_idx);
                        // Push return values
                        // We need to get return vals from the executor stack
                        // Since VmExecutor::execute doesn't return the stack, we
                        // can't easily get return values. This is a limitation.
                        // For now, just push nil.
                        let actual_n = if nresults == MULT_RET {
                            n
                        } else if nresults < 0 {
                            0
                        } else {
                            (n).min(nresults as usize)
                        };
                        for _ in 0..actual_n {
                            self.push_nil();
                        }
                        return 0; // OK
                    }
                    Ok(VmResult::Call { .. }) | Ok(VmResult::TailCall { .. }) => {
                        // Nested calls not yet supported
                        self.stack.truncate(func_idx);
                        self.push_string("nested calls not supported in pure Rust VM");
                        return ERR_RUN;
                    }
                    Ok(VmResult::Done) => {
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
                // C functions not supported in pure Rust VM
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
                // Handle special tagged closures
                self.stack.truncate(func_idx);
                self.push_string(&format!("attempt to call a non-function value (tag={})", tag as usize));
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
        // Basic libraries not yet implemented in pure Rust.
        // We pre-populate globals with essential items only.

        // arg table (will be filled by the caller)
        let arg_table = Table::new();
        self.globals.set(
            TValue::Str(self.string_table.intern("arg")),
            TValue::Table(arg_table),
        );
    }

    // ====== Hook ======

    pub fn set_hook(&mut self, _hook: Option<(usize, usize)>, _mask: i32, _count: i32) {
        // placeholder
    }

    // ====== Helper for Internal Use ======

    pub fn intern_str(&self, s: &str) -> LuaString {
        self.string_table.intern(s)
    }

    pub fn intern(&self, s: &str) -> LuaString {
        self.string_table.intern(s)
    }
}

const EOFMARK: &str = "<eof>";

fn format_float(f: f64) -> String {
    if f.is_nan() { return "nan".to_string(); }
    if f.is_infinite() { return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() }; }
    if f == 0.0 { return "0.0".to_string(); }
    let s = format!("{:.15}", f);
    let s = s.trim_end_matches('0');
    if s.ends_with('.') { format!("{}0", s) } else { s.to_string() }
}