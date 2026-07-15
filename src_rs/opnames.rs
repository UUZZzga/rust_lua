//! Lua 操作码名称表
//!
//! 对应 C 源码: lopnames.h
//! 提供所有 85 个 Lua 5.5 虚拟机操作码对应的名称字符串。

use std::fmt;

// ============================================================================
// 操作码名称枚举 — 与 opcodes.rs 中的 OpCode 一一对应
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum OpName {
    Move = 0,
    LoadI = 1,
    LoadF = 2,
    LoadK = 3,
    LoadKx = 4,
    LoadFalse = 5,
    LFalseSkip = 6,
    LoadTrue = 7,
    LoadNil = 8,
    GetUpval = 9,
    SetUpval = 10,
    GetTabUp = 11,
    GetTable = 12,
    GetI = 13,
    GetField = 14,
    SetTabUp = 15,
    SetTable = 16,
    SetI = 17,
    SetField = 18,
    NewTable = 19,
    Self_ = 20,
    AddI = 21,
    AddK = 22,
    SubK = 23,
    MulK = 24,
    ModK = 25,
    PowK = 26,
    DivK = 27,
    IDivK = 28,
    BandK = 29,
    BorK = 30,
    BxorK = 31,
    ShlI = 32,
    ShrI = 33,
    Add = 34,
    Sub = 35,
    Mul = 36,
    Mod = 37,
    Pow = 38,
    Div = 39,
    IDiv = 40,
    Band = 41,
    Bor = 42,
    Bxor = 43,
    Shl = 44,
    Shr = 45,
    MmBin = 46,
    MmBinI = 47,
    MmBinK = 48,
    Unm = 49,
    BNot = 50,
    Not = 51,
    Len = 52,
    Concat = 53,
    Close = 54,
    Tbc = 55,
    Jmp = 56,
    Eq = 57,
    Lt = 58,
    Le = 59,
    EqK = 60,
    EqI = 61,
    LtI = 62,
    LeI = 63,
    GtI = 64,
    GeI = 65,
    Test = 66,
    TestSet = 67,
    Call = 68,
    TailCall = 69,
    Return = 70,
    Return0 = 71,
    Return1 = 72,
    ForLoop = 73,
    ForPrep = 74,
    TForPrep = 75,
    TForCall = 76,
    TForLoop = 77,
    SetList = 78,
    Closure = 79,
    VarArg = 80,
    GetVArg = 81,
    ErrNNil = 82,
    VarArgPrep = 83,
    ExtraArg = 84,
}

pub const NUM_OPNAMES: usize = 85;

impl OpName {
    #[inline]
    pub fn from_u8(v: u8) -> Option<Self> {
        if (v as usize) < NUM_OPNAMES {
            Some(unsafe { std::mem::transmute::<u8, OpName>(v) })
        } else {
            None
        }
    }

    #[inline]
    pub fn to_str(self) -> &'static str {
        OPCODE_NAMES[self as usize]
    }

    #[inline]
    pub fn to_uppercase_str(self) -> &'static str {
        OPCODE_NAMES[self as usize]
    }
}

impl fmt::Display for OpName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.to_str())
    }
}

// ============================================================================
// 操作码名称表 — 对应 C 的 opnames[] 数组
// ============================================================================

pub static OPCODE_NAMES: &[&str; NUM_OPNAMES] = &[
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

// ============================================================================
// 按名称查找操作码编号
// ============================================================================

pub fn find_opcode(name: &str) -> Option<u8> {
    OPCODE_NAMES
        .iter()
        .position(|&n| n == name)
        .map(|i| i as u8)
}

// ============================================================================
// 操作码分类辅助函数
// ============================================================================

impl OpName {
    pub fn is_arithmetic(self) -> bool {
        matches!(
            self,
            OpName::Add
                | OpName::Sub
                | OpName::Mul
                | OpName::Mod
                | OpName::Pow
                | OpName::Div
                | OpName::IDiv
        )
    }

    pub fn is_bitwise(self) -> bool {
        matches!(
            self,
            OpName::Band | OpName::Bor | OpName::Bxor | OpName::Shl | OpName::Shr
        )
    }

    pub fn is_comparison(self) -> bool {
        matches!(self, OpName::Eq | OpName::Lt | OpName::Le)
    }

    pub fn is_jump(self) -> bool {
        matches!(
            self,
            OpName::Jmp | OpName::ForLoop | OpName::ForPrep | OpName::TForLoop | OpName::TForPrep
        )
    }

    pub fn is_call(self) -> bool {
        matches!(self, OpName::Call | OpName::TailCall)
    }

    pub fn is_return(self) -> bool {
        matches!(self, OpName::Return | OpName::Return0 | OpName::Return1)
    }

    pub fn is_load_constant(self) -> bool {
        matches!(
            self,
            OpName::LoadK | OpName::LoadKx | OpName::LoadI | OpName::LoadF
        )
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_names_present() {
        for i in 0..NUM_OPNAMES as u8 {
            let op = OpName::from_u8(i).unwrap();
            assert_eq!(OPCODE_NAMES[i as usize], op.to_str());
        }
    }

    #[test]
    fn test_display() {
        assert_eq!(OpName::Move.to_string(), "MOVE");
        assert_eq!(OpName::ExtraArg.to_string(), "EXTRAARG");
        assert_eq!(OpName::Call.to_string(), "CALL");
    }

    #[test]
    fn test_find_opcode() {
        assert_eq!(find_opcode("MOVE"), Some(0));
        assert_eq!(find_opcode("EXTRAARG"), Some(84));
        assert_eq!(find_opcode("UNKNOWN"), None);
    }

    #[test]
    fn test_invalid_opcode() {
        assert_eq!(OpName::from_u8(85), None);
        assert_eq!(OpName::from_u8(255), None);
    }

    #[test]
    fn test_classification() {
        assert!(OpName::Add.is_arithmetic());
        assert!(OpName::Band.is_bitwise());
        assert!(OpName::Eq.is_comparison());
        assert!(OpName::Jmp.is_jump());
        assert!(OpName::Call.is_call());
        assert!(OpName::Return.is_return());
        assert!(!OpName::Move.is_arithmetic());
        assert!(!OpName::Move.is_jump());
    }
}
