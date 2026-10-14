//! Lua 配置模块
//!
//! 对应 C 源码: luaconf.h
//! 提供 Lua 虚拟机所需的基础类型定义、常量、路径配置和数值运算辅助函数。

use const_format::concatcp;

// ============================================================================
// 版本信息
// ============================================================================

pub const VERSION_MAJOR: u8 = 5;
pub const VERSION_MINOR: u8 = 5;
pub const VERSION_NUM: u32 = (VERSION_MAJOR as u32) * 100 + (VERSION_MINOR as u32);

pub const VERSION_MAJOR_STR: &str = "5";
pub const VERSION_MINOR_STR: &str = "5";
pub const VERSION_RELEASE_STR: &str = "0";
pub const VERSION: &str = concatcp!("Lua ", VERSION_MAJOR_STR, ".", VERSION_MINOR_STR);
pub const RELEASE: &str = concatcp!(VERSION, ".", VERSION_RELEASE_STR);
pub const COPYRIGHT: &str = concatcp!(
    RELEASE,
    " Rust Edition  Copyright (C) 1994-2025 Lua.org, PUC-Rio"
);
pub const AUTHORS: &str = "R. Ierusalimschy, L. H. de Figueiredo, W. Celes";

pub const LUA_VERSUFFIX: &str = concatcp!("_", VERSION_MAJOR_STR, "_", VERSION_MINOR_STR);

// ============================================================================
// 数值类型 — Lua 的核心数字类型
// ============================================================================
//
// 对应 C 源码: luaconf.h 的 "Configuration for Number types" 段
//   LUA_INT_TYPE   ∈ { int, long, long long }      → LuaInteger / LuaUnsigned
//   LUA_FLOAT_TYPE ∈ { float, double, long double } → LuaNumber
//   LUA_32BITS: 32 位整数 + 32 位浮点（嵌入式受限平台）
//
// Rust 侧通过 cargo feature 选择（默认 = C 的默认配置: long long + double）:
//
//   cargo build --features embedded          # ≈ C 的 LUA_32BITS (i32 + f32)
//   cargo build --features num-i32           # 仅 32 位整数
//   cargo build --features num-f32           # 仅 32 位浮点
//
// 与 C 的 luaconf.h 一样：**任何与 Lua 连接的外部代码必须使用同一配置**
// （C API 的 lua_Integer / lua_Number 直接绑定这两个别名，见 capi.rs）。
// 不支持的组合: long double（Rust stable 无 f80/f128）。

/// Lua 整数类型 — 对应 C 的 `LUA_INTEGER`。
#[cfg(not(feature = "num-i32"))]
pub type LuaInteger = i64;
#[cfg(feature = "num-i32")]
pub type LuaInteger = i32;

/// `LuaInteger` 的无符号版本 — 对应 C 的 `LUA_UNSIGNED`。
/// 用于位运算/移位/取模（C 的 luaV_shiftl、luaV_mod 等经它转换）。
#[cfg(not(feature = "num-i32"))]
pub type LuaUnsigned = u64;
#[cfg(feature = "num-i32")]
pub type LuaUnsigned = u32;

/// Lua 浮点类型 — 对应 C 的 `LUA_NUMBER`。
#[cfg(not(feature = "num-f32"))]
pub type LuaNumber = f64;
#[cfg(feature = "num-f32")]
pub type LuaNumber = f32;

/// `LuaNumber` 的位模式容器（对应 C 里把 `lua_Number` memcpy 到整数再比较/去重的做法）。
///
/// 用于浮点常量的去重键与位布局运算：**必须随浮点宽度变化**，
/// 不能用 `LuaUnsigned`/`LuaInteger`（它们的宽度随整数配置变，
/// 在「i32 整数 + f64 浮点」组合下会把 f64 位模式截断成 32 位）。
#[cfg(not(feature = "num-f32"))]
pub type LuaNumberBits = u64;
#[cfg(feature = "num-f32")]
pub type LuaNumberBits = u32;

/// 对应 C 的 `LUA_MAXINTEGER`。
pub const MAX_INTEGER: LuaInteger = LuaInteger::MAX;
/// 对应 C 的 `LUA_MININTEGER`。
pub const MIN_INTEGER: LuaInteger = LuaInteger::MIN;
/// 对应 C 的 `LUA_MAXUNSIGNED`。
pub const MAX_UNSIGNED: LuaUnsigned = LuaUnsigned::MAX;

/// 整数位宽（对应 C 的 `NBITS`/`sizeof(lua_Integer) * CHAR_BIT`）。
pub const INTEGER_BITS: u32 = LuaInteger::BITS;

/// 对应 C 的 `LUA_INTEGER_FMT`（"%lld" / "%d"）。
/// 用法: `snprintf(buf, sz, INTEGER_FMT, n as LuaInteger)`。
#[cfg(not(feature = "num-i32"))]
pub const INTEGER_FMT: &str = "%lld";
#[cfg(feature = "num-i32")]
pub const INTEGER_FMT: &str = "%d";

/// 对应 C 的 `LUA_NUMBER_FMT`：保证 `tostring(tonumber(s)) == s` 的最大有效位数。
#[cfg(not(feature = "num-f32"))]
pub const NUMBER_FMT: &str = "%.15g";
#[cfg(feature = "num-f32")]
pub const NUMBER_FMT: &str = "%.7g";

/// 对应 C 的 `LUA_NUMBER_FMT_N`：保证 `tonumber(tostring(n)) == n` 的最少有效位数。
#[cfg(not(feature = "num-f32"))]
pub const NUMBER_FMT_N: &str = "%.17g";
#[cfg(feature = "num-f32")]
pub const NUMBER_FMT_N: &str = "%.9g";

/// 对应 C 的 `lua_numbertointeger`（llimits.h）：
/// 浮点值在 `LuaInteger` 范围内则转换为整数，否则返回 `None`。
/// 范围判定按 C 的写法（两补数下 `MIN_INTEGER` 可精确表示为浮点，
/// `MAX_INTEGER` 不一定，故上界用 `-(LuaNumber)MIN_INTEGER` 开区间）。
#[inline]
pub fn number_to_integer(n: LuaNumber) -> Option<LuaInteger> {
    if n >= MIN_INTEGER as LuaNumber && n < -(MIN_INTEGER as LuaNumber) {
        Some(n as LuaInteger)
    } else {
        None
    }
}

/// 对应 C 的 `l_intfitsf`（lvm.c）：判断整数能否无损转换为 `LuaNumber`。
/// 用于整数/浮点混合比较的快路径。当 `MAX_INTEGER` 的位数不超过尾数位数时
/// 恒为真（如 i32 + f64），编译期折叠为 `true`。
#[cfg(all(feature = "num-i32", not(feature = "num-f32")))]
#[inline]
pub const fn int_fits_float(_i: LuaInteger) -> bool {
    true
}

/// 见上；此分支用于 `MAX_INTEGER` 可能超出尾数精度的组合。
#[cfg(not(all(feature = "num-i32", not(feature = "num-f32"))))]
#[inline]
pub fn int_fits_float(i: LuaInteger) -> bool {
    /// 对应 C 的 `MAXINTFITSF` = `(lua_Unsigned)1 << NBM`（NBM = 尾数位数）。
    const MAXINTFITSF: LuaUnsigned = (1 as LuaUnsigned) << LuaNumber::MANTISSA_DIGITS;
    MAXINTFITSF.wrapping_add(i as LuaUnsigned) <= 2 * MAXINTFITSF
}

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
pub const CPATH_DEFAULT: &str = "!.\\?.dll;!.\\..\\lib\\lua\\5.5\\?.dll;!.\\loadall.dll;.\\?.dll";
#[cfg(not(target_os = "windows"))]
pub const CPATH_DEFAULT: &str =
    "/usr/local/lib/lua/5.5/?.so;/usr/local/lib/lua/5.5/loadall.so;./?.so";

// ============================================================================
// 其他常量
// ============================================================================

pub const SIGNATURE: &[u8; 4] = b"\x1bLua";
pub const FILEHANDLE: &str = "FILE*";

// ============================================================================
// 辅助函数
// ============================================================================

#[cfg_attr(not(size_optimized), inline)]
pub fn float_pow(a: LuaNumber, b: LuaNumber) -> LuaNumber {
    if b == 2.0 {
        a * a
    } else {
        a.powf(b)
    }
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
