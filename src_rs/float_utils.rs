//! 浮点数格式化/解析工具
//!
//! `size_optimized` 模式下用 libc 函数避免引入 Rust 的 flt2dec/dec2flt 代码 (~34KB):
//!   - `format!("{}", f)` 引入 flt2dec (format_shortest + CACHED_POW10, ~13KB)
//!   - `"3.14".parse::<f64>()` 引入 dec2flt (POWER_OF_FIVE_128, ~21KB)
//!
//! libc 的 `snprintf("%.14g")` / `strtod` 对应 C Lua 的浮点处理方式,
//! 行为更接近 C 实现, 且不引入额外 Rust 代码。
//!
//! **Miri 兼容性**: Miri 下 FFI 不受支持 (如 `strtod` / `snprintf` / `localeconv`),
//! 所以即使开启了 `size_optimized`, 也会自动回退到纯 Rust 实现.
//! 判定条件统一为 `cfg(all(size_optimized, not(miri)))`.

use crate::config::{LuaInteger, LuaNumber};
#[cfg(all(size_optimized, not(miri)))]
use crate::config::{INTEGER_FMT, NUMBER_FMT, NUMBER_FMT_N};
#[cfg(all(size_optimized, not(miri)))]
use const_format::concatcp;

/// `NUMBER_FMT` / `NUMBER_FMT_N` 的 NUL 终止 C 字符串形式 (供 snprintf 使用)。
/// 对应 C 的 `luaO_num2buf` 使用的 `LUA_NUMBER_FMT` / `LUA_NUMBER_FMT_N`。
#[cfg(all(size_optimized, not(miri)))]
const NUMBER_FMT_C: &str = concatcp!(NUMBER_FMT, "\0");
#[cfg(all(size_optimized, not(miri)))]
const NUMBER_FMT_N_C: &str = concatcp!(NUMBER_FMT_N, "\0");
/// `INTEGER_FMT` 的 NUL 终止 C 字符串形式 (对应 C 的 `LUA_INTEGER_FMT`, `%lld` / `%d`)。
#[cfg(all(size_optimized, not(miri)))]
const INTEGER_FMT_C: &str = concatcp!(INTEGER_FMT, "\0");

/// `INTEGER_FMT` 对应的 libc 变参实参类型: `%lld` 读 `long long`, `%d` 读 `int`。
/// 传错宽度是 UB, 故按 `LuaInteger` 的实际宽度选择 (C 的 `LUA_INTEGER_FMT` 与
/// `lua_Integer` 总是配对出现)。
#[cfg(all(size_optimized, not(miri), not(feature = "num-i32")))]
type LibcIntArg = libc::c_longlong;
#[cfg(all(size_optimized, not(miri), feature = "num-i32"))]
type LibcIntArg = libc::c_int;

// ============================================================================
// f64 -> String
// ============================================================================

/// 将 f64 格式化为字符串。
///
/// 对应 C Lua 5.5 的 tostringbuffFloat (lobject.c):
///   1. 先用 %.15g (LUA_NUMBER_FMT) 格式化
///   2. 用 strtod 读回, 若不等于原值则用 %.17g (LUA_NUMBER_FMT_N) 重试
///   3. 若结果只含数字/符号 (整数浮点数), 添加 ".0" 后缀
///
/// **libc 版本**: 仅 `size_optimized` 且非 Miri 下编译。
#[cfg(all(size_optimized, not(miri)))]
pub fn f64_to_string(f: LuaNumber) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    // C 变参默认实参提升 float → double (C 的 LUAI_UACNUMBER): snprintf 的 %g
    // 从 va_list 读 double, 必须传提升后的值。
    let f = f as f64;
    unsafe {
        let mut buf = [0u8; 64];

        // 阶段 1: NUMBER_FMT (LUA_NUMBER_FMT) — 对应 C Lua 的第一次尝试
        let n15 = libc::snprintf(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            NUMBER_FMT_C.as_ptr() as *const libc::c_char,
            f,
        );
        if n15 <= 0 || (n15 as usize) >= buf.len() {
            return "0.0".to_string();
        }

        // 检查往返: C 的 lua_str2number 读回 (strtod / strtof) == f ?
        #[cfg(not(feature = "num-f32"))]
        let check = libc::strtod(buf.as_ptr() as *const libc::c_char, std::ptr::null_mut());
        #[cfg(feature = "num-f32")]
        let check = libc::strtof(buf.as_ptr() as *const libc::c_char, std::ptr::null_mut());
        let final_n = if check as f64 != f {
            // 阶段 2: NUMBER_FMT_N (LUA_NUMBER_FMT_N) — 精度不足, 用更多数字
            let n17 = libc::snprintf(
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                NUMBER_FMT_N_C.as_ptr() as *const libc::c_char,
                f,
            );
            if n17 <= 0 || (n17 as usize) >= buf.len() {
                return "0.0".to_string();
            }
            n17
        } else {
            n15
        };

        let s = std::str::from_utf8_unchecked(&buf[..final_n as usize]);

        // 阶段 3: 整数浮点数添加 ".0" 后缀
        // 对应 C Lua 5.5: buff[strspn(buff, "-0123456789")] == '\0'
        let all_digits_or_sign = s.bytes().all(|b| b.is_ascii_digit() || b == b'-');
        if all_digits_or_sign {
            let mut out = String::with_capacity(s.len() + 2);
            out.push_str(s);
            out.push_str(".0");
            out
        } else {
            s.to_string()
        }
    }
}

/// 将 f64 格式化为字符串 (纯 Rust 版本)。
///
/// 用 Rust 原生 `format!` (Ryg 算法, 最短往返表示), 处理 nan/inf 和 ".0" 后缀
/// 以匹配 C Lua 5.5 行为.
///
/// **注意**: 该实现用于默认构建以及所有 Miri 构建。
#[cfg(any(not(size_optimized), miri))]
#[inline]
pub fn f64_to_string(f: LuaNumber) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    // -0.0 应输出 "0.0" 而非 "-0.0" (对应 C Lua 的行为)
    if f == 0.0 {
        return "0.0".to_string();
    }
    let s = format!("{}", f);
    // 检查是否为整数浮点数 (如 3.0 → "3", -1203.0 → "-1203"), 添加 ".0" 后缀.
    // 优化: 用 contains 而非 bytes().all, 大多数浮点数含 '.' 或 'e', 快速路径.
    if !s.contains('.') && !s.contains('e') && !s.contains('E') {
        format!("{}.0", s)
    } else {
        s
    }
}

// ============================================================================
// f64 -> String (定点 / 科学计数法)
// ============================================================================

/// 将 f64 格式化为定点表示 (对应 `format!("{:.N}", f)`)。
#[cfg(all(size_optimized, not(miri)))]
pub fn f64_to_string_fixed(f: LuaNumber, precision: usize) -> String {
    // 手动拼接 "%.Nf\0" 避免用 format! (会引入 fmt 代码).
    // 不用 "%.*f" + 变参 precision, 因为 Rust 调 C variadic 时混合 int/float
    // 参数传递在某些情况下可能出错 (实测 string.format("%.2f", 3.14) 输出 0.00).
    let mut fmt = [0u8; 8];
    fmt[0] = b'%';
    fmt[1] = b'.';
    let plen = write_precision(&mut fmt, 2, precision);
    fmt[2 + plen] = b'f';
    fmt[2 + plen + 1] = 0; // NUL 终止

    // C 变参默认实参提升 float → double (C 的 LUAI_UACNUMBER).
    let f = f as f64;
    unsafe {
        // 缓冲区需足够大: %.99f 对 1e308 输出可达 309+1+99+1=410 字符.
        // 用 512 字节确保安全.
        let mut buf = [0u8; 512];
        let n = libc::snprintf(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            fmt.as_ptr() as *const libc::c_char,
            f,
        );
        if n > 0 && (n as usize) < buf.len() {
            std::str::from_utf8_unchecked(&buf[..n as usize]).to_string()
        } else {
            // snprintf 失败或截断(极罕见), 返回占位值避免引入 flt2dec 代码
            "0.0".to_string()
        }
    }
}

/// 将 f64 格式化为定点表示 (纯 Rust 版本)。
#[cfg(any(not(size_optimized), miri))]
#[inline]
pub fn f64_to_string_fixed(f: LuaNumber, precision: usize) -> String {
    format!("{:.*}", precision, f)
}

/// 将 f64 格式化为科学计数法表示 (对应 `format!("{:.*e}", precision, f)`)。
#[cfg(all(size_optimized, not(miri)))]
pub fn f64_to_string_exp(f: LuaNumber, precision: usize, uppercase: bool) -> String {
    // 手动拼接 "%.Ne\0" / "%.NE\0" 避免用 format! (会引入 fmt 代码).
    let mut fmt = [0u8; 8];
    fmt[0] = b'%';
    fmt[1] = b'.';
    let plen = write_precision(&mut fmt, 2, precision);
    fmt[2 + plen] = if uppercase { b'E' } else { b'e' };
    fmt[2 + plen + 1] = 0; // NUL 终止

    // C 变参默认实参提升 float → double (C 的 LUAI_UACNUMBER).
    let f = f as f64;
    unsafe {
        // 缓冲区: %.99e 输出约 1+1+99+5=106 字符, 128 字节足够.
        let mut buf = [0u8; 128];
        let n = libc::snprintf(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            fmt.as_ptr() as *const libc::c_char,
            f,
        );
        if n > 0 && (n as usize) < buf.len() {
            std::str::from_utf8_unchecked(&buf[..n as usize]).to_string()
        } else {
            "0.0".to_string()
        }
    }
}

/// 将 f64 格式化为科学计数法表示 (纯 Rust 版本)。
#[cfg(any(not(size_optimized), miri))]
#[inline]
pub fn f64_to_string_exp(f: LuaNumber, precision: usize, uppercase: bool) -> String {
    if uppercase {
        format!("{:.*E}", precision, f)
    } else {
        format!("{:.*e}", precision, f)
    }
}

/// 将 `precision` 的十进制数字写入 `buf[offset..]`, 返回写入的字节数.
///
/// 仅 libc 路径使用. 调用方需要保证 `buf` 至少有 `offset + 20` 字节空间.
#[cfg(all(size_optimized, not(miri)))]
fn write_precision(buf: &mut [u8], offset: usize, precision: usize) -> usize {
    if precision == 0 {
        buf[offset] = b'0';
        return 1;
    }
    // 反向写入再翻转 (usize 最多 20 位十进制数字)
    let mut digits = [0u8; 20];
    let mut n = 0;
    let mut p = precision;
    while p > 0 {
        digits[n] = b'0' + (p % 10) as u8;
        p /= 10;
        n += 1;
    }
    for i in 0..n {
        buf[offset + i] = digits[n - 1 - i];
    }
    n
}

// ============================================================================
// str -> f64
// ============================================================================

/// 将字符串解析为 f64。
///
/// libc 版本用 `libc::strtod` (对应 C Lua 的 lua_str2number), 支持 locale 小数点
/// (如 pt_BR locale 下 "3,4" 解析为 3.4)。
///
/// 对应 C Lua l_str2d 的 locale fallback: 若 strtod 失败且字符串含 '.',
/// 将 '.' 替换为 locale 的 decimal_point 后重试。
/// (如 pt_BR locale 下 "3.4" → 替换为 "3,4" → strtod 成功)
#[cfg(all(size_optimized, not(miri)))]
pub fn f64_from_str(s: &str) -> Option<LuaNumber> {
    // strtod 内部会跳过前导空格, 但 Lua 的 tonumber 也允许前导/尾随空格.
    // 先去除尾随空格, 便于检查 end 是否到达字符串末尾.
    let trimmed = s.trim_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.is_empty() {
        return None;
    }

    // 第一次尝试: 用原始字符串 (strtod 会尊重 locale)
    if let Some(val) = try_strtod(trimmed) {
        return Some(val);
    }

    // Locale fallback (对应 C Lua l_str2d):
    // 若字符串含 '.', 替换为 locale 的 decimal_point 后重试
    if trimmed.contains('.') {
        let dec_point = unsafe { get_locale_decpoint() };
        if dec_point != '.' {
            let alt = trimmed.replace('.', &dec_point.to_string());
            if let Some(val) = try_strtod(&alt) {
                return Some(val);
            }
        }
    }
    None
}

/// 将字符串解析为 f64 (纯 Rust 版本)。
///
/// 对应 C Lua l_str2d 的语义: 先按 '.' 小数点解析 (覆盖 "C" locale 及
/// `load` 数字字面量场景); 失败且 locale 小数点不是 '.' 时, 将其替换为
/// '.' 后重试 (如 pt_BR locale 下 "3,4" → "3.4")。
///
/// **注意**: Rust 的 `str::parse::<f64>` 不识别十六进制浮点数 (如 `0x1.8p3`),
/// hex float 由 `objects::parse_hex_float` 单独处理。
#[cfg(any(not(size_optimized), miri))]
#[inline]
pub fn f64_from_str(s: &str) -> Option<LuaNumber> {
    let trimmed = s.trim_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.is_empty() {
        return None;
    }
    // 第一次尝试: '.' 作为小数点
    if let Ok(v) = trimmed.parse::<LuaNumber>() {
        return Some(v);
    }
    // Locale fallback (对应 C Lua l_str2d): strtod 在 pt_BR 等 locale 下
    // 接受 locale 小数点 ("3,4"), 这里对称地把 locale 小数点替换为 '.' 后重试。
    // Miri 下 get_locale_decpoint 恒为 '.', 不会调用 FFI。
    let dec_point = unsafe { get_locale_decpoint() };
    if dec_point != '.' {
        let alt = trimmed.replace(dec_point, ".");
        if let Ok(v) = alt.parse::<LuaNumber>() {
            return Some(v);
        }
    }
    None
}

/// 用 libc 的 `<stdlib.h>` 解析函数解析字符串, 要求整个字符串都被消费。
///
/// 按 `LuaNumber` 选择 `strtod` / `strtof` (对应 C 的 `lua_str2number`)。
#[cfg(all(size_optimized, not(miri)))]
fn try_strtod(s: &str) -> Option<LuaNumber> {
    use std::ffi::CString;

    let c_str = CString::new(s).ok()?;
    let mut end: *mut libc::c_char = std::ptr::null_mut();
    unsafe {
        #[cfg(not(feature = "num-f32"))]
        let val = libc::strtod(c_str.as_ptr(), &mut end);
        #[cfg(feature = "num-f32")]
        let val = libc::strtof(c_str.as_ptr(), &mut end);
        if end as *const libc::c_char == c_str.as_ptr() {
            return None;
        }
        if *end != 0 {
            return None;
        }
        Some(val)
    }
}

/// 获取当前 locale 的小数点字符 (对应 C 的 lua_getlocaledecpoint)。
///
/// 通过 CRT `localeconv()` 查询, `os.setlocale` 设置的 locale (如 pt_BR 的 ',')
/// 会立即生效。
#[cfg(not(miri))]
pub unsafe fn get_locale_decpoint() -> char {
    #[repr(C)]
    struct LConv {
        decimal_point: *const libc::c_char,
    }
    extern "C" {
        fn localeconv() -> *const LConv;
    }
    let lc = unsafe { localeconv() };
    if lc.is_null() || (*lc).decimal_point.is_null() {
        '.'
    } else {
        *(*lc).decimal_point as u8 as char
    }
}

/// 获取当前 locale 的小数点字符 (Miri 回退: 恒为 `.`)。
#[cfg(miri)]
#[inline]
pub unsafe fn get_locale_decpoint() -> char {
    '.'
}

// ============================================================================
// i64 -> String
// ============================================================================

/// 将 `LuaInteger` 格式化为字符串 (libc 版本)。
///
/// 对应 C 的 `lua_integer2str` / `luaO_num2buf` 的整数分支 (`LUA_INTEGER_FMT`)。
#[cfg(all(size_optimized, not(miri)))]
pub fn i64_to_string(i: LuaInteger) -> String {
    // i64 最大 20 位数字 (-9223372036854775808), 21 字节缓冲区足够 (含符号+NUL)
    let mut buf = [0u8; 24];
    unsafe {
        // 变参实参宽度必须与 INTEGER_FMT 匹配 (i64 → %lld 读 long long,
        // i32 → %d 读 int); 见 LibcIntArg。
        let n = libc::snprintf(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            INTEGER_FMT_C.as_ptr() as *const libc::c_char,
            i as LibcIntArg,
        );
        if n > 0 && (n as usize) < buf.len() {
            std::str::from_utf8_unchecked(&buf[..n as usize]).to_string()
        } else {
            "0".to_string()
        }
    }
}

/// 将 `LuaInteger` 格式化为字符串 (纯 Rust 版本)。
#[cfg(any(not(size_optimized), miri))]
#[inline]
pub fn i64_to_string(i: LuaInteger) -> String {
    i.to_string()
}

/// 计算 `LuaInteger` 格式化后的字符串长度 (不分配内存)。
///
/// 该函数在两种路径下都使用数学计算避免 `to_string()` 分配, 无需 cfg 分叉.
#[inline]
pub fn i64_str_len(i: LuaInteger) -> usize {
    if i == 0 {
        return 1;
    }
    // MIN_INTEGER 的绝对值无法用有符号类型表示, 用无符号别名处理
    // (对应 C 的 l_castS2U 后取负)。
    let abs: crate::config::LuaUnsigned = if i < 0 {
        i.unsigned_abs()
    } else {
        i as crate::config::LuaUnsigned
    };
    // 计算位数: log10(abs) + 1
    let mut digits = 1usize;
    let mut v = abs;
    while v >= 10 {
        v /= 10;
        digits += 1;
    }
    digits + if i < 0 { 1 } else { 0 }
}

// ============================================================================
// 编译期自检
// ============================================================================

// 确保 size_optimized 与 miri 不会同时走 libc 路径时出现空白 (防呆).
#[cfg(all(size_optimized, not(miri)))]
const _: () = {
    // libc 路径下, 上述函数均会生成; 无需额外检查.
};
