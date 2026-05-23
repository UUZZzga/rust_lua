use crate::lua_ffi;
use crate::opcodes::{
    self, OPNAMES, TM_EVENT_NAMES, get_opcode, getarg_a, getarg_b, getarg_c,
    getarg_bx, getarg_sbx, getarg_sj,
    getarg_vb, testarg_k, getarg, POS_A, SIZE_BX, SIZE_A,
};
use std::ffi::{c_int, c_void};
use std::ptr;

#[derive(Debug, Clone)]
pub struct DumpInstruction {
    pub opcode: u8,
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub k: u32,
    pub bx: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DumpConstant {
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

#[derive(Debug)]
pub struct DumpedFunction {
    pub linedefined: i32,
    pub lastlinedefined: i32,
    pub numparams: u8,
    pub flag: u8,
    pub maxstacksize: u8,
    pub code: Vec<DumpInstruction>,
    pub constants: Vec<DumpConstant>,
    pub protos: Vec<DumpedFunction>,
}

struct BytecodeReader {
    data: Vec<u8>,
    pos: usize,
    strings: Vec<String>,
}

impl BytecodeReader {
    fn new(data: Vec<u8>) -> Self {
        BytecodeReader {
            data,
            pos: 0,
            strings: Vec::new(),
        }
    }

    fn read_byte(&mut self) -> u8 {
        let b = self.data[self.pos];
        self.pos += 1;
        b
    }

    fn read_bytes(&mut self, n: usize) -> &[u8] {
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        slice
    }

    fn read_varint(&mut self) -> u64 {
        let mut x: u64 = 0;
        loop {
            let b = self.read_byte();
            x = (x << 7) | (b & 0x7f) as u64;
            if (b & 0x80) == 0 {
                break;
            }
        }
        x
    }

    fn read_int(&mut self) -> i32 {
        self.read_varint() as i32
    }

    fn read_size(&mut self) -> usize {
        self.read_varint() as usize
    }

    fn read_integer(&mut self) -> i64 {
        let cx = self.read_varint();
        if cx & 1 == 0 {
            (cx >> 1) as i64
        } else {
            -((cx >> 1) as i64) - 1
        }
    }

    fn read_float(&mut self) -> f64 {
        let bytes = self.read_bytes(8);
        let mut arr = [0u8; 8];
        arr.copy_from_slice(bytes);
        f64::from_le_bytes(arr)
    }

    fn align(&mut self, align: usize) {
        let padding = (align - (self.pos % align)) % align;
        self.pos += padding;
    }

    fn read_string(&mut self) -> String {
        let size = self.read_size();
        if size == 0 {
            let idx = self.read_varint();
            if idx == 0 {
                return String::new();
            }
            return self.strings[(idx - 1) as usize].clone();
        }
        let bytes = self.read_bytes(size);
        let len = if bytes.last() == Some(&0) { size - 1 } else { size };
        let s = String::from_utf8_lossy(&bytes[..len]).to_string();
        self.strings.push(s.clone());
        s
    }

    fn read_instruction(&mut self, raw: u32) -> DumpInstruction {
        use crate::opcodes;
        DumpInstruction {
            opcode: (raw & 0x7f) as u8,
            a: opcodes::getarg_a(raw) as u32,
            b: opcodes::getarg_b(raw) as u32,
            c: opcodes::getarg_c(raw) as u32,
            k: (raw >> opcodes::POS_K) & 1,
            bx: opcodes::getarg(raw, opcodes::POS_BX, opcodes::SIZE_BX) as u32,
        }
    }

    fn read_code(&mut self) -> Vec<DumpInstruction> {
        let sizecode = self.read_int() as usize;
        self.align(4);
        let mut code = Vec::with_capacity(sizecode);
        for _ in 0..sizecode {
            let bytes = self.read_bytes(4);
            let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            code.push(self.read_instruction(raw));
        }
        code
    }

    fn read_constants(&mut self) -> Vec<DumpConstant> {
        let n = self.read_int() as usize;
        let mut constants = Vec::with_capacity(n);
        for _ in 0..n {
            let tt = self.read_byte();
            let c = match tt {
                0 => DumpConstant::Nil,
                1 => DumpConstant::Boolean(false),
                17 => DumpConstant::Boolean(true),            // LUA_VTRUE = 0x11
                3 => DumpConstant::Integer(self.read_integer()),  // LUA_VNUMINT = 3
                19 => DumpConstant::Float(self.read_float()),     // LUA_VNUMFLT = 0x13
                4 | 20 => DumpConstant::String(self.read_string()), // 4=VSHRSTR, 20=VLNGSTR
                _ => DumpConstant::Nil,
            };
            constants.push(c);
        }
        constants
    }

    fn read_upvalues(&mut self) {
        let n = self.read_int() as usize;
        for _ in 0..n {
            let _instack = self.read_byte();
            let _idx = self.read_byte();
            let _kind = self.read_byte();
        }
    }

    fn read_function(&mut self) -> DumpedFunction {
        let linedefined = self.read_int();
        let lastlinedefined = self.read_int();
        let numparams = self.read_byte();
        let flag = self.read_byte();
        let maxstacksize = self.read_byte();
        let code = self.read_code();
        let constants = self.read_constants();
        self.read_upvalues();
        let nprotos = self.read_int() as usize;
        let mut protos = Vec::with_capacity(nprotos);
        for _ in 0..nprotos {
            protos.push(self.read_function());
        }
        let _source = self.read_string();
        self.skip_debug();
        DumpedFunction {
            linedefined,
            lastlinedefined,
            numparams,
            flag,
            maxstacksize,
            code,
            constants,
            protos,
        }
    }

    fn read_num_info_int(&mut self) {
        let _size = self.read_byte();
        let _value = self.read_bytes(4);
    }

    fn read_num_info_inst(&mut self) {
        let _size = self.read_byte();
        let _value = self.read_bytes(4);
    }

    fn read_num_info_integer(&mut self) {
        let _size = self.read_byte();
        let _value = self.read_bytes(8);
    }

    fn read_num_info_number(&mut self) {
        let _size = self.read_byte();
        let _value = self.read_bytes(8);
    }

    fn skip_debug(&mut self) {
        let n_lineinfo = self.read_int() as usize;
        if n_lineinfo > 0 {
            self.pos += n_lineinfo;
        }
        let n_abslineinfo = self.read_int() as usize;
        if n_abslineinfo > 0 {
            self.align(4);
            self.pos += n_abslineinfo * std::mem::size_of::<i32>() * 2;
        }
        let n_locvars = self.read_int() as usize;
        for _ in 0..n_locvars {
            let _varname = self.read_string();
            let _startpc = self.read_int();
            let _endpc = self.read_int();
        }
        let n_upvnames = self.read_int() as usize;
        for _ in 0..n_upvnames {
            let _name = self.read_string();
        }
    }
}

pub fn parse_dump(data: Vec<u8>) -> Result<DumpedFunction, String> {
    let mut reader = BytecodeReader::new(data);

    if reader.data.len() < 12 {
        return Err("dump too short".to_string());
    }

    let sig = reader.read_bytes(4);
    if sig != b"\x1bLua" {
        return Err("bad signature".to_string());
    }

    let _version = reader.read_byte();
    let _format = reader.read_byte();
    let _luac_data = reader.read_bytes(6);

    reader.read_num_info_int();
    reader.read_num_info_inst();
    reader.read_num_info_integer();
    reader.read_num_info_number();

    let _num_upvalues = reader.read_byte();
    let func = reader.read_function();

    Ok(func)
}

pub unsafe fn compile_with_c_lua(source: &[u8]) -> Result<Vec<u8>, String> {
    let L = lua_ffi::luaL_newstate();
    if L.is_null() {
        return Err("failed to create lua state".to_string());
    }
    lua_ffi::luaL_checkversion(L);
    lua_ffi::luaL_openselectedlibs(L, 0, 0);

    let load_result = lua_ffi::luaL_loadbufferx(
        L,
        source.as_ptr() as *const i8,
        source.len(),
        c"=test".as_ptr(),
        ptr::null(),
    );

    if load_result != lua_ffi::LUA_OK {
        let err_ptr = lua_ffi::lua_tolstring(L, -1, ptr::null_mut());
        let err = lua_ffi::from_cstr(err_ptr).unwrap_or("unknown error");
        lua_ffi::lua_close(L);
        return Err(format!("C compile error: {}", err));
    }

    let dump_data = Box::into_raw(Box::new(Vec::<u8>::new()));

    extern "C" fn writer(_L: *mut lua_ffi::lua_State, p: *const c_void, sz: usize, ud: *mut c_void) -> c_int {
        if sz == 0 { return 0; }
        let data: &mut Vec<u8> = unsafe { &mut *(ud as *mut Vec<u8>) };
        let slice = unsafe { std::slice::from_raw_parts(p as *const u8, sz) };
        data.extend_from_slice(slice);
        0
    }

    let result = lua_ffi::lua_dump(L, writer, dump_data as *mut c_void, 0);

    if result != 0 {
        let _ = Box::from_raw(dump_data);
        lua_ffi::lua_close(L);
        return Err("dump failed".to_string());
    }

    let result_data = *Box::from_raw(dump_data);

    lua_ffi::lua_close(L);

    Ok(result_data)
}

fn format_constant(constants: &[DumpConstant], idx: usize) -> String {
    if idx >= constants.len() {
        return format!("?{}", idx);
    }
    match &constants[idx] {
        DumpConstant::Nil => "nil".to_string(),
        DumpConstant::Boolean(b) => if *b { "true" } else { "false" }.to_string(),
        DumpConstant::Integer(i) => format!("{}", i),
        DumpConstant::Float(f) => {
            let s = format!("{}", f);
            if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                format!("{}.0", s)
            } else {
                s
            }
        }
        DumpConstant::String(s) => format!("\"{}\"", s.escape_debug()),
    }
}

fn format_operands(op: u32, a: i32, b: i32, c: i32, bx: i32, sbx: i32, sj: i32, k: bool) -> String {
    let isk = if k { "k" } else { "" };
    let sc = c - 127;
    let sb = b - 127;

    match get_opcode(op) {
        opcodes::OpCode::MOVE => format!("{} {}", a, b),
        opcodes::OpCode::LOADI | opcodes::OpCode::LOADF => format!("{} {}", a, sbx),
        opcodes::OpCode::LOADK => format!("{} {}", a, bx),
        opcodes::OpCode::LOADKX => format!("{}", a),
        opcodes::OpCode::LOADFALSE | opcodes::OpCode::LFALSESKIP | opcodes::OpCode::LOADTRUE => format!("{}", a),
        opcodes::OpCode::LOADNIL => format!("{} {}", a, b),
        opcodes::OpCode::GETUPVAL | opcodes::OpCode::SETUPVAL => format!("{} {}", a, b),
        opcodes::OpCode::GETTABUP | opcodes::OpCode::GETTABLE => format!("{} {} {}", a, b, c),
        opcodes::OpCode::GETI | opcodes::OpCode::GETFIELD => format!("{} {} {}", a, b, c),
        opcodes::OpCode::SETTABUP | opcodes::OpCode::SETTABLE | opcodes::OpCode::SETI
        | opcodes::OpCode::SETFIELD => format!("{} {} {}{}", a, b, c, isk),
        opcodes::OpCode::NEWTABLE => {
            let vb = getarg_vb(op);
            format!("{} {} {}{}", a, vb, c, isk)
        }
        opcodes::OpCode::SELF => format!("{} {} {}{}", a, b, c, isk),
        opcodes::OpCode::ADDI | opcodes::OpCode::SHLI | opcodes::OpCode::SHRI => format!("{} {} {}", a, b, sc),
        opcodes::OpCode::ADDK | opcodes::OpCode::SUBK | opcodes::OpCode::MULK
        | opcodes::OpCode::MODK | opcodes::OpCode::POWK | opcodes::OpCode::DIVK
        | opcodes::OpCode::IDIVK | opcodes::OpCode::BANDK | opcodes::OpCode::BORK
        | opcodes::OpCode::BXORK => format!("{} {} {}", a, b, c),
        opcodes::OpCode::ADD | opcodes::OpCode::SUB | opcodes::OpCode::MUL
        | opcodes::OpCode::MOD | opcodes::OpCode::POW | opcodes::OpCode::DIV
        | opcodes::OpCode::IDIV | opcodes::OpCode::BAND | opcodes::OpCode::BOR
        | opcodes::OpCode::BXOR | opcodes::OpCode::SHL | opcodes::OpCode::SHR
        => format!("{} {} {}", a, b, c),
        opcodes::OpCode::MMBIN => format!("{} {} {}", a, b, c),
        opcodes::OpCode::MMBINI => format!("{} {} {} {}", a, sb, c, isk),
        opcodes::OpCode::MMBINK => format!("{} {} {} {}", a, b, c, isk),
        opcodes::OpCode::UNM | opcodes::OpCode::BNOT | opcodes::OpCode::NOT
        | opcodes::OpCode::LEN => format!("{} {}", a, b),
        opcodes::OpCode::CONCAT => format!("{} {}", a, b),
        opcodes::OpCode::CLOSE | opcodes::OpCode::TBC => format!("{}", a),
        opcodes::OpCode::JMP => format!("{}", sj),
        opcodes::OpCode::EQ | opcodes::OpCode::LT | opcodes::OpCode::LE
        => format!("{} {} {}", a, b, isk),
        opcodes::OpCode::EQK => format!("{} {} {}", a, b, isk),
        opcodes::OpCode::EQI | opcodes::OpCode::LTI | opcodes::OpCode::LEI
        | opcodes::OpCode::GTI | opcodes::OpCode::GEI => format!("{} {} {}", a, sb, isk),
        opcodes::OpCode::TEST => format!("{} {}", a, isk),
        opcodes::OpCode::TESTSET => format!("{} {} {}", a, b, isk),
        opcodes::OpCode::CALL => format!("{} {} {}", a, b, c),
        opcodes::OpCode::TAILCALL => format!("{} {} {}{}", a, b, c, isk),
        opcodes::OpCode::RETURN => format!("{} {} {}{}", a, b, c, isk),
        opcodes::OpCode::RETURN0 => String::new(),
        opcodes::OpCode::RETURN1 => format!("{}", a),
        opcodes::OpCode::FORLOOP | opcodes::OpCode::FORPREP
        | opcodes::OpCode::TFORPREP | opcodes::OpCode::TFORLOOP => format!("{} {}", a, bx),
        opcodes::OpCode::TFORCALL => format!("{} {}", a, c),
        opcodes::OpCode::SETLIST => {
            let vb = getarg_vb(op);
            format!("{} {} {}{}", a, vb, c, isk)
        }
        opcodes::OpCode::CLOSURE => format!("{} {}", a, bx),
        opcodes::OpCode::VARARG => format!("{} {} {}{}", a, b, c, isk),
        opcodes::OpCode::GETVARG => format!("{} {} {}", a, b, c),
        opcodes::OpCode::ERRNNIL => format!("{} {}", a, bx),
        opcodes::OpCode::VARARGPREP => format!("{}", a),
        opcodes::OpCode::EXTRAARG => {
            let ax = getarg(op, POS_A, SIZE_BX + SIZE_A) as i32;
            format!("{}", ax)
        }
    }
}

fn format_c_comment(inst: &DumpInstruction, constants: &[DumpConstant]) -> String {
    let op = get_opcode(inst.opcode as u32);
    let c = inst.c as i32;
    let isk = inst.k != 0;
    let pc = 0;

    match op {
        opcodes::OpCode::LOADK | opcodes::OpCode::LOADKX => {
            let idx = if op == opcodes::OpCode::LOADKX { inst.c as usize } else { inst.bx as usize };
            if idx < constants.len() {
                format!("\t; {}", format_constant(constants, idx))
            } else { String::new() }
        }
        opcodes::OpCode::GETTABUP | opcodes::OpCode::GETFIELD => {
            format!("\t; {}", format_constant(constants, c as usize))
        }
        opcodes::OpCode::SETTABUP | opcodes::OpCode::SETFIELD => {
            let b_const = format_constant(constants, inst.b as usize);
            let mut s = format!("\t; {}", b_const);
            if isk { s.push_str(&format!(" {}", format_constant(constants, c as usize))); }
            s
        }
        opcodes::OpCode::SETTABLE | opcodes::OpCode::SETI => {
            if isk { format!("\t; {}", format_constant(constants, c as usize)) } else { String::new() }
        }
        opcodes::OpCode::NEWTABLE => {
            let total = c as usize + opcodes::SIZE_C as usize + 1;
            format!("\t; {}", total)
        }
        opcodes::OpCode::SELF => {
            if isk { format!("\t; {}", format_constant(constants, c as usize)) } else { String::new() }
        }
        opcodes::OpCode::ADDK | opcodes::OpCode::SUBK | opcodes::OpCode::MULK
        | opcodes::OpCode::MODK | opcodes::OpCode::POWK | opcodes::OpCode::DIVK
        | opcodes::OpCode::IDIVK | opcodes::OpCode::BANDK | opcodes::OpCode::BORK
        | opcodes::OpCode::BXORK => {
            format!("\t; {}", format_constant(constants, c as usize))
        }
        opcodes::OpCode::MMBIN => {
            let event_idx = c as usize;
            if event_idx < TM_EVENT_NAMES.len() {
                format!("\t; {}", TM_EVENT_NAMES[event_idx])
            } else { String::new() }
        }
        opcodes::OpCode::MMBINI => {
            let event_idx = c as usize;
            let mut s = if event_idx < TM_EVENT_NAMES.len() {
                format!("\t; {}", TM_EVENT_NAMES[event_idx])
            } else { String::new() };
            if isk { s.push_str(" flip"); }
            s
        }
        opcodes::OpCode::MMBINK => {
            let event_idx = c as usize;
            let mut s = if event_idx < TM_EVENT_NAMES.len() {
                format!("\t; {} ", TM_EVENT_NAMES[event_idx])
            } else { String::new() };
            s.push_str(&format_constant(constants, inst.b as usize));
            if isk { s.push_str(" flip"); }
            s
        }
        opcodes::OpCode::JMP => {
            let sj = (inst.a as i32) | ((inst.b as i32) << 8) | ((inst.c as i32) << 16) | ((inst.k as i32) << 17);
            let sj_signed = sj - opcodes::OFFSET_sJ;
            format!("\t; to {}", sj_signed + pc as i32 + 2)
        }
        opcodes::OpCode::EQK => {
            format!("\t; {}", format_constant(constants, inst.b as usize))
        }
        opcodes::OpCode::CALL => {
            let in_args = if inst.b == 0 { "all in".to_string() } else { format!("{} in", inst.b as i32 - 1) };
            let out_args = if inst.c == 0 { "all out".to_string() } else { format!("{} out", inst.c as i32 - 1) };
            format!("\t; {} {}", in_args, out_args)
        }
        opcodes::OpCode::TAILCALL => {
            format!("\t; {} in", inst.b as i32 - 1)
        }
        opcodes::OpCode::RETURN => {
            if inst.b == 0 { "\t; all out".to_string() } else { format!("\t; {} out", inst.b as i32 - 1) }
        }
        opcodes::OpCode::FORLOOP => {
            format!("\t; to {}", pc as i32 - inst.bx as i32 + 2)
        }
        opcodes::OpCode::FORPREP => {
            format!("\t; exit to {}", pc as i32 + inst.bx as i32 + 3)
        }
        opcodes::OpCode::TFORPREP => {
            format!("\t; to {}", pc as i32 + inst.bx as i32 + 2)
        }
        opcodes::OpCode::TFORLOOP => {
            format!("\t; to {}", pc as i32 - inst.bx as i32 + 2)
        }
        opcodes::OpCode::SETLIST => {
            if isk { format!("\t; {}", c as usize + opcodes::SIZE_C as usize + 1) } else { String::new() }
        }
        opcodes::OpCode::LOADNIL => {
            format!("\t; {} out", inst.b as i32 + 1)
        }
        opcodes::OpCode::VARARG => {
            if inst.c == 0 { "\t; all out".to_string() } else { format!("\t; {} out", inst.c as i32 - 1) }
        }
        opcodes::OpCode::ERRNNIL => {
            if inst.bx == 0 { "\t; ?".to_string() }
            else { format!("\t; {}", format_constant(constants, (inst.bx as usize) - 1)) }
        }
        _ => String::new(),
    }
}

pub fn compare_instructions(rust_code: &[u32], c_code: &[DumpInstruction]) -> Vec<String> {
    let mut diffs = Vec::new();
    let max_len = rust_code.len().max(c_code.len());
    for i in 0..max_len {
        let r_inst = rust_code.get(i).copied();
        let c_inst = c_code.get(i);

        match (r_inst, c_inst) {
            (Some(r), Some(c)) => {
                let r_op = r & 0x7f;
                let c_op = c.opcode as u32;
                if r_op != c_op {
                    diffs.push(format!(
                        "PC {}: opcode mismatch: Rust={:?}, C={:?}",
                        i, r_op, c_op
                    ));
                }
                let r_a = (r >> 7) & 0xff;
                let c_a = c.a;
                if r_a != c_a {
                    diffs.push(format!(
                        "PC {}: A field mismatch: Rust={}, C={} (op={:?})",
                        i, r_a, c_a, r_op
                    ));
                }
                let r_b = (r >> 16) & 0xff;
                let c_b = c.b;
                if r_b != c_b {
                    diffs.push(format!(
                        "PC {}: B field mismatch: Rust={}, C={} (op={:?})",
                        i, r_b, c_b, r_op
                    ));
                }
                let r_c = (r >> 24) & 0xff;
                let c_c = c.c;
                if r_c != c_c {
                    diffs.push(format!(
                        "PC {}: C field mismatch: Rust={}, C={} (op={:?})",
                        i, r_c, c_c, r_op
                    ));
                }
            }
            (Some(r), None) => {
                diffs.push(format!(
                    "PC {}: extra Rust instruction: {:08x}",
                    i, r
                ));
            }
            (None, Some(c)) => {
                diffs.push(format!(
                    "PC {}: extra C instruction: op={}, A={}, B={}, C={}",
                    i, c.opcode, c.a, c.b, c.c
                ));
            }
            (None, None) => {}
        }
    }
    diffs
}

pub fn format_instruction(raw: u32) -> String {
    let op = raw;
    let a = getarg_a(op);
    let b = getarg_b(op);
    let c = getarg_c(op);
    let k = testarg_k(op);
    let bx = getarg_bx(op);
    let sbx = getarg_sbx(op);
    let sj = getarg_sj(op);

    let opcode = get_opcode(op);
    let op_name = OPNAMES[opcode as usize];
    let operands = format_operands(op, a, b, c, bx, sbx, sj, k);
    format!("{:08x}\t{}\t{}", raw, op_name, operands)
}

pub fn dump_instructions(code: &[u32]) -> String {
    code.iter()
        .enumerate()
        .map(|(i, inst)| format!("{}\t[-]\t{}", i + 1, format_instruction(*inst)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn format_c_instruction(inst: &DumpInstruction, constants: &[DumpConstant]) -> String {
    let opcode = get_opcode(inst.opcode as u32);
    let op_name = OPNAMES[opcode as usize];
    let a = inst.a as i32;
    let b = inst.b as i32;
    let c = inst.c as i32;
    let k = inst.k != 0;
    let bx = inst.bx as i32;
    let sbx = bx - opcodes::OFFSET_SBX;
    let sj = getarg_sj(inst.opcode as u32);

    let operands = format_operands(inst.opcode as u32, a, b, c, bx, sbx, sj, k);
    let comment = format_c_comment(inst, constants);
    format!("{}\t{}\t{}", op_name, operands, comment)
}

pub fn dump_c_instructions(code: &[DumpInstruction], constants: &[DumpConstant]) -> String {
    code.iter()
        .enumerate()
        .map(|(i, inst)| format!("{}\t[-]\t{}", i + 1, format_c_instruction(inst, constants)))
        .collect::<Vec<_>>()
        .join("\n")
}