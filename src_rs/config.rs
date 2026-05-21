//! Lua 配置模块
//!
//! 对应 C 源码: luaconf.h
//! 提供 Lua 虚拟机所需的基础类型定义、常量、路径配置和数值运算辅助函数。

use std::ffi::CStr;
use std::os::raw::c_char;

// ============================================================================
// 版本信息
// ============================================================================

pub const VERSION_MAJOR: u8 = 5;
pub const VERSION_MINOR: u8 = 5;
pub const VERSION_RELEASE: u8 = 0;
pub const VERSION_NUM: u32 = (VERSION_MAJOR as u32) * 100 + (VERSION_MINOR as u32);
pub const VERSION_RELEASE_NUM: u32 = VERSION_NUM * 100 + (VERSION_RELEASE as u32);

pub const VERSION_MAJOR_STR: &str = "5";
pub const VERSION_MINOR_STR: &str = "5";
pub const VERSION_RELEASE_STR: &str = "0";
pub const VERSION: &str = "Lua 5.5";
pub const RELEASE: &str = "Lua 5.5.0";
pub const COPYRIGHT: &str = "Lua 5.5.0  Copyright (C) 1994-2025 Lua.org, PUC-Rio";
pub const AUTHORS: &str = "R. Ierusalimschy, L. H. de Figueiredo, W. Celes";

// ============================================================================
// 数值类型 — Lua 的核心数字类型
// ============================================================================

pub type LuaNumber = f64;
pub type LuaInteger = i64;
pub type LuaUnsigned = u64;
pub type LuaKContext = isize;

pub const MAX_INTEGER: LuaInteger = i64::MAX;
pub const MIN_INTEGER: LuaInteger = i64::MIN;
pub const MAX_UNSIGNED: LuaUnsigned = u64::MAX;

pub const INTEGER_FMT: &str = "%lld";
pub const NUMBER_FMT: &str = "%.15g";
pub const NUMBER_FMT_MAX: &str = "%.17g";

// ============================================================================
// 内存类型
// ============================================================================

pub type LuaMem = isize;
pub type LuaUMem = usize;
pub const MAX_MEM: LuaMem = LuaMem::MAX;

// ============================================================================
// 字节和状态类型
// ============================================================================

pub type LuByte = u8;
pub type LsByte = i8;
pub type TStatus = LuByte;

// ============================================================================
// 线程 / 错误状态码
// ============================================================================

pub const OK: i32 = 0;
pub const YIELD: i32 = 1;
pub const ERR_RUN: i32 = 2;
pub const ERR_SYNTAX: i32 = 3;
pub const ERR_MEM: i32 = 4;
pub const ERR_ERR: i32 = 5;

// ============================================================================
// 类型标签
// ============================================================================

pub const TNONE: i32 = -1;
pub const TNIL: i32 = 0;
pub const TBOOLEAN: i32 = 1;
pub const TLIGHTUSERDATA: i32 = 2;
pub const TNUMBER: i32 = 3;
pub const TSTRING: i32 = 4;
pub const TTABLE: i32 = 5;
pub const TFUNCTION: i32 = 6;
pub const TUSERDATA: i32 = 7;
pub const TTHREAD: i32 = 8;
pub const NUM_TYPES: i32 = 9;

// ============================================================================
// GC 参数
// ============================================================================

pub const GC_STOP: i32 = 0;
pub const GC_RESTART: i32 = 1;
pub const GC_COLLECT: i32 = 2;
pub const GC_COUNT: i32 = 3;
pub const GC_COUNTB: i32 = 4;
pub const GC_STEP: i32 = 5;
pub const GC_IS_RUNNING: i32 = 6;
pub const GC_GEN: i32 = 7;
pub const GC_INC: i32 = 8;
pub const GC_PARAM: i32 = 9;

pub const GCP_MINOR_MUL: i32 = 0;
pub const GCP_MAJOR_MINOR: i32 = 1;
pub const GCP_MINOR_MAJOR: i32 = 2;
pub const GCP_PAUSE: i32 = 3;
pub const GCP_STEP_MUL: i32 = 4;
pub const GCP_STEP_SIZE: i32 = 5;
pub const GCP_N: i32 = 6;

// ============================================================================
// 算术 / 比较操作码
// ============================================================================

pub const OP_ADD: i32 = 0;
pub const OP_SUB: i32 = 1;
pub const OP_MUL: i32 = 2;
pub const OP_MOD: i32 = 3;
pub const OP_POW: i32 = 4;
pub const OP_DIV: i32 = 5;
pub const OP_IDIV: i32 = 6;
pub const OP_BAND: i32 = 7;
pub const OP_BOR: i32 = 8;
pub const OP_BXOR: i32 = 9;
pub const OP_SHL: i32 = 10;
pub const OP_SHR: i32 = 11;
pub const OP_UNM: i32 = 12;
pub const OP_BNOT: i32 = 13;

pub const OP_EQ: i32 = 0;
pub const OP_LT: i32 = 1;
pub const OP_LE: i32 = 2;

// ============================================================================
// 栈 / 寄存器相关
// ============================================================================

pub const MIN_STACK: i32 = 20;
pub const MULTI_RET: i32 = -1;
pub const REGISTRY_INDEX: i32 = -(i32::MAX / 2 + 1000);

pub const RIDX_GLOBALS: i32 = 2;
pub const RIDX_MAINTHREAD: i32 = 3;
pub const RIDX_LAST: i32 = 3;

// ============================================================================
// Debug Hook 事件
// ============================================================================

pub const HOOK_CALL: i32 = 0;
pub const HOOK_RET: i32 = 1;
pub const HOOK_LINE: i32 = 2;
pub const HOOK_COUNT: i32 = 3;
pub const HOOK_TAIL_CALL: i32 = 4;

pub const MASK_CALL: i32 = 1 << HOOK_CALL;
pub const MASK_RET: i32 = 1 << HOOK_RET;
pub const MASK_LINE: i32 = 1 << HOOK_LINE;
pub const MASK_COUNT: i32 = 1 << HOOK_COUNT;

// ============================================================================
// 路径配置
// ============================================================================

pub const PATH_SEP: &str = ";";
pub const PATH_MARK: &str = "?";
pub const EXEC_DIR: &str = "!";
pub const IG_MARK: &str = "-";

#[cfg(target_os = "windows")]
pub const DIR_SEP: &str = "\\";
#[cfg(not(target_os = "windows"))]
pub const DIR_SEP: &str = "/";

#[cfg(target_os = "windows")]
pub const PATH_DEFAULT: &str = "!.\\?.lua;!.\\?\\init.lua;.\\?.lua;.\\?\\init.lua";
#[cfg(not(target_os = "windows"))]
pub const PATH_DEFAULT: &str = "/usr/local/share/lua/5.5/?.lua;/usr/local/share/lua/5.5/?/init.lua;/usr/local/lib/lua/5.5/?.lua;/usr/local/lib/lua/5.5/?/init.lua;./?.lua;./?/init.lua";

#[cfg(target_os = "windows")]
pub const CPATH_DEFAULT: &str = "!.\\?.dll;!.\\..\\lib\\lua\\5.5\\?.dll;!.\\loadall.dll;.\\?.dll";
#[cfg(not(target_os = "windows"))]
pub const CPATH_DEFAULT: &str = "/usr/local/lib/lua/5.5/?.so;/usr/local/lib/lua/5.5/loadall.so;./?.so";

// ============================================================================
// 其他常量
// ============================================================================

pub const SIGNATURE: &str = "\x1bLua";
pub const EXTRASPACE: usize = std::mem::size_of::<*const u8>();
pub const ID_SIZE: usize = 60;
pub const N2S_BUFF_SIZE: usize = 64;

// ============================================================================
// 辅助函数
// ============================================================================

#[inline]
pub fn upvalue_index(i: i32) -> i32 {
    REGISTRY_INDEX - i
}

#[inline]
pub fn is_pow2(x: usize) -> bool {
    x & (x.wrapping_sub(1)) == 0
}

#[inline]
pub const fn num_bits<T>() -> usize {
    std::mem::size_of::<T>() * 8
}

#[inline]
pub fn integer_to_str(n: LuaInteger) -> String {
    n.to_string()
}

#[inline]
pub fn unsigned_to_str(n: LuaUnsigned) -> String {
    n.to_string()
}

#[inline]
pub fn pointer_to_str(ptr: *const u8) -> String {
    format!("{:p}", ptr)
}

#[inline]
pub fn number_to_integer(n: LuaNumber) -> Option<LuaInteger> {
    if n >= (MIN_INTEGER as LuaNumber) && n < (-(MIN_INTEGER as LuaNumber)) {
        let i = n as LuaInteger;
        if (i as LuaNumber) == n {
            Some(i)
        } else {
            None
        }
    } else {
        None
    }
}

#[inline]
pub fn floor_div(a: LuaNumber, b: LuaNumber) -> LuaNumber {
    (a / b).floor()
}

#[inline]
pub fn float_mod(a: LuaNumber, b: LuaNumber) -> LuaNumber {
    let m = a % b;
    if (m > 0.0) == (b < 0.0) && m != 0.0 {
        m + b
    } else {
        m
    }
}

#[inline]
pub fn float_pow(a: LuaNumber, b: LuaNumber) -> LuaNumber {
    if b == 2.0 {
        a * a
    } else {
        a.powf(b)
    }
}

#[inline]
pub fn num_is_nan(n: LuaNumber) -> bool {
    n.is_nan()
}

// ============================================================================
// CStr 辅助：从 FFI 传入的 C 字符串构建安全的 &str
// ============================================================================

pub unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        ""
    } else {
        CStr::from_ptr(ptr).to_str().unwrap_or("")
    }
}

// ============================================================================
// sz → LuaInteger 安全转换（对 Lua 内部已知不超 MAX_SIZE 的 size_t）
// ============================================================================

#[inline]
pub fn size_to_integer(sz: usize) -> LuaInteger {
    sz as LuaInteger
}

// ============================================================================
// 字符串字面量长度（不含尾零）
// ============================================================================

#[macro_export]
macro_rules! lit_len {
    ($s:expr) => {
        ($s.len())
    };
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upvalue_index() {
        assert_eq!(upvalue_index(1), REGISTRY_INDEX - 1);
        assert_eq!(upvalue_index(3), REGISTRY_INDEX - 3);
    }

    #[test]
    fn test_is_pow2() {
        assert!(is_pow2(1));
        assert!(is_pow2(2));
        assert!(is_pow2(4));
        assert!(is_pow2(8));
        assert!(is_pow2(0));
        assert!(!is_pow2(3));
        assert!(!is_pow2(5));
        assert!(!is_pow2(6));
    }

    #[test]
    fn test_number_to_integer() {
        assert_eq!(number_to_integer(42.0), Some(42));
        assert_eq!(number_to_integer(42.5), None);
        assert_eq!(number_to_integer(f64::NAN), None);
        assert_eq!(number_to_integer(0.0), Some(0));
    }

    #[test]
    fn test_float_mod() {
        assert!((float_mod(7.0, 3.0) - 1.0).abs() < 1e-10);
        assert!((float_mod(-7.0, 3.0) - 2.0).abs() < 1e-10);
        assert!((float_mod(7.0, -3.0) - (-2.0)).abs() < 1e-10);
    }
}