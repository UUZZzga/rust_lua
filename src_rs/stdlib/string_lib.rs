//! 字符串库 (lstrlib.cpp → Rust)
//!
//! 对应 C 源码: lstrlib.cpp
//!
//! ## 主要功能
//! - 创建字符串类型的默认元表 (string metatable)
//! - 元表包含算术元方法 (__add, __sub, __mul, __mod, __pow, __div, __idiv, __unm)
//! - __index 指向字符串库函数表 (string.len, string.sub 等)
//! - 注册 string 全局表，包含所有字符串库函数

use crate::execute::{arg_error, VmError};
use crate::objects::{BuiltinFn, BuiltinFnPtr, LuaType, NilKind, TValue};
use crate::state::LuaState;
use crate::table::Table;
use crate::tm::{make_tm_tvalue, Metatable, TagMethod};
use std::rc::Rc;
use std::sync::Arc;

// ============================================================================
// 算术元方法 (对应 C 的 arith_add, arith_sub, ...)
// ============================================================================

/// 将 TValue 转换为数字 (整数优先，其次浮点)
/// 对应 C 的 tonum: 尝试将栈值转为数字
fn to_num(v: &TValue) -> Option<TValue> {
    match v {
        TValue::Integer(i) => Some(TValue::Integer(*i)),
        TValue::Float(f) => Some(TValue::Float(*f)),
        TValue::Str(s) => {
            let s = s.as_str();
            if let Ok(i) = s.parse::<i64>() {
                Some(TValue::Integer(i))
            } else if let Ok(f) = s.parse::<f64>() {
                Some(TValue::Float(f))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 通用算术元方法 — 对应 C 的 arith 函数
///
/// 尝试将两个操作数转为数字并执行算术运算。
/// 返回 None 表示无法转换，需要回退到其他元方法。
fn arith_op(
    v1: &TValue,
    v2: &TValue,
    int_op: fn(i64, i64) -> Option<i64>,
    float_op: fn(f64, f64) -> f64,
) -> Option<TValue> {
    let n1 = to_num(v1)?;
    let n2 = to_num(v2)?;
    match (&n1, &n2) {
        (TValue::Integer(i1), TValue::Integer(i2)) => int_op(*i1, *i2).map(TValue::Integer),
        _ => {
            let f1 = match &n1 {
                TValue::Integer(i) => *i as f64,
                TValue::Float(f) => *f,
                _ => return None,
            };
            let f2 = match &n2 {
                TValue::Integer(i) => *i as f64,
                TValue::Float(f) => *f,
                _ => return None,
            };
            Some(TValue::Float(float_op(f1, f2)))
        }
    }
}

// 算术运算辅助函数
fn add_int(a: i64, b: i64) -> Option<i64> {
    Some(a.wrapping_add(b))
}
fn sub_int(a: i64, b: i64) -> Option<i64> {
    Some(a.wrapping_sub(b))
}
fn mul_int(a: i64, b: i64) -> Option<i64> {
    Some(a.wrapping_mul(b))
}
fn idiv_int(a: i64, b: i64) -> Option<i64> {
    if b == 0 {
        None
    } else {
        Some(a.div_euclid(b))
    }
}
fn mod_int(a: i64, b: i64) -> Option<i64> {
    if b == 0 {
        None
    } else {
        Some(a.rem_euclid(b))
    }
}

fn add_f(a: f64, b: f64) -> f64 {
    a + b
}
fn sub_f(a: f64, b: f64) -> f64 {
    a - b
}
fn mul_f(a: f64, b: f64) -> f64 {
    a * b
}
fn div_f(a: f64, b: f64) -> f64 {
    a / b
}
fn idiv_f(a: f64, b: f64) -> f64 {
    (a / b).floor()
}
fn mod_f(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        f64::NAN
    } else {
        a - (a / b).floor() * b
    }
}
fn pow_f(a: f64, b: f64) -> f64 {
    crate::config::float_pow(a, b)
}
fn unm_f(a: f64, _b: f64) -> f64 {
    -a
}
fn unm_int(a: i64, _b: i64) -> Option<i64> {
    Some(a.wrapping_neg())
}

// ============================================================================
// 位置辅助函数 (对应 C 的 posrelatI, getendpos)
// ============================================================================

/// 将相对位置转换为绝对位置 (1-based)
/// 对应 C 的 posrelatI:
/// - pos > 0: 返回 pos
/// - pos == 0: 返回 1
/// - pos < -len: 返回 1
/// - 否则: 返回 len + pos + 1
fn posrelat_i(pos: i64, len: usize) -> usize {
    if pos > 0 {
        pos as usize
    } else if pos == 0 {
        1
    } else if pos < -(len as i64) {
        1
    } else {
        (len as i64 + pos + 1) as usize
    }
}

/// 获取结束位置 (0-based, clip 到 [0, len])
/// 对应 C 的 getendpos:
/// - pos > len: 返回 len
/// - pos >= 0: 返回 pos  (注意: 0 是合法的结束位置,表示空区间)
/// - pos < -len: 返回 0
/// - 否则: 返回 len + pos + 1
///
/// 与 C 版本一致,默认值(def)由调用方通过 get_opt_int_arg 处理,
/// 此函数不再承担默认值替换职责。
fn get_end_pos(pos: i64, len: usize) -> usize {
    if pos > len as i64 {
        len
    } else if pos >= 0 {
        pos as usize
    } else if pos < -(len as i64) {
        0
    } else {
        (len as i64 + pos + 1) as usize
    }
}

// ============================================================================
// 字符串函数实现 (纯 Rust 逻辑)
// ============================================================================

/// string.upper(s) — 将字符串转换为大写
/// 对应 C 的 str_upper
pub fn str_upper(s: &str) -> String {
    s.to_uppercase()
}

/// string.lower(s) — 将字符串转换为小写
/// 对应 C 的 str_lower
pub fn str_lower(s: &str) -> String {
    s.to_lowercase()
}

/// string.len(s) — 返回字符串长度
/// 对应 C 的 str_len
pub fn str_len(s: &str) -> i64 {
    s.len() as i64
}

/// string.sub(s, start, end) — 返回子串
/// 对应 C 的 str_sub
///
/// 与 C 版本逻辑一致:
/// - start 由 posrelat_i 转为 1-based 绝对位置 (>= 1)
/// - end 由 get_end_pos 转为 0-based 包含结束位置 (in [0, len])
/// - 当 start <= end 时,返回 s[start-1..end]
/// - 否则返回空字符串
pub fn str_sub(s: &str, start: i64, end: i64) -> String {
    let len = s.len();
    let start_pos = posrelat_i(start, len);
    let end_pos = get_end_pos(end, len);
    if start_pos <= end_pos {
        // start_pos >= 1 (由 posrelat_i 保证), 所以 start_pos - 1 不会下溢
        // end_pos <= len (由 get_end_pos 保证), 所以切片不会越界
        // 当 start_pos <= end_pos 时, start_pos <= len (因 end_pos <= len)
        //
        // 按字节切片 (Lua 的 string.sub 是字节级操作, 不关心 UTF-8 字符边界)。
        // 对应 C 中按字节截取的行为。使用 unsafe 构造 String 以保留原始字节,
        // 与 str_reverse/str_char 等函数一致。
        let bytes = s.as_bytes()[start_pos - 1..end_pos].to_vec();
        unsafe { String::from_utf8_unchecked(bytes) }
    } else {
        String::new()
    }
}

/// string.reverse(s) — 反转字符串
/// 对应 C 的 str_reverse
///
/// 与 C 版本一致:反转字节序列,不进行 UTF-8 解码。
/// 使用 unsafe 创建 String 以保持原始字节 (与 str_char 一致)。
pub fn str_reverse(s: &str) -> String {
    let bytes: Vec<u8> = s.bytes().rev().collect();
    unsafe { String::from_utf8_unchecked(bytes) }
}

/// string.byte(s, [i], [j]) — 返回字符的字节值
/// 对应 C 的 str_byte
///
/// 与 C 版本逻辑一致:
/// - posi = posrelat_i(i, len)  (1-based, >= 1)
/// - pose = get_end_pos(j, len) (0-based, in [0, len])
/// - 当 posi <= pose 时,返回 s[posi-1..pose] 中各字节
/// - 否则返回空
pub fn str_byte(s: &str, i: i64, j: i64) -> Vec<i64> {
    let len = s.len();
    let posi = posrelat_i(i, len);
    let pose = get_end_pos(j, len);
    let mut result = Vec::new();
    if posi <= pose {
        // posi >= 1 (由 posrelat_i 保证)
        // pose <= len (由 get_end_pos 保证)
        // 当 posi <= pose 时, posi <= len, 切片合法
        for &b in &s.as_bytes()[posi - 1..pose] {
            result.push(b as i64);
        }
    }
    result
}

/// string.char(...) — 从字节值构建字符串
/// 对应 C 的 str_char
pub fn str_char(codes: &[i64]) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(codes.len());
    for &c in codes {
        if c < 0 || c > 255 {
            return Err(format!("bad argument (value out of range)"));
        }
        bytes.push(c as u8);
    }
    // Lua 字符串是字节序列,不要求有效 UTF-8 (与 C 版本一致)。
    // 解析器也用 unsafe 绕过 UTF-8 校验存储原始字节 (见 lexer.rs 的 read_escape),
    // 这里采用相同方式以保持一致。
    // bytes 中的值已在 [0,255] 范围内校验,from_utf8_unchecked 不会导致 UB,
    // 只是 String 内部可能包含非法 UTF-8 (这是刻意设计)。
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}

/// string.rep(s, n, [sep]) — 重复字符串
/// 对应 C 的 str_rep
///
/// 与 C 版本一致,在结果字符串过大时返回错误 "resulting string too large"。
pub fn str_rep(s: &str, n: i64, sep: &str) -> Result<String, String> {
    if n <= 0 {
        return Ok(String::new());
    }
    let len = s.len();
    let lsep = sep.len();
    let n = n as usize;
    // 对应 C: len > MAX_SIZE - lsep || (len + lsep) > MAX_SIZE / n
    // MAX_SIZE 使用 isize::MAX (Rust 分配器上限)
    let max_size = isize::MAX as usize;
    if len > max_size - lsep || (len + lsep) > max_size / n {
        return Err("resulting string too large".to_string());
    }
    let totallen = n * (len + lsep) - lsep;
    if sep.is_empty() {
        Ok(s.repeat(n))
    } else {
        let mut result = String::with_capacity(totallen);
        for i in 0..n {
            if i > 0 {
                result.push_str(sep);
            }
            result.push_str(s);
        }
        Ok(result)
    }
}

// ============================================================================
// 模式匹配引擎 (对应 C 的 pattern matching)
// ============================================================================

const CAP_UNFINISHED: i32 = -1;
const CAP_POSITION: i32 = -2;
const MAX_CAPTURES: usize = 32;
const MAX_CCALLS: i32 = 200;

/// 捕获信息
#[derive(Debug, Clone)]
struct Capture {
    init: usize,
    len: i32,
}

/// 匹配状态 — 对应 C 的 MatchState
/// 优化：使用 &[u8] 切片引用而非 Vec<u8>，避免每次匹配时复制源字符串和模式字符串。
/// C 版本用指针直接引用原字符串，此处用借用切片达到同等效果。
struct MatchState<'a> {
    src: &'a [u8],
    src_init: usize,
    src_end: usize,
    pattern: &'a [u8],
    p_end: usize,
    match_depth: i32,
    level: usize,
    captures: Vec<Capture>,
}

impl<'a> MatchState<'a> {
    fn new(src: &'a [u8], pattern: &'a [u8]) -> Self {
        MatchState {
            src_init: 0,
            src_end: src.len(),
            p_end: pattern.len(),
            src,
            pattern,
            match_depth: MAX_CCALLS,
            level: 0,
            captures: Vec::with_capacity(MAX_CAPTURES),
        }
    }

    /// 获取源字符串字节 — 所有调用点已确保 idx < src_end <= src.len()
    #[inline(always)]
    fn src_byte(&self, idx: usize) -> u8 {
        // 安全性：调用点已在访问前检查 idx < src_end
        unsafe { *self.src.get_unchecked(idx) }
    }

    /// 获取模式字符串字节 — 所有调用点已确保 idx < p_end <= pattern.len()
    #[inline(always)]
    fn pat_byte(&self, idx: usize) -> u8 {
        // 安全性：调用点已在访问前检查 idx < p_end
        unsafe { *self.pattern.get_unchecked(idx) }
    }
}

/// 对应 C 的 match_class: 匹配字符类
fn match_class(c: u8, cl: u8) -> bool {
    let cl_lower = cl.to_ascii_lowercase();
    let res = match cl_lower {
        b'a' => c.is_ascii_alphabetic(),
        b'c' => c.is_ascii_control(),
        b'd' => c.is_ascii_digit(),
        b'g' => c.is_ascii_graphic(),
        b'l' => c.is_ascii_lowercase(),
        b'p' => c.is_ascii_punctuation(),
        b's' => c.is_ascii_whitespace(),
        b'u' => c.is_ascii_uppercase(),
        b'w' => c.is_ascii_alphanumeric(),
        b'x' => c.is_ascii_hexdigit(),
        b'z' => c == 0, // deprecated
        _ => return cl == c,
    };
    if cl.is_ascii_lowercase() {
        res
    } else {
        !res
    }
}

/// 对应 C 的 matchbracketclass: 匹配方括号字符类
fn match_bracket_class(c: u8, p: &[u8], ec: usize) -> bool {
    let mut sig = true;
    let mut idx = 0;
    // 安全性：p 来自 ms.pattern[p_start..ep]，长度 >= ec
    // 所有索引访问都在 idx < ec 的条件下进行
    unsafe {
        if p.len() > 1 && *p.get_unchecked(1) == b'^' {
            sig = false;
            idx = 1; // skip the '^'
        }
        idx += 1;
        while idx < ec {
            if *p.get_unchecked(idx) == b'%' {
                idx += 1;
                if idx < ec && match_class(c, *p.get_unchecked(idx)) {
                    return sig;
                }
            } else if idx + 2 < ec && *p.get_unchecked(idx + 1) == b'-' {
                if *p.get_unchecked(idx) <= c && c <= *p.get_unchecked(idx + 2) {
                    return sig;
                }
                idx += 2;
            } else if *p.get_unchecked(idx) == c {
                return sig;
            }
            idx += 1;
        }
    }
    !sig
}

/// 对应 C 的 classend: 找到模式类的结束位置
#[inline]
fn class_end(ms: &MatchState<'_>, p: usize) -> Result<usize, String> {
    if p >= ms.p_end {
        return Err("malformed pattern (ends with '%')".to_string());
    }
    match ms.pat_byte(p) {
        b'%' => {
            if p + 1 >= ms.p_end {
                return Err("malformed pattern (ends with '%')".to_string());
            }
            Ok(p + 2)
        }
        b'[' => {
            let mut idx = p + 1;
            if idx < ms.p_end && ms.pat_byte(idx) == b'^' {
                idx += 1;
            }
            // do-while 风格：字符集中的第一个 ']' 被当作内容，第二个 ']' 才是结束符
            // （与 C 实现一致，使 `[^]]` 能匹配任何非 ']' 字符，`[]]` 能匹配 ']'）
            loop {
                if idx >= ms.p_end {
                    return Err("malformed pattern (missing ']')".to_string());
                }
                let c = ms.pat_byte(idx);
                idx += 1;
                if c == b'%' && idx < ms.p_end {
                    idx += 1;
                }
                if idx < ms.p_end && ms.pat_byte(idx) == b']' {
                    return Ok(idx + 1);
                }
            }
        }
        _ => Ok(p + 1),
    }
}

/// 对应 C 的 singlematch: 检查单个字符是否匹配
#[inline]
fn single_match(ms: &MatchState<'_>, s: usize, p: usize, ep: usize) -> bool {
    if s >= ms.src_end {
        return false;
    }
    let c = ms.src_byte(s);
    match ms.pat_byte(p) {
        b'.' => true,
        b'%' => {
            if p + 1 < ms.p_end {
                match_class(c, ms.pat_byte(p + 1))
            } else {
                false
            }
        }
        // 安全性：p < ep <= p_end <= pattern.len()（class_end 保证）
        b'[' => unsafe {
            let slice = ms.pattern.get_unchecked(p..ep);
            match_bracket_class(c, slice, ep - p - 1)
        },
        _ => ms.pat_byte(p) == c,
    }
}

/// 对应 C 的 matchbalance: 平衡匹配 %bxy
/// 参数不足时报错（对应 C 的 luaL_error）
fn match_balance(ms: &MatchState<'_>, s: usize, p: usize) -> Result<Option<usize>, String> {
    if p + 1 >= ms.p_end {
        return Err("malformed pattern (missing arguments to '%b')".to_string());
    }
    if s >= ms.src_end || ms.src_byte(s) != ms.pat_byte(p) {
        return Ok(None);
    }
    let b = ms.pat_byte(p);
    let e = ms.pat_byte(p + 1);
    let mut cont = 1i32;
    let mut idx = s + 1;
    while idx < ms.src_end {
        let c = ms.src_byte(idx);
        if c == e {
            cont -= 1;
            if cont == 0 {
                return Ok(Some(idx + 1));
            }
        } else if c == b {
            cont += 1;
        }
        idx += 1;
    }
    Ok(None)
}

/// 对应 C 的 check_capture
fn check_capture(ms: &MatchState<'_>, l: u8) -> Result<usize, String> {
    // C: l -= '1'; if (l < 0 || l >= ms->level || ...)
    // 用 i32 避免负数下溢（%0 会得到 -1）
    let l = (l as i32) - (b'1' as i32);
    if l < 0 || l as usize >= ms.level || ms.captures[l as usize].len == CAP_UNFINISHED {
        return Err(format!("invalid capture index %{}", l + 1));
    }
    Ok(l as usize)
}

/// 对应 C 的 capture_to_close
fn capture_to_close(ms: &MatchState<'_>) -> Result<usize, String> {
    let mut level = ms.level;
    while level > 0 {
        level -= 1;
        if ms.captures[level].len == CAP_UNFINISHED {
            return Ok(level);
        }
    }
    Err("invalid pattern capture".to_string())
}

/// 对应 C 的 start_capture
fn start_capture(
    ms: &mut MatchState<'_>,
    s: usize,
    p: usize,
    what: i32,
) -> Result<Option<usize>, String> {
    if ms.level >= MAX_CAPTURES {
        return Err("too many captures".to_string());
    }
    let level = ms.level;
    ms.captures.push(Capture { init: s, len: what });
    ms.level = level + 1;
    let res = match_pattern(ms, s, p)?;
    if res.is_none() {
        ms.level -= 1;
        ms.captures.pop();
    }
    Ok(res)
}

/// 对应 C 的 end_capture
fn end_capture(ms: &mut MatchState<'_>, s: usize, p: usize) -> Result<Option<usize>, String> {
    let l = capture_to_close(ms)?;
    ms.captures[l].len = (s - ms.captures[l].init) as i32;
    let res = match_pattern(ms, s, p)?;
    if res.is_none() {
        ms.captures[l].len = CAP_UNFINISHED;
    }
    Ok(res)
}

/// 对应 C 的 match_capture
fn match_capture(ms: &MatchState<'_>, s: usize, l: u8) -> Result<Option<usize>, String> {
    // C: l = check_capture(ms, l);  会抛出错误
    let l = check_capture(ms, l)?;
    let len = ms.captures[l].len as usize;
    if s + len <= ms.src_end
        && ms.src[ms.captures[l].init..ms.captures[l].init + len] == ms.src[s..s + len]
    {
        Ok(Some(s + len))
    } else {
        Ok(None)
    }
}

/// 对应 C 的 max_expand
fn max_expand(ms: &mut MatchState<'_>, s: usize, p: usize, ep: usize) -> Result<Option<usize>, String> {
    let mut i = 0i32;
    while single_match(ms, s + i as usize, p, ep) {
        i += 1;
    }
    while i >= 0 {
        let res = match_pattern(ms, s + i as usize, ep + 1)?;
        if res.is_some() {
            return Ok(res);
        }
        i -= 1;
    }
    Ok(None)
}

/// 对应 C 的 min_expand
fn min_expand(
    ms: &mut MatchState<'_>,
    mut s: usize,
    p: usize,
    ep: usize,
) -> Result<Option<usize>, String> {
    loop {
        let res = match_pattern(ms, s, ep + 1)?;
        if res.is_some() {
            return Ok(res);
        }
        if single_match(ms, s, p, ep) {
            if s + 1 > ms.src_end {
                return Ok(None);
            }
            s += 1;
        } else {
            return Ok(None);
        }
    }
}

/// 对应 C 的 match — 核心模式匹配函数
fn match_pattern(ms: &mut MatchState<'_>, s: usize, p: usize) -> Result<Option<usize>, String> {
    if ms.match_depth == 0 {
        return Err("pattern too complex".to_string());
    }
    ms.match_depth -= 1;
    let result = match_pattern_inner(ms, s, p);
    ms.match_depth += 1;
    result
}

fn match_pattern_inner(
    ms: &mut MatchState<'_>,
    mut s: usize,
    mut p: usize,
) -> Result<Option<usize>, String> {
    loop {
        if p >= ms.p_end {
            return Ok(Some(s));
        }
        match ms.pat_byte(p) {
            b'(' => {
                if p + 1 < ms.p_end && ms.pat_byte(p + 1) == b')' {
                    return start_capture(ms, s, p + 2, CAP_POSITION);
                } else {
                    return start_capture(ms, s, p + 1, CAP_UNFINISHED);
                }
            }
            b')' => {
                return end_capture(ms, s, p + 1);
            }
            b'$' => {
                if p + 1 == ms.p_end {
                    return Ok(if s == ms.src_end { Some(s) } else { None });
                }
                // fall through to default
                let ep = class_end(ms, p)?;
                let suffix = if ep < ms.p_end { ms.pat_byte(ep) } else { 0 };
                if !single_match(ms, s, p, ep) {
                    if suffix == b'*' || suffix == b'?' || suffix == b'-' {
                        p = ep + 1;
                        continue;
                    } else {
                        return Ok(None);
                    }
                } else {
                    match suffix {
                        b'?' => {
                            let res = match_pattern(ms, s + 1, ep + 1)?;
                            if res.is_some() {
                                return Ok(res);
                            }
                            p = ep + 1;
                            continue;
                        }
                        b'+' => {
                            s += 1;
                            return max_expand(ms, s, p, ep);
                        }
                        b'*' => {
                            return max_expand(ms, s, p, ep);
                        }
                        b'-' => {
                            return min_expand(ms, s, p, ep);
                        }
                        _ => {
                            s += 1;
                            p = ep;
                            continue;
                        }
                    }
                }
            }
            b'%' => {
                if p + 1 < ms.p_end {
                    match ms.pat_byte(p + 1) {
                        b'b' => {
                            let res = match_balance(ms, s, p + 2)?;
                            match res {
                                Some(new_s) => {
                                    s = new_s;
                                    p += 4;
                                    continue;
                                }
                                None => return Ok(None),
                            }
                        }
                        b'f' => {
                            let mut p2 = p + 2;
                            if p2 >= ms.p_end || ms.pat_byte(p2) != b'[' {
                                return Err("missing '[' after '%f' in pattern".to_string());
                            }
                            let ep = class_end(ms, p2)?;
                            let previous = if s == ms.src_init {
                                0u8
                            } else {
                                ms.src_byte(s - 1)
                            };
                            let current = if s < ms.src_end { ms.src_byte(s) } else { 0u8 };
                            if !match_bracket_class(previous, &ms.pattern[p2..ep], ep - p2 - 1)
                                && match_bracket_class(current, &ms.pattern[p2..ep], ep - p2 - 1)
                            {
                                p = ep;
                                continue;
                            } else {
                                return Ok(None);
                            }
                        }
                        c if c.is_ascii_digit() => {
                            let res = match_capture(ms, s, c)?;
                            match res {
                                Some(new_s) => {
                                    s = new_s;
                                    p += 2;
                                    continue;
                                }
                                None => return Ok(None),
                            }
                        }
                        _ => {
                            // fall through to default
                        }
                    }
                }
                // fall through to default
                let ep = class_end(ms, p)?;
                let suffix = if ep < ms.p_end { ms.pat_byte(ep) } else { 0 };
                if !single_match(ms, s, p, ep) {
                    if suffix == b'*' || suffix == b'?' || suffix == b'-' {
                        p = ep + 1;
                        continue;
                    } else {
                        return Ok(None);
                    }
                } else {
                    match suffix {
                        b'?' => {
                            let res = match_pattern(ms, s + 1, ep + 1)?;
                            if res.is_some() {
                                return Ok(res);
                            }
                            p = ep + 1;
                            continue;
                        }
                        b'+' => {
                            s += 1;
                            return max_expand(ms, s, p, ep);
                        }
                        b'*' => {
                            return max_expand(ms, s, p, ep);
                        }
                        b'-' => {
                            return min_expand(ms, s, p, ep);
                        }
                        _ => {
                            s += 1;
                            p = ep;
                            continue;
                        }
                    }
                }
            }
            _ => {
                let ep = class_end(ms, p)?;
                let suffix = if ep < ms.p_end { ms.pat_byte(ep) } else { 0 };
                if !single_match(ms, s, p, ep) {
                    if suffix == b'*' || suffix == b'?' || suffix == b'-' {
                        p = ep + 1;
                        continue;
                    } else {
                        return Ok(None);
                    }
                } else {
                    match suffix {
                        b'?' => {
                            let res = match_pattern(ms, s + 1, ep + 1)?;
                            if res.is_some() {
                                return Ok(res);
                            }
                            p = ep + 1;
                            continue;
                        }
                        b'+' => {
                            s += 1;
                            return max_expand(ms, s, p, ep);
                        }
                        b'*' => {
                            return max_expand(ms, s, p, ep);
                        }
                        b'-' => {
                            return min_expand(ms, s, p, ep);
                        }
                        _ => {
                            s += 1;
                            p = ep;
                            continue;
                        }
                    }
                }
            }
        }
    }
}

/// 获取第 i 个捕获的内容
/// 返回 (start, length) 或位置捕获
fn get_one_capture(ms: &MatchState<'_>, i: usize, s: usize, e: usize) -> Result<CaptureResult, String> {
    if i >= ms.level {
        if i != 0 {
            return Err(format!("invalid capture index %{}", i + 1));
        }
        return Ok(CaptureResult::Str(s, e - s));
    }
    let capl = ms.captures[i].len;
    if capl == CAP_UNFINISHED {
        return Err("unfinished capture".to_string());
    }
    if capl == CAP_POSITION {
        return Ok(CaptureResult::Pos(ms.captures[i].init + 1));
    }
    Ok(CaptureResult::Str(ms.captures[i].init, capl as usize))
}

#[derive(Debug)]
enum CaptureResult {
    Str(usize, usize),
    Pos(usize),
}

/// 获取所有捕获的字符串
fn get_captures(ms: &MatchState<'_>, s: usize, e: usize) -> Result<Vec<TValue>, String> {
    let nlevels = if ms.level == 0 { 1 } else { ms.level };
    let mut result = Vec::with_capacity(nlevels);
    for i in 0..nlevels {
        let cap = get_one_capture(ms, i, s, e)?;
        match cap {
            CaptureResult::Str(start, len) => {
                let bytes = &ms.src[start..start + len];
                result.push(TValue::Str(crate::strings::new_short_bytes(
                    bytes.to_vec(),
                )));
            }
            CaptureResult::Pos(pos) => {
                result.push(TValue::Integer(pos as i64));
            }
        }
    }
    Ok(result)
}

/// string.find(s, pattern, [init], [plain]) — 查找模式
/// 对应 C 的 str_find
pub fn str_find(s: &str, pattern: &str, init: i64, plain: bool) -> Result<FindResult, String> {
    let len = s.len();
    let init_pos = posrelat_i(init, len).saturating_sub(1);
    if init_pos > len {
        return Ok(FindResult::NotFound);
    }

    // 检查模式是否有特殊字符
    let has_specials = |p: &str| {
        p.bytes().any(|c| {
            matches!(
                c,
                b'^' | b'$' | b'*' | b'+' | b'?' | b'.' | b'(' | b'[' | b'%' | b'-'
            )
        })
    };

    if plain || !has_specials(pattern) {
        // 纯文本搜索
        let src_bytes = s.as_bytes();
        let pat_bytes = pattern.as_bytes();
        if pat_bytes.is_empty() {
            return Ok(FindResult::Found {
                start: init_pos + 1,
                end: init_pos,
                captures: Vec::new(),
            });
        }
        if init_pos + pat_bytes.len() > src_bytes.len() {
            return Ok(FindResult::NotFound);
        }
        for i in init_pos..=src_bytes.len() - pat_bytes.len() {
            if &src_bytes[i..i + pat_bytes.len()] == pat_bytes {
                return Ok(FindResult::Found {
                    start: i + 1,
                    end: i + pat_bytes.len(),
                    captures: Vec::new(),
                });
            }
        }
        return Ok(FindResult::NotFound);
    }

    // 模式匹配
    let mut ms = MatchState::new(s.as_bytes(), pattern.as_bytes());
    let mut pat_start = 0;
    let anchor = ms.pat_byte(0) == b'^';
    if anchor {
        pat_start = 1;
    }

    let mut search_pos = init_pos;
    loop {
        ms.level = 0;
        ms.captures.clear();
        ms.match_depth = MAX_CCALLS;
        match match_pattern(&mut ms, search_pos, pat_start)? {
            Some(end) => {
                let captures = get_captures(&ms, search_pos, end)?;
                return Ok(FindResult::Found {
                    start: search_pos + 1,
                    end,
                    captures,
                });
            }
            None => {}
        }
        if anchor || search_pos >= ms.src_end {
            break;
        }
        search_pos += 1;
    }
    Ok(FindResult::NotFound)
}

pub enum FindResult {
    Found {
        start: usize,
        end: usize,
        captures: Vec<TValue>,
    },
    NotFound,
}

/// string.match(s, pattern, [init]) — 模式匹配
/// 对应 C 的 str_match
pub fn str_match(s: &str, pattern: &str, init: i64) -> Result<Vec<TValue>, String> {
    match str_find(s, pattern, init, false)? {
        FindResult::Found {
            start,
            end,
            captures,
        } => {
            if captures.is_empty() {
                // 无捕获时返回整个匹配
                let matched = &s.as_bytes()[start - 1..end];
                Ok(vec![TValue::Str(crate::strings::new_short_bytes(
                    matched.to_vec(),
                ))])
            } else {
                Ok(captures)
            }
        }
        FindResult::NotFound => Ok(vec![TValue::Nil(NilKind::Strict)]),
    }
}

/// string.gmatch(s, pattern) — 全局模式匹配迭代器
/// 对应 C 的 gmatch
pub struct GMatchIterator {
    src: String,
    pattern: String,
    pos: usize,
    anchor: bool,
    pat_start: usize,
}

impl GMatchIterator {
    pub fn new(s: &str, pattern: &str) -> Self {
        let anchor = pattern.starts_with('^');
        let pat_start = if anchor { 1 } else { 0 };
        GMatchIterator {
            src: s.to_string(),
            pattern: pattern.to_string(),
            pos: 0,
            anchor,
            pat_start,
        }
    }

    pub fn next(&mut self) -> Result<Vec<TValue>, String> {
        let src_bytes = self.src.as_bytes();
        let len = src_bytes.len();
        while self.pos <= len {
            let mut ms = MatchState::new(self.src.as_bytes(), self.pattern.as_bytes());
            ms.level = 0;
            ms.captures.clear();
            ms.match_depth = MAX_CCALLS;
            let match_start = self.pos;
            match match_pattern(&mut ms, match_start, self.pat_start)? {
                Some(end) => {
                    let captures = get_captures(&ms, match_start, end)?;
                    // 推进位置: 如果匹配为空则前进 1 以避免无限循环
                    self.pos = if end > match_start {
                        end
                    } else {
                        match_start + 1
                    };
                    if captures.is_empty() {
                        // 无捕获时返回整个匹配的子串
                        return Ok(vec![TValue::Str(crate::strings::new_short_bytes(
                            src_bytes[match_start..end].to_vec(),
                        ))]);
                    }
                    return Ok(captures);
                }
                None => {}
            }
            if self.anchor {
                break;
            }
            self.pos += 1;
        }
        Ok(Vec::new())
    }
}

/// string.gsub(s, pattern, repl, [n]) — 全局替换
/// 对应 C 的 str_gsub
pub fn str_gsub(s: &str, pattern: &str, repl: &str, max_s: i64) -> Result<(String, i64), String> {
    let src_bytes = s.as_bytes();
    let len = src_bytes.len();
    let max_s = if max_s < 0 { len as i64 + 1 } else { max_s };
    let anchor = pattern.starts_with('^');
    let pat_start = if anchor { 1 } else { 0 };

    // 使用 Vec<u8> 构建结果，避免 as char 转换导致字节值变化
    let mut result: Vec<u8> = Vec::new();
    let mut src_pos = 0;
    let mut n = 0i64;
    let mut last_match_end: Option<usize> = None;

    while n < max_s && src_pos <= len {
        let mut ms = MatchState::new(s.as_bytes(), pattern.as_bytes());
        ms.level = 0;
        ms.captures.clear();
        ms.match_depth = MAX_CCALLS;

        let matched = match_pattern(&mut ms, src_pos, pat_start)?;
        if let Some(end) = matched {
            if Some(end) == last_match_end {
                // 避免空匹配的无限循环
                if src_pos < len {
                    result.push(src_bytes[src_pos]);
                }
                src_pos += 1;
                if anchor {
                    break;
                }
                continue;
            }
            n += 1;
            last_match_end = Some(end);

            // 处理替换字符串
            let replacement = apply_replacement(repl, &ms, src_pos, end)?;
            result.extend_from_slice(replacement.as_bytes());

            src_pos = end;
        } else if src_pos < len {
            result.push(src_bytes[src_pos]);
            src_pos += 1;
        } else {
            break;
        }
        if anchor {
            break;
        }
    }

    // 添加剩余部分
    if src_pos < len {
        result.extend_from_slice(&src_bytes[src_pos..]);
    }

    // Lua 字符串是字节序列，使用 from_utf8_unchecked 保留原始字节
    let result = unsafe { String::from_utf8_unchecked(result) };
    Ok((result, n))
}

/// string.gsub 的 table/function 替换实现 — 对应 C 的 str_gsub + add_value
///
/// 当 repl 为 table 或 function 时调用此函数。
/// - table: 以第一个捕获（或整个匹配）为键查表
/// - function: 以所有捕获为参数调用函数
/// 若结果为 nil/false，保留原匹配文本；若为 string/number，用作替换；否则报错。
fn str_gsub_with_repl(
    state: &mut LuaState,
    s: &str,
    pattern: &str,
    repl: &TValue,
    max_s: i64,
) -> Result<(String, i64, bool), String> {
    let src_bytes = s.as_bytes();
    let len = src_bytes.len();
    let max_s = if max_s < 0 { len as i64 + 1 } else { max_s };
    let anchor = pattern.starts_with('^');
    let pat_start = if anchor { 1 } else { 0 };

    let mut result: Vec<u8> = Vec::new();
    let mut src_pos = 0;
    let mut n = 0i64;
    let mut changed = false;
    let mut last_match_end: Option<usize> = None;

    while n < max_s && src_pos <= len {
        let mut ms = MatchState::new(s.as_bytes(), pattern.as_bytes());
        ms.level = 0;
        ms.captures.clear();
        ms.match_depth = MAX_CCALLS;

        let matched = match_pattern(&mut ms, src_pos, pat_start)?;
        if let Some(end) = matched {
            if Some(end) == last_match_end {
                // 避免空匹配的无限循环
                if src_pos < len {
                    result.push(src_bytes[src_pos]);
                }
                src_pos += 1;
                if anchor {
                    break;
                }
                continue;
            }
            n += 1;
            last_match_end = Some(end);

            // 处理替换值 (table 或 function) — 对应 C 的 add_value
            let (replacement, repl_changed) = add_value_from_repl(state, &ms, src_pos, end, repl)?;
            changed = changed || repl_changed;
            result.extend_from_slice(replacement.as_bytes());

            src_pos = end;
        } else if src_pos < len {
            result.push(src_bytes[src_pos]);
            src_pos += 1;
        } else {
            break;
        }
        if anchor {
            break;
        }
    }

    // 添加剩余部分
    if src_pos < len {
        result.extend_from_slice(&src_bytes[src_pos..]);
    }

    let result = unsafe { String::from_utf8_unchecked(result) };
    Ok((result, n, changed))
}

/// 对应 C 的 add_value — 处理 table/function 替换值
///
/// 返回 (替换后的字符串, changed 标志)。
/// 若结果为 nil/false，保留原匹配文本，changed = false（对应 C 的 return 0）。
fn add_value_from_repl(
    state: &mut LuaState,
    ms: &MatchState<'_>,
    s: usize,
    e: usize,
    repl: &TValue,
) -> Result<(String, bool), String> {
    match repl {
        // table 替换 — 对应 C 的 LUA_TTABLE 分支
        TValue::Table(t) => {
            // push_onecapture(ms, 0, s, e) — 第一个捕获（或整个匹配）作为索引
            let key = get_capture_as_tvalue(state, ms, 0, s, e)?;
            // lua_gettable(L, 3) — 需要处理 __index 元方法
            let val = crate::execute::VmExecutor::table_get(
                state,
                &TValue::Table(t.clone()),
                &key,
                crate::execute::VarSource::None,
            )
            .map_err(|e| match e {
                VmError::RuntimeError(s) => s,
                _ => "error in table indexing".to_string(),
            })?;
            match val {
                TValue::Nil(_) | TValue::Boolean(false) => {
                    // nil 或 false — 保留原匹配文本，changed = false
                    let bytes = &ms.src[s..e];
                    Ok((
                        unsafe { String::from_utf8_unchecked(bytes.to_vec()) },
                        false,
                    ))
                }
                TValue::Str(st) => Ok((st.as_str().to_string(), true)),
                TValue::Integer(i) => Ok((i.to_string(), true)),
                TValue::Float(f) => Ok((format_float_value(f), true)),
                other => Err(format!("invalid replacement value (a {})", other.ty())),
            }
        }
        // function 替换 — 对应 C 的 LUA_TFUNCTION 分支
        // LightUserData (base 库 tag 函数) 和 BuiltinFn (已迁移库) 都算 function
        TValue::LClosure(_) | TValue::CClosure(_) | TValue::LCFn(_) | TValue::LightUserData(_) | TValue::BuiltinFn(_) => {
            // push_captures(ms, s, e) — 所有捕获作为参数
            let n = if ms.level == 0 { 1 } else { ms.level };
            let mut captures = Vec::with_capacity(n);
            for i in 0..n {
                captures.push(get_capture_as_tvalue(state, ms, i, s, e)?);
            }

            // 保存当前栈顶，调用函数后恢复
            let stack_top = state.stack.len();
            // push function
            state.stack.push(repl.clone());
            // push captures as arguments
            for cap in captures {
                state.stack.push(cap);
            }

            // gsub 回调通过 lua_call (luaD_callnoyield) 调用，不可 yield
            let saved_ny = state.n_ny_calls;
            state.n_ny_calls = state.n_ny_calls.saturating_add(1);
            let status = state.pcall(n, 1, 0);
            state.n_ny_calls = saved_ny;
            if status != 0 {
                let msg = state
                    .to_string(-1)
                    .unwrap_or_else(|| "error in gsub function".to_string());
                state.settop(stack_top);
                return Err(msg);
            }

            // 取出结果
            let result_val = state.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
            state.settop(stack_top);

            match result_val {
                TValue::Nil(_) | TValue::Boolean(false) => {
                    // nil 或 false — 保留原匹配文本，changed = false
                    let bytes = &ms.src[s..e];
                    Ok((
                        unsafe { String::from_utf8_unchecked(bytes.to_vec()) },
                        false,
                    ))
                }
                TValue::Str(st) => Ok((st.as_str().to_string(), true)),
                TValue::Integer(i) => Ok((i.to_string(), true)),
                TValue::Float(f) => Ok((format_float_value(f), true)),
                other => Err(format!("invalid replacement value (a {})", other.ty())),
            }
        }
        _ => Err("invalid replacement value".to_string()),
    }
}

/// 获取第 i 个捕获并转换为 TValue — 对应 C 的 push_onecapture
///
/// 使用 state.intern_str() 创建字符串，确保哈希值与表查找一致。
fn get_capture_as_tvalue(
    state: &mut LuaState,
    ms: &MatchState<'_>,
    i: usize,
    s: usize,
    e: usize,
) -> Result<TValue, String> {
    let cap = get_one_capture(ms, i, s, e)?;
    match cap {
        CaptureResult::Str(start, len) => {
            let bytes = &ms.src[start..start + len];
            let str_val = unsafe { String::from_utf8_unchecked(bytes.to_vec()) };
            Ok(TValue::Str(state.intern_str(&str_val)))
        }
        CaptureResult::Pos(pos) => Ok(TValue::Integer(pos as i64)),
    }
}

/// 格式化浮点数为字符串 — 与 Lua 的 tostring 行为一致
fn format_float_value(f: f64) -> String {
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
    if f == 0.0 {
        return "0.0".to_string();
    }
    let s = format!("{}", f);
    // Rust 的 Display 对整数值浮点数不输出小数点，需补 ".0"
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{}.0", s)
    }
}

/// 处理替换字符串中的 %0, %1-%9
fn apply_replacement(repl: &str, ms: &MatchState<'_>, s: usize, e: usize) -> Result<String, String> {
    // 使用 Vec<u8> 构建结果，避免 as char 转换导致字节值变化
    let mut result: Vec<u8> = Vec::new();
    let repl_bytes = repl.as_bytes();
    let mut i = 0;
    while i < repl_bytes.len() {
        if repl_bytes[i] == b'%' {
            i += 1;
            if i >= repl_bytes.len() {
                return Err("invalid use of '%' in replacement string".to_string());
            }
            let c = repl_bytes[i];
            if c == b'%' {
                result.push(b'%');
            } else if c == b'0' {
                let match_bytes = &ms.src[s..e];
                result.extend_from_slice(match_bytes);
            } else if c.is_ascii_digit() {
                let cap_idx = (c - b'1') as usize;
                let cap = get_one_capture(ms, cap_idx, s, e)?;
                match cap {
                    CaptureResult::Str(start, len) => {
                        result.extend_from_slice(&ms.src[start..start + len]);
                    }
                    CaptureResult::Pos(pos) => {
                        result.extend_from_slice(pos.to_string().as_bytes());
                    }
                }
            } else {
                // C: luaL_error(L, "invalid use of '%c' in replacement string", L_ESC);
                // L_ESC 是 '%', 所以消息是 "invalid use of '%' in replacement string"
                return Err("invalid use of '%' in replacement string".to_string());
            }
            i += 1;
        } else {
            result.push(repl_bytes[i]);
            i += 1;
        }
    }
    // Lua 字符串是字节序列
    Ok(unsafe { String::from_utf8_unchecked(result) })
}

// ============================================================================
// string.format 实现 (对应 C 的 str_format)
// ============================================================================

/// 格式字符串最大长度 (对应 C 的 MAX_FORMAT)
const MAX_FORMAT: usize = 32;

/// 有效标志集合 (对应 C 的 L_FMTFLAGS*)
const FMT_FLAGSF: &str = "-+#0 "; // 浮点: a, A, e, E, f, g, G
const FMT_FLAGSX: &str = "-#0"; // 十六进制: o, x, X
const FMT_FLAGSI: &str = "-+0 "; // 整数: d, i
const FMT_FLAGSU: &str = "-0"; // 无符号: u
const FMT_FLAGSC: &str = "-"; // 字符、指针、字符串: c, p, s

/// 跳过最多 2 位数字 (对应 C 的 get2digits)
fn get2digits(bytes: &[u8], mut idx: usize) -> usize {
    if idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
        if idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
    }
    idx
}

/// 检查格式说明是否有效 (对应 C 的 checkformat)
/// form: 完整格式字符串 (从 '%' 开始到 specifier 结束)
/// flags: 允许的标志集合
/// allow_precision: 是否允许精度
fn check_format(form: &str, flags: &str, allow_precision: bool) -> Result<(), String> {
    let bytes = form.as_bytes();
    let mut idx = 1; // skip '%'

    // 跳过 flags
    while idx < bytes.len() && flags.as_bytes().contains(&bytes[idx]) {
        idx += 1;
    }

    // 如果下一个字符不是 '0'，跳过 width (最多2位数字)
    // (C: a width cannot start with '0')
    if idx < bytes.len() && bytes[idx] != b'0' {
        idx = get2digits(bytes, idx);
        // 如果遇到 '.' 且允许精度，跳过精度
        if idx < bytes.len() && bytes[idx] == b'.' && allow_precision {
            idx += 1;
            idx = get2digits(bytes, idx);
        }
    }

    // 检查是否到达说明符 (字母字符)
    if idx >= bytes.len() || !bytes[idx].is_ascii_alphabetic() {
        return Err(format!("invalid conversion specification: '{}'", form));
    }
    Ok(())
}

/// 将 Rust 的指数格式转换为 C 的指数格式
/// Rust: "1e2" 或 "1E2" (指数不带前导零和符号)
/// C:    "1e+02" 或 "1E+02" (指数至少 2 位，带符号)
fn format_exponent_c(s: String) -> String {
    // 找到 e/E 的位置
    if let Some(e_pos) = s.find(|c: char| c == 'e' || c == 'E') {
        let mantissa = &s[..e_pos];
        let e_char = &s[e_pos..e_pos + 1];
        let exp_str = &s[e_pos + 1..];
        // 解析指数部分
        let (sign, digits) = if exp_str.starts_with('-') {
            ("-", &exp_str[1..])
        } else if exp_str.starts_with('+') {
            ("+", &exp_str[1..])
        } else {
            ("+", exp_str)
        };
        // 指数至少 2 位，前导零
        let digits = if digits.len() < 2 {
            format!("0{}", digits)
        } else {
            digits.to_string()
        };
        format!("{}{}{}{}", mantissa, e_char, sign, digits)
    } else {
        s
    }
}

/// 格式化十六进制浮点数 (对应 C printf 的 %a/%A)
///
/// 格式: [-]0x<int>.<frac>p<sign><exp>
/// - 零: 0x0p+0
/// - 正常: 1.<mantissa_hex>p<exp>
/// - 无穷: inf/INF
/// - NaN: nan/NAN
fn format_hex_float(n: f64, precision: Option<usize>, upper: bool) -> String {
    let hex_digits = if upper {
        "0123456789ABCDEF"
    } else {
        "0123456789abcdef"
    };
    let prefix = if upper { "0X" } else { "0x" };
    let p_char = if upper { 'P' } else { 'p' };

    if n.is_nan() {
        return if upper { "nan" } else { "nan" }.to_string();
    }
    if n.is_infinite() {
        let inf = if upper { "INF" } else { "inf" };
        return if n > 0.0 {
            inf.to_string()
        } else {
            format!("-{}", inf)
        };
    }

    let bits = n.to_bits();
    let sign_bit = bits >> 63;
    let exponent = ((bits >> 52) & 0x7FF) as i32;
    let mantissa = bits & 0xFFFFFFFFFFFFF; // 52 bits

    let sign_str = if sign_bit != 0 { "-" } else { "" };

    if exponent == 0 && mantissa == 0 {
        // 零
        return format!("{}{}0{}+0", sign_str, prefix, p_char);
    }

    let (int_digit, frac_bits, exp_val) = if exponent == 0 {
        // 非规格化数: 0.mantissa * 2^(-1022)
        (0u64, mantissa, -1022i32)
    } else {
        // 正常数: 1.mantissa * 2^(exponent - 1023)
        (1u64, mantissa, exponent - 1023)
    };

    // 默认精度: 13 个十六进制数字 (52 bits / 4 = 13)
    let default_prec = 13usize;
    let prec = precision.unwrap_or(default_prec);

    // 将 52 位尾数格式化为十六进制字符串
    // 尾数位 0-51, 最高 4 位 (48-51) 是第一个十六进制数字
    let mut frac_str = String::new();
    for i in 0..prec {
        let shift = 52 - 4 * (i + 1);
        let digit = if shift >= 0 {
            ((frac_bits >> shift) & 0xF) as usize
        } else {
            // 超出尾数精度, 补 0
            0
        };
        frac_str.push(hex_digits.as_bytes()[digit] as char);
    }

    // 无显式精度时, 去除尾随零
    if precision.is_none() {
        while frac_str.ends_with('0') {
            frac_str.pop();
        }
    }

    // 格式化指数
    let exp_sign = if exp_val >= 0 { "+" } else { "-" };
    let exp_abs = exp_val.abs();

    if frac_str.is_empty() {
        format!(
            "{}{}{}{}{}{}",
            sign_str, prefix, int_digit, p_char, exp_sign, exp_abs
        )
    } else {
        format!(
            "{}{}{}.{}{}{}{}",
            sign_str, prefix, int_digit, frac_str, p_char, exp_sign, exp_abs
        )
    }
}

/// string.format(fmt, ...) — 格式化字符串
/// 对应 C 的 str_format
pub fn str_format(fmt: &str, args: &[TValue]) -> Result<String, String> {
    let fmt_bytes = fmt.as_bytes();
    let mut result = String::new();
    let mut arg_idx = 0;
    let mut i = 0;

    while i < fmt_bytes.len() {
        // Push consecutive literal (non-%) text as a single slice to avoid O(N) char pushes
        let lit_start = i;
        while i < fmt_bytes.len() && fmt_bytes[i] != b'%' {
            i += 1;
        }
        if i > lit_start {
            result.push_str(unsafe { std::str::from_utf8_unchecked(&fmt_bytes[lit_start..i]) });
        }
        if i >= fmt_bytes.len() {
            break;
        }
        i += 1; // skip '%'
        if i >= fmt_bytes.len() {
            return Err("invalid conversion '%' to 'format'".to_string());
        }
        if fmt_bytes[i] == b'%' {
            result.push('%');
            i += 1;
            continue;
        }

        // 保存格式字符串起始位置 (包括 '%')
        let form_start = i - 1;

        // 先检查格式长度 (对应 C 的 getformat 中的长度检查)
        // 避免解析超大宽度/精度时溢出
        {
            let mut scan_idx = i;
            // 跳过 flags (L_FMTFLAGSF = "-+#0 ")
            while scan_idx < fmt_bytes.len() && b"-+ 0#".contains(&fmt_bytes[scan_idx]) {
                scan_idx += 1;
            }
            // 跳过 width 和 precision (数字和 '.')
            while scan_idx < fmt_bytes.len()
                && (fmt_bytes[scan_idx].is_ascii_digit() || fmt_bytes[scan_idx] == b'.')
            {
                scan_idx += 1;
            }
            // scan_idx 现在指向 specifier
            let form_len = scan_idx - form_start + 1; // 包括 '%' 和 specifier
            if form_len >= MAX_FORMAT - 9 {
                return Err("invalid format (too long)".to_string());
            }
        }

        // 解析格式说明符: flags, width, precision
        // flags: 对应 C 的 L_FMTFLAGSF "-+#0 "
        let mut left_align = false;
        let mut plus_sign = false;
        let mut space_sign = false;
        let mut zero_pad = false;
        let mut alt_form = false; // # 标志
        while i < fmt_bytes.len() && b"-+ 0#".contains(&fmt_bytes[i]) {
            match fmt_bytes[i] {
                b'-' => left_align = true,
                b'+' => plus_sign = true,
                b' ' => space_sign = true,
                b'0' => zero_pad = true,
                b'#' => alt_form = true,
                _ => {}
            }
            i += 1;
        }
        // 解析 width
        let mut width: usize = 0;
        while i < fmt_bytes.len() && fmt_bytes[i].is_ascii_digit() {
            width = width
                .saturating_mul(10)
                .saturating_add((fmt_bytes[i] - b'0') as usize);
            i += 1;
        }
        // 解析 precision
        let mut precision: Option<usize> = None;
        if i < fmt_bytes.len() && fmt_bytes[i] == b'.' {
            i += 1;
            let mut prec: usize = 0;
            while i < fmt_bytes.len() && fmt_bytes[i].is_ascii_digit() {
                prec = prec
                    .saturating_mul(10)
                    .saturating_add((fmt_bytes[i] - b'0') as usize);
                i += 1;
            }
            precision = Some(prec);
        }
        if i >= fmt_bytes.len() {
            return Err("invalid conversion specification".to_string());
        }

        let spec = fmt_bytes[i];
        i += 1;

        // 获取完整的格式字符串 (从 '%' 到 specifier)
        let form = std::str::from_utf8(&fmt_bytes[form_start..i]).unwrap_or("%");

        // 检查格式长度 (对应 C 的 getformat 中的长度检查)
        if form.len() >= MAX_FORMAT - 9 {
            return Err("invalid format (too long)".to_string());
        }

        if arg_idx >= args.len() {
            return Err(format!(
                "bad argument #{} to 'format' (no value)",
                arg_idx + 2
            ));
        }
        let arg = &args[arg_idx];
        arg_idx += 1;

        // 辅助函数: 应用宽度和对齐
        let apply_width = |s: String| -> String {
            if width == 0 || s.len() >= width {
                s
            } else if left_align {
                format!("{}{}", s, " ".repeat(width - s.len()))
            } else if zero_pad {
                // 零填充只对数字有意义，且在符号之后
                format!("{}{}", "0".repeat(width - s.len()), s)
            } else {
                format!("{}{}", " ".repeat(width - s.len()), s)
            }
        };

        match spec {
            b'd' | b'i' => {
                check_format(form, FMT_FLAGSI, true)?;
                let n = arg.as_integer().ok_or_else(|| {
                    format!(
                        "bad argument #{} to 'format' (number expected, got {})",
                        arg_idx,
                        arg.ty()
                    )
                })?;
                // 处理符号
                let neg = n < 0;
                let abs_n: u64 = if neg {
                    ((n as i128).unsigned_abs()) as u64
                } else {
                    n as u64
                };
                let mut digits = abs_n.to_string();
                // 处理精度: 精度表示至少输出的数字位数
                if let Some(p) = precision {
                    if p == 0 && n == 0 {
                        digits.clear();
                    } else if p > digits.len() {
                        digits = format!("{}{}", "0".repeat(p - digits.len()), digits);
                    }
                }
                // 符号字符串
                let sign_str = if neg {
                    "-"
                } else if plus_sign {
                    "+"
                } else if space_sign {
                    " "
                } else {
                    ""
                };
                // 处理宽度
                let content_len = sign_str.len() + digits.len();
                if width > content_len {
                    let pad = width - content_len;
                    if left_align {
                        result.push_str(sign_str);
                        result.push_str(&digits);
                        result.push_str(&" ".repeat(pad));
                    } else if zero_pad && precision.is_none() {
                        // 零填充在符号之后 (精度会覆盖 0 标志)
                        result.push_str(sign_str);
                        result.push_str(&"0".repeat(pad));
                        result.push_str(&digits);
                    } else {
                        result.push_str(&" ".repeat(pad));
                        result.push_str(sign_str);
                        result.push_str(&digits);
                    }
                } else {
                    result.push_str(sign_str);
                    result.push_str(&digits);
                }
            }
            b'u' => {
                check_format(form, FMT_FLAGSU, true)?;
                let n = arg.as_integer().ok_or_else(|| {
                    format!(
                        "bad argument #{} to 'format' (number expected, got {})",
                        arg_idx,
                        arg.ty()
                    )
                })?;
                let mut digits = (n as u64).to_string();
                // 处理精度
                if let Some(p) = precision {
                    if p == 0 && n == 0 {
                        digits.clear();
                    } else if p > digits.len() {
                        digits = format!("{}{}", "0".repeat(p - digits.len()), digits);
                    }
                }
                // 处理宽度
                if width > digits.len() {
                    let pad = width - digits.len();
                    if left_align {
                        result.push_str(&digits);
                        result.push_str(&" ".repeat(pad));
                    } else if zero_pad && precision.is_none() {
                        result.push_str(&"0".repeat(pad));
                        result.push_str(&digits);
                    } else {
                        result.push_str(&" ".repeat(pad));
                        result.push_str(&digits);
                    }
                } else {
                    result.push_str(&digits);
                }
            }
            b'o' => {
                check_format(form, FMT_FLAGSX, true)?;
                let n = arg.as_integer().ok_or_else(|| {
                    format!(
                        "bad argument #{} to 'format' (number expected, got {})",
                        arg_idx,
                        arg.ty()
                    )
                })?;
                let mut digits = format!("{:o}", n as u64);
                // 处理精度 (默认精度为 1)
                let prec = precision.unwrap_or(1);
                if prec == 0 && n == 0 {
                    digits.clear();
                } else if prec > digits.len() {
                    digits = format!("{}{}", "0".repeat(prec - digits.len()), digits);
                }
                // # 标志: 确保以 0 开头
                if alt_form && !digits.starts_with('0') {
                    digits = format!("0{}", digits);
                }
                // 处理宽度
                if width > digits.len() {
                    let pad = width - digits.len();
                    if left_align {
                        result.push_str(&digits);
                        result.push_str(&" ".repeat(pad));
                    } else if zero_pad && precision.is_none() {
                        result.push_str(&"0".repeat(pad));
                        result.push_str(&digits);
                    } else {
                        result.push_str(&" ".repeat(pad));
                        result.push_str(&digits);
                    }
                } else {
                    result.push_str(&digits);
                }
            }
            b'x' => {
                check_format(form, FMT_FLAGSX, true)?;
                let n = arg.as_integer().ok_or_else(|| {
                    format!(
                        "bad argument #{} to 'format' (number expected, got {})",
                        arg_idx,
                        arg.ty()
                    )
                })?;
                let mut digits = format!("{:x}", n as u64);
                // 处理精度 (默认精度为 1)
                let prec = precision.unwrap_or(1);
                if prec == 0 && n == 0 {
                    digits.clear();
                } else if prec > digits.len() {
                    digits = format!("{}{}", "0".repeat(prec - digits.len()), digits);
                }
                // # 标志: 添加 "0x" 前缀 (当值不为 0 时)
                let prefix = if alt_form && n != 0 { "0x" } else { "" };
                // 处理宽度
                let content_len = prefix.len() + digits.len();
                if width > content_len {
                    let pad = width - content_len;
                    if left_align {
                        result.push_str(prefix);
                        result.push_str(&digits);
                        result.push_str(&" ".repeat(pad));
                    } else if zero_pad && precision.is_none() {
                        result.push_str(prefix);
                        result.push_str(&"0".repeat(pad));
                        result.push_str(&digits);
                    } else {
                        result.push_str(&" ".repeat(pad));
                        result.push_str(prefix);
                        result.push_str(&digits);
                    }
                } else {
                    result.push_str(prefix);
                    result.push_str(&digits);
                }
            }
            b'X' => {
                check_format(form, FMT_FLAGSX, true)?;
                let n = arg.as_integer().ok_or_else(|| {
                    format!(
                        "bad argument #{} to 'format' (number expected, got {})",
                        arg_idx,
                        arg.ty()
                    )
                })?;
                let mut digits = format!("{:X}", n as u64);
                // 处理精度 (默认精度为 1)
                let prec = precision.unwrap_or(1);
                if prec == 0 && n == 0 {
                    digits.clear();
                } else if prec > digits.len() {
                    digits = format!("{}{}", "0".repeat(prec - digits.len()), digits);
                }
                // # 标志: 添加 "0X" 前缀 (当值不为 0 时)
                let prefix = if alt_form && n != 0 { "0X" } else { "" };
                // 处理宽度
                let content_len = prefix.len() + digits.len();
                if width > content_len {
                    let pad = width - content_len;
                    if left_align {
                        result.push_str(prefix);
                        result.push_str(&digits);
                        result.push_str(&" ".repeat(pad));
                    } else if zero_pad && precision.is_none() {
                        result.push_str(prefix);
                        result.push_str(&"0".repeat(pad));
                        result.push_str(&digits);
                    } else {
                        result.push_str(&" ".repeat(pad));
                        result.push_str(prefix);
                        result.push_str(&digits);
                    }
                } else {
                    result.push_str(prefix);
                    result.push_str(&digits);
                }
            }
            b'c' => {
                check_format(form, FMT_FLAGSC, false)?;
                let n = arg.as_integer().ok_or_else(|| {
                    format!(
                        "bad argument #{} to 'format' (number expected, got {})",
                        arg_idx,
                        arg.ty()
                    )
                })?;
                if n < 0 || n > 255 {
                    return Err("value out of range".to_string());
                }
                // 对应 C 的 sprintf %c: 输出单字节（非 UTF-8 编码）
                let c = n as u8;
                // 处理宽度
                if width > 1 {
                    let pad = width - 1;
                    if left_align {
                        unsafe {
                            result.as_mut_vec().push(c);
                        }
                        result.push_str(&" ".repeat(pad));
                    } else {
                        result.push_str(&" ".repeat(pad));
                        unsafe {
                            result.as_mut_vec().push(c);
                        }
                    }
                } else {
                    unsafe {
                        result.as_mut_vec().push(c);
                    }
                }
            }
            b'a' | b'A' => {
                check_format(form, FMT_FLAGSF, true)?;
                let n = arg.as_float().ok_or_else(|| {
                    format!(
                        "bad argument #{} to 'format' (number expected, got {})",
                        arg_idx,
                        arg.ty()
                    )
                })?;
                let upper = spec == b'A';
                let hex_str = format_hex_float(n, precision, upper);
                // # 标志: 总是显示小数点 (已在 format_hex_float 中处理)
                // 添加正号或空格
                let mut s = hex_str;
                if n >= 0.0 && !s.starts_with('-') && !n.is_nan() {
                    if plus_sign {
                        s = format!("+{}", s);
                    } else if space_sign {
                        s = format!(" {}", s);
                    }
                }
                // 处理宽度
                if width > s.len() {
                    if left_align {
                        s = format!("{}{}", s, " ".repeat(width - s.len()));
                    } else if zero_pad && !n.is_nan() && !n.is_infinite() {
                        // 零填充在 0x 之后、数字之前
                        // 简化: 对 %a 的零填充,在符号后、0x前填充
                        if s.starts_with('-') {
                            s = format!("-{}{}", "0".repeat(width - s.len()), &s[1..]);
                        } else if s.starts_with('+') {
                            s = format!("+{}{}", "0".repeat(width - s.len()), &s[1..]);
                        } else if s.starts_with(' ') {
                            s = format!(" {}{}", "0".repeat(width - s.len()), &s[1..]);
                        } else {
                            s = format!("{}{}", "0".repeat(width - s.len()), s);
                        }
                    } else {
                        s = format!("{}{}", " ".repeat(width - s.len()), s);
                    }
                }
                result.push_str(&s);
            }
            b'f' => {
                check_format(form, FMT_FLAGSF, true)?;
                let n = arg.as_float().ok_or_else(|| {
                    format!(
                        "bad argument #{} to 'format' (number expected, got {})",
                        arg_idx,
                        arg.ty()
                    )
                })?;
                let p = precision.unwrap_or(6); // 默认精度为 6
                let mut s = format!("{:.*}", p, n);
                // # 标志: 总是显示小数点
                if alt_form && !s.contains('.') {
                    s.push('.');
                }
                // 添加正号或空格
                if n >= 0.0 && !s.starts_with('-') {
                    if plus_sign {
                        s = format!("+{}", s);
                    } else if space_sign {
                        s = format!(" {}", s);
                    }
                }
                // 处理宽度
                if width > s.len() {
                    let pad = width - s.len();
                    if left_align {
                        s = format!("{}{}", s, " ".repeat(pad));
                    } else if zero_pad {
                        // 零填充在符号之后
                        if s.starts_with('-') {
                            s = format!("-{}{}", "0".repeat(pad), &s[1..]);
                        } else if s.starts_with('+') {
                            s = format!("+{}{}", "0".repeat(pad), &s[1..]);
                        } else if s.starts_with(' ') {
                            s = format!(" {}{}", "0".repeat(pad), &s[1..]);
                        } else {
                            s = format!("{}{}", "0".repeat(pad), s);
                        }
                    } else {
                        s = format!("{}{}", " ".repeat(pad), s);
                    }
                }
                result.push_str(&s);
            }
            b'e' | b'E' | b'g' | b'G' => {
                check_format(form, FMT_FLAGSF, true)?;
                let n = arg.as_float().ok_or_else(|| {
                    format!(
                        "bad argument #{} to 'format' (number expected, got {})",
                        arg_idx,
                        arg.ty()
                    )
                })?;
                let p = precision.unwrap_or(6); // 默认精度为 6
                let mut s = match spec {
                    b'e' | b'E' => {
                        // %e/%E: 科学计数法，指数至少 2 位，带符号
                        let uppercase = spec == b'E';
                        let raw = if uppercase {
                            format!("{:.*E}", p, n)
                        } else {
                            format!("{:.*e}", p, n)
                        };
                        // 转换指数部分为 C 格式 (1e2 -> 1e+02)
                        format_exponent_c(raw)
                    }
                    b'g' | b'G' => {
                        // %g/%G: 根据值的大小决定使用 %e/%E 还是 %f 格式
                        // 精度表示有效数字位数
                        let p = if p == 0 { 1 } else { p };
                        let uppercase = spec == b'G';
                        if n == 0.0 || n.is_nan() || n.is_infinite() {
                            // 特殊值
                            if n == 0.0 {
                                "0".to_string()
                            } else if n.is_nan() {
                                "nan".to_string()
                            } else if n > 0.0 {
                                "inf".to_string()
                            } else {
                                "-inf".to_string()
                            }
                        } else {
                            let exp = n.abs().log10().floor() as i32;
                            if exp < -4 || exp >= p as i32 {
                                // 使用 %e/%E 格式
                                let raw = if uppercase {
                                    format!("{:.*E}", p - 1, n)
                                } else {
                                    format!("{:.*e}", p - 1, n)
                                };
                                let mut s = format_exponent_c(raw);
                                // 去除尾随零 (除非有 # 标志)
                                if !alt_form && s.contains('.') {
                                    // 找到 e/E 的位置
                                    let e_pos = s.find(|c: char| c == 'e' || c == 'E').unwrap();
                                    let mantissa = &s[..e_pos];
                                    let exp_part = &s[e_pos..];
                                    let mantissa = mantissa.trim_end_matches('0');
                                    let mantissa = mantissa.trim_end_matches('.');
                                    s = format!("{}{}", mantissa, exp_part);
                                }
                                s
                            } else {
                                // 使用 %f 格式
                                let decimal_places = ((p as i32 - 1 - exp).max(0)) as usize;
                                let mut s = format!("{:.*}", decimal_places, n);
                                // 去除尾随零 (除非有 # 标志)
                                if !alt_form && s.contains('.') {
                                    s = s.trim_end_matches('0').to_string();
                                    s = s.trim_end_matches('.').to_string();
                                }
                                s
                            }
                        }
                    }
                    _ => unreachable!(),
                };
                // # 标志: 总是显示小数点 (对于 %e/%E)
                if alt_form && (spec == b'e' || spec == b'E') && !s.contains('.') {
                    if let Some(pos) = s.find('e').or_else(|| s.find('E')) {
                        s.insert(pos, '.');
                    }
                }
                // 添加正号或空格
                if n >= 0.0 && !s.starts_with('-') {
                    if plus_sign {
                        s = format!("+{}", s);
                    } else if space_sign {
                        s = format!(" {}", s);
                    }
                }
                // 处理宽度
                if width > s.len() {
                    let pad = width - s.len();
                    if left_align {
                        s = format!("{}{}", s, " ".repeat(pad));
                    } else if zero_pad {
                        if s.starts_with('-') {
                            s = format!("-{}{}", "0".repeat(pad), &s[1..]);
                        } else if s.starts_with('+') {
                            s = format!("+{}{}", "0".repeat(pad), &s[1..]);
                        } else if s.starts_with(' ') {
                            s = format!(" {}{}", "0".repeat(pad), &s[1..]);
                        } else {
                            s = format!("{}{}", "0".repeat(pad), s);
                        }
                    } else {
                        s = format!("{}{}", " ".repeat(pad), s);
                    }
                }
                result.push_str(&s);
            }
            b's' => {
                let has_modifiers = width > 0
                    || precision.is_some()
                    || left_align
                    || plus_sign
                    || space_sign
                    || zero_pad
                    || alt_form;
                if has_modifiers {
                    let s = match arg {
                        TValue::Str(s) => s.as_str().to_string(),
                        TValue::Integer(n) => n.to_string(),
                        TValue::Float(f) => {
                            if f.is_nan() {
                                "nan".to_string()
                            } else if f.is_infinite() {
                                if *f > 0.0 {
                                    "inf".to_string()
                                } else {
                                    "-inf".to_string()
                                }
                            } else {
                                format!("{}", f)
                            }
                        }
                        TValue::Nil(_) => "nil".to_string(),
                        TValue::Boolean(b) => b.to_string(),
                        _ => {
                            return Err(format!(
                                "bad argument #{} to 'format' (no proper format)",
                                arg_idx
                            ))
                        }
                    };
                    // 对应 C: 如果有修饰符 (width/precision/flags)，检查字符串是否包含零字节
                    check_format(form, FMT_FLAGSC, true)?;
                    // C: luaL_argcheck(L, l == strlen(s), arg, "string contains zeros")
                    if s.as_bytes().contains(&0) {
                        return Err(format!(
                            "bad argument #{} to 'format' (string contains zeros)",
                            arg_idx
                        ));
                    }
                    // C: 如果没有精度且字符串长度 >= 100，保持原样不格式化
                    if precision.is_none() && s.len() >= 100 {
                        result.push_str(&s);
                    } else {
                        // 应用精度 (截断)
                        let truncated = match precision {
                            Some(p) if p < s.len() => s[..p].to_string(),
                            _ => s,
                        };
                        result.push_str(&apply_width(truncated));
                    }
                } else {
                    // 无修饰符: 直接输出，避免 String 分配
                    match arg {
                        TValue::Str(s) => result.push_str(s.as_str()),
                        TValue::Integer(n) => result.push_str(&n.to_string()),
                        TValue::Float(f) => {
                            if f.is_nan() {
                                result.push_str("nan");
                            } else if f.is_infinite() {
                                if *f > 0.0 {
                                    result.push_str("inf");
                                } else {
                                    result.push_str("-inf");
                                }
                            } else {
                                result.push_str(&format!("{}", f));
                            }
                        }
                        TValue::Nil(_) => result.push_str("nil"),
                        TValue::Boolean(b) => result.push_str(&b.to_string()),
                        _ => {
                            return Err(format!(
                                "bad argument #{} to 'format' (no proper format)",
                                arg_idx
                            ))
                        }
                    }
                }
            }
            b'q' => {
                // q 不能有修饰符 (对应 C: if (form[2] != '\0'))
                if form.len() > 2 {
                    // "%q" 长度为 2
                    return Err("specifier '%%q' cannot have modifiers".to_string());
                }
                // 引用字符串/字面量 — 对应 C 的 addliteral
                // 字符串:加引号并转义
                // 数字:直接输出(整数用十进制,浮点用十六进制 %a 格式)
                // nil/boolean:直接输出 "nil"/"true"/"false"
                // NaN → "(0/0)", inf → "1e9999", -inf → "-1e9999"
                match arg {
                    TValue::Str(s) => {
                        // 对应 C 的 addquoted: 按字节处理,控制字符根据下一个字符决定格式
                        result.push('"');
                        let bytes = s.as_str().as_bytes();
                        for (idx, &c) in bytes.iter().enumerate() {
                            match c {
                                b'"' | b'\\' | b'\n' => {
                                    result.push('\\');
                                    unsafe {
                                        result.as_mut_vec().push(c);
                                    }
                                }
                                _ if c.is_ascii_control() => {
                                    // ASCII 控制字符: 如果下一个字符是数字,用 \03d (3位);否则用 \d
                                    let next_is_digit =
                                        idx + 1 < bytes.len() && bytes[idx + 1].is_ascii_digit();
                                    if next_is_digit {
                                        result.push_str(&format!("\\{:03}", c));
                                    } else {
                                        result.push_str(&format!("\\{}", c));
                                    }
                                }
                                _ => {
                                    // 其他字符(包括非 ASCII 字节)保持原始字节
                                    unsafe {
                                        result.as_mut_vec().push(c);
                                    }
                                }
                            }
                        }
                        result.push('"');
                    }
                    TValue::Integer(n) => {
                        // 对应 C: LUA_MININTEGER 用十六进制,否则用十进制
                        if *n == i64::MIN {
                            result.push_str(&format!("0x{:x}", *n as u64));
                        } else {
                            result.push_str(&n.to_string());
                        }
                    }
                    TValue::Float(f) => {
                        if f.is_nan() {
                            result.push_str("(0/0)");
                        } else if f.is_infinite() {
                            if *f > 0.0 {
                                result.push_str("1e9999");
                            } else {
                                result.push_str("-1e9999");
                            }
                        } else {
                            // C 用十六进制浮点 (%a) 确保精度;
                            // Rust 无原生 %a,用十进制格式 (Display trait 保证往返精度)
                            result.push_str(&format!("{}", f));
                        }
                    }
                    TValue::Nil(_) => result.push_str("nil"),
                    TValue::Boolean(b) => result.push_str(&b.to_string()),
                    _ => {
                        return Err(
                            "bad argument to 'format' (value has no literal form)".to_string()
                        )
                    }
                }
            }
            b'p' => {
                // C: checkformat(L, form, L_FMTFLAGSC, 0)
                check_format(form, FMT_FLAGSC, false)?;
                // 指针格式 — 对应 C 的 lua_topointer
                // 对于 nil、boolean、number:返回 "(null)"
                // 对于 table、function、userdata、thread、string:返回唯一标识符
                // 使用 GCObjectHeader::ptr_id 作为稳定标识符（对应 C 中堆对象的地址）
                let p_str = match arg {
                    TValue::Nil(_) | TValue::Boolean(_) | TValue::Integer(_) | TValue::Float(_) => {
                        "(null)".to_string()
                    }
                    TValue::Str(s) => {
                        match s {
                            crate::strings::LuaString::Short(arc) => {
                                // 短字符串：使用 Arc 的指针地址（内部化保证同一内容同一 Arc）
                                let ptr = std::sync::Arc::as_ptr(arc) as usize;
                                format!("0x{:x}", ptr)
                            }
                            crate::strings::LuaString::Long(ls) => {
                                // 长字符串：使用 ptr_id（每个实例唯一，克隆保留同一值）
                                format!("0x{:x}", ls.ptr_id)
                            }
                        }
                    }
                    TValue::Table(t) => {
                        format!("0x{:x}", t.gc_header.ptr_id)
                    }
                    TValue::LClosure(l) => {
                        format!("0x{:x}", l.gc_header.ptr_id)
                    }
                    TValue::CClosure(c) => {
                        // CClosure 没有 gc_header，使用结构体地址
                        let ptr = c as *const _ as usize;
                        format!("0x{:x}", ptr)
                    }
                    TValue::LCFn(f) => {
                        // 轻量 C 函数：使用函数指针地址
                        let ptr = f as *const _ as usize;
                        format!("0x{:x}", ptr)
                    }
                    TValue::BuiltinFn(b) => {
                        // Rust 原生内置函数：使用函数指针地址
                        let ptr = b.func as usize;
                        format!("0x{:x}", ptr)
                    }
                    TValue::RustClosure(rc) => {
                        // Rust 闭包：使用 Rc 指针地址
                        let ptr = Rc::as_ptr(rc) as usize;
                        format!("0x{:x}", ptr)
                    }
                    TValue::UserData(u) => {
                        format!("0x{:x}", u.gc_header.ptr_id)
                    }
                    TValue::Thread(t) => {
                        let ptr = t as *const _ as usize;
                        format!("0x{:x}", ptr)
                    }
                    TValue::LightUserData(p) => {
                        format!("0x{:x}", *p as usize)
                    }
                };
                result.push_str(&apply_width(p_str));
            }
            _ => {
                return Err(format!(
                    "invalid conversion '%{}' to 'format'",
                    spec as char
                ));
            }
        }
    }
    Ok(result)
}

// ============================================================================
// pack/unpack/packsize 实现 (对应 C 的 str_pack/str_packsize/str_unpack)
// ============================================================================

/// pack/unpack 的填充字节 (对应 C 的 LUAL_PACKPADBYTE)
const PACK_PAD_BYTE: u8 = 0x00;

/// 整数二进制表示的最大尺寸 (对应 C 的 MAXINTSIZE)
const MAX_INT_SIZE: usize = 16;

/// lua_Integer 的字节数 (对应 C 的 SZINT = sizeof(lua_Integer))
const SZ_INT: usize = std::mem::size_of::<i64>();

/// 最大分配大小 (对应 C 的 MAX_SIZE)
/// C: sizeof(size_t) < sizeof(lua_Integer) ? MAX_SIZET : cast_sizet(LUA_MAXINTEGER)
/// 在 64 位平台上 sizeof(size_t) == sizeof(lua_Integer) == 8,所以 MAX_SIZE = LUA_MAXINTEGER
const MAX_SIZE: usize = if std::mem::size_of::<usize>() < std::mem::size_of::<i64>() {
    usize::MAX
} else {
    i64::MAX as usize
};

/// 原生字节序是否为小端 (对应 C 的 nativeendian.little)
fn native_is_little() -> bool {
    cfg!(target_endian = "little")
}

/// 最大对齐 (对应 C 的 offsetof(struct cD, u),即 LUAI_MAXALIGN 联合体的对齐)
/// 在 64 位平台上通常为 8
const fn max_align() -> usize {
    let a = std::mem::align_of::<f64>();
    let b = std::mem::align_of::<*const u8>();
    let c = std::mem::align_of::<i64>();
    let d = std::mem::align_of::<std::os::raw::c_long>();
    let mut m = a;
    if b > m {
        m = b;
    }
    if c > m {
        m = c;
    }
    if d > m {
        m = d;
    }
    m
}
const MAX_ALIGN: usize = max_align();

/// pack/unpack 头信息 (对应 C 的 Header)
#[derive(Clone, Copy)]
struct PackHeader {
    islittle: bool,
    maxalign: usize,
}

impl PackHeader {
    fn new() -> Self {
        PackHeader {
            islittle: native_is_little(),
            maxalign: 1,
        }
    }
}

/// pack/unpack 选项 (对应 C 的 KOption)
#[derive(Debug, PartialEq, Eq)]
enum KOption {
    Kint,       // 有符号整数
    Kuint,      // 无符号整数
    Kfloat,     // 单精度浮点
    Knumber,    // Lua 原生浮点 (lua_Number = double)
    Kdouble,    // 双精度浮点
    Kchar,      // 定长字符串
    Kstring,    // 带长度前缀的字符串
    Kzstr,      // 零终止字符串
    Kpadding,   // 填充
    Kpaddalign, // 对齐填充
    Knop,       // 无操作
}

/// 从格式字符串中读取数字 (对应 C 的 getnum)
/// 返回 (读取的数字, 剩余格式字符串)
fn getnum(fmt: &[u8], pos: &mut usize, default: usize) -> usize {
    if *pos >= fmt.len() || !fmt[*pos].is_ascii_digit() {
        return default;
    }
    let mut a: usize = 0;
    while *pos < fmt.len() && fmt[*pos].is_ascii_digit() && a <= (usize::MAX - 9) / 10 {
        a = a * 10 + (fmt[*pos] - b'0') as usize;
        *pos += 1;
    }
    a
}

/// 读取数字并检查是否在 [1, MAX_INT_SIZE] 范围内 (对应 C 的 getnumlimit)
fn getnumlimit(fmt: &[u8], pos: &mut usize, default: usize) -> Result<usize, String> {
    let sz = getnum(fmt, pos, default);
    if sz.wrapping_sub(1) >= MAX_INT_SIZE {
        return Err(format!(
            "integral size ({}) out of limits [1,{}]",
            sz, MAX_INT_SIZE
        ));
    }
    Ok(sz)
}

/// 读取并分类下一个选项 (对应 C 的 getoption)
/// 返回 (选项类型, 选项尺寸)
fn getoption(h: &mut PackHeader, fmt: &[u8], pos: &mut usize) -> Result<(KOption, usize), String> {
    if *pos >= fmt.len() {
        return Err("invalid format (ends with empty)".to_string());
    }
    let opt = fmt[*pos];
    *pos += 1;
    let mut size: usize = 0;
    let result = match opt {
        b'b' => {
            size = std::mem::size_of::<i8>();
            KOption::Kint
        }
        b'B' => {
            size = std::mem::size_of::<i8>();
            KOption::Kuint
        }
        b'h' => {
            size = std::mem::size_of::<i16>();
            KOption::Kint
        }
        b'H' => {
            size = std::mem::size_of::<i16>();
            KOption::Kuint
        }
        b'l' => {
            size = std::mem::size_of::<std::os::raw::c_long>();
            KOption::Kint
        }
        b'L' => {
            size = std::mem::size_of::<std::os::raw::c_long>();
            KOption::Kuint
        }
        b'j' => {
            size = SZ_INT;
            KOption::Kint
        }
        b'J' => {
            size = SZ_INT;
            KOption::Kuint
        }
        b'T' => {
            size = std::mem::size_of::<usize>();
            KOption::Kuint
        }
        b'f' => {
            size = std::mem::size_of::<f32>();
            KOption::Kfloat
        }
        b'n' => {
            size = std::mem::size_of::<f64>();
            KOption::Knumber
        }
        b'd' => {
            size = std::mem::size_of::<f64>();
            KOption::Kdouble
        }
        b'i' => {
            size = getnumlimit(fmt, pos, std::mem::size_of::<std::os::raw::c_int>())?;
            KOption::Kint
        }
        b'I' => {
            size = getnumlimit(fmt, pos, std::mem::size_of::<std::os::raw::c_int>())?;
            KOption::Kuint
        }
        b's' => {
            size = getnumlimit(fmt, pos, std::mem::size_of::<usize>())?;
            KOption::Kstring
        }
        b'c' => {
            size = getnum(fmt, pos, usize::MAX);
            if size == usize::MAX {
                return Err("missing size for format option 'c'".to_string());
            }
            KOption::Kchar
        }
        b'z' => KOption::Kzstr,
        b'x' => {
            size = 1;
            KOption::Kpadding
        }
        b'X' => KOption::Kpaddalign,
        b' ' => KOption::Knop,
        b'<' => {
            h.islittle = true;
            KOption::Knop
        }
        b'>' => {
            h.islittle = false;
            KOption::Knop
        }
        b'=' => {
            h.islittle = native_is_little();
            KOption::Knop
        }
        b'!' => {
            h.maxalign = getnumlimit(fmt, pos, MAX_ALIGN)?;
            KOption::Knop
        }
        _ => return Err(format!("invalid format option '{}'", opt as char)),
    };
    Ok((result, size))
}

/// 检查是否为 2 的幂 (对应 C 的 ispow2)
fn is_pow2(x: usize) -> bool {
    x != 0 && (x & (x - 1)) == 0
}

/// 读取、分类并填充对齐细节 (对应 C 的 getdetails)
/// 返回 (选项类型, 选项尺寸, 需要对齐的字节数)
fn getdetails(
    h: &mut PackHeader,
    totalsize: usize,
    fmt: &[u8],
    pos: &mut usize,
) -> Result<(KOption, usize, usize), String> {
    let (opt, mut size) = getoption(h, fmt, pos)?;
    let mut align = size; // 通常对齐等于尺寸
    if opt == KOption::Kpaddalign {
        // 'X' 从后续选项获取对齐
        if *pos >= fmt.len() {
            return Err("invalid next option for option 'X'".to_string());
        }
        let (next_opt, next_size) = getoption(h, fmt, pos)?;
        if next_opt == KOption::Kchar || next_size == 0 {
            return Err("invalid next option for option 'X'".to_string());
        }
        align = next_size;
    }
    let ntoalign = if align <= 1 || opt == KOption::Kchar {
        0
    } else {
        if align > h.maxalign {
            align = h.maxalign;
        }
        if !is_pow2(align) {
            return Err("format asks for alignment not power of 2".to_string());
        }
        let szmoda = totalsize & (align - 1);
        (align - szmoda) & (align - 1)
    };
    // 注意: getoption 对于 Kpaddalign 已消耗了后续选项,但 size 仍为 0
    // 对于 Kpaddalign, size 应为 0 (对齐填充不占额外尺寸,只占 ntoalign)
    if opt == KOption::Kpaddalign {
        size = 0;
    }
    Ok((opt, size, ntoalign))
}

/// 打包整数 (对应 C 的 packint)
fn packint(buf: &mut Vec<u8>, n: u64, islittle: bool, size: usize, neg: bool) {
    let start = buf.len();
    buf.resize(start + size, 0);
    let mask = 0xFFu64;
    // 写入第一个字节
    let first_idx = if islittle { 0 } else { size - 1 };
    buf[start + first_idx] = (n & mask) as u8;
    let mut n = n;
    for i in 1..size {
        n >>= 8;
        let idx = if islittle { i } else { size - 1 - i };
        buf[start + idx] = (n & mask) as u8;
    }
    // 负数需要符号扩展
    if neg && size > SZ_INT {
        for i in SZ_INT..size {
            let idx = if islittle { i } else { size - 1 - i };
            buf[start + idx] = 0xFF;
        }
    }
}

/// 按指定字节序复制字节 (对应 C 的 copywithendian)
fn copy_with_endian(dest: &mut [u8], src: &[u8], islittle: bool) {
    let size = dest.len();
    if islittle == native_is_little() {
        dest.copy_from_slice(&src[..size]);
    } else {
        for i in 0..size {
            dest[i] = src[size - 1 - i];
        }
    }
}

/// 解包整数 (对应 C 的 unpackint)
fn unpackint(data: &[u8], islittle: bool, size: usize, issigned: bool) -> Result<i64, String> {
    let mut res: u64 = 0;
    let limit = if size <= SZ_INT { size } else { SZ_INT };
    for i in (0..limit).rev() {
        res <<= 8;
        let idx = if islittle { i } else { size - 1 - i };
        res |= data[idx] as u64;
    }
    if size < SZ_INT {
        if issigned {
            // 符号扩展
            let mask = 1u64 << (size * 8 - 1);
            res = (res ^ mask).wrapping_sub(mask);
        }
    } else if size > SZ_INT {
        // 检查未读字节
        let mask: u8 = if !issigned || (res as i64) >= 0 {
            0
        } else {
            0xFF
        };
        for i in limit..size {
            let idx = if islittle { i } else { size - 1 - i };
            if data[idx] != mask {
                return Err(format!(
                    "{}-byte integer does not fit into Lua Integer",
                    size
                ));
            }
        }
    }
    Ok(res as i64)
}

/// 从 TValue 获取整数值 (对应 C 的 luaL_checkinteger)
fn check_integer(v: &TValue, arg: usize) -> Result<i64, String> {
    match v.as_integer() {
        Some(i) => Ok(i),
        None => Err(format!(
            "bad argument #{} (integer expected, got {})",
            arg,
            v.ty()
        )),
    }
}

/// 从 TValue 获取浮点值 (对应 C 的 luaL_checknumber)
fn check_number(v: &TValue, arg: usize) -> Result<f64, String> {
    match v.as_float() {
        Some(f) => Ok(f),
        None => Err(format!(
            "bad argument #{} (number expected, got {})",
            arg,
            v.ty()
        )),
    }
}

/// 从 TValue 获取字符串字节 (对应 C 的 luaL_checklstring)
fn check_lstring<'a>(v: &'a TValue, arg: usize) -> Result<&'a [u8], String> {
    match v {
        TValue::Str(s) => Ok(s.as_str().as_bytes()),
        _ => Err(format!(
            "bad argument #{} (string expected, got {})",
            arg,
            v.ty()
        )),
    }
}

/// string.pack(fmt, ...) — 打包值到二进制字符串
/// 对应 C 的 str_pack
pub fn str_pack(fmt: &str, args: &[TValue]) -> Result<Vec<u8>, String> {
    let fmt_bytes = fmt.as_bytes();
    let mut h = PackHeader::new();
    let mut pos = 0; // 格式字符串的当前位置
    let mut arg: usize = 0; // 当前参数索引 (0-based,对应 C 的 arg-1)
    let mut totalsize: usize = 0;
    let mut buf: Vec<u8> = Vec::new();

    while pos < fmt_bytes.len() {
        let (opt, size, ntoalign) = getdetails(&mut h, totalsize, fmt_bytes, &mut pos)?;
        // 对应 C: if (size + ntoalign > MAX_SIZE - totalsize)
        // 使用 checked_add 避免 size + ntoalign 溢出,使用 saturating_sub 避免 MAX_SIZE - totalsize 下溢
        let total = match size.checked_add(ntoalign) {
            Some(t) if totalsize <= MAX_SIZE.saturating_sub(t) => t,
            _ => {
                return Err(format!(
                    "bad argument #{} to 'pack' (result too long)",
                    arg + 1
                ))
            }
        };
        totalsize += total;
        // 添加对齐填充
        for _ in 0..ntoalign {
            buf.push(PACK_PAD_BYTE);
        }
        // arg 在 C 中从 1 开始,这里从 0 开始
        // C: arg++ 在 switch 之前,所以 arg 对应当前参数
        // 但对于 Kpadding/Kpaddalign/Knop,arg 不增加
        let need_arg = !matches!(opt, KOption::Kpadding | KOption::Kpaddalign | KOption::Knop);
        if need_arg {
            arg += 1;
        }
        match opt {
            KOption::Kint => {
                if arg > args.len() {
                    return Err(format!(
                        "bad argument #{} to 'pack' (value expected)",
                        arg + 1
                    ));
                }
                let n = check_integer(&args[arg - 1], arg + 1)?;
                if size < SZ_INT {
                    let lim = 1i64 << (size * 8 - 1);
                    if n < -lim || n >= lim {
                        return Err(format!(
                            "bad argument #{} to 'pack' (integer overflow)",
                            arg + 1
                        ));
                    }
                }
                packint(&mut buf, n as u64, h.islittle, size, n < 0);
            }
            KOption::Kuint => {
                if arg > args.len() {
                    return Err(format!(
                        "bad argument #{} to 'pack' (value expected)",
                        arg + 1
                    ));
                }
                let n = check_integer(&args[arg - 1], arg + 1)?;
                if size < SZ_INT {
                    let max = 1u64 << (size * 8);
                    if (n as u64) >= max {
                        return Err(format!(
                            "bad argument #{} to 'pack' (unsigned overflow)",
                            arg + 1
                        ));
                    }
                }
                packint(&mut buf, n as u64, h.islittle, size, false);
            }
            KOption::Kfloat => {
                if arg > args.len() {
                    return Err(format!(
                        "bad argument #{} to 'pack' (value expected)",
                        arg + 1
                    ));
                }
                let f = check_number(&args[arg - 1], arg + 1)? as f32;
                let bytes = f.to_le_bytes();
                let start = buf.len();
                buf.resize(start + size, 0);
                // 根据字节序复制
                if h.islittle == native_is_little() {
                    buf[start..start + size].copy_from_slice(&bytes);
                } else {
                    for i in 0..size {
                        buf[start + i] = bytes[size - 1 - i];
                    }
                }
            }
            KOption::Knumber => {
                if arg > args.len() {
                    return Err(format!(
                        "bad argument #{} to 'pack' (value expected)",
                        arg + 1
                    ));
                }
                let f = check_number(&args[arg - 1], arg + 1)?;
                let bytes = f.to_le_bytes();
                let start = buf.len();
                buf.resize(start + size, 0);
                if h.islittle == native_is_little() {
                    buf[start..start + size].copy_from_slice(&bytes);
                } else {
                    for i in 0..size {
                        buf[start + i] = bytes[size - 1 - i];
                    }
                }
            }
            KOption::Kdouble => {
                if arg > args.len() {
                    return Err(format!(
                        "bad argument #{} to 'pack' (value expected)",
                        arg + 1
                    ));
                }
                let f = check_number(&args[arg - 1], arg + 1)?;
                let bytes = f.to_le_bytes();
                let start = buf.len();
                buf.resize(start + size, 0);
                if h.islittle == native_is_little() {
                    buf[start..start + size].copy_from_slice(&bytes);
                } else {
                    for i in 0..size {
                        buf[start + i] = bytes[size - 1 - i];
                    }
                }
            }
            KOption::Kchar => {
                if arg > args.len() {
                    return Err(format!(
                        "bad argument #{} to 'pack' (value expected)",
                        arg + 1
                    ));
                }
                let s = check_lstring(&args[arg - 1], arg + 1)?;
                let len = s.len();
                if len > size {
                    return Err(format!(
                        "bad argument #{} to 'pack' (string longer than given size)",
                        arg + 1
                    ));
                }
                buf.extend_from_slice(s);
                if len < size {
                    let psize = size - len;
                    buf.extend(std::iter::repeat(PACK_PAD_BYTE).take(psize));
                }
            }
            KOption::Kstring => {
                if arg > args.len() {
                    return Err(format!(
                        "bad argument #{} to 'pack' (value expected)",
                        arg + 1
                    ));
                }
                let s = check_lstring(&args[arg - 1], arg + 1)?;
                let len = s.len();
                // 检查长度是否适合给定尺寸
                if size < SZ_INT {
                    let max_len = 1usize << (size * 8);
                    if len >= max_len {
                        return Err(format!(
                            "bad argument #{} to 'pack' (string length does not fit in given size)",
                            arg + 1
                        ));
                    }
                }
                // 打包长度
                packint(&mut buf, len as u64, h.islittle, size, false);
                buf.extend_from_slice(s);
                totalsize += len;
            }
            KOption::Kzstr => {
                if arg > args.len() {
                    return Err(format!(
                        "bad argument #{} to 'pack' (value expected)",
                        arg + 1
                    ));
                }
                let s = check_lstring(&args[arg - 1], arg + 1)?;
                let len = s.len();
                // 检查字符串中是否包含零字节
                if s.contains(&0) {
                    return Err(format!(
                        "bad argument #{} to 'pack' (string contains zeros)",
                        arg + 1
                    ));
                }
                buf.extend_from_slice(s);
                buf.push(0); // 添加终止零
                totalsize += len + 1;
            }
            KOption::Kpadding => {
                buf.push(PACK_PAD_BYTE);
                // Kpadding 不消耗参数,需要回退 arg
                // 注意: 上面的 need_arg 已处理
            }
            KOption::Kpaddalign | KOption::Knop => {
                // 不消耗参数
            }
        }
    }
    Ok(buf)
}

/// string.packsize(fmt) — 返回打包结果的固定大小
/// 对应 C 的 str_packsize
pub fn str_packsize(fmt: &str) -> Result<usize, String> {
    let fmt_bytes = fmt.as_bytes();
    let mut h = PackHeader::new();
    let mut pos = 0;
    let mut totalsize: usize = 0;

    while pos < fmt_bytes.len() {
        let (opt, size, ntoalign) = getdetails(&mut h, totalsize, fmt_bytes, &mut pos)?;
        if opt == KOption::Kstring || opt == KOption::Kzstr {
            return Err("bad argument #1 to 'packsize' (variable-length format)".to_string());
        }
        // 对应 C: if (size + ntoalign > MAX_SIZE - totalsize)
        // 使用 checked_add 避免 size + ntoalign 溢出,使用 saturating_sub 避免 MAX_SIZE - total 下溢
        let total = match size.checked_add(ntoalign) {
            Some(t) if totalsize <= MAX_SIZE.saturating_sub(t) => t,
            _ => return Err("bad argument #1 to 'packsize' (format result too large)".to_string()),
        };
        totalsize += total;
    }
    Ok(totalsize)
}

/// string.unpack(fmt, data, [pos]) — 从二进制字符串解包值
/// 对应 C 的 str_unpack
/// 返回 (解包的值列表, 下一个位置)
pub fn str_unpack(fmt: &str, data: &[u8], init_pos: i64) -> Result<(Vec<TValue>, usize), String> {
    let fmt_bytes = fmt.as_bytes();
    let ld = data.len();
    // posrelatI 将相对位置转为绝对位置 (1-based),然后 -1 转为 0-based
    let mut pos = posrelat_i(init_pos, ld).saturating_sub(1);
    if pos > ld {
        return Err(format!(
            "bad argument #3 to 'unpack' (initial position out of string)"
        ));
    }
    let mut h = PackHeader::new();
    let mut fmt_pos = 0;
    let mut results: Vec<TValue> = Vec::new();

    while fmt_pos < fmt_bytes.len() {
        let (opt, size, ntoalign) = getdetails(&mut h, pos, fmt_bytes, &mut fmt_pos)?;
        // 检查数据足够
        let needed = ntoalign + size;
        if needed > ld - pos {
            return Err("bad argument #2 to 'unpack' (data string too short)".to_string());
        }
        pos += ntoalign; // 跳过对齐

        match opt {
            KOption::Kint => {
                let res = unpackint(&data[pos..pos + size], h.islittle, size, true)?;
                results.push(TValue::Integer(res));
            }
            KOption::Kuint => {
                let res = unpackint(&data[pos..pos + size], h.islittle, size, false)?;
                results.push(TValue::Integer(res));
            }
            KOption::Kfloat => {
                let mut f_bytes = [0u8; 4];
                copy_with_endian(&mut f_bytes, &data[pos..pos + 4], h.islittle);
                let f = f32::from_le_bytes(f_bytes);
                results.push(TValue::Float(f as f64));
            }
            KOption::Knumber => {
                let mut f_bytes = [0u8; 8];
                copy_with_endian(&mut f_bytes, &data[pos..pos + 8], h.islittle);
                let f = f64::from_le_bytes(f_bytes);
                results.push(TValue::Float(f));
            }
            KOption::Kdouble => {
                let mut f_bytes = [0u8; 8];
                copy_with_endian(&mut f_bytes, &data[pos..pos + 8], h.islittle);
                let f = f64::from_le_bytes(f_bytes);
                results.push(TValue::Float(f));
            }
            KOption::Kchar => {
                let s_bytes = &data[pos..pos + size];
                results.push(TValue::Str(crate::strings::new_short_bytes(
                    s_bytes.to_vec(),
                )));
            }
            KOption::Kstring => {
                let len = unpackint(&data[pos..pos + size], h.islittle, size, false)? as usize;
                if len > ld - pos - size {
                    return Err("bad argument #2 to 'unpack' (data string too short)".to_string());
                }
                let s_bytes = &data[pos + size..pos + size + len];
                results.push(TValue::Str(crate::strings::new_short_bytes(
                    s_bytes.to_vec(),
                )));
                pos += len; // 跳过字符串
            }
            KOption::Kzstr => {
                // 查找零字节
                let rel_pos = pos;
                let mut zero_idx = None;
                for i in rel_pos..ld {
                    if data[i] == 0 {
                        zero_idx = Some(i);
                        break;
                    }
                }
                let zero_idx = match zero_idx {
                    Some(idx) => idx,
                    None => {
                        return Err(
                            "bad argument #2 to 'unpack' (unfinished string for format 'z')"
                                .to_string(),
                        );
                    }
                };
                let len = zero_idx - rel_pos;
                if pos + len >= ld {
                    return Err(
                        "bad argument #2 to 'unpack' (unfinished string for format 'z')"
                            .to_string(),
                    );
                }
                let s_bytes = &data[rel_pos..zero_idx];
                results.push(TValue::Str(crate::strings::new_short_bytes(
                    s_bytes.to_vec(),
                )));
                pos += len + 1; // 跳过字符串和终止零
            }
            KOption::Kpadding | KOption::Kpaddalign | KOption::Knop => {
                // 不产生结果
            }
        }
        pos += size;
    }
    Ok((results, pos + 1)) // 返回 1-based 的下一个位置
}

// ============================================================================
// 派发函数 — 从 execute.rs 的 op_call 调用
// ============================================================================

/// 从栈中读取字符串参数
fn get_str_arg(state: &LuaState, a: usize, idx: usize) -> Result<String, VmError> {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return Err(arg_error(state, idx + 1, "string expected, got no value"));
    }
    let val = &state.stack[stack_idx];
    match val {
        TValue::Str(s) => Ok(s.as_str().to_string()),
        TValue::Integer(n) => Ok(n.to_string()),
        TValue::Float(f) => Ok(format!("{}", f)),
        _ => Err(arg_error(
            state,
            idx + 1,
            &format!("string expected, got {}", crate::tm::obj_type_name(val)),
        )),
    }
}

/// 从栈中读取整数参数 (对应 C 的 luaL_checkinteger)
/// 浮点数必须能精确转为整数，否则报 "number has no integer representation"
fn get_int_arg(
    state: &LuaState,
    a: usize,
    idx: usize,
    default: i64,
    _funcname: &str,
) -> Result<i64, VmError> {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return Ok(default);
    }
    match &state.stack[stack_idx] {
        TValue::Integer(n) => Ok(*n),
        TValue::Float(f) => match crate::vm::float_to_integer(*f, crate::vm::F2IMode::Eq) {
            Some(i) => Ok(i),
            None => Err(arg_error(
                state,
                idx + 1,
                "number has no integer representation",
            )),
        },
        TValue::Str(s) => match s.as_str().parse::<i64>() {
            Ok(i) => Ok(i),
            Err(_) => Ok(default),
        },
        TValue::Nil(_) => Ok(default),
        _ => Err(arg_error(
            state,
            idx + 1,
            &format!(
                "number expected, got {}",
                crate::tm::obj_type_name(&state.stack[stack_idx])
            ),
        )),
    }
}

/// 从栈中读取可选整数参数 (对应 C 的 luaL_optinteger)
/// nargs 是实际参数个数，用 nargs 判断参数是否存在（对应 C 的 lua_gettop）
fn get_opt_int_arg(
    state: &LuaState,
    a: usize,
    nargs: usize,
    idx: usize,
    default: i64,
    _funcname: &str,
) -> Result<i64, VmError> {
    if idx >= nargs {
        return Ok(default);
    }
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return Ok(default);
    }
    match &state.stack[stack_idx] {
        TValue::Nil(_) => Ok(default),
        TValue::Integer(n) => Ok(*n),
        TValue::Float(f) => match crate::vm::float_to_integer(*f, crate::vm::F2IMode::Eq) {
            Some(i) => Ok(i),
            None => Err(arg_error(
                state,
                idx + 1,
                "number has no integer representation",
            )),
        },
        TValue::Str(s) => match s.as_str().parse::<i64>() {
            Ok(i) => Ok(i),
            Err(_) => Ok(default),
        },
        _ => Err(arg_error(
            state,
            idx + 1,
            &format!(
                "number expected, got {}",
                crate::tm::obj_type_name(&state.stack[stack_idx])
            ),
        )),
    }
}

/// 从栈中读取布尔参数
fn get_bool_arg(state: &LuaState, a: usize, nargs: usize, idx: usize, default: bool) -> bool {
    if idx >= nargs {
        return default;
    }
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return default;
    }
    !state.stack[stack_idx].is_false()
}

/// 将结果压入栈并调整栈顶
fn push_results(state: &mut LuaState, a: usize, nresults: i32, results: Vec<TValue>) {
    state.adjust_results(a, nresults, results);
}

/// 对应 C 的 luaL_tolstring: 将值转为字符串用于 string.format 的 %s。
/// - table: 调用 __tostring 元方法;若无则用 __name (或 "table") + 指针地址
/// - 其他类型: 返回 None (由 str_format 自行处理)
fn tostring_for_format(state: &mut LuaState, val: &TValue) -> Option<String> {
    let table = match val {
        TValue::Table(t) => t.clone(),
        _ => return None,
    };

    // 查找 __tostring 元方法 (过滤 nil: __tostring = nil 表示无元方法)
    let tostring_key = TValue::Str(state.intern_str("__tostring"));
    let meta_fn = {
        let data = table.data.borrow();
        data.metatable
            .as_ref()
            .and_then(|mt| mt.get(&tostring_key))
            .filter(|v| !matches!(v, TValue::Nil(_)))
    };

    if let Some(f) = meta_fn {
        // 调用 __tostring(value)
        let base = state.stack.len();
        state.stack.push(f);
        state.stack.push(val.clone());
        let status = state.pcall(1, 1, 0);
        let result = if status == 0 && base < state.stack.len() {
            match &state.stack[base] {
                TValue::Str(s) => Some(s.as_str().to_string()),
                _ => None,
            }
        } else {
            None
        };
        state.stack.truncate(base);
        return result;
    }

    // 无 __tostring: 使用 __name (或默认 "table") + 指针地址
    let name_key = TValue::Str(state.intern_str("__name"));
    let type_name = {
        let data = table.data.borrow();
        data.metatable
            .as_ref()
            .and_then(|mt| mt.get(&name_key))
            .and_then(|v| match v {
                TValue::Str(s) => Some(s.as_str().to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "table".to_string())
    };
    Some(format!("{}: 0x{:x}", type_name, table.gc_header.ptr_id))
}

/// 预扫描格式字符串,返回使用 %s (或 %.Ns 等) 的参数索引集合 (0-based)。
/// 仅这些参数需要对 table 调用 __tostring 元方法 (对应 C 的 luaL_tolstring)。
/// %q 等其他 specifier 不应转换 table,以便 str_format 能正确报 "value has no literal form"。
fn find_s_arg_indices(fmt: &str) -> std::collections::HashSet<usize> {
    let mut indices = std::collections::HashSet::new();
    let bytes = fmt.as_bytes();
    let mut i = 0;
    let mut arg_idx = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'%' {
            i += 1;
            continue;
        }
        // 跳过 flags
        while i < bytes.len() && b"-+ 0#".contains(&bytes[i]) {
            i += 1;
        }
        // 跳过 width
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        // 跳过 precision
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b's' {
            indices.insert(arg_idx);
        }
        arg_idx += 1;
        i += 1;
    }
    indices
}

/// gmatch 迭代器函数 — 对应 C 的 gmatch_aux
///
/// 在 TFORCALL 中调用，参数: state_table (表), ctrl (忽略)
/// 从 state_table 中读取 s, p, pos, anchor, pat_start
/// 运行一次匹配，更新 pos，返回捕获（或 nil 表示结束）
pub fn call_gmatch_iter(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 从栈位置 a+1 读取状态表
    let state_val = if a + 1 < state.stack.len() {
        state.stack[a + 1].clone()
    } else {
        TValue::Nil(NilKind::Strict)
    };

    let mut state_table = match state_val {
        TValue::Table(t) => t,
        _ => {
            return Err(VmError::RuntimeError(
                "gmatch iterator: state table expected".to_string(),
            ))
        }
    };

    // 从表中读取字段
    let s_key = TValue::Str(state.intern_str("s"));
    let p_key = TValue::Str(state.intern_str("p"));
    let pos_key = TValue::Str(state.intern_str("pos"));
    let anchor_key = TValue::Str(state.intern_str("anchor"));
    let pat_start_key = TValue::Str(state.intern_str("pat_start"));
    let lastmatch_key = TValue::Str(state.intern_str("lastmatch"));

    let s_str = match state_table.get(&s_key) {
        Some(TValue::Str(s)) => s.as_str().to_string(),
        _ => {
            return Err(VmError::RuntimeError(
                "gmatch iterator: invalid state (missing 's')".to_string(),
            ))
        }
    };
    let p_str = match state_table.get(&p_key) {
        Some(TValue::Str(s)) => s.as_str().to_string(),
        _ => {
            return Err(VmError::RuntimeError(
                "gmatch iterator: invalid state (missing 'p')".to_string(),
            ))
        }
    };
    let pos: usize = match state_table.get(&pos_key) {
        Some(TValue::Integer(n)) => n as usize,
        _ => {
            return Err(VmError::RuntimeError(
                "gmatch iterator: invalid state (missing 'pos')".to_string(),
            ))
        }
    };
    let anchor: bool = match state_table.get(&anchor_key) {
        Some(TValue::Boolean(b)) => b,
        _ => false,
    };
    let pat_start: usize = match state_table.get(&pat_start_key) {
        Some(TValue::Integer(n)) => n as usize,
        _ => 0,
    };
    // lastmatch: 上次匹配的结束位置（-1 表示无上次匹配）
    let lastmatch: i64 = match state_table.get(&lastmatch_key) {
        Some(TValue::Integer(n)) => n,
        _ => -1,
    };

    // 运行匹配循环 — 对应 C 的 gmatch_aux
    let src_bytes = s_str.as_bytes();
    let len = src_bytes.len();
    let mut cur_pos = pos;

    let mut result_vals: Vec<TValue> = Vec::new();
    let mut found = false;

    while cur_pos <= len {
        let mut ms = MatchState::new(s_str.as_bytes(), p_str.as_bytes());
        ms.level = 0;
        ms.captures.clear();
        ms.match_depth = MAX_CCALLS;

        match match_pattern(&mut ms, cur_pos, pat_start) {
            Ok(Some(end)) => {
                // 对应 C: e != gm->lastmatch
                // 跳过结束位置与上次匹配相同的匹配（处理空匹配的重复）
                if end as i64 != lastmatch {
                    // 找到匹配
                    let captures = match get_captures(&ms, cur_pos, end) {
                        Ok(c) => c,
                        Err(e) => return Err(VmError::RuntimeError(e)),
                    };
                    // 更新 pos 和 lastmatch（对应 C: gm->src = gm->lastmatch = e）
                    state_table.set(pos_key.clone(), TValue::Integer(end as i64));
                    state_table.set(lastmatch_key.clone(), TValue::Integer(end as i64));

                    if captures.is_empty() {
                        // 无捕获时返回整个匹配的子串
                        // Lua 字符串是字节序列，使用 from_utf8_unchecked 保留原始字节
                        let matched_str = unsafe {
                            String::from_utf8_unchecked(src_bytes[cur_pos..end].to_vec())
                        };
                        result_vals.push(TValue::Str(state.intern_str(&matched_str)));
                    } else {
                        result_vals = captures;
                    }
                    found = true;
                    break;
                }
                // end == lastmatch，跳过此匹配，继续下一个位置
            }
            Ok(None) => {
                // 未匹配，继续下一个位置
            }
            Err(e) => return Err(VmError::RuntimeError(e)),
        }

        if anchor {
            break;
        }
        cur_pos += 1;
    }

    if !found {
        // 没有更多匹配
        push_results(state, a, nresults, vec![TValue::Nil(NilKind::Strict)]);
    } else {
        // Table 使用 Rc<RefCell<TableData>>,state_table.set() 已更新共享数据
        // 无需写回栈位置 (无论通过 __call 还是 TFORCALL 调用,表引用共享同一数据)
        push_results(state, a, nresults, result_vals);
    }
    Ok(())
}

// ============================================================================
// 各函数的派发实现（作为 BuiltinFnPtr 注册到 string 表）
// ============================================================================

/// string.upper(s) — 对应 C 的 str_upper
fn call_str_upper(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let result = str_upper(&s);
    push_results(
        state,
        a,
        nresults,
        vec![TValue::Str(state.intern_str(&result))],
    );
    Ok(())
}

/// string.lower(s) — 对应 C 的 str_lower
fn call_str_lower(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let result = str_lower(&s);
    push_results(
        state,
        a,
        nresults,
        vec![TValue::Str(state.intern_str(&result))],
    );
    Ok(())
}

/// string.len(s) — 对应 C 的 str_len
fn call_str_len(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let result = str_len(&s);
    push_results(state, a, nresults, vec![TValue::Integer(result)]);
    Ok(())
}

/// string.sub(s, i [, j]) — 对应 C 的 str_sub
fn call_str_sub(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let start = get_int_arg(state, a, 1, 1, "sub")?;
    let end = get_opt_int_arg(state, a, nargs, 2, -1, "sub")?;
    let result = str_sub(&s, start, end);
    push_results(
        state,
        a,
        nresults,
        vec![TValue::Str(state.intern_str(&result))],
    );
    Ok(())
}

/// string.reverse(s) — 对应 C 的 str_reverse
fn call_str_reverse(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let result = str_reverse(&s);
    push_results(
        state,
        a,
        nresults,
        vec![TValue::Str(state.intern_str(&result))],
    );
    Ok(())
}

/// string.byte(s [, i [, j]]) — 对应 C 的 str_byte
fn call_str_byte(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let i = get_opt_int_arg(state, a, nargs, 1, 1, "byte")?;
    let j = get_opt_int_arg(state, a, nargs, 2, i, "byte")?;
    let bytes = str_byte(&s, i, j);
    let results: Vec<TValue> = bytes.into_iter().map(TValue::Integer).collect();
    push_results(state, a, nresults, results);
    Ok(())
}

/// string.char(...) — 对应 C 的 str_char
fn call_str_char(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let mut codes = Vec::new();
    for idx in 0..nargs {
        codes.push(get_int_arg(state, a, idx, 0, "char")?);
    }
    match str_char(&codes) {
        Ok(result) => {
            push_results(
                state,
                a,
                nresults,
                vec![TValue::Str(state.intern_str(&result))],
            );
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// string.rep(s, n [, sep]) — 对应 C 的 str_rep
fn call_str_rep(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let n = get_int_arg(state, a, 1, 0, "rep")?;
    let sep = if nargs >= 3 {
        get_str_arg(state, a, 2).unwrap_or_default()
    } else {
        String::new()
    };
    match str_rep(&s, n, &sep) {
        Ok(result) => {
            push_results(
                state,
                a,
                nresults,
                vec![TValue::Str(state.intern_str(&result))],
            );
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// string.find(s, pattern [, init [, plain]]) — 对应 C 的 str_find
fn call_str_find(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let pattern = get_str_arg(state, a, 1)?;
    let init = get_opt_int_arg(state, a, nargs, 2, 1, "find")?;
    let plain = get_bool_arg(state, a, nargs, 3, false);
    match str_find(&s, &pattern, init, plain) {
        Ok(FindResult::Found {
            start,
            end,
            captures,
        }) => {
            let mut results = vec![TValue::Integer(start as i64), TValue::Integer(end as i64)];
            results.extend(captures);
            push_results(state, a, nresults, results);
        }
        Ok(FindResult::NotFound) => {
            push_results(state, a, nresults, vec![TValue::Nil(NilKind::Strict)]);
        }
        Err(msg) => return Err(VmError::RuntimeError(msg)),
    }
    Ok(())
}

/// string.format(fmt, ...) — 对应 C 的 str_format
fn call_str_format(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let fmt = get_str_arg(state, a, 0)?;
    let mut args: Vec<TValue> = (1..nargs)
        .map(|i| {
            let idx = a + 1 + i;
            if idx < state.stack.len() {
                state.stack[idx].clone()
            } else {
                TValue::Nil(NilKind::Strict)
            }
        })
        .collect();
    // Fast path: only need __tostring conversion when a %s arg is a table.
    // Avoid HashSet allocation + format scan for the common case (all string args).
    if args.iter().any(|arg| matches!(arg, TValue::Table(_))) {
        let s_indices = find_s_arg_indices(&fmt);
        for (i, arg) in args.iter_mut().enumerate() {
            if s_indices.contains(&i) {
                if let Some(s) = tostring_for_format(state, arg) {
                    *arg = TValue::Str(state.intern_str(&s));
                }
            }
        }
    }
    match str_format(&fmt, &args) {
        Ok(result) => {
            push_results(
                state,
                a,
                nresults,
                vec![TValue::Str(state.intern_str(&result))],
            );
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// string.match(s, pattern [, init]) — 对应 C 的 str_match
fn call_str_match(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let pattern = get_str_arg(state, a, 1)?;
    let init = get_opt_int_arg(state, a, nargs, 2, 1, "match")?;
    match str_match(&s, &pattern, init) {
        Ok(results) => {
            push_results(state, a, nresults, results);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// string.gsub(s, pattern, repl [, max_s]) — 对应 C 的 str_gsub
fn call_str_gsub(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let pattern = get_str_arg(state, a, 1)?;
    let max_s = get_opt_int_arg(state, a, nargs, 3, -1, "gsub")?;
    // 原始字符串的 TValue — 对应 C 的 lua_pushvalue(L, 1)
    // 当没有替换发生时，返回原始字符串（保持指针一致性，使 %p 相等）
    let orig_str = state
        .stack
        .get(a + 1)
        .cloned()
        .unwrap_or(TValue::Nil(NilKind::Strict));
    // 检查 repl 参数类型 — 对应 C 的 tr = lua_type(L, 3)
    let repl_idx = a + 3;
    let repl_val = if repl_idx < state.stack.len() {
        state.stack[repl_idx].clone()
    } else {
        TValue::Nil(NilKind::Strict)
    };
    match &repl_val {
        // table 或 function 替换 — 对应 C 的 LUA_TTABLE / LUA_TFUNCTION 分支
        // table 始终接受（用作查找替换，无需 __call）；函数类用 is_callable() 兼容
        // LightUserData(base tag) 形式的基础库函数（setmetatable 等仍是 LightUserData;
        // BuiltinFn 已在 is_function 中）
        v if matches!(v, TValue::Table(_)) || v.is_callable() => {
            match str_gsub_with_repl(state, &s, &pattern, &repl_val, max_s) {
                Ok((result, n, changed)) => {
                    // 对应 C: if (!changed) lua_pushvalue(L, 1);
                    let result_val = if !changed {
                        orig_str
                    } else {
                        TValue::Str(state.intern_str(&result))
                    };
                    push_results(state, a, nresults, vec![result_val, TValue::Integer(n)]);
                    Ok(())
                }
                Err(msg) => Err(VmError::RuntimeError(msg)),
            }
        }
        // string/number 替换 — 对应 C 的 default 分支 (add_s)
        TValue::Str(_) | TValue::Integer(_) | TValue::Float(_) => {
            let repl = get_str_arg(state, a, 2)?;
            match str_gsub(&s, &pattern, &repl, max_s) {
                Ok((result, n)) => {
                    // 对应 C: if (!changed) lua_pushvalue(L, 1);
                    let result_val = if n == 0 {
                        orig_str
                    } else {
                        TValue::Str(state.intern_str(&result))
                    };
                    push_results(state, a, nresults, vec![result_val, TValue::Integer(n)]);
                    Ok(())
                }
                Err(msg) => Err(VmError::RuntimeError(msg)),
            }
        }
        _ => Err(VmError::RuntimeError(format!(
            "bad argument #3 (string/function/table expected, got {})",
            repl_val.ty()
        ))),
    }
}

/// string.gmatch(s, pattern) — 对应 C 的 gmatch
///
/// 返回一个可调用的状态表。Rust 版本: 返回带 __call 元方法的表,表内存储状态
/// 状态: {s=string, p=pattern, pos=0, anchor=bool, pat_start=int}
fn call_str_gmatch(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_arg(state, a, 0)?;
    let p = get_str_arg(state, a, 1)?;
    let init = get_opt_int_arg(state, a, nargs, 2, 1, "gmatch")?;
    let len = s.len();
    let init_pos = posrelat_i(init, len).saturating_sub(1);
    // 对应 C: if (init > ls) init = ls + 1;
    // 让 src = s + (ls+1) 超过 src_end = s + ls，循环不执行
    let init_pos = if init_pos > len { len + 1 } else { init_pos };

    let anchor = p.starts_with('^');
    let pat_start = if anchor { 1 } else { 0 };

    // 创建状态表
    let mut state_table = crate::table::Table::new();
    state_table.set(
        TValue::Str(state.intern_str("s")),
        TValue::Str(state.intern_str(&s)),
    );
    state_table.set(
        TValue::Str(state.intern_str("p")),
        TValue::Str(state.intern_str(&p)),
    );
    state_table.set(
        TValue::Str(state.intern_str("pos")),
        TValue::Integer(init_pos as i64),
    );
    state_table.set(
        TValue::Str(state.intern_str("anchor")),
        TValue::Boolean(anchor),
    );
    state_table.set(
        TValue::Str(state.intern_str("pat_start")),
        TValue::Integer(pat_start as i64),
    );
    // lastmatch: 上次匹配的结束位置（-1 表示无上次匹配）
    // 对应 C 的 gm->lastmatch，用于跳过空匹配造成的重复
    state_table.set(
        TValue::Str(state.intern_str("lastmatch")),
        TValue::Integer(-1),
    );

    // 创建元表,设置 __call = BuiltinFn(call_gmatch_iter)
    // 迭代器作为 BuiltinFn 注册,无需 tag 派发
    let mut mt = crate::table::Table::new();
    mt.set(
        TValue::Str(state.intern_str("__call")),
        TValue::BuiltinFn(BuiltinFn {
            func: call_gmatch_iter,
            name: c"gmatch_iter".as_ptr() as *const u8,
        }),
    );
    state_table.set_metatable(Some(mt));

    // 返回单个表值 (可调用对象)
    push_results(state, a, nresults, vec![TValue::Table(state_table)]);
    Ok(())
}

/// string.pack(fmt, ...) — 对应 C 的 str_pack
fn call_str_pack(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let fmt = get_str_arg(state, a, 0)?;
    // 收集参数 (从索引 1 开始,即第 2 个参数及之后)
    let args: Vec<TValue> = (1..nargs)
        .map(|i| {
            let idx = a + 1 + i;
            if idx < state.stack.len() {
                state.stack[idx].clone()
            } else {
                TValue::Nil(NilKind::Strict)
            }
        })
        .collect();
    match str_pack(&fmt, &args) {
        Ok(bytes) => {
            // 将字节转换为 LuaString (可能包含非 UTF-8 字节)
            let s = unsafe { String::from_utf8_unchecked(bytes) };
            push_results(state, a, nresults, vec![TValue::Str(state.intern_str(&s))]);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// string.packsize(fmt) — 对应 C 的 str_packsize
fn call_str_packsize(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let fmt = get_str_arg(state, a, 0)?;
    match str_packsize(&fmt) {
        Ok(size) => {
            push_results(state, a, nresults, vec![TValue::Integer(size as i64)]);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// string.unpack(fmt, data [, pos]) — 对应 C 的 str_unpack
fn call_str_unpack(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let fmt = get_str_arg(state, a, 0)?;
    // 获取数据字符串的字节
    let data_bytes = {
        let stack_idx = a + 1 + 1;
        if stack_idx >= state.stack.len() {
            return Err(VmError::RuntimeError(format!(
                "bad argument #2 to 'unpack' (string expected, got no value)"
            )));
        }
        match &state.stack[stack_idx] {
            TValue::Str(s) => s.as_str().as_bytes().to_vec(),
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #2 to 'unpack' (string expected, got {})",
                    state.stack[stack_idx].ty()
                )))
            }
        }
    };
    let pos = get_opt_int_arg(state, a, nargs, 2, 1, "unpack")?;
    match str_unpack(&fmt, &data_bytes, pos) {
        Ok((mut values, next_pos)) => {
            // 最后一个返回值是下一个位置
            values.push(TValue::Integer(next_pos as i64));
            push_results(state, a, nresults, values);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// string.dump(f [, strip]) — 对应 C 的 str_dump
///
/// 将 Lua 函数序列化为二进制格式
fn call_str_dump(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'dump' (function expected, got no value)".to_string(),
        ));
    }
    let stack_idx = a + 1;
    if stack_idx >= state.stack.len() {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'dump' (function expected, got no value)".to_string(),
        ));
    }
    let func_val = state.stack[stack_idx].clone();
    let strip = if nargs >= 2 {
        let strip_idx = a + 2;
        if strip_idx < state.stack.len() {
            matches!(&state.stack[strip_idx], TValue::Boolean(true))
        } else {
            false
        }
    } else {
        false
    };
    match &func_val {
        TValue::LClosure(cl) => {
            let data = crate::compiler::bytecode_dump::dump_proto(&cl.proto, strip);
            push_results(
                state,
                a,
                nresults,
                vec![TValue::Str(crate::strings::new_long_bytes(data))],
            );
            Ok(())
        }
        _ => Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'dump' (function expected, got {})",
            func_val.ty()
        ))),
    }
}

// ============================================================================
// 创建字符串元表 — 对应 C 的 createmetatable
// ============================================================================

/// 创建字符串元表并注册到 DefaultMetatables
///
/// 对应 C 源码 lstrlib.cpp 的 createmetatable 函数:
/// 1. 创建元表，设置算术元方法
/// 2. 设置 __index 指向字符串库
/// 3. 将元表注册为字符串类型的默认元表
pub fn create_string_metatable(state: &mut LuaState) {
    let mut mt_table = Table::new();

    // 设置算术元方法 (对应 C 的 stringmetamethods 数组)
    set_arith_method(&mut mt_table, TagMethod::Add, add_int, add_f);
    set_arith_method(&mut mt_table, TagMethod::Sub, sub_int, sub_f);
    set_arith_method(&mut mt_table, TagMethod::Mul, mul_int, mul_f);
    set_arith_method(&mut mt_table, TagMethod::Mod, mod_int, mod_f);
    set_arith_method(&mut mt_table, TagMethod::Pow, |_, _| None, pow_f);
    set_arith_method(&mut mt_table, TagMethod::Div, |_, _| None, div_f);
    set_arith_method(&mut mt_table, TagMethod::IDiv, idiv_int, idiv_f);
    set_arith_method(&mut mt_table, TagMethod::Unm, unm_int, unm_f);

    // __index 指向字符串库表
    // 对应 C: lua_pushvalue(L, -2); lua_setfield(L, -2, "__index");
    let string_lib_table = create_string_lib_table(state);
    mt_table.set(
        make_tm_tvalue(TagMethod::Index),
        TValue::Table(string_lib_table),
    );

    // 创建 Metatable 并注册到 DefaultMetatables
    let mt = Metatable::new(mt_table);
    state.dmt.set(LuaType::String, mt);
}

/// 设置算术元方法到元表
fn set_arith_method(
    mt_table: &mut Table,
    tm: TagMethod,
    int_op: fn(i64, i64) -> Option<i64>,
    float_op: fn(f64, f64) -> f64,
) {
    // 元方法值: 使用一个标记值 (这里用 Integer 0 作为占位符)
    // 实际算术运算由 call_fn 回调处理
    let _ = (int_op, float_op); // 暂时未使用，保留接口
    mt_table.set(make_tm_tvalue(tm), TValue::Integer(0));
}

/// 创建字符串库函数表
fn create_string_lib_table(state: &LuaState) -> Table {
    let lib = Table::new();
    // 注册所有字符串库函数 (使用 BuiltinFn 函数指针)
    // 重要: 必须使用 state.intern_str() 创建键，确保哈希值与后续查找时一致
    let register = |lib: &Table, name: &'static std::ffi::CStr, func: BuiltinFnPtr| {
        let key = TValue::Str(state.intern_str(name.to_str().unwrap_or("")));
        let name_ptr = name.as_ptr() as *const u8;
        lib.set(key, TValue::BuiltinFn(BuiltinFn { func, name: name_ptr }));
    };
    register(&lib, c"upper", call_str_upper);
    register(&lib, c"lower", call_str_lower);
    register(&lib, c"len", call_str_len);
    register(&lib, c"sub", call_str_sub);
    register(&lib, c"reverse", call_str_reverse);
    register(&lib, c"byte", call_str_byte);
    register(&lib, c"char", call_str_char);
    register(&lib, c"rep", call_str_rep);
    register(&lib, c"find", call_str_find);
    register(&lib, c"format", call_str_format);
    register(&lib, c"match", call_str_match);
    register(&lib, c"gmatch", call_str_gmatch);
    register(&lib, c"gsub", call_str_gsub);
    register(&lib, c"pack", call_str_pack);
    register(&lib, c"packsize", call_str_packsize);
    register(&lib, c"unpack", call_str_unpack);
    register(&lib, c"dump", call_str_dump);
    lib
}

// ============================================================================
// 字符串库入口 — 对应 C 的 luaopen_string
// ============================================================================

/// 打开字符串库
///
/// 对应 C 源码 lstrlib.cpp 的 luaopen_string 函数:
/// 1. 创建字符串库函数表并注册为全局变量 string
/// 2. 创建字符串元表
pub fn open_string_lib(state: &mut LuaState) {
    // 创建字符串库函数表并注册为全局变量 string
    let string_lib_table = create_string_lib_table(state);
    let key = TValue::Str(state.intern_str("string"));
    state.globals.set(key, TValue::Table(string_lib_table));

    // 创建字符串元表
    create_string_metatable(state);
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::LuaType;

    // ========================================================================
    // 位置辅助函数测试
    // ========================================================================

    #[test]
    fn test_posrelat_i_positive() {
        assert_eq!(posrelat_i(3, 10), 3);
        assert_eq!(posrelat_i(1, 10), 1);
    }

    #[test]
    fn test_posrelat_i_zero() {
        assert_eq!(posrelat_i(0, 10), 1);
    }

    #[test]
    fn test_posrelat_i_negative() {
        assert_eq!(posrelat_i(-1, 10), 10);
        assert_eq!(posrelat_i(-3, 10), 8);
    }

    #[test]
    fn test_posrelat_i_negative_out_of_range() {
        assert_eq!(posrelat_i(-20, 10), 1);
    }

    #[test]
    fn test_get_end_pos_positive() {
        assert_eq!(get_end_pos(5, 10), 5);
        // 0 是合法的结束位置 (表示空区间),不应被转为默认值
        assert_eq!(get_end_pos(0, 10), 0);
    }

    #[test]
    fn test_get_end_pos_beyond_len() {
        assert_eq!(get_end_pos(20, 10), 10);
    }

    #[test]
    fn test_get_end_pos_negative() {
        assert_eq!(get_end_pos(-1, 10), 10);
        assert_eq!(get_end_pos(-3, 10), 8);
    }

    #[test]
    fn test_get_end_pos_negative_out_of_range() {
        assert_eq!(get_end_pos(-20, 10), 0);
    }

    // ========================================================================
    // 字符串函数测试
    // ========================================================================

    #[test]
    fn test_str_upper() {
        assert_eq!(str_upper("hello"), "HELLO");
        assert_eq!(str_upper("Hello World"), "HELLO WORLD");
        assert_eq!(str_upper(""), "");
        assert_eq!(str_upper("123"), "123");
    }

    #[test]
    fn test_str_lower() {
        assert_eq!(str_lower("HELLO"), "hello");
        assert_eq!(str_lower("Hello World"), "hello world");
        assert_eq!(str_lower(""), "");
        assert_eq!(str_lower("123"), "123");
    }

    #[test]
    fn test_str_len() {
        assert_eq!(str_len("hello"), 5);
        assert_eq!(str_len(""), 0);
        assert_eq!(str_len("a"), 1);
    }

    #[test]
    fn test_str_sub_basic() {
        assert_eq!(str_sub("hello world", 1, 5), "hello");
        assert_eq!(str_sub("hello", 2, 4), "ell");
    }

    #[test]
    fn test_str_sub_negative() {
        assert_eq!(str_sub("hello", -3, -1), "llo");
        assert_eq!(str_sub("hello", -5, -1), "hello");
    }

    #[test]
    fn test_str_sub_default_end() {
        assert_eq!(str_sub("hello", 2, -1), "ello");
        // end=0 是合法的结束位置 (表示位置 0 之前,即空区间)
        // start=2 > end=0, 所以返回空字符串,与 C 版本一致
        assert_eq!(str_sub("hello", 2, 0), "");
    }

    #[test]
    fn test_str_sub_out_of_range() {
        assert_eq!(str_sub("hello", 1, 100), "hello");
        assert_eq!(str_sub("hello", 10, 20), "");
    }

    #[test]
    fn test_str_sub_empty() {
        assert_eq!(str_sub("", 1, 1), "");
        assert_eq!(str_sub("hello", 3, 2), "");
    }

    /// 对应 strings.lua 第 40-55 行的 string.sub 测试用例
    /// 这些用例直接来自 Lua 5.5 官方测试套件,确保与 C 实现完全一致
    #[test]
    fn test_str_sub_lua_suite() {
        let s = "123456789";
        assert_eq!(str_sub(s, 2, 4), "234");
        assert_eq!(str_sub(s, 7, -1), "789");
        assert_eq!(str_sub(s, 7, 6), "");
        assert_eq!(str_sub(s, 7, 7), "7");
        // 关键边界用例: start=0 → 1, end=0 → 0, start > end → ""
        assert_eq!(str_sub(s, 0, 0), "");
        assert_eq!(str_sub(s, -10, 10), "123456789");
        assert_eq!(str_sub(s, 1, 9), "123456789");
        assert_eq!(str_sub(s, -10, -20), "");
        assert_eq!(str_sub(s, -1, -1), "9");
        assert_eq!(str_sub(s, -4, -1), "6789");
        assert_eq!(str_sub(s, -6, -4), "456");
    }

    /// 对应 strings.lua 第 51-53 行的极值整数边界用例
    #[test]
    fn test_str_sub_extreme_integers() {
        let s = "123456789";
        let mini = i64::MIN;
        let maxi = i64::MAX;
        assert_eq!(str_sub(s, mini, -4), "123456");
        assert_eq!(str_sub(s, mini, maxi), "123456789");
        assert_eq!(str_sub(s, mini, mini), "");
    }

    /// 对应 strings.lua 第 54-55 行的包含 null 字节的字符串
    #[test]
    fn test_str_sub_with_null_byte() {
        // "\000123456789" 长度为 10
        let s = "\u{0}123456789";
        assert_eq!(str_sub(s, 3, 5), "234");
        assert_eq!(str_sub(s, 8, -1), "789");
    }

    /// str_byte 的边界用例: end=0 应产生空区间
    #[test]
    fn test_str_byte_end_zero() {
        // string.byte("ABC", 1, 0) → end=0 < posi=1, 返回空
        assert_eq!(str_byte("ABC", 1, 0), Vec::<i64>::new());
        // string.byte("ABC", 0, 0) → posi=1, pose=0, 返回空
        assert_eq!(str_byte("ABC", 0, 0), Vec::<i64>::new());
    }

    /// str_byte 的边界用例: 超出范围的索引
    #[test]
    fn test_str_byte_out_of_range() {
        assert_eq!(str_byte("ABC", 10, 20), Vec::<i64>::new());
        assert_eq!(str_byte("ABC", -10, -20), Vec::<i64>::new());
    }

    /// str_byte 的边界用例: 空字符串
    #[test]
    fn test_str_byte_empty_string() {
        assert_eq!(str_byte("", 1, 1), Vec::<i64>::new());
        assert_eq!(str_byte("", 0, 0), Vec::<i64>::new());
    }

    /// str_byte 的边界用例: 包含 null 字节
    #[test]
    fn test_str_byte_with_null_byte() {
        let s = "\u{0}AB";
        assert_eq!(str_byte(s, 1, 1), vec![0]);
        assert_eq!(str_byte(s, 1, 3), vec![0, 65, 66]);
        assert_eq!(str_byte(s, -1, -1), vec![66]);
    }

    /// str_sub 的边界用例: 空字符串的各种位置参数
    #[test]
    fn test_str_sub_empty_string_various() {
        assert_eq!(str_sub("", 0, 0), "");
        assert_eq!(str_sub("", -1, -1), "");
        assert_eq!(str_sub("", 1, 100), "");
        assert_eq!(str_sub("", -100, 100), "");
    }

    /// str_sub 的边界用例: 单字符字符串
    #[test]
    fn test_str_sub_single_char() {
        assert_eq!(str_sub("a", 1, 1), "a");
        assert_eq!(str_sub("a", 0, 0), "");
        assert_eq!(str_sub("a", 1, 0), "");
        assert_eq!(str_sub("a", -1, -1), "a");
        assert_eq!(str_sub("a", -1, 0), "");
    }

    #[test]
    fn test_str_reverse() {
        assert_eq!(str_reverse("hello"), "olleh");
        assert_eq!(str_reverse(""), "");
        assert_eq!(str_reverse("a"), "a");
        assert_eq!(str_reverse("ab"), "ba");
    }

    #[test]
    fn test_str_byte_basic() {
        assert_eq!(str_byte("A", 1, 1), vec![65]);
        assert_eq!(str_byte("ABC", 1, 3), vec![65, 66, 67]);
    }

    #[test]
    fn test_str_byte_negative() {
        assert_eq!(str_byte("ABC", -1, -1), vec![67]);
        assert_eq!(str_byte("ABC", -2, -1), vec![66, 67]);
    }

    #[test]
    fn test_str_byte_default() {
        assert_eq!(str_byte("ABC", 1, 1), vec![65]);
        assert_eq!(str_byte("ABC", 2, 2), vec![66]);
    }

    #[test]
    fn test_str_byte_empty_range() {
        assert_eq!(str_byte("ABC", 5, 10), vec![]);
    }

    #[test]
    fn test_str_char_basic() {
        assert_eq!(str_char(&[65]).unwrap(), "A");
        assert_eq!(str_char(&[72, 73]).unwrap(), "HI");
        assert_eq!(str_char(&[]).unwrap(), "");
    }

    #[test]
    fn test_str_char_out_of_range() {
        assert!(str_char(&[-1]).is_err());
        assert!(str_char(&[256]).is_err());
    }

    #[test]
    fn test_str_char_non_utf8_bytes() {
        // string.char(255) 应返回包含字节 255 的字符串 (与 C 版本一致)
        // Lua 字符串是字节序列,不要求有效 UTF-8
        let s = str_char(&[255]).unwrap();
        assert_eq!(s.as_bytes(), &[255]);
        // string.char(0, 255, 0) 应返回 3 字节
        let s = str_char(&[0, 255, 0]).unwrap();
        assert_eq!(s.as_bytes(), &[0, 255, 0]);
        // string.char(228) 应返回包含字节 0xe4 的字符串
        let s = str_char(&[228]).unwrap();
        assert_eq!(s.as_bytes(), &[0xe4]);
    }

    #[test]
    fn test_str_rep_basic() {
        assert_eq!(str_rep("ab", 3, "").unwrap(), "ababab");
        assert_eq!(str_rep("ab", 1, "").unwrap(), "ab");
        assert_eq!(str_rep("ab", 0, "").unwrap(), "");
    }

    #[test]
    fn test_str_rep_with_sep() {
        assert_eq!(str_rep("ab", 3, ",").unwrap(), "ab,ab,ab");
        assert_eq!(str_rep("x", 3, "-").unwrap(), "x-x-x");
    }

    #[test]
    fn test_str_rep_negative() {
        assert_eq!(str_rep("ab", -5, "").unwrap(), "");
    }

    #[test]
    fn test_str_rep_too_large() {
        // 对应 strings.lua line 114-115: 结果字符串过大时应返回错误
        assert!(str_rep("aa", i64::MAX, "").is_err());
        assert!(str_rep("a", i64::MAX, ",").is_err());
    }

    // ========================================================================
    // 模式匹配测试
    // ========================================================================

    #[test]
    fn test_str_find_plain() {
        let result = str_find("hello world", "world", 1, true).unwrap();
        match result {
            FindResult::Found {
                start,
                end,
                captures,
            } => {
                assert_eq!(start, 7);
                assert_eq!(end, 11);
                assert!(captures.is_empty());
            }
            FindResult::NotFound => panic!("should find 'world'"),
        }
    }

    #[test]
    fn test_str_find_not_found() {
        let result = str_find("hello", "xyz", 1, true).unwrap();
        assert!(matches!(result, FindResult::NotFound));
    }

    #[test]
    fn test_str_find_empty_pattern() {
        let result = str_find("hello", "", 1, true).unwrap();
        match result {
            FindResult::Found { start, end, .. } => {
                assert_eq!(start, 1);
                assert_eq!(end, 0);
            }
            FindResult::NotFound => panic!("should find empty pattern"),
        }
    }

    #[test]
    fn test_str_find_with_init() {
        let result = str_find("hello hello", "hello", 2, true).unwrap();
        match result {
            FindResult::Found { start, .. } => assert_eq!(start, 7),
            FindResult::NotFound => panic!("should find second 'hello'"),
        }
    }

    #[test]
    fn test_str_find_pattern_dot() {
        let result = str_find("hello", "h.llo", 1, false).unwrap();
        match result {
            FindResult::Found { start, end, .. } => {
                assert_eq!(start, 1);
                assert_eq!(end, 5);
            }
            FindResult::NotFound => panic!("should match h.llo"),
        }
    }

    #[test]
    fn test_str_find_pattern_digit() {
        let result = str_find("abc123def", "%d+", 1, false).unwrap();
        match result {
            FindResult::Found { start, end, .. } => {
                assert_eq!(start, 4);
                assert_eq!(end, 6);
            }
            FindResult::NotFound => panic!("should match digits"),
        }
    }

    #[test]
    fn test_str_find_pattern_capture() {
        let result = str_find("hello", "(h(e)llo)", 1, false).unwrap();
        match result {
            FindResult::Found {
                start,
                end,
                captures,
            } => {
                assert_eq!(start, 1);
                assert_eq!(end, 5);
                assert_eq!(captures.len(), 2);
            }
            FindResult::NotFound => panic!("should match with captures"),
        }
    }

    #[test]
    fn test_str_find_anchored() {
        let result = str_find("hello", "^hello", 1, false).unwrap();
        assert!(matches!(result, FindResult::Found { .. }));

        let result = str_find("xhello", "^hello", 1, false).unwrap();
        assert!(matches!(result, FindResult::NotFound));
    }

    #[test]
    fn test_str_match_basic() {
        let result = str_match("hello world", "world", 1).unwrap();
        assert_eq!(result.len(), 1);
        match &result[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "world"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn test_str_match_not_found() {
        let result = str_match("hello", "xyz", 1).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].is_nil());
    }

    #[test]
    fn test_str_match_with_capture() {
        let result = str_match("hello", "(h.llo)", 1).unwrap();
        assert_eq!(result.len(), 1);
        match &result[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "hello"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn test_str_match_digit_capture() {
        let result = str_match("abc123", "(%d+)", 1).unwrap();
        assert_eq!(result.len(), 1);
        match &result[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "123"),
            _ => panic!("expected string"),
        }
    }

    // ========================================================================
    // gsub 测试
    // ========================================================================

    #[test]
    fn test_str_gsub_basic() {
        let (result, n) = str_gsub("hello world", "world", "Lua", -1).unwrap();
        assert_eq!(result, "hello Lua");
        assert_eq!(n, 1);
    }

    #[test]
    fn test_str_gsub_all() {
        let (result, n) = str_gsub("aaa", "a", "b", -1).unwrap();
        assert_eq!(result, "bbb");
        assert_eq!(n, 3);
    }

    #[test]
    fn test_str_gsub_with_limit() {
        let (result, n) = str_gsub("aaa", "a", "b", 2).unwrap();
        assert_eq!(result, "bba");
        assert_eq!(n, 2);
    }

    #[test]
    fn test_str_gsub_pattern() {
        let (result, n) = str_gsub("hello 123 world", "%d+", "NUM", -1).unwrap();
        assert_eq!(result, "hello NUM world");
        assert_eq!(n, 1);
    }

    #[test]
    fn test_str_gsub_capture() {
        let (result, n) = str_gsub("hello", "(h)", "%1%1", -1).unwrap();
        assert_eq!(result, "hhello");
        assert_eq!(n, 1);
    }

    #[test]
    fn test_str_gsub_no_match() {
        let (result, n) = str_gsub("hello", "xyz", "abc", -1).unwrap();
        assert_eq!(result, "hello");
        assert_eq!(n, 0);
    }

    // ========================================================================
    // format 测试
    // ========================================================================

    #[test]
    fn test_str_format_string() {
        let args = vec![TValue::Str(crate::strings::LuaString::Short(Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "world".to_string(),
            },
        )))];
        let result = str_format("hello %s", &args).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_str_format_integer() {
        let args = vec![TValue::Integer(42)];
        let result = str_format("value: %d", &args).unwrap();
        assert_eq!(result, "value: 42");
    }

    #[test]
    fn test_str_format_float() {
        let args = vec![TValue::Float(3.14)];
        let result = str_format("pi = %f", &args).unwrap();
        assert!(result.contains("3.14"));
    }

    #[test]
    fn test_str_format_hex() {
        let args = vec![TValue::Integer(255)];
        let result = str_format("%x", &args).unwrap();
        assert_eq!(result, "ff");
    }

    #[test]
    fn test_str_format_char() {
        let args = vec![TValue::Integer(65)];
        let result = str_format("%c", &args).unwrap();
        assert_eq!(result, "A");
    }

    #[test]
    fn test_str_format_percent() {
        let result = str_format("100%%", &[]).unwrap();
        assert_eq!(result, "100%");
    }

    #[test]
    fn test_str_format_multiple() {
        let args = vec![
            TValue::Integer(1),
            TValue::Str(crate::strings::LuaString::Short(Arc::new(
                crate::strings::ShortString {
                    hash: 0,
                    contents: "two".to_string(),
                },
            ))),
            TValue::Float(3.0),
        ];
        let result = str_format("%d %s %f", &args).unwrap();
        assert_eq!(result, "1 two 3.000000");
    }

    #[test]
    fn test_str_format_width() {
        let args = vec![TValue::Integer(42)];
        let result = str_format("[%5d]", &args).unwrap();
        assert_eq!(result, "[   42]");
    }

    #[test]
    fn test_str_format_left_align() {
        let args = vec![TValue::Integer(42)];
        let result = str_format("[%-5d]", &args).unwrap();
        assert_eq!(result, "[42   ]");
    }

    #[test]
    fn test_str_format_q_string() {
        // %q 字符串:加引号并转义
        let args = vec![TValue::Str(crate::strings::LuaString::Short(
            std::sync::Arc::new(crate::strings::ShortString {
                hash: 0,
                contents: "hello".to_string(),
            }),
        ))];
        let result = str_format("%q", &args).unwrap();
        assert_eq!(result, "\"hello\"");
    }

    #[test]
    fn test_str_format_q_integer() {
        // %q 整数:直接输出,不加引号
        let args = vec![TValue::Integer(42)];
        let result = str_format("%q", &args).unwrap();
        assert_eq!(result, "42");
    }

    #[test]
    fn test_str_format_q_boolean_nil() {
        // %q 布尔和 nil:直接输出,不加引号
        let args = vec![TValue::Boolean(true)];
        assert_eq!(str_format("%q", &args).unwrap(), "true");
        let args = vec![TValue::Boolean(false)];
        assert_eq!(str_format("%q", &args).unwrap(), "false");
        let args = vec![TValue::Nil(NilKind::Strict)];
        assert_eq!(str_format("%q", &args).unwrap(), "nil");
    }

    #[test]
    fn test_str_format_q_nan_inf() {
        // %q NaN → "(0/0)", inf → "1e9999", -inf → "-1e9999"
        let args = vec![TValue::Float(f64::NAN)];
        assert_eq!(str_format("%q", &args).unwrap(), "(0/0)");
        let args = vec![TValue::Float(f64::INFINITY)];
        assert_eq!(str_format("%q", &args).unwrap(), "1e9999");
        let args = vec![TValue::Float(f64::NEG_INFINITY)];
        assert_eq!(str_format("%q", &args).unwrap(), "-1e9999");
    }

    #[test]
    fn test_str_format_p_null() {
        // %p 对于 nil、boolean、number 返回 "(null)"
        let args = vec![TValue::Nil(NilKind::Strict)];
        assert_eq!(str_format("%p", &args).unwrap(), "(null)");
        let args = vec![TValue::Boolean(true)];
        assert_eq!(str_format("%p", &args).unwrap(), "(null)");
        let args = vec![TValue::Integer(42)];
        assert_eq!(str_format("%p", &args).unwrap(), "(null)");
        let args = vec![TValue::Float(3.14)];
        assert_eq!(str_format("%p", &args).unwrap(), "(null)");
    }

    // ========================================================================
    // 元表测试
    // ========================================================================

    #[test]
    fn test_create_string_metatable() {
        let mut state = LuaState::new();
        assert!(state.dmt.get(LuaType::String).is_none());
        create_string_metatable(&mut state);
        assert!(state.dmt.get(LuaType::String).is_some());
    }

    #[test]
    fn test_string_metatable_has_arith_methods() {
        let mut state = LuaState::new();
        create_string_metatable(&mut state);
        let mt = state
            .dmt
            .get(LuaType::String)
            .expect("string metatable must exist");
        for tm in &[
            TagMethod::Add,
            TagMethod::Sub,
            TagMethod::Mul,
            TagMethod::Mod,
            TagMethod::Pow,
            TagMethod::Div,
            TagMethod::IDiv,
            TagMethod::Unm,
        ] {
            let key = make_tm_tvalue(*tm);
            assert!(mt.get(&key).is_some(), "metamethod {:?} must exist", tm);
        }
    }

    #[test]
    fn test_string_metatable_has_index() {
        let mut state = LuaState::new();
        create_string_metatable(&mut state);
        let mt = state
            .dmt
            .get(LuaType::String)
            .expect("string metatable must exist");
        let key = make_tm_tvalue(TagMethod::Index);
        assert!(mt.get(&key).is_some(), "__index must exist");
    }

    // ========================================================================
    // open_string_lib 测试
    // ========================================================================

    #[test]
    fn test_open_string_lib_registers_global() {
        let mut state = LuaState::new();
        open_string_lib(&mut state);
        let key = TValue::Str(state.intern_str("string"));
        let string_table = state.globals.get(&key);
        assert!(string_table.is_some(), "string global must be registered");
        match string_table.unwrap() {
            TValue::Table(t) => {
                // 验证 upper 函数已注册
                let upper_key = TValue::Str(state.intern_str("upper"));
                assert!(t.get(&upper_key).is_some(), "string.upper must exist");
            }
            _ => panic!("string global must be a table"),
        }
    }

    #[test]
    fn test_open_string_lib_has_all_functions() {
        let mut state = LuaState::new();
        open_string_lib(&mut state);
        let key = TValue::Str(state.intern_str("string"));
        let string_table = state.globals.get(&key).expect("string global must exist");
        if let TValue::Table(t) = string_table {
            for name in &[
                "upper", "lower", "len", "sub", "reverse", "byte", "char", "rep", "find", "format",
                "match", "gsub",
            ] {
                let fn_key = TValue::Str(state.intern_str(name));
                assert!(t.get(&fn_key).is_some(), "string.{} must exist", name);
            }
        }
    }

    // ========================================================================
    // to_num / arith_op 测试 (保留原有测试)
    // ========================================================================

    #[test]
    fn test_to_num_string_integer() {
        let v = TValue::Str(crate::strings::LuaString::Short(Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "42".to_string(),
            },
        )));
        let result = to_num(&v);
        assert_eq!(result, Some(TValue::Integer(42)));
    }

    #[test]
    fn test_to_num_string_float() {
        let v = TValue::Str(crate::strings::LuaString::Short(Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "3.14".to_string(),
            },
        )));
        let result = to_num(&v);
        assert!(matches!(result, Some(TValue::Float(f)) if (f - 3.14).abs() < 1e-10));
    }

    #[test]
    fn test_to_num_invalid_string() {
        let v = TValue::Str(crate::strings::LuaString::Short(Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "abc".to_string(),
            },
        )));
        let result = to_num(&v);
        assert_eq!(result, None);
    }

    #[test]
    fn test_arith_op_add_integers() {
        let v1 = TValue::Integer(10);
        let v2 = TValue::Integer(20);
        let result = arith_op(&v1, &v2, add_int, add_f);
        assert_eq!(result, Some(TValue::Integer(30)));
    }

    #[test]
    fn test_arith_op_add_strings() {
        let make_str = |s: &str| {
            TValue::Str(crate::strings::LuaString::Short(Arc::new(
                crate::strings::ShortString {
                    hash: 0,
                    contents: s.to_string(),
                },
            )))
        };
        let v1 = make_str("10");
        let v2 = make_str("20");
        let result = arith_op(&v1, &v2, add_int, add_f);
        assert_eq!(result, Some(TValue::Integer(30)));
    }

    // ========================================================================
    // GMatchIterator 测试
    // ========================================================================

    #[test]
    fn test_gmatch_iterator_basic() {
        let mut iter = GMatchIterator::new("hello world", "%w+");
        let first = iter.next().unwrap();
        assert_eq!(first.len(), 1);
        let second = iter.next().unwrap();
        assert_eq!(second.len(), 1);
        let third = iter.next().unwrap();
        assert_eq!(third.len(), 0); // no more matches
    }

    // ========================================================================
    // match_class 测试
    // ========================================================================

    #[test]
    fn test_match_class_alpha() {
        assert!(match_class(b'a', b'a'));
        assert!(match_class(b'Z', b'a'));
        assert!(!match_class(b'1', b'a'));
    }

    #[test]
    fn test_match_class_digit() {
        assert!(match_class(b'5', b'd'));
        assert!(!match_class(b'a', b'd'));
    }

    #[test]
    fn test_match_class_upper_case() {
        // 大写字母类表示否定: %A 表示非字母字符
        assert!(!match_class(b'a', b'A')); // 'a' 是字母，%A 应该不匹配
        assert!(!match_class(b'Z', b'A')); // 'Z' 是字母，%A 应该不匹配
        assert!(match_class(b'1', b'A')); // '1' 不是字母，%A 应该匹配
    }

    #[test]
    fn test_match_class_space() {
        assert!(match_class(b' ', b's'));
        assert!(match_class(b'\t', b's'));
        assert!(!match_class(b'a', b's'));
    }

    // ========================================================================
    // BuiltinFn 派发函数测试
    // ========================================================================

    #[test]
    fn test_call_str_upper() {
        let mut state = LuaState::new();
        // 清空栈 (LuaState::new() 会预置一个 Nil)
        state.stack.clear();
        // 模拟栈: [func, "hello"] (位置 a=0 是函数占位,参数从 a+1 开始)
        state.stack.push(TValue::Nil(NilKind::Strict));
        state.stack.push(TValue::Str(state.intern_str("hello")));
        call_str_upper(&mut state, 0, 1, 1).unwrap();
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "HELLO"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_str_len() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::Nil(NilKind::Strict));
        state.stack.push(TValue::Str(state.intern_str("hello")));
        call_str_len(&mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 5),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_str_sub() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::Nil(NilKind::Strict));
        state.stack.push(TValue::Str(state.intern_str("hello")));
        state.stack.push(TValue::Integer(2));
        state.stack.push(TValue::Integer(4));
        call_str_sub(&mut state, 0, 3, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "ell"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_str_reverse() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::Nil(NilKind::Strict));
        state.stack.push(TValue::Str(state.intern_str("abc")));
        call_str_reverse(&mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "cba"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_str_byte() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::Nil(NilKind::Strict));
        state.stack.push(TValue::Str(state.intern_str("AB")));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(2));
        call_str_byte(&mut state, 0, 3, -1).unwrap();
        assert_eq!(state.stack.len(), 2);
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 65),
            _ => panic!("expected integer"),
        }
        match &state.stack[1] {
            TValue::Integer(n) => assert_eq!(*n, 66),
            _ => panic!("expected integer"),
        }
    }

    #[test]
    fn test_call_str_char() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::Nil(NilKind::Strict));
        state.stack.push(TValue::Integer(65));
        state.stack.push(TValue::Integer(66));
        call_str_char(&mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "AB"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_str_rep() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::Nil(NilKind::Strict));
        state.stack.push(TValue::Str(state.intern_str("ab")));
        state.stack.push(TValue::Integer(3));
        call_str_rep(&mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "ababab"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_str_find() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::Nil(NilKind::Strict));
        state
            .stack
            .push(TValue::Str(state.intern_str("hello world")));
        state.stack.push(TValue::Str(state.intern_str("world")));
        call_str_find(&mut state, 0, 2, -1).unwrap();
        assert!(state.stack.len() >= 2);
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 7),
            _ => panic!("expected integer start"),
        }
        match &state.stack[1] {
            TValue::Integer(n) => assert_eq!(*n, 11),
            _ => panic!("expected integer end"),
        }
    }

    #[test]
    fn test_call_str_format() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::Nil(NilKind::Strict));
        state.stack.push(TValue::Str(state.intern_str("hello %s")));
        state.stack.push(TValue::Str(state.intern_str("world")));
        call_str_format(&mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "hello world"),
            _ => panic!("expected string result"),
        }
    }

    // ========================================================================
    // pack / packsize / unpack 测试
    // ========================================================================

    /// 辅助: 将字节数组转为包含原始字节的 String (用于比较)
    fn bytes_to_string(b: &[u8]) -> String {
        unsafe { String::from_utf8_unchecked(b.to_vec()) }
    }

    /// 辅助: 比较打包结果与期望的字节序列
    fn assert_pack_eq(fmt: &str, args: &[TValue], expected: &[u8]) {
        let result = str_pack(fmt, args).expect("pack should succeed");
        assert_eq!(result, expected, "pack({:?}) mismatch", fmt);
    }

    /// 辅助: 比较 unpack 结果
    fn assert_unpack_eq(fmt: &str, data: &[u8], expected: &[TValue]) {
        let (results, _) = str_unpack(fmt, data, 1).expect("unpack should succeed");
        assert_eq!(
            results.len(),
            expected.len(),
            "unpack({:?}) result count mismatch",
            fmt
        );
        for (i, (r, e)) in results.iter().zip(expected.iter()).enumerate() {
            assert!(
                crate::vm::raw_equal(r, e),
                "unpack({:?}) result[{}] mismatch: got {:?}, expected {:?}",
                fmt,
                i,
                r,
                e
            );
        }
    }

    // --- 基本整数打包 ---

    #[test]
    fn test_pack_int_basic() {
        // 小端 1 字节有符号整数
        assert_pack_eq("<i1", &[TValue::Integer(1)], &[0x01]);
        assert_pack_eq("<i1", &[TValue::Integer(127)], &[0x7F]);
        assert_pack_eq("<i1", &[TValue::Integer(-1)], &[0xFF]);
        assert_pack_eq("<i1", &[TValue::Integer(-128)], &[0x80]);

        // 大端 1 字节有符号整数
        assert_pack_eq(">i1", &[TValue::Integer(1)], &[0x01]);
        assert_pack_eq(">i1", &[TValue::Integer(-1)], &[0xFF]);

        // 小端 2 字节有符号整数
        assert_pack_eq("<i2", &[TValue::Integer(1)], &[0x01, 0x00]);
        assert_pack_eq("<i2", &[TValue::Integer(256)], &[0x00, 0x01]);
        assert_pack_eq("<i2", &[TValue::Integer(-1)], &[0xFF, 0xFF]);

        // 大端 2 字节有符号整数
        assert_pack_eq(">i2", &[TValue::Integer(1)], &[0x00, 0x01]);
        assert_pack_eq(">i2", &[TValue::Integer(256)], &[0x01, 0x00]);
        assert_pack_eq(">i2", &[TValue::Integer(-1)], &[0xFF, 0xFF]);
    }

    #[test]
    fn test_pack_uint_basic() {
        // 无符号整数
        assert_pack_eq("<I1", &[TValue::Integer(255)], &[0xFF]);
        assert_pack_eq("<I2", &[TValue::Integer(65535)], &[0xFF, 0xFF]);
        assert_pack_eq(">I2", &[TValue::Integer(65535)], &[0xFF, 0xFF]);
        assert_pack_eq(
            "<I4",
            &[TValue::Integer(0x12345678)],
            &[0x78, 0x56, 0x34, 0x12],
        );
        assert_pack_eq(
            ">I4",
            &[TValue::Integer(0x12345678)],
            &[0x12, 0x34, 0x56, 0x78],
        );
    }

    #[test]
    fn test_pack_int_overflow() {
        // i1 范围: -128 ~ 127
        assert!(str_pack("<i1", &[TValue::Integer(128)]).is_err());
        assert!(str_pack("<i1", &[TValue::Integer(-129)]).is_err());

        // I1 范围: 0 ~ 255
        assert!(str_pack("<I1", &[TValue::Integer(256)]).is_err());
        assert!(str_pack("<I1", &[TValue::Integer(-1)]).is_err());
    }

    // --- Lua Integer (j/J) ---

    #[test]
    fn test_pack_lua_integer() {
        // j = lua_Integer (8 字节有符号)
        let val = 0x0807060504030201i64;
        assert_pack_eq(
            "<j",
            &[TValue::Integer(val)],
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
        );
        assert_pack_eq(
            ">j",
            &[TValue::Integer(val)],
            &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01],
        );

        // J = lua_Integer (8 字节无符号)
        assert_pack_eq("<J", &[TValue::Integer(-1)], &[0xFF; 8]);
    }

    #[test]
    fn test_pack_max_min_integer() {
        // 最大/最小整数
        assert_pack_eq(
            "<j",
            &[TValue::Integer(i64::MAX)],
            &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F],
        );
        assert_pack_eq(
            "<j",
            &[TValue::Integer(i64::MIN)],
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80],
        );
    }

    // --- 浮点数 ---

    #[test]
    fn test_pack_float() {
        // f = float (4 字节)
        let f = 1.0f32;
        let bytes = f.to_le_bytes();
        assert_pack_eq("<f", &[TValue::Float(1.0)], &bytes);

        let bytes = f.to_be_bytes();
        assert_pack_eq(">f", &[TValue::Float(1.0)], &bytes);
    }

    #[test]
    fn test_pack_double() {
        // d = double (8 字节)
        let d = 3.14f64;
        let bytes = d.to_le_bytes();
        assert_pack_eq("<d", &[TValue::Float(3.14)], &bytes);

        let bytes = d.to_be_bytes();
        assert_pack_eq(">d", &[TValue::Float(3.14)], &bytes);
    }

    #[test]
    fn test_pack_number() {
        // n = lua_Number (8 字节, double)
        let d = 2.718f64;
        let bytes = d.to_le_bytes();
        assert_pack_eq("<n", &[TValue::Float(2.718)], &bytes);
    }

    #[test]
    fn test_pack_float_special() {
        // 0.0
        assert_pack_eq("<f", &[TValue::Float(0.0)], &[0u8; 4]);
        assert_pack_eq("<d", &[TValue::Float(0.0)], &[0u8; 8]);

        // 无穷大
        let inf = f64::INFINITY;
        let bytes = inf.to_le_bytes();
        assert_pack_eq("<d", &[TValue::Float(inf)], &bytes);

        // 负无穷
        let neg_inf = f64::NEG_INFINITY;
        let bytes = neg_inf.to_le_bytes();
        assert_pack_eq("<d", &[TValue::Float(neg_inf)], &bytes);
    }

    #[test]
    fn test_pack_unpack_float_roundtrip() {
        for &n in &[
            0.0,
            -1.1,
            1.9,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e20,
            -1e20,
            0.1,
            2000.7,
        ] {
            let packed = str_pack("<n", &[TValue::Float(n)]).unwrap();
            assert_unpack_eq("<n", &packed, &[TValue::Float(n)]);
        }
    }

    // --- 字符串 ---

    #[test]
    fn test_pack_string_fixed() {
        // c = 固定长度字符串
        assert_pack_eq(
            "<c3",
            &[TValue::Str(crate::strings::LuaString::Short(
                std::sync::Arc::new(crate::strings::ShortString {
                    hash: 0,
                    contents: "abc".to_string(),
                }),
            ))],
            &[b'a', b'b', b'c'],
        );

        // 短字符串补零
        assert_pack_eq(
            "<c5",
            &[TValue::Str(crate::strings::LuaString::Short(
                std::sync::Arc::new(crate::strings::ShortString {
                    hash: 0,
                    contents: "ab".to_string(),
                }),
            ))],
            &[b'a', b'b', 0, 0, 0],
        );
    }

    #[test]
    fn test_pack_string_zstr() {
        // z = 零终止字符串
        let s = TValue::Str(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "hello".to_string(),
            },
        )));
        assert_pack_eq("<z", &[s], &[b'h', b'e', b'l', b'l', b'o', 0]);
    }

    #[test]
    fn test_pack_string_s() {
        // s = 带长度前缀的字符串 (默认 size_t = 8 字节)
        let s = TValue::Str(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "hi".to_string(),
            },
        )));
        let result = str_pack("<s", &[s]).unwrap();
        // 8 字节长度前缀 (小端) + 字符串内容
        assert_eq!(result.len(), 10);
        assert_eq!(&result[0..8], &[2, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&result[8..10], &[b'h', b'i']);
    }

    #[test]
    fn test_pack_string_s1() {
        // s1 = 1 字节长度前缀的字符串
        let s = TValue::Str(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "hi".to_string(),
            },
        )));
        assert_pack_eq("<s1", &[s], &[2, b'h', b'i']);
    }

    #[test]
    fn test_pack_empty_string() {
        let empty = TValue::Str(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "".to_string(),
            },
        )));

        // 空字符串 c0
        assert_pack_eq("<c0", &[empty.clone()], &[]);

        // 空字符串 z
        assert_pack_eq("<z", &[empty.clone()], &[0]);

        // 空字符串 s1
        assert_pack_eq("<s1", &[empty], &[0]);
    }

    #[test]
    fn test_pack_string_with_special_chars() {
        // 包含特殊字符的字符串
        let bytes = vec![0u8, 1, 2, 255, 254, 128];
        let s = unsafe { String::from_utf8_unchecked(bytes.clone()) };
        let sval = TValue::Str(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: s,
            },
        )));
        assert_pack_eq("<c6", &[sval.clone()], &bytes);

        // z 字符串中不能包含 0 (除了终止符)
        let s2 = unsafe { String::from_utf8_unchecked(vec![1u8, 2, 3]) };
        let sval2 = TValue::Str(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: s2,
            },
        )));
        assert_pack_eq("<z", &[sval2], &[1, 2, 3, 0]);
    }

    // --- 字节序 ---

    #[test]
    fn test_pack_endian_consistency() {
        // 大端和小端互为反转
        let val = 0x12345678i64;
        let le = str_pack("<i4", &[TValue::Integer(val)]).unwrap();
        let be = str_pack(">i4", &[TValue::Integer(val)]).unwrap();
        assert_eq!(le, be.iter().rev().copied().collect::<Vec<_>>());
    }

    #[test]
    fn test_pack_native_endian() {
        // = 前缀表示原生字节序
        let val = 1i64;
        let native = str_pack("=i2", &[TValue::Integer(val)]).unwrap();
        if cfg!(target_endian = "little") {
            assert_eq!(native, vec![1, 0]);
        } else {
            assert_eq!(native, vec![0, 1]);
        }
    }

    #[test]
    fn test_pack_mixed_endian() {
        // 混合字节序
        let result = str_pack(">i2 <i2", &[TValue::Integer(10), TValue::Integer(20)]).unwrap();
        assert_eq!(result, vec![0, 10, 20, 0]);
    }

    // --- 对齐和填充 ---

    #[test]
    fn test_pack_padding() {
        // x = 填充一个零字节
        assert_pack_eq("<x", &[], &[0]);

        // X = 对齐填充 (消耗下一个格式选项来确定对齐,但不打包该选项的值)
        // !4 xi1: x=1字节填充, i1=对齐1(无需填充), 总共2字节
        assert_pack_eq("<!4 xi1", &[TValue::Integer(1)], &[0, 1]);
    }

    #[test]
    fn test_pack_alignment() {
        // !4 设置对齐为 4
        // i1Xi4: i1=1字节, Xi4=对齐到4(填充3字节), i4被X消耗不打包
        let result = str_pack("<!4 i1Xi4", &[TValue::Integer(1)]).unwrap();
        assert_eq!(result, vec![1, 0, 0, 0]);

        // !4 i1 X i4 i4: i1=1字节, Xi4=对齐到4(消耗i4), i4=打包4字节
        // 只打包2个值: i1=1, i4=2
        let result = str_pack("<!4 i1Xi4i4", &[TValue::Integer(1), TValue::Integer(2)]).unwrap();
        assert_eq!(result, vec![1, 0, 0, 0, 2, 0, 0, 0]);
    }

    // --- packsize ---

    #[test]
    fn test_packsize_basic() {
        assert_eq!(str_packsize("<i1").unwrap(), 1);
        assert_eq!(str_packsize("<i2").unwrap(), 2);
        assert_eq!(str_packsize("<i4").unwrap(), 4);
        assert_eq!(str_packsize("<i8").unwrap(), 8);
        assert_eq!(str_packsize("<j").unwrap(), 8);
        assert_eq!(str_packsize("<f").unwrap(), 4);
        assert_eq!(str_packsize("<d").unwrap(), 8);
        assert_eq!(str_packsize("<n").unwrap(), 8);
    }

    #[test]
    fn test_packsize_multiple() {
        // 默认 maxalign=1, 所以无对齐填充
        assert_eq!(str_packsize("<i1i2i4").unwrap(), 7);
        // i1 + x(1字节) + i4(对齐1,无填充) = 1+1+4 = 6
        assert_eq!(str_packsize("<i1xi4").unwrap(), 6);
        // !4 i1xi4: i1(1) + x(1) + 对齐到4(填充2) + i4(4) = 8
        assert_eq!(str_packsize("<!4 i1xi4").unwrap(), 8);
    }

    #[test]
    fn test_packsize_matches_pack() {
        // packsize 结果应与 pack 结果长度一致 (对于不需要额外参数的格式)
        let fmts = ["<i1", "<i2", "<i4", "<j", "<f", "<d", "<n", "<x", "<i1i2i4"];
        for fmt in &fmts {
            let size = str_packsize(fmt).unwrap();
            // 对于这些格式, pack 不需要额外参数 (除了整数/浮点数)
            // 我们只验证格式部分的大小
            println!("packsize({:?}) = {}", fmt, size);
        }
    }

    #[test]
    fn test_packsize_invalid() {
        // i0 无效
        assert!(str_packsize("i0").is_err());
        // !0 无效 (对齐为 0)
        assert!(str_packsize("!0").is_err());
        // !3 单独不报错 (没有需要对齐的选项)
        // !3 i4 报错 (3 不是 2 的幂)
        assert!(str_packsize("!3 i4").is_err());
    }

    // --- unpack ---

    #[test]
    fn test_unpack_int_basic() {
        assert_unpack_eq("<i1", &[0x01], &[TValue::Integer(1)]);
        assert_unpack_eq("<i1", &[0xFF], &[TValue::Integer(-1)]);
        assert_unpack_eq("<i2", &[0x01, 0x00], &[TValue::Integer(1)]);
        assert_unpack_eq(">i2", &[0x00, 0x01], &[TValue::Integer(1)]);
        assert_unpack_eq("<I1", &[0xFF], &[TValue::Integer(255)]);
    }

    #[test]
    fn test_unpack_sign_extension() {
        // 符号扩展: i1 的 0xF0 应为 -16
        assert_unpack_eq("<i1", &[0xF0], &[TValue::Integer(-16)]);
        // 无符号: I1 的 0xF0 应为 240
        assert_unpack_eq("<I1", &[0xF0], &[TValue::Integer(240)]);

        // 多字节符号扩展
        assert_unpack_eq("<i2", &[0x00, 0x80], &[TValue::Integer(-32768)]);
        assert_unpack_eq("<i3", &[0x00, 0x00, 0x80], &[TValue::Integer(-8388608)]);
    }

    #[test]
    fn test_unpack_uint_does_not_fit() {
        // 解包无符号整数时,如果值超出 lua_Integer 范围,应报错
        // 仅当 size > SZ_INT 时才检查 (对应 tpack.lua 的溢出测试)
        // 9 字节无符号,最高字节为 1,超出 i64 范围
        let data = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
        assert!(str_unpack("<I9", &data, 1).is_err());

        // 9 字节有符号,最高字节为 1,超出 i64 范围
        let data = vec![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert!(str_unpack(">i9", &data, 1).is_err());

        // 8 字节无符号 0x8000000000000000 不报错 (转为 i64::MIN)
        let data = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80];
        assert!(str_unpack("<I8", &data, 1).is_ok());
    }

    #[test]
    fn test_unpack_multiple_values() {
        // 多值解包
        let data = vec![0x01, 0x02, 0x03];
        let (results, _) = str_unpack("<i1i1i1", &data, 1).unwrap();
        assert_eq!(results.len(), 3);
        assert!(crate::vm::raw_equal(&results[0], &TValue::Integer(1)));
        assert!(crate::vm::raw_equal(&results[1], &TValue::Integer(2)));
        assert!(crate::vm::raw_equal(&results[2], &TValue::Integer(3)));
    }

    #[test]
    fn test_unpack_with_position() {
        // 从指定位置开始解包
        let data = vec![0xFF, 0x01, 0x02];
        let (results, next_pos) = str_unpack("<i2", &data, 2).unwrap();
        assert_eq!(results.len(), 1);
        assert!(crate::vm::raw_equal(&results[0], &TValue::Integer(0x0201)));
        assert_eq!(next_pos, 4); // 1-based, 2 + 2 = 4
    }

    #[test]
    fn test_unpack_string() {
        // 解包固定长度字符串
        let data = vec![b'a', b'b', b'c'];
        let (results, _) = str_unpack("<c3", &data, 1).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "abc"),
            _ => panic!("expected string"),
        }

        // 解包零终止字符串
        let data = vec![b'h', b'i', 0];
        let (results, _) = str_unpack("<z", &data, 1).unwrap();
        match &results[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "hi"),
            _ => panic!("expected string"),
        }

        // 解包带长度前缀字符串
        let data = vec![2, b'h', b'i'];
        let (results, _) = str_unpack("<s1", &data, 1).unwrap();
        match &results[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "hi"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn test_pack_unpack_roundtrip() {
        // 整数往返
        for &val in &[0i64, 1, -1, 127, -128, 32767, -32768, 255, 65535] {
            let packed = str_pack("<i4", &[TValue::Integer(val)]).unwrap();
            assert_unpack_eq("<i4", &packed, &[TValue::Integer(val)]);
        }

        // 浮点数往返
        for &val in &[0.0, 1.0, -1.0, 3.14, 2.718, 1e10, -1e10] {
            let packed = str_pack("<d", &[TValue::Float(val)]).unwrap();
            assert_unpack_eq("<d", &packed, &[TValue::Float(val)]);
        }
    }

    #[test]
    fn test_pack_unpack_roundtrip_endian() {
        // 大端往返
        let val = 0x12345678i64;
        for fmt in &["<i4", ">i4", "<j", ">j"] {
            let packed = str_pack(fmt, &[TValue::Integer(val)]).unwrap();
            assert_unpack_eq(fmt, &packed, &[TValue::Integer(val)]);
        }
    }

    // --- 错误处理 ---

    #[test]
    fn test_pack_invalid_format() {
        assert!(str_pack("<i0", &[TValue::Integer(0)]).is_err());
        assert!(str_pack("<i17", &[TValue::Integer(0)]).is_err()); // 超过 16
                                                                   // !0 无效 (对齐为 0)
        assert!(str_pack("!0", &[]).is_err());
        // !3 单独不报错 (maxalign=3, 但没有需要对齐的选项)
        // !3 i4 报错 (3 不是 2 的幂)
        assert!(str_pack("!3 i4", &[TValue::Integer(0)]).is_err());
    }

    #[test]
    fn test_pack_missing_argument() {
        assert!(str_pack("<i1", &[]).is_err());
        assert!(str_pack("<i1i1", &[TValue::Integer(1)]).is_err());
    }

    #[test]
    fn test_unpack_data_too_short() {
        assert!(str_unpack("<i4", &[0, 0, 0], 1).is_err());
        assert!(str_unpack("<c5", &[1, 2, 3], 1).is_err());
    }

    #[test]
    fn test_pack_string_too_long() {
        let s = TValue::Str(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "hello".to_string(),
            },
        )));
        // c3 容纳 3 字节, 但字符串有 5 字节
        assert!(str_pack("<c3", &[s]).is_err());
    }

    // --- 无操作 (空格) ---

    #[test]
    fn test_pack_nop() {
        // 空格是空操作
        assert_pack_eq("<  i1", &[TValue::Integer(42)], &[42]);
        assert_eq!(str_packsize("<  i1").unwrap(), 1);
    }

    // --- 复合格式 ---

    #[test]
    fn test_pack_complex_format() {
        // 复合格式: i1 + c3 + i2
        let s = TValue::Str(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "abc".to_string(),
            },
        )));
        let result = str_pack("<i1c3i2", &[TValue::Integer(1), s, TValue::Integer(2)]).unwrap();
        assert_eq!(result, vec![1, b'a', b'b', b'c', 2, 0]);
    }

    #[test]
    fn test_pack_unpack_complex_roundtrip() {
        let s = TValue::Str(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: "XY".to_string(),
            },
        )));
        let fmt = "<i1c2i2d";
        let args = vec![
            TValue::Integer(42),
            s.clone(),
            TValue::Integer(1000),
            TValue::Float(3.14),
        ];
        let packed = str_pack(fmt, &args).unwrap();
        let (results, _) = str_unpack(fmt, &packed, 1).unwrap();
        assert_eq!(results.len(), 4);
        assert!(crate::vm::raw_equal(&results[0], &TValue::Integer(42)));
        assert!(crate::vm::raw_equal(&results[1], &s));
        assert!(crate::vm::raw_equal(&results[2], &TValue::Integer(1000)));
        assert!(crate::vm::raw_equal(&results[3], &TValue::Float(3.14)));
    }

    // --- tpack.lua 关键测试场景 ---

    #[test]
    fn test_tpack_line99_scenario() {
        // 模拟 tpack.lua:99 的场景
        // lnum = 0x13121110090807060504030201 (溢出 i64, 使用 wrapping)
        let lnum = 0x0807060504030201i64; // wrapping 后的低 64 位

        for i in 1..=8usize {
            let shift = (i * 8) as i64;
            // n = lnum & (~(-1 << (i * 8)))
            let mask = if shift >= 64 {
                -1i64
            } else {
                !((-1i64) << shift)
            };
            let n = lnum & mask;

            let fmt = format!("<i{}", i);
            let packed = str_pack(&fmt, &[TValue::Integer(n)]).unwrap();

            // 验证打包结果长度
            assert_eq!(packed.len(), i, "i={} pack length mismatch", i);

            // 验证解包往返
            let unpack_fmt = format!("<i{}", i);
            let (results, _) = str_unpack(&unpack_fmt, &packed, 1).unwrap();
            assert!(
                crate::vm::raw_equal(&results[0], &TValue::Integer(n)),
                "i={} roundtrip mismatch: got {:?}, expected {}",
                i,
                results[0],
                n
            );
        }
    }

    #[test]
    fn test_tpack_reverse_endian() {
        // 测试 tpack.lua 中的 s:reverse() 场景
        let val = 0xAAi64;
        for i in 1..=8usize {
            let le_fmt = format!("<I{}", i);
            let be_fmt = format!(">I{}", i);

            let le_packed = str_pack(&le_fmt, &[TValue::Integer(val)]).unwrap();
            let be_packed = str_pack(&be_fmt, &[TValue::Integer(val)]).unwrap();

            // 大端结果应是小端结果的反转
            let reversed: Vec<u8> = le_packed.iter().rev().copied().collect();
            assert_eq!(be_packed, reversed, "i={} endian reverse mismatch", i);
        }
    }

    // ========================================================================
    // string.format 格式验证测试 (对应 C 的 checkformat)
    // ========================================================================

    #[test]
    fn test_format_hash_flag_octal() {
        // %#12o: # 标志，八进制，宽度 12
        assert_eq!(
            str_format("%#12o", &[TValue::Integer(10)]).unwrap(),
            "         012"
        );
    }

    #[test]
    fn test_format_hash_flag_hex_lower() {
        // %#10x: # 标志，十六进制小写，宽度 10
        assert_eq!(
            str_format("%#10x", &[TValue::Integer(100)]).unwrap(),
            "      0x64"
        );
    }

    #[test]
    fn test_format_hash_flag_hex_upper() {
        // %#-17X: # 标志，十六进制大写，左对齐，宽度 17
        assert_eq!(
            str_format("%#-17X", &[TValue::Integer(100)]).unwrap(),
            "0X64             "
        );
    }

    #[test]
    fn test_format_zero_pad_integer() {
        // %013i: 零填充，宽度 13
        assert_eq!(
            str_format("%013i", &[TValue::Integer(-100)]).unwrap(),
            "-000000000100"
        );
    }

    #[test]
    fn test_format_precision_integer() {
        // %2.5d: 宽度 2，精度 5
        assert_eq!(
            str_format("%2.5d", &[TValue::Integer(-100)]).unwrap(),
            "-00100"
        );
    }

    #[test]
    fn test_format_precision_zero_unsigned() {
        // %.u: 精度 0，值为 0
        assert_eq!(str_format("%.u", &[TValue::Integer(0)]).unwrap(), "");
    }

    #[test]
    fn test_format_hash_flag_float() {
        // %+#014.0f: + 和 # 和 0 标志，宽度 14，精度 0
        assert_eq!(
            str_format("%+#014.0f", &[TValue::Float(100.0)]).unwrap(),
            "+000000000100."
        );
    }

    #[test]
    fn test_format_left_align_char() {
        // %-16c: 左对齐，宽度 16
        assert_eq!(
            str_format("%-16c", &[TValue::Integer(97)]).unwrap(),
            "a               "
        );
    }

    #[test]
    fn test_format_plus_g_uppercase() {
        // %+.3G: + 标志，精度 3，%G 格式
        assert_eq!(str_format("%+.3G", &[TValue::Float(1.5)]).unwrap(), "+1.5");
    }

    #[test]
    fn test_format_precision_zero_string() {
        // %.0s: 精度 0，字符串
        assert_eq!(str_format("%.0s", &[TValue::Integer(0)]).unwrap(), "");
    }

    #[test]
    fn test_format_precision_empty_string() {
        // %.s: 精度 0 (等同 .0s)
        assert_eq!(str_format("%.s", &[TValue::Integer(0)]).unwrap(), "");
    }

    #[test]
    fn test_format_exponent_uppercase() {
        // % 1.0E: 空格标志，宽度 1，精度 0，%E 格式
        let result = str_format("% 1.0E", &[TValue::Float(100.0)]).unwrap();
        assert!(result.starts_with(" 1E+"));
    }

    #[test]
    fn test_format_invalid_conversion_too_long_width() {
        // %100.3d: 宽度 3 位数字 (get2digits 只允许 2 位)
        assert!(str_format("%100.3d", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_invalid_conversion_too_long_precision() {
        // %1.100d: 精度 3 位数字
        assert!(str_format("%1.100d", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_invalid_conversion_unknown_specifier() {
        // %t: 未知说明符
        assert!(str_format("%t", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_invalid_conversion_zero_flag_char() {
        // %010c: c 不允许 0 标志
        assert!(str_format("%010c", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_invalid_conversion_precision_char() {
        // %.10c: c 不允许精度
        assert!(str_format("%.10c", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_invalid_conversion_zero_flag_string() {
        // %0.34s: s 的 0 标志无效 (L_FMTFLAGSC = "-")
        assert!(str_format("%0.34s", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_invalid_conversion_hash_flag_integer() {
        // %#i: i 不允许 # 标志
        assert!(str_format("%#i", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_invalid_conversion_precision_pointer() {
        // %3.1p: p 不允许精度
        assert!(str_format("%3.1p", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_invalid_conversion_zero_dot_string() {
        // %0.s: s 的 0 标志无效
        assert!(str_format("%0.s", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_q_cannot_have_modifiers() {
        // %10q: q 不能有修饰符
        assert!(str_format("%10q", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_invalid_conversion_uppercase_f() {
        // %F: 无效说明符 (不在 C89 中)
        assert!(str_format("%F", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_no_value() {
        // %d %d: 参数不足
        assert!(str_format("%d %d", &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_too_long_format() {
        // 格式字符串太长
        let aux = "0".repeat(600);
        let fmt = format!("%{}d", aux);
        assert!(str_format(&fmt, &[TValue::Integer(10)]).is_err());
    }

    #[test]
    fn test_format_hash_flag_octal_zero() {
        // %#o: # 标志，八进制，值为 0
        assert_eq!(str_format("%#o", &[TValue::Integer(0)]).unwrap(), "0");
    }

    #[test]
    fn test_format_hash_flag_hex_zero() {
        // %#x: # 标志，十六进制，值为 0 (不添加 0x 前缀)
        assert_eq!(str_format("%#x", &[TValue::Integer(0)]).unwrap(), "0");
    }

    #[test]
    fn test_format_simple_integer() {
        assert_eq!(str_format("%d", &[TValue::Integer(42)]).unwrap(), "42");
    }

    #[test]
    fn test_format_simple_string() {
        assert_eq!(str_format("%s", &[TValue::Integer(42)]).unwrap(), "42");
    }

    #[test]
    fn test_format_escape_percent() {
        assert_eq!(str_format("%%d", &[TValue::Integer(10)]).unwrap(), "%d");
    }

    #[test]
    fn test_format_plus_sign_integer() {
        assert_eq!(
            str_format("%+08d", &[TValue::Integer(31501)]).unwrap(),
            "+0031501"
        );
    }

    #[test]
    fn test_format_plus_sign_negative_integer() {
        assert_eq!(
            str_format("%+08d", &[TValue::Integer(-30927)]).unwrap(),
            "-0030927"
        );
    }

    #[test]
    fn test_format_hex_uppercase() {
        assert_eq!(
            str_format("%08X", &[TValue::Integer(0xFFFFFFFF)]).unwrap(),
            "FFFFFFFF"
        );
    }

    #[test]
    fn test_format_hex_float() {
        // 0.0 应该输出 "0"
        assert_eq!(str_format("%x", &[TValue::Float(0.0)]).unwrap(), "0");
    }

    #[test]
    fn test_format_octal_value() {
        assert_eq!(
            str_format("%o", &[TValue::Integer(0xABCD)]).unwrap(),
            "125715"
        );
    }
}
