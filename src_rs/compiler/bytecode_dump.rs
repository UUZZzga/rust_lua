#[cfg(feature = "ffi")]
use crate::lua_ffi;
use crate::opcodes::{
    self, get_opcode, get_opmode, getarg, getarg_a, getarg_b, getarg_bx, getarg_c, getarg_sbx,
    getarg_sj, getarg_vb, getarg_vc, testarg_k, OFFSET_sJ, OpCode, OpMode, OPNAMES, POS_A, POS_B,
    POS_BX, POS_C, POS_K, POS_SJ, POS_VB, POS_VC, SIZE_A, SIZE_BX, TM_EVENT_NAMES,
};
use imara_diff::{Algorithm, Diff, InternedInput};
use std::ffi::{c_int, c_void};
use std::ptr;
use std::rc::Rc;

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
    pub upvalues: Vec<(bool, u8, u8)>, // (instack, idx, kind)
    pub protos: Vec<DumpedFunction>,
    // 调试信息
    pub source: Option<String>,
    pub line_info: Vec<i8>,
    pub abs_line_info: Vec<(i32, i32)>,            // (pc, line)
    pub loc_vars: Vec<(Option<String>, i32, i32)>, // (varname, startpc, endpc)
    pub upvalue_names: Vec<Option<String>>,
    // 调试信息大小（用于对比）
    pub size_line_info: i32,
    pub size_abs_line_info: i32,
    pub size_loc_vars: i32,
}

struct BytecodeReader {
    data: Vec<u8>,
    pos: usize,
    strings: Vec<String>,
    /// 截断错误标志 — 对应 C 的 error(S, "truncated chunk")
    error: Option<String>,
}

impl BytecodeReader {
    fn new(data: Vec<u8>) -> Self {
        BytecodeReader {
            data,
            pos: 0,
            strings: Vec::new(),
            error: None,
        }
    }

    fn read_byte(&mut self) -> u8 {
        if self.error.is_some() {
            return 0;
        }
        if self.pos >= self.data.len() {
            self.error = Some("truncated chunk".to_string());
            return 0;
        }
        let b = self.data[self.pos];
        self.pos += 1;
        b
    }

    fn read_bytes(&mut self, n: usize) -> &[u8] {
        if self.error.is_some() {
            return &[];
        }
        if self.pos + n > self.data.len() {
            self.error = Some("truncated chunk".to_string());
            return &[];
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        slice
    }

    /// 检查截断错误 — 在 header 校验前调用, 对应 C 的 loadBlock 失败时的 error(S, "truncated chunk")
    fn check(&self) -> Result<(), String> {
        if let Some(ref err) = self.error {
            return Err(err.clone());
        }
        Ok(())
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
        if bytes.len() < 8 {
            return 0.0;
        }
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
            let idx = (idx - 1) as usize;
            if idx >= self.strings.len() {
                self.error = Some("truncated chunk".to_string());
                return String::new();
            }
            return self.strings[idx].clone();
        }
        let bytes = self.read_bytes(size).to_vec();
        if bytes.is_empty() && self.error.is_some() {
            return String::new();
        }
        let len = if bytes.last() == Some(&0) {
            size - 1
        } else {
            size
        };
        // Lua 字符串可包含任意字节 (包括非 UTF-8)，用 from_utf8_unchecked 保留原始字节
        // 对应 C Lua 中 TString 可以存储任意字节序列
        let s = unsafe { String::from_utf8_unchecked(bytes[..len].to_vec()) };
        self.strings.push(s.clone());
        s
    }

    fn read_instruction(&mut self, raw: u32) -> DumpInstruction {
        use crate::opcodes;
        let opcode_val = (raw & 0x7f) as u8;
        let op = opcodes::OpCode::from_u8(opcode_val).unwrap_or(opcodes::OpCode::MOVE);
        let is_vabc = opcodes::get_opmode(op) == opcodes::OpMode::IvABC;
        DumpInstruction {
            opcode: opcode_val,
            a: opcodes::getarg_a(raw) as u32,
            b: if is_vabc {
                opcodes::getarg_vb(raw) as u32
            } else {
                opcodes::getarg_b(raw) as u32
            },
            c: if is_vabc {
                opcodes::getarg_vc(raw) as u32
            } else {
                opcodes::getarg_c(raw) as u32
            },
            k: (raw >> opcodes::POS_K) & 1,
            bx: opcodes::getarg(raw, opcodes::POS_BX, opcodes::SIZE_BX) as u32,
        }
    }

    fn read_code(&mut self) -> Vec<DumpInstruction> {
        let sizecode = self.read_int() as usize;
        self.align(4);
        let mut code = Vec::with_capacity(sizecode);
        for _i in 0..sizecode {
            let bytes = self.read_bytes(4).to_vec();
            if bytes.len() < 4 {
                return code;
            }
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
                17 => DumpConstant::Boolean(true), // LUA_VTRUE = 0x11
                3 => DumpConstant::Integer(self.read_integer()), // LUA_VNUMINT = 3
                19 => DumpConstant::Float(self.read_float()), // LUA_VNUMFLT = 0x13
                4 | 20 => DumpConstant::String(self.read_string()), // 4=VSHRSTR, 20=VLNGSTR
                _ => DumpConstant::Nil,
            };
            constants.push(c);
        }
        constants
    }

    fn read_upvalues(&mut self) -> Vec<(bool, u8, u8)> {
        let n = self.read_int() as usize;
        let mut upvalues = Vec::with_capacity(n);
        for _ in 0..n {
            let instack = self.read_byte() != 0;
            let idx = self.read_byte();
            let kind = self.read_byte();
            upvalues.push((instack, idx, kind));
        }
        upvalues
    }

    fn read_function(&mut self) -> DumpedFunction {
        let linedefined = self.read_int();
        let lastlinedefined = self.read_int();
        let numparams = self.read_byte();
        let flag = self.read_byte();
        let maxstacksize = self.read_byte();
        let code = self.read_code();
        let constants = self.read_constants();
        let upvalues = self.read_upvalues();
        let nprotos = self.read_int() as usize;
        let mut protos = Vec::with_capacity(nprotos);
        for _ in 0..nprotos {
            protos.push(self.read_function());
        }
        let source_str = self.read_string();
        let (
            source,
            line_info,
            abs_line_info,
            loc_vars,
            upvalue_names,
            size_line_info,
            size_abs_line_info,
            size_loc_vars,
        ) = self.read_debug();
        DumpedFunction {
            linedefined,
            lastlinedefined,
            numparams,
            flag,
            maxstacksize,
            code,
            constants,
            upvalues,
            protos,
            source: if source_str.is_empty() {
                None
            } else {
                Some(source_str)
            },
            line_info,
            abs_line_info,
            loc_vars,
            upvalue_names,
            size_line_info,
            size_abs_line_info,
            size_loc_vars,
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

    fn read_debug(
        &mut self,
    ) -> (
        Option<String>,
        Vec<i8>,
        Vec<(i32, i32)>,
        Vec<(Option<String>, i32, i32)>,
        Vec<Option<String>>,
        i32,
        i32,
        i32,
    ) {
        // line_info
        let size_line_info = self.read_int() as i32;
        let mut line_info = Vec::new();
        if size_line_info > 0 {
            for _ in 0..size_line_info {
                line_info.push(self.read_byte() as i8);
            }
        }

        // abs_line_info
        let size_abs_line_info = self.read_int() as i32;
        let mut abs_line_info = Vec::new();
        if size_abs_line_info > 0 {
            self.align(4);
            for _ in 0..size_abs_line_info {
                let pc = i32::from_le_bytes([
                    self.read_byte(),
                    self.read_byte(),
                    self.read_byte(),
                    self.read_byte(),
                ]);
                let line = i32::from_le_bytes([
                    self.read_byte(),
                    self.read_byte(),
                    self.read_byte(),
                    self.read_byte(),
                ]);
                abs_line_info.push((pc, line));
            }
        }

        // loc_vars
        let size_loc_vars = self.read_int() as i32;
        let mut loc_vars = Vec::new();
        for _ in 0..size_loc_vars {
            let varname = self.read_string();
            let startpc = self.read_int();
            let endpc = self.read_int();
            loc_vars.push((
                if varname.is_empty() {
                    None
                } else {
                    Some(varname)
                },
                startpc,
                endpc,
            ));
        }

        // upvalue names
        let n_upvnames = self.read_int() as usize;
        let mut upvalue_names = Vec::new();
        for _ in 0..n_upvnames {
            let name = self.read_string();
            upvalue_names.push(if name.is_empty() { None } else { Some(name) });
        }

        (
            None,
            line_info,
            abs_line_info,
            loc_vars,
            upvalue_names,
            size_line_info,
            size_abs_line_info,
            size_loc_vars,
        )
    }
}

pub fn parse_dump(data: Vec<u8>) -> Result<DumpedFunction, String> {
    let mut reader = BytecodeReader::new(data);

    if reader.data.len() < 12 {
        return Err("truncated chunk".to_string());
    }

    // 校验签名 \x1bLua — 对应 C 的 checkliteral(LUA_SIGNATURE)
    let sig = reader.read_bytes(4).to_vec();
    reader.check()?;
    if sig != b"\x1bLua" {
        return Err("not a binary chunk".to_string());
    }

    // 校验版本号 — 对应 C 的 LUAC_VERSION (5.5 = 0x55)
    let version = reader.read_byte();
    reader.check()?;
    if version != 0x55 {
        return Err("version mismatch".to_string());
    }

    // 校验格式 — 对应 C 的 LUAC_FORMAT = 0
    let format = reader.read_byte();
    reader.check()?;
    if format != 0 {
        return Err("format mismatch".to_string());
    }

    // 校验 LUAC_DATA — 对应 C 的 checkliteral(LUAC_DATA, "corrupted chunk")
    let luac_data = reader.read_bytes(6).to_vec();
    reader.check()?;
    if luac_data != b"\x19\x93\r\n\x1a\n" {
        return Err("corrupted chunk".to_string());
    }

    // 校验 int: size=4, value=-0x5678 (i32) — 对应 C 的 checknum(int, LUAC_INT)
    let int_size = reader.read_byte();
    reader.check()?;
    if int_size as usize != std::mem::size_of::<i32>() {
        return Err("int size mismatch".to_string());
    }
    let int_val_bytes = reader.read_bytes(4).to_vec();
    reader.check()?;
    let int_val = i32::from_ne_bytes([
        int_val_bytes[0],
        int_val_bytes[1],
        int_val_bytes[2],
        int_val_bytes[3],
    ]);
    if int_val != -0x5678 {
        return Err("int format mismatch".to_string());
    }

    // 校验 Instruction: size=4, value=0x12345678 (u32) — 对应 C 的 checknum(Instruction, LUAC_INST)
    let inst_size = reader.read_byte();
    reader.check()?;
    if inst_size as usize != std::mem::size_of::<u32>() {
        return Err("instruction size mismatch".to_string());
    }
    let inst_val_bytes = reader.read_bytes(4).to_vec();
    reader.check()?;
    let inst_val = u32::from_ne_bytes([
        inst_val_bytes[0],
        inst_val_bytes[1],
        inst_val_bytes[2],
        inst_val_bytes[3],
    ]);
    if inst_val != 0x12345678 {
        return Err("instruction format mismatch".to_string());
    }

    // 校验 lua_Integer: size=8, value=-0x5678 (i64) — 对应 C 的 checknum(lua_Integer, LUAC_INT)
    let integer_size = reader.read_byte();
    reader.check()?;
    if integer_size as usize != std::mem::size_of::<i64>() {
        return Err("Lua integer size mismatch".to_string());
    }
    let integer_val_bytes = reader.read_bytes(8).to_vec();
    reader.check()?;
    let integer_val = i64::from_ne_bytes([
        integer_val_bytes[0],
        integer_val_bytes[1],
        integer_val_bytes[2],
        integer_val_bytes[3],
        integer_val_bytes[4],
        integer_val_bytes[5],
        integer_val_bytes[6],
        integer_val_bytes[7],
    ]);
    if integer_val != -0x5678 {
        return Err("Lua integer format mismatch".to_string());
    }

    // 校验 lua_Number: size=8, value=-370.5 (f64) — 对应 C 的 checknum(lua_Number, LUAC_NUM)
    // 注意: 浮点数的内部表示可能有 padding（long double 情况），所以只校验 size
    // 对应 calls.lua:524: headlen = headlen - string.packsize("n")  -- remove float check
    let number_size = reader.read_byte();
    reader.check()?;
    if number_size as usize != std::mem::size_of::<f64>() {
        return Err("Lua number size mismatch".to_string());
    }
    // 读取但不严格校验 value（对应 C 中 long double padding 的容忍）
    let _number_val_bytes = reader.read_bytes(number_size as usize).to_vec();
    reader.check()?;

    let _num_upvalues = reader.read_byte();
    let func = reader.read_function();

    // 检查截断错误 — 对应 C 中 loadBlock/loadByte 遇到 EOZ 时的 "truncated chunk"
    if let Some(err) = reader.error {
        return Err(err);
    }

    Ok(func)
}

pub unsafe fn compile_with_c_lua(source: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(feature = "ffi")]
    {
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

        extern "C" fn writer(
            _L: *mut lua_ffi::lua_State,
            p: *const c_void,
            sz: usize,
            ud: *mut c_void,
        ) -> c_int {
            if sz == 0 {
                return 0;
            }
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
    #[cfg(not(feature = "ffi"))]
    {
        let _ = source;
        Err("compile_with_c_lua requires the 'ffi' feature (links C lua)".to_string())
    }
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
        opcodes::OpCode::LOADFALSE | opcodes::OpCode::LFALSESKIP | opcodes::OpCode::LOADTRUE => {
            format!("{}", a)
        }
        opcodes::OpCode::LOADNIL => format!("{} {}", a, b),
        opcodes::OpCode::GETUPVAL | opcodes::OpCode::SETUPVAL => format!("{} {}", a, b),
        opcodes::OpCode::GETTABUP | opcodes::OpCode::GETTABLE => format!("{} {} {}", a, b, c),
        opcodes::OpCode::GETI | opcodes::OpCode::GETFIELD => format!("{} {} {}", a, b, c),
        opcodes::OpCode::SETTABUP
        | opcodes::OpCode::SETTABLE
        | opcodes::OpCode::SETI
        | opcodes::OpCode::SETFIELD => format!("{} {} {}{}", a, b, c, isk),
        opcodes::OpCode::NEWTABLE => {
            let vb = getarg_vb(op);
            let vc = getarg_vc(op);
            format!("{} {} {}{}", a, vb, vc, isk)
        }
        opcodes::OpCode::SELF => format!("{} {} {}{}", a, b, c, isk),
        opcodes::OpCode::ADDI | opcodes::OpCode::SHLI | opcodes::OpCode::SHRI => {
            format!("{} {} {}", a, b, sc)
        }
        opcodes::OpCode::ADDK
        | opcodes::OpCode::SUBK
        | opcodes::OpCode::MULK
        | opcodes::OpCode::MODK
        | opcodes::OpCode::POWK
        | opcodes::OpCode::DIVK
        | opcodes::OpCode::IDIVK
        | opcodes::OpCode::BANDK
        | opcodes::OpCode::BORK
        | opcodes::OpCode::BXORK => format!("{} {} {}", a, b, c),
        opcodes::OpCode::ADD
        | opcodes::OpCode::SUB
        | opcodes::OpCode::MUL
        | opcodes::OpCode::MOD
        | opcodes::OpCode::POW
        | opcodes::OpCode::DIV
        | opcodes::OpCode::IDIV
        | opcodes::OpCode::BAND
        | opcodes::OpCode::BOR
        | opcodes::OpCode::BXOR
        | opcodes::OpCode::SHL
        | opcodes::OpCode::SHR => format!("{} {} {}", a, b, c),
        opcodes::OpCode::MMBIN => format!("{} {} {}", a, b, c),
        opcodes::OpCode::MMBINI => format!("{} {} {} {}", a, sb, c, isk),
        opcodes::OpCode::MMBINK => format!("{} {} {} {}", a, b, c, isk),
        opcodes::OpCode::UNM
        | opcodes::OpCode::BNOT
        | opcodes::OpCode::NOT
        | opcodes::OpCode::LEN => format!("{} {}", a, b),
        opcodes::OpCode::CONCAT => format!("{} {}", a, b),
        opcodes::OpCode::CLOSE | opcodes::OpCode::TBC => format!("{}", a),
        opcodes::OpCode::JMP => format!("{}", sj),
        opcodes::OpCode::EQ | opcodes::OpCode::LT | opcodes::OpCode::LE => {
            format!("{} {} {}", a, b, isk)
        }
        opcodes::OpCode::EQK => format!("{} {} {}", a, b, isk),
        opcodes::OpCode::EQI
        | opcodes::OpCode::LTI
        | opcodes::OpCode::LEI
        | opcodes::OpCode::GTI
        | opcodes::OpCode::GEI => format!("{} {} {}", a, sb, isk),
        opcodes::OpCode::TEST => format!("{} {}", a, isk),
        opcodes::OpCode::TESTSET => format!("{} {} {}", a, b, isk),
        opcodes::OpCode::CALL => format!("{} {} {}", a, b, c),
        opcodes::OpCode::TAILCALL => format!("{} {} {}{}", a, b, c, isk),
        opcodes::OpCode::RETURN => format!("{} {} {}{}", a, b, c, isk),
        opcodes::OpCode::RETURN0 => String::new(),
        opcodes::OpCode::RETURN1 => format!("{}", a),
        opcodes::OpCode::FORLOOP
        | opcodes::OpCode::FORPREP
        | opcodes::OpCode::TFORPREP
        | opcodes::OpCode::TFORLOOP => format!("{} {}", a, bx),
        opcodes::OpCode::TFORCALL => format!("{} {}", a, c),
        opcodes::OpCode::SETLIST => {
            let vb = getarg_vb(op);
            let vc = getarg_vc(op);
            format!("{} {} {}{}", a, vb, vc, isk)
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
            let idx = if op == opcodes::OpCode::LOADKX {
                inst.c as usize
            } else {
                inst.bx as usize
            };
            if idx < constants.len() {
                format!("\t; {}", format_constant(constants, idx))
            } else {
                String::new()
            }
        }
        opcodes::OpCode::GETTABUP | opcodes::OpCode::GETFIELD => {
            format!("\t; {}", format_constant(constants, c as usize))
        }
        opcodes::OpCode::SETTABUP | opcodes::OpCode::SETFIELD => {
            let b_const = format_constant(constants, inst.b as usize);
            let mut s = format!("\t; {}", b_const);
            if isk {
                s.push_str(&format!(" {}", format_constant(constants, c as usize)));
            }
            s
        }
        opcodes::OpCode::SETTABLE | opcodes::OpCode::SETI => {
            if isk {
                format!("\t; {}", format_constant(constants, c as usize))
            } else {
                String::new()
            }
        }
        opcodes::OpCode::NEWTABLE => {
            format!("\t; {}", c)
        }
        opcodes::OpCode::SELF => {
            if isk {
                format!("\t; {}", format_constant(constants, c as usize))
            } else {
                String::new()
            }
        }
        opcodes::OpCode::ADDK
        | opcodes::OpCode::SUBK
        | opcodes::OpCode::MULK
        | opcodes::OpCode::MODK
        | opcodes::OpCode::POWK
        | opcodes::OpCode::DIVK
        | opcodes::OpCode::IDIVK
        | opcodes::OpCode::BANDK
        | opcodes::OpCode::BORK
        | opcodes::OpCode::BXORK => {
            format!("\t; {}", format_constant(constants, c as usize))
        }
        opcodes::OpCode::MMBIN => {
            let event_idx = c as usize;
            if event_idx < TM_EVENT_NAMES.len() {
                format!("\t; {}", TM_EVENT_NAMES[event_idx])
            } else {
                String::new()
            }
        }
        opcodes::OpCode::MMBINI => {
            let event_idx = c as usize;
            let mut s = if event_idx < TM_EVENT_NAMES.len() {
                format!("\t; {}", TM_EVENT_NAMES[event_idx])
            } else {
                String::new()
            };
            if isk {
                s.push_str(" flip");
            }
            s
        }
        opcodes::OpCode::MMBINK => {
            let event_idx = c as usize;
            let mut s = if event_idx < TM_EVENT_NAMES.len() {
                format!("\t; {} ", TM_EVENT_NAMES[event_idx])
            } else {
                String::new()
            };
            s.push_str(&format_constant(constants, inst.b as usize));
            if isk {
                s.push_str(" flip");
            }
            s
        }
        opcodes::OpCode::JMP => {
            let sj_raw = inst.a | (inst.bx << 8);
            let sj_signed = sj_raw as i32 - OFFSET_sJ;
            format!("\t; to {}", sj_signed + pc as i32 + 2)
        }
        opcodes::OpCode::EQK => {
            format!("\t; {}", format_constant(constants, inst.b as usize))
        }
        opcodes::OpCode::CALL => {
            let in_args = if inst.b == 0 {
                "all in".to_string()
            } else {
                format!("{} in", inst.b as i32 - 1)
            };
            let out_args = if inst.c == 0 {
                "all out".to_string()
            } else {
                format!("{} out", inst.c as i32 - 1)
            };
            format!("\t; {} {}", in_args, out_args)
        }
        opcodes::OpCode::TAILCALL => {
            format!("\t; {} in", inst.b as i32 - 1)
        }
        opcodes::OpCode::RETURN => {
            if inst.b == 0 {
                "\t; all out".to_string()
            } else {
                format!("\t; {} out", inst.b as i32 - 1)
            }
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
            if isk {
                format!("\t; {}", c as usize + opcodes::SIZE_C as usize + 1)
            } else {
                String::new()
            }
        }
        opcodes::OpCode::LOADNIL => {
            format!("\t; {} out", inst.b as i32 + 1)
        }
        opcodes::OpCode::VARARG => {
            if inst.c == 0 {
                "\t; all out".to_string()
            } else {
                format!("\t; {} out", inst.c as i32 - 1)
            }
        }
        opcodes::OpCode::ERRNNIL => {
            if inst.bx == 0 {
                "\t; ?".to_string()
            } else {
                format!("\t; {}", format_constant(constants, (inst.bx as usize) - 1))
            }
        }
        _ => String::new(),
    }
}

pub fn dump_inst_to_raw(inst: &DumpInstruction) -> u32 {
    let op = OpCode::from_u8(inst.opcode).unwrap_or(OpCode::MOVE);
    let is_vabc = get_opmode(op) == OpMode::IvABC;
    if is_vabc {
        (inst.opcode as u32)
            | ((inst.a as u32) << POS_A)
            | ((inst.k as u32) << POS_K)
            | ((inst.b as u32) << POS_VB)
            | ((inst.c as u32) << POS_VC)
    } else {
        (inst.opcode as u32)
            | ((inst.a as u32) << POS_A)
            | ((inst.k as u32) << POS_K)
            | ((inst.b as u32) << POS_B)
            | ((inst.c as u32) << POS_C)
    }
}

/// Normalize an instruction for comparison: replace constant indices with a placeholder.
/// This allows comparing instruction structure while ignoring differences in constant pool ordering.
fn normalize_instruction(raw: u32) -> String {
    let opcode = get_opcode(raw);
    let a = getarg_a(raw);
    let b = getarg_b(raw);
    let c = getarg_c(raw);
    let k = testarg_k(raw);
    let bx = getarg_bx(raw);
    let sbx = getarg_sbx(raw);
    let sj = getarg_sj(raw);
    let op_name = OPNAMES[opcode as usize];

    // Use format_operands to get the correct operand formatting,
    // then replace constant indices with "K".
    let operands = format_operands(raw, a, b, c, bx, sbx, sj, k);

    // Determine which operands are constant references based on opcode
    match opcode {
        // IABx instructions: Bx is always a constant index
        opcodes::OpCode::LOADK
        | opcodes::OpCode::CLOSURE
        | opcodes::OpCode::ERRNNIL
        | opcodes::OpCode::FORLOOP
        | opcodes::OpCode::FORPREP
        | opcodes::OpCode::TFORPREP
        | opcodes::OpCode::TFORLOOP => {
            // operands is "A Bx" - replace the Bx part
            let parts: Vec<&str> = operands.splitn(2, ' ').collect();
            if parts.len() == 2 {
                format!("{}\t{} K", op_name, parts[0])
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        // IABC instructions where B or C can be constant references (ISK bit set)
        opcodes::OpCode::GETTABUP => {
            // operands is "A B C" - C is always a constant index (K[C]:shortstring)
            let parts: Vec<&str> = operands.split(' ').collect();
            if parts.len() >= 3 {
                format!("{}\t{} {} K", op_name, parts[0], parts[1])
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        opcodes::OpCode::GETTABLE | opcodes::OpCode::GETI => {
            // operands is "A B C" - C is register/index for GETTABLE/GETI
            format!("{}\t{}", op_name, operands)
        }
        opcodes::OpCode::GETFIELD => {
            // operands is "A B C" - C is always a constant index (K[C]:shortstring)
            let parts: Vec<&str> = operands.split(' ').collect();
            if parts.len() >= 3 {
                format!("{}\t{} {} K", op_name, parts[0], parts[1])
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        opcodes::OpCode::SETTABUP => {
            // operands is "A B Ck" - B is always a constant index (K[B]:shortstring), C is RK(C)
            // When k bit is set, C is a constant index; otherwise it's a register
            let parts: Vec<&str> = operands.split(' ').collect();
            if parts.len() >= 3 {
                let c_norm = if k || c >= 256 {
                    "K"
                } else {
                    parts[2].trim_end_matches('k')
                };
                let k_str = if k { "k" } else { "" };
                format!("{}\t{} K {}{}", op_name, parts[0], c_norm, k_str)
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        opcodes::OpCode::SETTABLE | opcodes::OpCode::SETI => {
            // operands is "A B Ck" - C is RK(C)
            // When k bit is set, C is a constant index; otherwise it's a register
            let parts: Vec<&str> = operands.split(' ').collect();
            if parts.len() >= 3 {
                let c_norm = if k || c >= 256 {
                    "K"
                } else {
                    parts[2].trim_end_matches('k')
                };
                let k_str = if k { "k" } else { "" };
                format!("{}\t{} {} {}{}", op_name, parts[0], parts[1], c_norm, k_str)
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        opcodes::OpCode::SETFIELD => {
            // operands is "A B Ck" - B is always a constant index (K[B]:shortstring), C is RK(C)
            // When k bit is set, C is a constant index; otherwise it's a register
            let parts: Vec<&str> = operands.split(' ').collect();
            if parts.len() >= 3 {
                let c_norm = if k || c >= 256 {
                    "K"
                } else {
                    parts[2].trim_end_matches('k')
                };
                let k_str = if k { "k" } else { "" };
                format!("{}\t{} K {}{}", op_name, parts[0], c_norm, k_str)
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        opcodes::OpCode::SELF => {
            // operands is "A B Ck" - C is always a constant index (K[C]:shortstring)
            let parts: Vec<&str> = operands.split(' ').collect();
            if parts.len() >= 3 {
                let k_str = if k { "k" } else { "" };
                format!("{}\t{} {} K{}", op_name, parts[0], parts[1], k_str)
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        opcodes::OpCode::ADDK
        | opcodes::OpCode::SUBK
        | opcodes::OpCode::MULK
        | opcodes::OpCode::MODK
        | opcodes::OpCode::POWK
        | opcodes::OpCode::DIVK
        | opcodes::OpCode::IDIVK
        | opcodes::OpCode::BANDK
        | opcodes::OpCode::BORK
        | opcodes::OpCode::BXORK => {
            // operands is "A B C" - C is always a constant index
            let parts: Vec<&str> = operands.split(' ').collect();
            if parts.len() >= 3 {
                format!("{}\t{} {} K", op_name, parts[0], parts[1])
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        opcodes::OpCode::EQK => {
            // operands is "A B k" - B is always a constant index
            let parts: Vec<&str> = operands.split(' ').collect();
            if parts.len() >= 2 {
                let k_str = if k { "k" } else { "" };
                format!("{}\t{} K {}", op_name, parts[0], k_str)
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        opcodes::OpCode::MMBINK => {
            // operands is "A B C k" - B is always a constant index (K[B])
            let parts: Vec<&str> = operands.split(' ').collect();
            if parts.len() >= 3 {
                let k_str = if k { "k" } else { "" };
                format!(
                    "{}\t{} K {} {}",
                    op_name,
                    parts[0],
                    parts[2].trim_end_matches('k'),
                    k_str
                )
            } else {
                format!("{}\t{}", op_name, operands)
            }
        }
        opcodes::OpCode::NEWTABLE => {
            // operands is "A vb vc k" - vb and vc are not constant indices
            format!("{}\t{}", op_name, operands)
        }
        opcodes::OpCode::SETLIST => {
            // operands is "A vb vc k" - not constant indices
            format!("{}\t{}", op_name, operands)
        }
        opcodes::OpCode::VARARG => {
            // operands is "A B C k" - not constant indices
            format!("{}\t{}", op_name, operands)
        }
        // IAx instructions
        opcodes::OpCode::EXTRAARG => {
            format!("{}\tK", op_name)
        }
        // All other instructions: use format_operands as-is (no constant indices)
        _ => {
            format!("{}\t{}", op_name, operands)
        }
    }
}

pub fn compare_instructions(rust_code: &[u32], c_code: &[DumpInstruction]) -> Vec<String> {
    let mut diffs = Vec::new();

    let rust_formatted: Vec<String> = rust_code
        .iter()
        .map(|&raw| format_instruction(raw))
        .collect();

    let c_formatted: Vec<String> = c_code
        .iter()
        .map(|inst| format_instruction(dump_inst_to_raw(inst)))
        .collect();

    let rust_normalized: Vec<String> = rust_code
        .iter()
        .map(|&raw| normalize_instruction(raw))
        .collect();

    let c_normalized: Vec<String> = c_code
        .iter()
        .map(|inst| normalize_instruction(dump_inst_to_raw(inst)))
        .collect();

    // Use text diff on normalized instructions to find structural differences
    let rust_str = rust_normalized.join("\n");
    let c_str = c_normalized.join("\n");
    let input = InternedInput::new(rust_str.as_str(), c_str.as_str());
    let diff = Diff::compute(Algorithm::Myers, &input);

    for hunk in diff.hunks() {
        let before_start = hunk.before.start as usize;
        let before_end = hunk.before.end as usize;
        let after_start = hunk.after.start as usize;
        let after_end = hunk.after.end as usize;

        if before_start == before_end {
            for j in after_start..after_end {
                diffs.push(format!("PC {}: extra C: {}", j, c_formatted[j]));
            }
        } else if after_start == after_end {
            for i in before_start..before_end {
                diffs.push(format!("PC {}: extra Rust: {}", i, rust_formatted[i]));
            }
        } else {
            let before_count = before_end - before_start;
            let after_count = after_end - after_start;
            let max_count = before_count.max(after_count);
            for k in 0..max_count {
                let ri = before_start + k;
                let ci = after_start + k;
                if k < before_count && k < after_count {
                    diffs.push(format!(
                        "PC {}: Rust [{}] C [{}]",
                        ri, rust_formatted[ri], c_formatted[ci]
                    ));
                } else if k < before_count {
                    diffs.push(format!("PC {}: extra Rust: {}", ri, rust_formatted[ri]));
                } else {
                    diffs.push(format!("PC {}: extra C: {}", ci, c_formatted[ci]));
                }
            }
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
    format!("{}\t{}", op_name, operands)
}

pub fn dump_instructions(code: &[u32]) -> String {
    code.iter()
        .enumerate()
        .map(|(i, inst)| format!("{}\t[-]\t{}", i + 1, format_instruction(*inst)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn format_c_instruction(inst: &DumpInstruction, constants: &[DumpConstant]) -> String {
    let raw = dump_inst_to_raw(inst);
    let opcode = get_opcode(raw);
    let op_name = OPNAMES[opcode as usize];
    let a = inst.a as i32;
    let b = inst.b as i32;
    let c = inst.c as i32;
    let k = inst.k != 0;
    let bx = inst.bx as i32;
    let sbx = bx - opcodes::OFFSET_SBX;
    let sj = getarg_sj(raw);

    let operands = format_operands(raw, a, b, c, bx, sbx, sj, k);
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

// ============================================================================
// Proto 序列化 (对应 C 的 ldump.cpp)
// ============================================================================

use crate::objects::{AbsLineInfo, NilKind, Proto, TValue};
use std::collections::HashMap;

/// Lua 二进制格式常量
const LUA_SIGNATURE: &[u8] = b"\x1bLua";
/// LUAC_VERSION = LUA_VERSION_MAJOR_N * 16 + LUA_VERSION_MINOR_N = 5*16+5 = 85
const LUAC_VERSION: u8 = 85;
/// LUAC_FORMAT = 0
const LUAC_FORMAT: u8 = 0;
/// LUAC_DATA = "\x19\x93\r\n\x1a\n"
const LUAC_DATA: &[u8] = b"\x19\x93\r\n\x1a\n";
/// LUAC_INT (int 和 lua_Integer 共用)
const LUAC_INT: i32 = -0x5678;
/// LUAC_INST
const LUAC_INST: u32 = 0x12345678;
/// LUAC_NUM
const LUAC_NUM: f64 = -370.5;

/// 类型标签常量 (对应 C 的 lobject.h)
const LUA_VNIL: u8 = 0;
const LUA_VFALSE: u8 = 1;
const LUA_VTRUE: u8 = 0x11;
const LUA_VNUMINT: u8 = 3;
const LUA_VNUMFLT: u8 = 0x13;
const LUA_VSHRSTR: u8 = 4;
const LUA_VLNGSTR: u8 = 0x14;

/// 字节码写入器 — 对应 C 的 DumpState
struct BytecodeWriter {
    data: Vec<u8>,
    offset: usize,
    strip: bool,
    /// 字符串去重表: 字符串内容 -> 索引 (1-based)
    strings: HashMap<String, usize>,
    /// 下一个字符串索引
    nstr: usize,
}

impl BytecodeWriter {
    fn new(strip: bool) -> Self {
        BytecodeWriter {
            data: Vec::new(),
            offset: 0,
            strip,
            strings: HashMap::new(),
            nstr: 0,
        }
    }

    fn dump_block(&mut self, b: &[u8]) {
        self.data.extend_from_slice(b);
        self.offset += b.len();
    }

    /// 对齐到 align 的倍数
    fn dump_align(&mut self, align: usize) {
        let padding = align - (self.offset % align);
        if padding < align {
            let zeros = vec![0u8; padding];
            self.dump_block(&zeros);
        }
    }

    fn dump_byte(&mut self, x: u8) {
        self.dump_block(&[x]);
    }

    /// MSB Varint 编码 (对应 C 的 dumpVarint)
    fn dump_varint(&mut self, mut x: u64) {
        let mut buff = Vec::new();
        buff.push((x & 0x7f) as u8);
        x >>= 7;
        while x != 0 {
            buff.push(((x & 0x7f) | 0x80) as u8);
            x >>= 7;
        }
        buff.reverse();
        self.dump_block(&buff);
    }

    fn dump_size(&mut self, sz: usize) {
        self.dump_varint(sz as u64);
    }

    fn dump_int(&mut self, x: i32) {
        self.dump_varint(x as u64);
    }

    fn dump_number(&mut self, x: f64) {
        self.dump_block(&x.to_le_bytes());
    }

    /// 有符号整数编码: 非负 x 编码为 2x; 负 x 编码为 -2x-1
    fn dump_integer(&mut self, x: i64) {
        let cx: u64 = if x >= 0 {
            2u64.wrapping_mul(x as u64)
        } else {
            2u64.wrapping_mul(!(x as u64)).wrapping_add(1)
        };
        self.dump_varint(cx);
    }

    /// 序列化字符串 (带去重)
    /// size==0 + index==0: NULL 字符串
    /// size==0 + index>0: 复用已保存的字符串
    /// size>=1: 新字符串, 后跟内容 + null
    fn dump_string(&mut self, s: Option<&str>) {
        match s {
            None => {
                self.dump_varint(0);
                self.dump_varint(0);
            }
            Some(content) => {
                if let Some(&idx) = self.strings.get(content) {
                    self.dump_varint(0);
                    self.dump_varint(idx as u64);
                } else {
                    self.nstr += 1;
                    let idx = self.nstr;
                    self.strings.insert(content.to_string(), idx);
                    self.dump_size(content.len() + 1);
                    let mut bytes = content.as_bytes().to_vec();
                    bytes.push(0);
                    self.dump_block(&bytes);
                }
            }
        }
    }

    fn dump_num_info_int(&mut self, value: i32) {
        self.dump_byte(std::mem::size_of::<i32>() as u8);
        self.dump_block(&value.to_le_bytes());
    }

    fn dump_num_info_inst(&mut self, value: u32) {
        self.dump_byte(std::mem::size_of::<u32>() as u8);
        self.dump_block(&value.to_le_bytes());
    }

    fn dump_num_info_integer(&mut self, value: i64) {
        self.dump_byte(std::mem::size_of::<i64>() as u8);
        self.dump_block(&value.to_le_bytes());
    }

    fn dump_num_info_number(&mut self, value: f64) {
        self.dump_byte(std::mem::size_of::<f64>() as u8);
        self.dump_block(&value.to_le_bytes());
    }

    fn dump_header(&mut self) {
        self.dump_block(LUA_SIGNATURE);
        self.dump_byte(LUAC_VERSION);
        self.dump_byte(LUAC_FORMAT);
        self.dump_block(LUAC_DATA);
        self.dump_num_info_int(LUAC_INT);
        self.dump_num_info_inst(LUAC_INST);
        self.dump_num_info_integer(LUAC_INT as i64);
        self.dump_num_info_number(LUAC_NUM);
    }

    fn dump_code(&mut self, f: &Proto) {
        self.dump_int(f.code.len() as i32);
        self.dump_align(std::mem::size_of::<u32>());
        for &inst in &f.code[..] {
            self.dump_block(&inst.to_le_bytes());
        }
    }

    fn dump_constants(&mut self, f: &Proto) {
        self.dump_int(f.constants.len() as i32);
        for c in &f.constants[..] {
            match c {
                TValue::Nil(_) => {
                    self.dump_byte(LUA_VNIL);
                }
                TValue::Boolean(false) => {
                    self.dump_byte(LUA_VFALSE);
                }
                TValue::Boolean(true) => {
                    self.dump_byte(LUA_VTRUE);
                }
                TValue::Integer(i) => {
                    self.dump_byte(LUA_VNUMINT);
                    self.dump_integer(*i);
                }
                TValue::Float(fl) => {
                    self.dump_byte(LUA_VNUMFLT);
                    self.dump_number(*fl);
                }
                TValue::Str(s) => {
                    match s {
                        crate::strings::LuaString::Short(_) => {
                            self.dump_byte(LUA_VSHRSTR);
                        }
                        crate::strings::LuaString::Long(_) => {
                            self.dump_byte(LUA_VLNGSTR);
                        }
                    }
                    self.dump_string(Some(s.as_str()));
                }
                _ => {
                    self.dump_byte(LUA_VNIL);
                }
            }
        }
    }

    fn dump_upvalues(&mut self, f: &Proto) {
        self.dump_int(f.upvalues.len() as i32);
        for uv in &f.upvalues[..] {
            self.dump_byte(if uv.in_stack { 1 } else { 0 });
            self.dump_byte(uv.idx);
            self.dump_byte(uv.kind);
        }
    }

    fn dump_protos(&mut self, f: &Proto) {
        self.dump_int(f.protos.len() as i32);
        for p in f.protos.iter() {
            self.dump_function(p);
        }
    }

    fn dump_debug(&mut self, f: &Proto) {
        // line_info
        let n = if self.strip { 0 } else { f.line_info.len() };
        self.dump_int(n as i32);
        if !self.strip {
            for &li in &f.line_info {
                self.dump_byte(li as u8);
            }
        }

        // abs_line_info
        let n = if self.strip { 0 } else { f.abs_line_info.len() };
        self.dump_int(n as i32);
        if n > 0 {
            self.dump_align(std::mem::size_of::<i32>());
            for ai in &f.abs_line_info {
                self.dump_block(&ai.pc.to_le_bytes());
                self.dump_block(&ai.line.to_le_bytes());
            }
        }

        // loc_vars
        let n = if self.strip { 0 } else { f.loc_vars.len() };
        self.dump_int(n as i32);
        if !self.strip {
            for lv in &f.loc_vars {
                self.dump_string(lv.varname.as_ref().map(|s| s.as_str()));
                self.dump_int(lv.start_pc);
                self.dump_int(lv.end_pc);
            }
        }

        // upvalue names
        let n = if self.strip { 0 } else { f.upvalues.len() };
        self.dump_int(n as i32);
        if !self.strip {
            for uv in &f.upvalues[..] {
                self.dump_string(uv.name.as_ref().map(|s| s.as_str()));
            }
        }
    }

    fn dump_function(&mut self, f: &Proto) {
        self.dump_int(f.line_defined);
        self.dump_int(f.last_line_defined);
        self.dump_byte(f.num_params);
        self.dump_byte(f.flag);
        self.dump_byte(f.max_stack_size);
        self.dump_code(f);
        self.dump_constants(f);
        self.dump_upvalues(f);
        self.dump_protos(f);
        // source
        if self.strip {
            self.dump_string(None);
        } else {
            self.dump_string(f.source.as_ref().map(|s| s.as_str()));
        }
        self.dump_debug(f);
    }
}

/// 将 Proto 序列化为 Lua 5.5 二进制格式
/// 对应 C 的 luaU_dump
pub fn dump_proto(f: &Proto, strip: bool) -> Vec<u8> {
    let mut w = BytecodeWriter::new(strip);
    w.dump_header();
    // dumpByte(f->sizeupvalues) — 主函数的上值数量
    w.dump_byte(f.upvalues.len() as u8);
    w.dump_function(f);
    w.data
}

// ============================================================================
// DumpedFunction → Proto 转换 (用于 load 加载二进制格式)
// ============================================================================

use crate::objects::{LocVar, UpvalDesc};
use crate::strings::{LongString, LuaString};
use std::sync::atomic::{AtomicU64, AtomicU8};

/// 创建长字符串的辅助函数
/// 使用 with_nul 添加额外 NUL 终止符，与 as_str_inner 的 NUL 剥离机制配合
fn make_long_string(s: &str) -> LuaString {
    LuaString::Long(Box::new(LongString {
        contents: LuaString::with_nul(s),
        hash: AtomicU64::new(0),
        extra: AtomicU8::new(0),
        ptr_id: crate::gc::new_ptr_id(),
    }))
}

/// 将 DumpedFunction 转换为 Proto
/// 对应 C 的 luaU_undump 后的 Proto 构建
pub fn dumped_to_proto(df: &DumpedFunction) -> Proto {
    let mut proto = new_proto_internal();
    proto.line_defined = df.linedefined;
    proto.last_line_defined = df.lastlinedefined;
    proto.num_params = df.numparams;
    proto.flag = df.flag;
    proto.max_stack_size = df.maxstacksize;

    // code: DumpInstruction → u32
    proto.code = Rc::new(df.code.iter().map(|inst| dump_inst_to_raw(inst)).collect());

    // constants: DumpConstant → TValue
    proto.constants = Rc::new(df
        .constants
        .iter()
        .map(|c| match c {
            DumpConstant::Nil => TValue::Nil(NilKind::Strict),
            DumpConstant::Boolean(b) => TValue::Boolean(*b),
            DumpConstant::Integer(i) => TValue::Integer(*i),
            DumpConstant::Float(f) => TValue::Float(*f),
            DumpConstant::String(s) => TValue::Str(make_long_string(s)),
        })
        .collect());

    // upvalues: (bool, u8, u8) → UpvalDesc
    proto.upvalues = Rc::new(df
        .upvalues
        .iter()
        .enumerate()
        .map(|(i, (instack, idx, kind))| UpvalDesc {
            name: df
                .upvalue_names
                .get(i)
                .and_then(|n| n.as_ref().map(|s| make_long_string(s))),
            in_stack: *instack,
            idx: *idx,
            parent_local_idx: 0,
            kind: *kind,
        })
        .collect());

    // protos: 递归转换 — 包装为 Rc<Vec> 让 op_call 共享，避免每次调用 clone O(n) Vec 分配
    proto.protos = Rc::new(
        df.protos
            .iter()
            .map(|p| std::rc::Rc::new(dumped_to_proto(p)))
            .collect(),
    );

    // source
    proto.source = df.source.as_ref().map(|s| make_long_string(s));

    // 调试信息
    proto.line_info = df.line_info.clone();
    proto.abs_line_info = df
        .abs_line_info
        .iter()
        .map(|(pc, line)| AbsLineInfo {
            pc: *pc,
            line: *line,
        })
        .collect();
    proto.loc_vars = df
        .loc_vars
        .iter()
        .map(|(varname, startpc, endpc)| LocVar {
            varname: varname.as_ref().map(|s| make_long_string(s)),
            start_pc: *startpc,
            end_pc: *endpc,
        })
        .collect();

    // 设置 size 字段
    proto.size_upvalues = proto.upvalues.len() as i32;
    proto.size_k = proto.constants.len() as i32;
    proto.size_code = proto.code.len() as i32;
    proto.size_line_info = proto.line_info.len() as i32;
    proto.size_p = proto.protos.len() as i32;
    proto.size_loc_vars = proto.loc_vars.len() as i32;
    proto.size_abs_line_info = proto.abs_line_info.len() as i32;

    proto
}

/// 创建新的 Proto（内部使用，避免循环依赖）
fn new_proto_internal() -> Proto {
    Proto {
        num_params: 0,
        flag: 0,
        max_stack_size: 2,
        size_upvalues: 0,
        size_k: 0,
        size_code: 0,
        size_line_info: 0,
        size_p: 0,
        size_loc_vars: 0,
        size_abs_line_info: 0,
        line_defined: 0,
        last_line_defined: 0,
        constants: Rc::new(Vec::new()),
        code: Rc::new(Vec::new()),
        protos: Rc::new(Vec::new()),
        upvalues: Rc::new(Vec::new()),
        line_info: Vec::new(),
        abs_line_info: Vec::new(),
        loc_vars: Vec::new(),
        source: None,
    }
}

/// 从二进制数据加载 Proto
/// 对应 C 的 luaU_undump
pub fn undump_to_proto(data: &[u8]) -> Result<Proto, String> {
    let df = parse_dump(data.to_vec())?;
    Ok(dumped_to_proto(&df))
}
