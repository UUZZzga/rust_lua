//! Lua 5.5 操作码编解码（纯 Rust 实现）
//!
//! 操作模式表与 C 的 luaP_opmodes 完全一致。

// ============================================================================
// 操作码枚举
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpCode {
    MOVE = 0,
    LOADI = 1,
    LOADF = 2,
    LOADK = 3,
    LOADKX = 4,
    LOADFALSE = 5,
    LFALSESKIP = 6,
    LOADTRUE = 7,
    LOADNIL = 8,
    GETUPVAL = 9,
    SETUPVAL = 10,
    GETTABUP = 11,
    GETTABLE = 12,
    GETI = 13,
    GETFIELD = 14,
    SETTABUP = 15,
    SETTABLE = 16,
    SETI = 17,
    SETFIELD = 18,
    NEWTABLE = 19,
    SELF = 20,
    ADDI = 21,
    ADDK = 22,
    SUBK = 23,
    MULK = 24,
    MODK = 25,
    POWK = 26,
    DIVK = 27,
    IDIVK = 28,
    BANDK = 29,
    BORK = 30,
    BXORK = 31,
    SHLI = 32,
    SHRI = 33,
    ADD = 34,
    SUB = 35,
    MUL = 36,
    MOD = 37,
    POW = 38,
    DIV = 39,
    IDIV = 40,
    BAND = 41,
    BOR = 42,
    BXOR = 43,
    SHL = 44,
    SHR = 45,
    MMBIN = 46,
    MMBINI = 47,
    MMBINK = 48,
    UNM = 49,
    BNOT = 50,
    NOT = 51,
    LEN = 52,
    CONCAT = 53,
    CLOSE = 54,
    TBC = 55,
    JMP = 56,
    EQ = 57,
    LT = 58,
    LE = 59,
    EQK = 60,
    EQI = 61,
    LTI = 62,
    LEI = 63,
    GTI = 64,
    GEI = 65,
    TEST = 66,
    TESTSET = 67,
    CALL = 68,
    TAILCALL = 69,
    RETURN = 70,
    RETURN0 = 71,
    RETURN1 = 72,
    FORLOOP = 73,
    FORPREP = 74,
    TFORPREP = 75,
    TFORCALL = 76,
    TFORLOOP = 77,
    SETLIST = 78,
    CLOSURE = 79,
    VARARG = 80,
    GETVARG = 81,
    ERRNNIL = 82,
    VARARGPREP = 83,
    EXTRAARG = 84,
}

pub type Instruction = u32;
pub const NUM_OPCODES: usize = 85;

impl OpCode {
    #[inline]
    pub fn from_u8(v: u8) -> Option<OpCode> {
        if (v as usize) < NUM_OPCODES {
            Some(unsafe { std::mem::transmute::<u8, OpCode>(v) })
        } else {
            None
        }
    }
}

// ============================================================================
// 操作模式枚举 & 常量
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpMode {
    IABC = 0,
    IABx = 1,
    IAsBx = 2,
    IAx = 3,
    IsJ = 4,
    IvABC = 5,
}

pub const SIZE_C: u32 = 8;
pub const SIZE_B: u32 = 8;
pub const SIZE_VB: u32 = 6;
pub const SIZE_VC: u32 = 10;
pub const SIZE_BX: u32 = SIZE_C + SIZE_B + 1;
pub const SIZE_Ax: u32 = SIZE_BX + SIZE_A;
pub const SIZE_A: u32 = 8;
pub const SIZE_OP: u32 = 7;
pub const POS_OP: u32 = 0;
pub const POS_A: u32 = POS_OP + SIZE_OP;
pub const POS_K: u32 = POS_A + SIZE_A;
pub const POS_B: u32 = POS_K + 1;
pub const POS_VB: u32 = POS_K + 1;
pub const POS_VC: u32 = POS_VB + SIZE_VB;
pub const POS_C: u32 = POS_B + SIZE_B;
pub const POS_BX: u32 = POS_K;
pub const POS_SJ: u32 = POS_A;
pub const OFFSET_SBX: i32 = (((1i64 << SIZE_BX) - 1) >> 1) as i32;

pub const SIZE_sJ: u32 = SIZE_BX + SIZE_A;
pub const OFFSET_sJ: i32 = (((1i64 << SIZE_sJ) - 1) >> 1) as i32;

pub const NO_REG: u8 = ((1u16 << SIZE_A) - 1) as u8;
pub const MAX_FSTACK: u8 = NO_REG;

pub const MAXARG_B: u32 = (1u32 << SIZE_B) - 1;
pub const MAXINDEXRK: u32 = MAXARG_B;
pub const MAXARG_BX: u32 = (1u32 << SIZE_BX) - 1;
pub const MAXARG_Ax: u32 = (1u32 << SIZE_Ax) - 1;

// ============================================================================
// 位操作
// ============================================================================

#[inline]
pub const fn mask1(n: u32, p: u32) -> u32 {
    (!((!0u32) << n)) << p
}
#[inline]
pub const fn mask0(n: u32, p: u32) -> u32 {
    !mask1(n, p)
}
#[inline]
pub const fn getarg(i: u32, pos: u32, size: u32) -> i32 {
    ((i >> pos) & mask1(size, 0)) as i32
}
#[inline]
pub fn setarg(i: &mut u32, v: i32, pos: u32, size: u32) {
    *i = (*i & mask0(size, pos)) | (((v as u32) << pos) & mask1(size, pos));
}

// ============================================================================
// 指令编解码
// ============================================================================

#[inline]
pub fn get_opcode(i: Instruction) -> OpCode {
    OpCode::from_u8(((i >> POS_OP) & mask1(SIZE_OP, 0)) as u8).unwrap_or(OpCode::MOVE)
}
#[inline]
pub fn getarg_a(i: Instruction) -> i32 {
    getarg(i, POS_A, SIZE_A)
}
#[inline]
pub fn getarg_ax(i: Instruction) -> i32 {
    getarg(i, POS_A, SIZE_Ax)
}
#[inline]
pub fn getarg_b(i: Instruction) -> i32 {
    getarg(i, POS_B, SIZE_B)
}
#[inline]
pub fn getarg_vb(i: Instruction) -> i32 {
    getarg(i, POS_VB, SIZE_VB)
}
#[inline]
pub fn getarg_vc(i: Instruction) -> i32 {
    getarg(i, POS_VC, SIZE_VC)
}
#[inline]
pub fn getarg_c(i: Instruction) -> i32 {
    getarg(i, POS_C, SIZE_C)
}
#[inline]
pub fn testarg_k(i: Instruction) -> bool {
    (i & (1u32 << POS_K)) != 0
}
#[inline]
pub fn getarg_bx(i: Instruction) -> i32 {
    getarg(i, POS_BX, SIZE_BX)
}
#[inline]
pub fn SET_OPCODE(i: &mut Instruction, o: OpCode) {
    *i = (*i & mask0(SIZE_OP, POS_OP)) | ((o as u32) << POS_OP);
}
#[inline]
pub fn SETARG_k(i: &mut Instruction, v: u32) {
    setarg(i, v as i32, POS_K, 1);
}
#[inline]
pub fn SETARG_C(i: &mut Instruction, v: i32) {
    setarg(i, v, POS_C, SIZE_C);
}
#[inline]
pub fn getarg_sbx(i: Instruction) -> i32 {
    getarg(i, POS_BX, SIZE_BX) - OFFSET_SBX
}
#[inline]
pub fn getarg_sj(i: Instruction) -> i32 {
    let v = getarg(i, POS_SJ, SIZE_BX + SIZE_A);
    v - ((((1i64 << (SIZE_BX + SIZE_A)) - 1) >> 1) as i32)
}

pub const OFFSET_SC: i32 = (((1 << SIZE_C) - 1) >> 1) as i32;

#[inline]
pub fn getarg_sc(i: Instruction) -> i32 {
    getarg_c(i) - OFFSET_SC
}
#[inline]
pub fn getarg_sb(i: Instruction) -> i32 {
    getarg_b(i) - OFFSET_SC
}

// ============================================================================
// 操作模式表 — 纯 Rust 实现，与 C 的 luaP_opmodes 完全一致
// ============================================================================

const fn opmode_entry(mm: u8, ot: u8, it: u8, t: u8, a: u8, m: OpMode) -> u8 {
    ((mm) << 7) | ((ot) << 6) | ((it) << 5) | ((t) << 4) | ((a) << 3) | (m as u8)
}

static OP_MODES: [u8; NUM_OPCODES] = [
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // MOVE
    opmode_entry(0, 0, 0, 0, 1, OpMode::IAsBx), // LOADI
    opmode_entry(0, 0, 0, 0, 1, OpMode::IAsBx), // LOADF
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABx),  // LOADK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABx),  // LOADKX
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // LOADFALSE
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // LFALSESKIP
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // LOADTRUE
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // LOADNIL
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // GETUPVAL
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // SETUPVAL
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // GETTABUP
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // GETTABLE
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // GETI
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // GETFIELD
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // SETTABUP
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // SETTABLE
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // SETI
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // SETFIELD
    opmode_entry(0, 0, 0, 0, 1, OpMode::IvABC), // NEWTABLE
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // SELF
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // ADDI
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // ADDK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // SUBK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // MULK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // MODK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // POWK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // DIVK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // IDIVK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // BANDK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // BORK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // BXORK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // SHLI
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // SHRI
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // ADD
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // SUB
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // MUL
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // MOD
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // POW
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // DIV
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // IDIV
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // BAND
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // BOR
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // BXOR
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // SHL
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // SHR
    opmode_entry(1, 0, 0, 0, 0, OpMode::IABC),  // MMBIN
    opmode_entry(1, 0, 0, 0, 0, OpMode::IABC),  // MMBINI
    opmode_entry(1, 0, 0, 0, 0, OpMode::IABC),  // MMBINK
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // UNM
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // BNOT
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // NOT
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // LEN
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // CONCAT
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // CLOSE
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // TBC
    opmode_entry(0, 0, 0, 0, 0, OpMode::IsJ),   // JMP
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // EQ
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // LT
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // LE
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // EQK
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // EQI
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // LTI
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // LEI
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // GTI
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // GEI
    opmode_entry(0, 0, 0, 1, 0, OpMode::IABC),  // TEST
    opmode_entry(0, 0, 0, 1, 1, OpMode::IABC),  // TESTSET
    opmode_entry(0, 1, 1, 0, 1, OpMode::IABC),  // CALL
    opmode_entry(0, 1, 1, 0, 1, OpMode::IABC),  // TAILCALL
    opmode_entry(0, 0, 1, 0, 0, OpMode::IABC),  // RETURN
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // RETURN0
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // RETURN1
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABx),  // FORLOOP
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABx),  // FORPREP
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABx),  // TFORPREP
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABC),  // TFORCALL
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABx),  // TFORLOOP
    opmode_entry(0, 0, 1, 0, 0, OpMode::IvABC), // SETLIST
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABx),  // CLOSURE
    opmode_entry(0, 1, 0, 0, 1, OpMode::IABC),  // VARARG
    opmode_entry(0, 0, 0, 0, 1, OpMode::IABC),  // GETVARG
    opmode_entry(0, 0, 0, 0, 0, OpMode::IABx),  // ERRNNIL
    opmode_entry(0, 0, 1, 0, 1, OpMode::IABC),  // VARARGPREP
    opmode_entry(0, 0, 0, 0, 0, OpMode::IAx),   // EXTRAARG
];

pub fn opmodes() -> &'static [u8] {
    &OP_MODES
}

#[inline]
pub fn get_opmode(op: OpCode) -> OpMode {
    let raw = OP_MODES[op as usize] & 7;
    unsafe { std::mem::transmute::<u8, OpMode>(raw) }
}

#[inline]
pub fn is_ot(i: Instruction) -> bool {
    let op = get_opcode(i);
    match op {
        OpCode::TAILCALL => true,
        _ => test_ot_mode(op) && getarg_c(i) == 0,
    }
}

#[inline]
pub fn is_it(i: Instruction) -> bool {
    let op = get_opcode(i);
    match op {
        OpCode::SETLIST => test_it_mode(op) && getarg_vb(i) == 0,
        _ => test_it_mode(op) && getarg_b(i) == 0,
    }
}

#[inline]
pub fn test_a_mode(op: OpCode) -> bool {
    OP_MODES[op as usize] & (1 << 3) != 0
}
#[inline]
pub fn test_t_mode(op: OpCode) -> bool {
    OP_MODES[op as usize] & (1 << 4) != 0
}
#[inline]
pub fn test_it_mode(op: OpCode) -> bool {
    OP_MODES[op as usize] & (1 << 5) != 0
}
#[inline]
pub fn test_ot_mode(op: OpCode) -> bool {
    OP_MODES[op as usize] & (1 << 6) != 0
}
#[inline]
pub fn test_mm_mode(op: OpCode) -> bool {
    OP_MODES[op as usize] & (1 << 7) != 0
}

// ============================================================================
// 指令创建
// ============================================================================

#[inline]
pub fn create_abck(o: OpCode, a: i32, b: i32, c: i32, k: i32) -> Instruction {
    ((o as u32) << POS_OP)
        | ((a as u32) << POS_A)
        | ((b as u32) << POS_B)
        | ((c as u32) << POS_C)
        | ((k as u32) << POS_K)
}

#[inline]
pub fn create_vabck(o: OpCode, a: i32, b: i32, c: i32, k: i32) -> Instruction {
    ((o as u32) << POS_OP)
        | ((a as u32) << POS_A)
        | ((b as u32) << POS_VB)
        | ((c as u32) << POS_VC)
        | ((k as u32) << POS_K)
}

pub static OPNAMES: &[&str] = &[
    "MOVE",
    "LOADI",
    "LOADF",
    "LOADK",
    "LOADKX",
    "LOADFALSE",
    "LFALSESKIP",
    "LOADTRUE",
    "LOADNIL",
    "GETUPVAL",
    "SETUPVAL",
    "GETTABUP",
    "GETTABLE",
    "GETI",
    "GETFIELD",
    "SETTABUP",
    "SETTABLE",
    "SETI",
    "SETFIELD",
    "NEWTABLE",
    "SELF",
    "ADDI",
    "ADDK",
    "SUBK",
    "MULK",
    "MODK",
    "POWK",
    "DIVK",
    "IDIVK",
    "BANDK",
    "BORK",
    "BXORK",
    "SHLI",
    "SHRI",
    "ADD",
    "SUB",
    "MUL",
    "MOD",
    "POW",
    "DIV",
    "IDIV",
    "BAND",
    "BOR",
    "BXOR",
    "SHL",
    "SHR",
    "MMBIN",
    "MMBINI",
    "MMBINK",
    "UNM",
    "BNOT",
    "NOT",
    "LEN",
    "CONCAT",
    "CLOSE",
    "TBC",
    "JMP",
    "EQ",
    "LT",
    "LE",
    "EQK",
    "EQI",
    "LTI",
    "LEI",
    "GTI",
    "GEI",
    "TEST",
    "TESTSET",
    "CALL",
    "TAILCALL",
    "RETURN",
    "RETURN0",
    "RETURN1",
    "FORLOOP",
    "FORPREP",
    "TFORPREP",
    "TFORCALL",
    "TFORLOOP",
    "SETLIST",
    "CLOSURE",
    "VARARG",
    "GETVARG",
    "ERRNNIL",
    "VARARGPREP",
    "EXTRAARG",
];

pub static TM_EVENT_NAMES: &[&str] = &[
    "__index",
    "__newindex",
    "__gc",
    "__mode",
    "__len",
    "__eq",
    "__add",
    "__sub",
    "__mul",
    "__mod",
    "__pow",
    "__div",
    "__idiv",
    "__band",
    "__bor",
    "__bxor",
    "__shl",
    "__shr",
    "__unm",
    "__bnot",
    "__lt",
    "__le",
    "__concat",
    "__call",
    "__close",
];

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opmodes_from_c() {
        let modes = opmodes();
        assert_eq!(modes.len(), NUM_OPCODES);
        assert_eq!(get_opmode(OpCode::MOVE), OpMode::IABC);
        assert_eq!(get_opmode(OpCode::JMP), OpMode::IsJ);
        assert_eq!(get_opmode(OpCode::LOADI), OpMode::IAsBx);
    }

    #[test]
    fn test_is_ot_from_c() {
        let call = create_abck(OpCode::CALL, 0, 1, 0, 0);
        assert!(is_ot(call));
        let add = create_abck(OpCode::ADD, 0, 1, 2, 0);
        assert!(!is_ot(add));
    }

    #[test]
    fn test_is_it_from_c() {
        let call = create_abck(OpCode::CALL, 0, 0, 1, 0);
        assert!(is_it(call));
    }

    #[test]
    fn test_opcode_from_u8() {
        assert_eq!(OpCode::from_u8(0), Some(OpCode::MOVE));
        assert_eq!(OpCode::from_u8(84), Some(OpCode::EXTRAARG));
        assert_eq!(OpCode::from_u8(85), None);
    }
}
