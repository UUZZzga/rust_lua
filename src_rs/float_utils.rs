//! 浮点数格式化/解析工具
//!
//! size_optimized 模式下用 libc 函数避免引入 Rust 的 flt2dec/dec2flt 代码 (~34KB):
//!   - `format!("{}", f)` 引入 flt2dec (format_shortest + CACHED_POW10, ~13KB)
//!   - `"3.14".parse::<f64>()` 引入 dec2flt (POWER_OF_FIVE_128, ~21KB)
//!
//! libc 的 `snprintf("%.14g")` / `strtod` 对应 C Lua 的浮点处理方式,
//! 行为更接近 C 实现, 且不引入额外 Rust 代码。

use std::ffi::CString;

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
/// size_optimized 模式: 用 libc snprintf/strtod 避免 Rust flt2dec/dec2flt 代码
/// 默认模式: 用 format!("{}", f) (Rust 最短往返表示, 等效于两阶段)
#[cfg(size_optimized)]
pub fn f64_to_string(f: f64) -> String {
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
    unsafe {
        let mut buf = [0u8; 64];
        // 阶段 1: %.15g (LUA_NUMBER_FMT) — 对应 C Lua 的第一次尝试
        let fmt_15 = b"%.15g\0";
        let n15 = libc::snprintf(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            fmt_15.as_ptr() as *const libc::c_char,
            f,
        );
        if n15 <= 0 || (n15 as usize) >= buf.len() {
            return "0.0".to_string();
        }
        // 检查往返: strtod(buf) == f ?
        let mut end: *mut libc::c_char = std::ptr::null_mut();
        let check = libc::strtod(buf.as_ptr() as *const libc::c_char, &mut end);
        let final_n = if check != f {
            // 阶段 2: %.17g (LUA_NUMBER_FMT_N) — 精度不足, 用更多数字
            let fmt_17 = b"%.17g\0";
            let n17 = libc::snprintf(
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                fmt_17.as_ptr() as *const libc::c_char,
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

#[cfg(not(size_optimized))]
#[cfg_attr(not(size_optimized), inline)]
pub fn f64_to_string(f: f64) -> String {
    // 性能优先: 用 Rust 原生 format! (Ryg 算法, 最短往返表示)
    // 处理 nan/inf 和 ".0" 后缀以匹配 C Lua 5.5 行为.
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

/// 将 f64 格式化为定点表示 (对应 `format!("{:.N}", f)`)。
///
/// size_optimized 模式: 用 `libc::snprintf("%.Nf", f)` (precision 拼接到格式字符串)
/// 默认模式: 用 `format!("{:.N}", f)`
#[cfg(size_optimized)]
pub fn f64_to_string_fixed(f: f64, precision: usize) -> String {
    // 手动拼接 "%.Nf\0" 避免用 format! (会引入 fmt 代码).
    // 不用 "%.*f" + 变参 precision, 因为 Rust 调 C variadic 时混合 int/float
    // 参数传递在某些情况下可能出错 (实测 string.format("%.2f", 3.14) 输出 0.00).
    let mut fmt = [0u8; 8];
    fmt[0] = b'%';
    fmt[1] = b'.';
    let plen = write_precision(&mut fmt, 2, precision);
    fmt[2 + plen] = b'f';
    fmt[2 + plen + 1] = 0; // NUL 终止

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

#[cfg(not(size_optimized))]
#[cfg_attr(not(size_optimized), inline)]
pub fn f64_to_string_fixed(f: f64, precision: usize) -> String {
    format!("{:.*}", precision, f)
}

/// 将 f64 格式化为科学计数法表示 (对应 `format!("{:.*e}", precision, f)`)。
///
/// size_optimized 模式: 用 `libc::snprintf("%.Ne", f)` (precision 拼接到格式字符串)
/// 默认模式: 用 `format!("{:.*e}", precision, f)`
#[cfg(size_optimized)]
pub fn f64_to_string_exp(f: f64, precision: usize, uppercase: bool) -> String {
    // 手动拼接 "%.Ne\0" / "%.NE\0" 避免用 format! (会引入 fmt 代码).
    let mut fmt = [0u8; 8];
    fmt[0] = b'%';
    fmt[1] = b'.';
    let plen = write_precision(&mut fmt, 2, precision);
    fmt[2 + plen] = if uppercase { b'E' } else { b'e' };
    fmt[2 + plen + 1] = 0; // NUL 终止

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

#[cfg(not(size_optimized))]
#[cfg_attr(not(size_optimized), inline)]
pub fn f64_to_string_exp(f: f64, precision: usize, uppercase: bool) -> String {
    if uppercase {
        format!("{:.*E}", precision, f)
    } else {
        format!("{:.*e}", precision, f)
    }
}

// ============================================================================
// str -> f64
// ============================================================================

/// 将字符串解析为 f64。
///
/// size_optimized 模式: 用 `libc::strtod` (对应 C Lua 的 lua_str2number)
/// 默认模式: 用 `str::parse::<f64>()` (Rust dec2flt)
///
/// 注意: `strtod` 会解析前缀有效部分, 忽略尾部无效字符 (如 "1e" 解析为 1.0).
/// 为匹配 Lua `tonumber` 语义 (整个字符串必须是有效数字), 需检查 `end` 是否
/// 指向字符串末尾 (跳过尾随空格).
#[cfg(size_optimized)]
pub fn f64_from_str(s: &str) -> Option<f64> {
    // strtod 内部会跳过前导空格, 但 Lua 的 tonumber 也允许前导/尾随空格.
    // 先去除尾随空格, 便于检查 end 是否到达字符串末尾.
    let trimmed = s.trim_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.is_empty() {
        return None;
    }
    let c_str = CString::new(trimmed).ok()?;
    let mut end: *mut libc::c_char = std::ptr::null_mut();
    unsafe {
        let val = libc::strtod(c_str.as_ptr(), &mut end);
        if end as *const libc::c_char == c_str.as_ptr() {
            // 没有解析到任何字符
            return None;
        }
        // 检查是否整个字符串都被消费 (end 指向 NUL 终止符).
        // strtod 会解析 "1e" 为 1.0 并把 end 指向 'e', 这种情况应拒绝.
        if *end != 0 {
            return None;
        }
        Some(val)
    }
}

#[cfg(not(size_optimized))]
#[cfg_attr(not(size_optimized), inline)]
pub fn f64_from_str(s: &str) -> Option<f64> {
    s.parse::<f64>().ok()
}

// ============================================================================
// i64 -> String
// ============================================================================

/// 将 i64 格式化为字符串。
///
/// size_optimized 模式: 用 `libc::snprintf("%lld", i)` 避免 Rust `core::fmt` 代码
/// 默认模式: 用 `i.to_string()`
#[cfg(size_optimized)]
pub fn i64_to_string(i: i64) -> String {
    // i64 最大 20 位数字 (-9223372036854775808), 21 字节缓冲区足够 (含符号+NUL)
    let mut buf = [0u8; 24];
    let fmt = b"%lld\0";
    unsafe {
        let n = libc::snprintf(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            fmt.as_ptr() as *const libc::c_char,
            i,
        );
        if n > 0 && (n as usize) < buf.len() {
            std::str::from_utf8_unchecked(&buf[..n as usize]).to_string()
        } else {
            "0".to_string()
        }
    }
}

#[cfg(not(size_optimized))]
#[cfg_attr(not(size_optimized), inline)]
pub fn i64_to_string(i: i64) -> String {
    i.to_string()
}

/// 计算 i64 格式化后的字符串长度 (不分配内存)。
///
/// size_optimized 模式: 用数学计算避免 `to_string().len()` 分配
/// 默认模式: 用 `to_string().len()`
#[cfg(size_optimized)]
#[cfg_attr(not(size_optimized), inline)]
pub fn i64_str_len(i: i64) -> usize {
    if i == 0 {
        return 1;
    }
    let abs = if i < 0 {
        // i64::MIN 的绝对值无法用 i64 表示, 用 u64
        (i as u64).wrapping_neg() as u64
    } else {
        i as u64
    };
    // 计算位数: log10(abs) + 1
    let mut digits = 1u32;
    let mut v = abs;
    while v >= 10 {
        v /= 10;
        digits += 1;
    }
    digits as usize + if i < 0 { 1 } else { 0 }
}

#[cfg(not(size_optimized))]
#[cfg_attr(not(size_optimized), inline)]
pub fn i64_str_len(i: i64) -> usize {
    if i == 0 {
        return 1;
    }
    if i < 0 {
        (i as i128).unsigned_abs().to_string().len() + 1
    } else {
        (i as u64).to_string().len()
    }
}
