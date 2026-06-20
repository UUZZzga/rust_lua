//! 字符串库 (lstrlib.cpp → Rust)
//!
//! 对应 C 源码: lstrlib.cpp
//!
//! ## 主要功能
//! - 创建字符串类型的默认元表 (string metatable)
//! - 元表包含算术元方法 (__add, __sub, __mul, __mod, __pow, __div, __idiv, __unm)
//! - __index 指向字符串库函数表 (string.len, string.sub 等)
//! - 注册 string 全局表，包含所有字符串库函数

use crate::objects::{LuaType, NilKind, TValue};
use crate::state::LuaState;
use crate::table::Table;
use crate::tm::{Metatable, TagMethod, make_tm_tvalue};
use crate::execute::VmError;
use std::sync::Arc;

// ============================================================================
// 字符串函数标签 (LightUserData 占位符值)
// ============================================================================
// 标签 1-6 已被内置函数 (print, setmetatable, getmetatable, type, pcall, error) 占用
// 字符串库函数使用标签 100+

pub const STR_UPPER: usize = 100;
pub const STR_LOWER: usize = 101;
pub const STR_LEN: usize = 102;
pub const STR_SUB: usize = 103;
pub const STR_REVERSE: usize = 104;
pub const STR_BYTE: usize = 105;
pub const STR_CHAR: usize = 106;
pub const STR_REP: usize = 107;
pub const STR_FIND: usize = 108;
pub const STR_FORMAT: usize = 109;
pub const STR_MATCH: usize = 110;
pub const STR_GMATCH: usize = 111;
pub const STR_GSUB: usize = 112;

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
        (TValue::Integer(i1), TValue::Integer(i2)) => {
            int_op(*i1, *i2).map(TValue::Integer)
        }
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
fn add_int(a: i64, b: i64) -> Option<i64> { Some(a.wrapping_add(b)) }
fn sub_int(a: i64, b: i64) -> Option<i64> { Some(a.wrapping_sub(b)) }
fn mul_int(a: i64, b: i64) -> Option<i64> { Some(a.wrapping_mul(b)) }
fn idiv_int(a: i64, b: i64) -> Option<i64> {
    if b == 0 { None } else { Some(a.div_euclid(b)) }
}
fn mod_int(a: i64, b: i64) -> Option<i64> {
    if b == 0 { None } else { Some(a.rem_euclid(b)) }
}

fn add_f(a: f64, b: f64) -> f64 { a + b }
fn sub_f(a: f64, b: f64) -> f64 { a - b }
fn mul_f(a: f64, b: f64) -> f64 { a * b }
fn div_f(a: f64, b: f64) -> f64 { a / b }
fn idiv_f(a: f64, b: f64) -> f64 { (a / b).floor() }
fn mod_f(a: f64, b: f64) -> f64 {
    if b == 0.0 { f64::NAN } else { a - (a / b).floor() * b }
}
fn pow_f(a: f64, b: f64) -> f64 { a.powf(b) }
fn unm_f(a: f64, _b: f64) -> f64 { -a }
fn unm_int(a: i64, _b: i64) -> Option<i64> { Some(a.wrapping_neg()) }

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
/// - pos >= 0: 返回 pos
/// - pos < -len: 返回 0
/// - 否则: 返回 len + pos + 1
fn get_end_pos(pos: i64, def: i64, len: usize) -> usize {
    let pos = if pos == 0 { def } else { pos };
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
pub fn str_sub(s: &str, start: i64, end: i64) -> String {
    let len = s.len();
    let start_pos = posrelat_i(start, len);
    let end_pos = get_end_pos(end, -1, len);
    if start_pos <= end_pos && start_pos <= len {
        let start_idx = start_pos.saturating_sub(1).min(len);
        let end_idx = end_pos.min(len);
        if start_idx < end_idx {
            s[start_idx..end_idx].to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    }
}

/// string.reverse(s) — 反转字符串
/// 对应 C 的 str_reverse
pub fn str_reverse(s: &str) -> String {
    s.bytes().rev().map(|b| b as char).collect()
}

/// string.byte(s, [i], [j]) — 返回字符的字节值
/// 对应 C 的 str_byte
pub fn str_byte(s: &str, i: i64, j: i64) -> Vec<i64> {
    let len = s.len();
    let posi = posrelat_i(i, len);
    let pose = get_end_pos(j, i, len);
    let mut result = Vec::new();
    if posi <= pose && posi <= len {
        let start = posi.saturating_sub(1).min(len);
        let end = pose.min(len);
        for &b in &s.as_bytes()[start..end] {
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
    String::from_utf8(bytes).map_err(|_| "invalid UTF-8 sequence".to_string())
}

/// string.rep(s, n, [sep]) — 重复字符串
/// 对应 C 的 str_rep
pub fn str_rep(s: &str, n: i64, sep: &str) -> String {
    if n <= 0 {
        return String::new();
    }
    let n = n as usize;
    if sep.is_empty() {
        s.repeat(n)
    } else {
        let mut result = String::with_capacity(n * s.len() + (n - 1) * sep.len());
        for i in 0..n {
            if i > 0 {
                result.push_str(sep);
            }
            result.push_str(s);
        }
        result
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
struct MatchState {
    src: Vec<u8>,
    src_init: usize,
    src_end: usize,
    pattern: Vec<u8>,
    p_end: usize,
    match_depth: i32,
    level: usize,
    captures: Vec<Capture>,
}

impl MatchState {
    fn new(src: &str, pattern: &str) -> Self {
        let src_bytes = src.as_bytes().to_vec();
        let pat_bytes = pattern.as_bytes().to_vec();
        MatchState {
            src_init: 0,
            src_end: src_bytes.len(),
            p_end: pat_bytes.len(),
            src: src_bytes,
            pattern: pat_bytes,
            match_depth: MAX_CCALLS,
            level: 0,
            captures: Vec::with_capacity(MAX_CAPTURES),
        }
    }

    fn src_byte(&self, idx: usize) -> u8 {
        self.src[idx]
    }

    fn pat_byte(&self, idx: usize) -> u8 {
        self.pattern[idx]
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
    if cl.is_ascii_lowercase() { res } else { !res }
}

/// 对应 C 的 matchbracketclass: 匹配方括号字符类
fn match_bracket_class(c: u8, p: &[u8], ec: usize) -> bool {
    let mut sig = true;
    let mut idx = 0;
    if p.len() > 1 && p[1] == b'^' {
        sig = false;
        idx = 1; // skip the '^'
    }
    idx += 1;
    while idx < ec {
        if p[idx] == b'%' {
            idx += 1;
            if idx < ec && match_class(c, p[idx]) {
                return sig;
            }
        } else if idx + 2 < ec && p[idx + 1] == b'-' {
            if p[idx] <= c && c <= p[idx + 2] {
                return sig;
            }
            idx += 2;
        } else if p[idx] == c {
            return sig;
        }
        idx += 1;
    }
    !sig
}

/// 对应 C 的 classend: 找到模式类的结束位置
fn class_end(ms: &MatchState, p: usize) -> Result<usize, String> {
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
            loop {
                if idx >= ms.p_end {
                    return Err("malformed pattern (missing ']')".to_string());
                }
                let c = ms.pat_byte(idx);
                idx += 1;
                if c == b'%' && idx < ms.p_end {
                    idx += 1;
                } else if c == b']' {
                    break;
                }
            }
            Ok(idx)
        }
        _ => Ok(p + 1),
    }
}

/// 对应 C 的 singlematch: 检查单个字符是否匹配
fn single_match(ms: &MatchState, s: usize, p: usize, ep: usize) -> bool {
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
        b'[' => match_bracket_class(c, &ms.pattern[p..ep], ep - p - 1),
        _ => ms.pat_byte(p) == c,
    }
}

/// 对应 C 的 matchbalance: 平衡匹配 %bxy
fn match_balance(ms: &MatchState, s: usize, p: usize) -> Option<usize> {
    if p + 1 >= ms.p_end {
        return None;
    }
    if s >= ms.src_end || ms.src_byte(s) != ms.pat_byte(p) {
        return None;
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
                return Some(idx + 1);
            }
        } else if c == b {
            cont += 1;
        }
        idx += 1;
    }
    None
}

/// 对应 C 的 check_capture
fn check_capture(ms: &MatchState, l: u8) -> Result<usize, String> {
    let l = (l - b'1') as usize;
    if l >= ms.level || ms.captures[l].len == CAP_UNFINISHED {
        return Err(format!("invalid capture index %{}", l + 1));
    }
    Ok(l)
}

/// 对应 C 的 capture_to_close
fn capture_to_close(ms: &MatchState) -> Result<usize, String> {
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
fn start_capture(ms: &mut MatchState, s: usize, p: usize, what: i32) -> Result<Option<usize>, String> {
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
fn end_capture(ms: &mut MatchState, s: usize, p: usize) -> Result<Option<usize>, String> {
    let l = capture_to_close(ms)?;
    ms.captures[l].len = (s - ms.captures[l].init) as i32;
    let res = match_pattern(ms, s, p)?;
    if res.is_none() {
        ms.captures[l].len = CAP_UNFINISHED;
    }
    Ok(res)
}

/// 对应 C 的 match_capture
fn match_capture(ms: &MatchState, s: usize, l: u8) -> Option<usize> {
    let l = check_capture(ms, l).ok()?;
    let len = ms.captures[l].len as usize;
    if s + len <= ms.src_end && ms.src[ms.captures[l].init..ms.captures[l].init + len] == ms.src[s..s + len] {
        Some(s + len)
    } else {
        None
    }
}

/// 对应 C 的 max_expand
fn max_expand(ms: &mut MatchState, s: usize, p: usize, ep: usize) -> Result<Option<usize>, String> {
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
fn min_expand(ms: &mut MatchState, s: usize, p: usize, ep: usize) -> Result<Option<usize>, String> {
    loop {
        let res = match_pattern(ms, s, ep + 1)?;
        if res.is_some() {
            return Ok(res);
        }
        if single_match(ms, s, p, ep) {
            if s + 1 > ms.src_end {
                return Ok(None);
            }
            return min_expand(ms, s + 1, p, ep);
        } else {
            return Ok(None);
        }
    }
}

/// 对应 C 的 match — 核心模式匹配函数
fn match_pattern(ms: &mut MatchState, s: usize, p: usize) -> Result<Option<usize>, String> {
    if ms.match_depth == 0 {
        return Err("pattern too complex".to_string());
    }
    ms.match_depth -= 1;
    let result = match_pattern_inner(ms, s, p);
    ms.match_depth += 1;
    result
}

fn match_pattern_inner(ms: &mut MatchState, mut s: usize, mut p: usize) -> Result<Option<usize>, String> {
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
                            let res = match_balance(ms, s, p + 2);
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
                            let previous = if s == ms.src_init { 0u8 } else { ms.src_byte(s - 1) };
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
                            let res = match_capture(ms, s, c);
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
fn get_one_capture(ms: &MatchState, i: usize, s: usize, e: usize) -> Result<CaptureResult, String> {
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
fn get_captures(ms: &MatchState, s: usize, e: usize) -> Result<Vec<TValue>, String> {
    let nlevels = if ms.level == 0 { 1 } else { ms.level };
    let mut result = Vec::with_capacity(nlevels);
    for i in 0..nlevels {
        let cap = get_one_capture(ms, i, s, e)?;
        match cap {
            CaptureResult::Str(start, len) => {
                let bytes = &ms.src[start..start + len];
                let s = String::from_utf8_lossy(bytes).into_owned();
                result.push(TValue::Str(crate::strings::LuaString::Short(
                    Arc::new(crate::strings::ShortString { hash: 0, contents: s })
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
        p.bytes().any(|c| matches!(c, b'^' | b'$' | b'*' | b'+' | b'?' | b'.' | b'(' | b'[' | b'%' | b'-'))
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
    let mut ms = MatchState::new(s, pattern);
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
        if let Ok(Some(end)) = match_pattern(&mut ms, search_pos, pat_start) {
            let captures = get_captures(&ms, search_pos, end)?;
            return Ok(FindResult::Found {
                start: search_pos + 1,
                end,
                captures,
            });
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
        FindResult::Found { start, end, captures } => {
            if captures.is_empty() {
                // 无捕获时返回整个匹配
                let matched = &s.as_bytes()[start - 1..end];
                let matched_str = String::from_utf8_lossy(matched).into_owned();
                Ok(vec![TValue::Str(crate::strings::LuaString::Short(
                    Arc::new(crate::strings::ShortString { hash: 0, contents: matched_str })
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
            let mut ms = MatchState::new(&self.src, &self.pattern);
            ms.level = 0;
            ms.captures.clear();
            ms.match_depth = MAX_CCALLS;
            let match_start = self.pos;
            if let Ok(Some(end)) = match_pattern(&mut ms, match_start, self.pat_start) {
                let captures = get_captures(&ms, match_start, end)?;
                // 推进位置: 如果匹配为空则前进 1 以避免无限循环
                self.pos = if end > match_start { end } else { match_start + 1 };
                if captures.is_empty() {
                    // 无捕获时返回整个匹配的子串
                    let matched_str = String::from_utf8_lossy(&src_bytes[match_start..end]).into_owned();
                    return Ok(vec![TValue::Str(crate::strings::LuaString::Short(
                        Arc::new(crate::strings::ShortString { hash: 0, contents: matched_str })
                    ))]);
                }
                return Ok(captures);
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

    let mut result = String::new();
    let mut src_pos = 0;
    let mut n = 0i64;
    let mut last_match_end: Option<usize> = None;

    while n < max_s && src_pos <= len {
        let mut ms = MatchState::new(s, pattern);
        ms.level = 0;
        ms.captures.clear();
        ms.match_depth = MAX_CCALLS;

        if let Ok(Some(end)) = match_pattern(&mut ms, src_pos, pat_start) {
            if Some(end) == last_match_end {
                // 避免空匹配的无限循环
                if src_pos < len {
                    result.push(src_bytes[src_pos] as char);
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
            let match_str = &src_bytes[src_pos..end];
            let match_str = String::from_utf8_lossy(match_str);
            let replacement = apply_replacement(repl, &ms, src_pos, end)?;
            result.push_str(&replacement);

            src_pos = end;
        } else if src_pos < len {
            result.push(src_bytes[src_pos] as char);
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
        result.push_str(&String::from_utf8_lossy(&src_bytes[src_pos..]));
    }

    Ok((result, n))
}

/// 处理替换字符串中的 %0, %1-%9
fn apply_replacement(repl: &str, ms: &MatchState, s: usize, e: usize) -> Result<String, String> {
    let mut result = String::new();
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
                result.push('%');
            } else if c == b'0' {
                let match_bytes = &ms.src[s..e];
                result.push_str(&String::from_utf8_lossy(match_bytes));
            } else if c.is_ascii_digit() {
                let cap_idx = (c - b'1') as usize;
                let cap = get_one_capture(ms, cap_idx, s, e)?;
                match cap {
                    CaptureResult::Str(start, len) => {
                        result.push_str(&String::from_utf8_lossy(&ms.src[start..start + len]));
                    }
                    CaptureResult::Pos(pos) => {
                        result.push_str(&pos.to_string());
                    }
                }
            } else {
                return Err(format!("invalid use of '%{}' in replacement string", c as char));
            }
            i += 1;
        } else {
            result.push(repl_bytes[i] as char);
            i += 1;
        }
    }
    Ok(result)
}

// ============================================================================
// string.format 实现 (对应 C 的 str_format)
// ============================================================================

/// string.format(fmt, ...) — 格式化字符串
/// 对应 C 的 str_format (简化版，支持常用格式)
pub fn str_format(fmt: &str, args: &[TValue]) -> Result<String, String> {
    let fmt_bytes = fmt.as_bytes();
    let mut result = String::new();
    let mut arg_idx = 0;
    let mut i = 0;

    while i < fmt_bytes.len() {
        if fmt_bytes[i] != b'%' {
            result.push(fmt_bytes[i] as char);
            i += 1;
            continue;
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

        // 解析格式说明符: flags, width, precision
        let fmt_start = i;
        // 解析 flags
        let mut left_align = false;
        let mut plus_sign = false;
        let mut space_sign = false;
        let mut zero_pad = false;
        while i < fmt_bytes.len() && b"-+ 0".contains(&fmt_bytes[i]) {
            match fmt_bytes[i] {
                b'-' => left_align = true,
                b'+' => plus_sign = true,
                b' ' => space_sign = true,
                b'0' => zero_pad = true,
                _ => {}
            }
            i += 1;
        }
        // 解析 width
        let mut width: usize = 0;
        while i < fmt_bytes.len() && fmt_bytes[i].is_ascii_digit() {
            width = width * 10 + (fmt_bytes[i] - b'0') as usize;
            i += 1;
        }
        // 解析 precision
        let mut precision: Option<usize> = None;
        if i < fmt_bytes.len() && fmt_bytes[i] == b'.' {
            i += 1;
            let mut prec: usize = 0;
            while i < fmt_bytes.len() && fmt_bytes[i].is_ascii_digit() {
                prec = prec * 10 + (fmt_bytes[i] - b'0') as usize;
                i += 1;
            }
            precision = Some(prec);
        }
        if i >= fmt_bytes.len() {
            return Err("invalid conversion specification".to_string());
        }

        let spec = fmt_bytes[i];
        let _ = fmt_start; // 保留用于调试
        i += 1;

        if arg_idx >= args.len() {
            return Err(format!("bad argument #{} to 'format' (no value)", arg_idx + 2));
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
                let n = arg.as_integer()
                    .ok_or_else(|| format!("bad argument #{} to 'format' (number expected, got {})", arg_idx, arg.ty()))?;
                let mut s = if n < 0 {
                    n.to_string()
                } else if plus_sign {
                    format!("+{}", n)
                } else if space_sign {
                    format!(" {}", n)
                } else {
                    n.to_string()
                };
                if zero_pad && width > s.len() {
                    let pad = width - s.len();
                    if n < 0 {
                        s = format!("-{}{}", "0".repeat(pad), s[1..].to_string());
                    } else if plus_sign {
                        s = format!("+{}{}", "0".repeat(pad), s[1..].to_string());
                    } else if space_sign {
                        s = format!(" {}{}", "0".repeat(pad), s[1..].to_string());
                    } else {
                        s = format!("{}{}", "0".repeat(pad), s);
                    }
                } else {
                    s = apply_width(s);
                }
                result.push_str(&s);
            }
            b'u' => {
                let n = arg.as_integer()
                    .ok_or_else(|| format!("bad argument #{} to 'format' (number expected, got {})", arg_idx, arg.ty()))?;
                let s = (n as u64).to_string();
                result.push_str(&apply_width(s));
            }
            b'o' => {
                let n = arg.as_integer()
                    .ok_or_else(|| format!("bad argument #{} to 'format' (number expected, got {})", arg_idx, arg.ty()))?;
                let s = format!("{:o}", n as u64);
                result.push_str(&apply_width(s));
            }
            b'x' => {
                let n = arg.as_integer()
                    .ok_or_else(|| format!("bad argument #{} to 'format' (number expected, got {})", arg_idx, arg.ty()))?;
                let s = format!("{:x}", n as u64);
                result.push_str(&apply_width(s));
            }
            b'X' => {
                let n = arg.as_integer()
                    .ok_or_else(|| format!("bad argument #{} to 'format' (number expected, got {})", arg_idx, arg.ty()))?;
                let s = format!("{:X}", n as u64);
                result.push_str(&apply_width(s));
            }
            b'c' => {
                let n = arg.as_integer()
                    .ok_or_else(|| format!("bad argument #{} to 'format' (number expected, got {})", arg_idx, arg.ty()))?;
                if n < 0 || n > 255 {
                    return Err("value out of range".to_string());
                }
                result.push(n as u8 as char);
            }
            b'f' | b'F' => {
                let n = arg.as_float()
                    .ok_or_else(|| format!("bad argument #{} to 'format' (number expected, got {})", arg_idx, arg.ty()))?;
                let s = match precision {
                    Some(p) => format!("{:.*}", p, n),
                    None => format!("{}", n),
                };
                result.push_str(&apply_width(s));
            }
            b'e' | b'E' => {
                let n = arg.as_float()
                    .ok_or_else(|| format!("bad argument #{} to 'format' (number expected, got {})", arg_idx, arg.ty()))?;
                let s = match precision {
                    Some(p) => format!("{:.*e}", p, n),
                    None => format!("{:e}", n),
                };
                result.push_str(&apply_width(s));
            }
            b'g' | b'G' => {
                let n = arg.as_float()
                    .ok_or_else(|| format!("bad argument #{} to 'format' (number expected, got {})", arg_idx, arg.ty()))?;
                let s = format!("{}", n);
                result.push_str(&apply_width(s));
            }
            b's' => {
                let s = match arg {
                    TValue::Str(s) => s.as_str().to_string(),
                    TValue::Integer(n) => n.to_string(),
                    TValue::Float(f) => {
                        if f.is_nan() { "nan".to_string() }
                        else if f.is_infinite() { if *f > 0.0 { "inf".to_string() } else { "-inf".to_string() } }
                        else { format!("{}", f) }
                    }
                    TValue::Nil(_) => "nil".to_string(),
                    TValue::Boolean(b) => b.to_string(),
                    _ => return Err(format!("bad argument #{} to 'format' (no proper format)", arg_idx)),
                };
                // 应用精度 (截断)
                let truncated = match precision {
                    Some(p) if p < s.len() => s[..p].to_string(),
                    _ => s,
                };
                result.push_str(&apply_width(truncated));
            }
            b'q' => {
                // 引用字符串
                result.push('"');
                match arg {
                    TValue::Str(s) => {
                        for c in s.as_str().bytes() {
                            match c {
                                b'"' | b'\\' | b'\n' => {
                                    result.push('\\');
                                    result.push(c as char);
                                }
                                _ if c.is_ascii_control() => {
                                    result.push_str(&format!("\\{}", c));
                                }
                                _ => result.push(c as char),
                            }
                        }
                    }
                    TValue::Integer(n) => result.push_str(&n.to_string()),
                    TValue::Float(f) => result.push_str(&format!("{}", f)),
                    TValue::Nil(_) => result.push_str("nil"),
                    TValue::Boolean(b) => result.push_str(&b.to_string()),
                    _ => return Err("value has no literal form".to_string()),
                }
                result.push('"');
            }
            _ => {
                return Err(format!("invalid conversion '%{}' to 'format'", spec as char));
            }
        }
    }
    Ok(result)
}

// ============================================================================
// 派发函数 — 从 execute.rs 的 op_call 调用
// ============================================================================

/// 从栈中读取字符串参数
fn get_str_arg(state: &LuaState, a: usize, idx: usize) -> Result<String, VmError> {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return Err(VmError::RuntimeError(format!(
            "bad argument #{} (string expected, got no value)",
            idx + 1
        )));
    }
    let val = &state.stack[stack_idx];
    match val {
        TValue::Str(s) => Ok(s.as_str().to_string()),
        TValue::Integer(n) => Ok(n.to_string()),
        TValue::Float(f) => Ok(format!("{}", f)),
        _ => Err(VmError::RuntimeError(format!(
            "bad argument #{} (string expected, got {})",
            idx + 1,
            val.ty()
        ))),
    }
}

/// 从栈中读取整数参数
fn get_int_arg(state: &LuaState, a: usize, idx: usize, default: i64) -> i64 {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return default;
    }
    match &state.stack[stack_idx] {
        TValue::Integer(n) => *n,
        TValue::Float(f) => *f as i64,
        TValue::Str(s) => s.as_str().parse::<i64>().unwrap_or(default),
        _ => default,
    }
}

/// 从栈中读取可选整数参数
fn get_opt_int_arg(state: &LuaState, a: usize, idx: usize, default: i64) -> i64 {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return default;
    }
    match &state.stack[stack_idx] {
        TValue::Nil(_) => default,
        TValue::Integer(n) => *n,
        TValue::Float(f) => *f as i64,
        TValue::Str(s) => s.as_str().parse::<i64>().unwrap_or(default),
        _ => default,
    }
}

/// 从栈中读取布尔参数
fn get_bool_arg(state: &LuaState, a: usize, idx: usize, default: bool) -> bool {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return default;
    }
    !state.stack[stack_idx].is_false()
}

/// 将结果压入栈并调整栈顶
fn push_results(state: &mut LuaState, a: usize, nresults: i32, results: Vec<TValue>) {
    state.stack.truncate(a);
    let nresults = if nresults < 0 { results.len() as i32 } else { nresults };
    for i in 0..nresults as usize {
        if i < results.len() {
            state.stack.push(results[i].clone());
        } else {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
    }
}

/// 字符串库函数派发
/// 从 execute.rs 的 op_call 调用，当 LightUserData 标签 >= 100 时
pub fn call_string_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    match tag {
        STR_UPPER => {
            let s = get_str_arg(state, a, 0)?;
            let result = str_upper(&s);
            push_results(state, a, nresults, vec![TValue::Str(state.intern_str(&result))]);
            Ok(())
        }
        STR_LOWER => {
            let s = get_str_arg(state, a, 0)?;
            let result = str_lower(&s);
            push_results(state, a, nresults, vec![TValue::Str(state.intern_str(&result))]);
            Ok(())
        }
        STR_LEN => {
            let s = get_str_arg(state, a, 0)?;
            let result = str_len(&s);
            push_results(state, a, nresults, vec![TValue::Integer(result)]);
            Ok(())
        }
        STR_SUB => {
            let s = get_str_arg(state, a, 0)?;
            let start = get_int_arg(state, a, 1, 1);
            let end = get_opt_int_arg(state, a, 2, -1);
            let result = str_sub(&s, start, end);
            push_results(state, a, nresults, vec![TValue::Str(state.intern_str(&result))]);
            Ok(())
        }
        STR_REVERSE => {
            let s = get_str_arg(state, a, 0)?;
            let result = str_reverse(&s);
            push_results(state, a, nresults, vec![TValue::Str(state.intern_str(&result))]);
            Ok(())
        }
        STR_BYTE => {
            let s = get_str_arg(state, a, 0)?;
            let i = get_opt_int_arg(state, a, 1, 1);
            let j = get_opt_int_arg(state, a, 2, i);
            let bytes = str_byte(&s, i, j);
            let results: Vec<TValue> = bytes.into_iter().map(TValue::Integer).collect();
            push_results(state, a, nresults, results);
            Ok(())
        }
        STR_CHAR => {
            let mut codes = Vec::new();
            for idx in 0..nargs {
                codes.push(get_int_arg(state, a, idx, 0));
            }
            match str_char(&codes) {
                Ok(result) => {
                    push_results(state, a, nresults, vec![TValue::Str(state.intern_str(&result))]);
                    Ok(())
                }
                Err(msg) => Err(VmError::RuntimeError(msg)),
            }
        }
        STR_REP => {
            let s = get_str_arg(state, a, 0)?;
            let n = get_int_arg(state, a, 1, 0);
            let sep = if nargs >= 3 {
                get_str_arg(state, a, 2).unwrap_or_default()
            } else {
                String::new()
            };
            let result = str_rep(&s, n, &sep);
            push_results(state, a, nresults, vec![TValue::Str(state.intern_str(&result))]);
            Ok(())
        }
        STR_FIND => {
            let s = get_str_arg(state, a, 0)?;
            let pattern = get_str_arg(state, a, 1)?;
            let init = get_opt_int_arg(state, a, 2, 1);
            let plain = get_bool_arg(state, a, 3, false);
            match str_find(&s, &pattern, init, plain) {
                Ok(FindResult::Found { start, end, captures }) => {
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
        STR_FORMAT => {
            let fmt = get_str_arg(state, a, 0)?;
            let args: Vec<TValue> = (1..nargs).map(|i| {
                let idx = a + 1 + i;
                if idx < state.stack.len() {
                    state.stack[idx].clone()
                } else {
                    TValue::Nil(NilKind::Strict)
                }
            }).collect();
            match str_format(&fmt, &args) {
                Ok(result) => {
                    push_results(state, a, nresults, vec![TValue::Str(state.intern_str(&result))]);
                    Ok(())
                }
                Err(msg) => Err(VmError::RuntimeError(msg)),
            }
        }
        STR_MATCH => {
            let s = get_str_arg(state, a, 0)?;
            let pattern = get_str_arg(state, a, 1)?;
            let init = get_opt_int_arg(state, a, 2, 1);
            match str_match(&s, &pattern, init) {
                Ok(results) => {
                    push_results(state, a, nresults, results);
                    Ok(())
                }
                Err(msg) => Err(VmError::RuntimeError(msg)),
            }
        }
        STR_GSUB => {
            let s = get_str_arg(state, a, 0)?;
            let pattern = get_str_arg(state, a, 1)?;
            let repl = get_str_arg(state, a, 2)?;
            let max_s = get_opt_int_arg(state, a, 3, -1);
            match str_gsub(&s, &pattern, &repl, max_s) {
                Ok((result, n)) => {
                    push_results(state, a, nresults, vec![
                        TValue::Str(state.intern_str(&result)),
                        TValue::Integer(n),
                    ]);
                    Ok(())
                }
                Err(msg) => Err(VmError::RuntimeError(msg)),
            }
        }
        STR_GMATCH => {
            // gmatch 返回一个迭代器函数
            // 简化实现: 使用 LightUserData 标签存储状态
            // 由于架构限制，gmatch 的完整实现需要闭包支持
            // 这里返回一个错误提示
            Err(VmError::RuntimeError("string.gmatch not fully supported in this build".to_string()))
        }
        _ => Err(VmError::RuntimeError(format!("unknown string function tag: {}", tag))),
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
    let mut lib = Table::new();
    // 注册所有字符串库函数，使用 LightUserData 标签
    // 重要: 必须使用 state.intern_str() 创建键，确保哈希值与后续查找时一致
    let register = |lib: &mut Table, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        lib.set(key, TValue::LightUserData(tag as *mut std::ffi::c_void));
    };
    register(&mut lib, "upper", STR_UPPER);
    register(&mut lib, "lower", STR_LOWER);
    register(&mut lib, "len", STR_LEN);
    register(&mut lib, "sub", STR_SUB);
    register(&mut lib, "reverse", STR_REVERSE);
    register(&mut lib, "byte", STR_BYTE);
    register(&mut lib, "char", STR_CHAR);
    register(&mut lib, "rep", STR_REP);
    register(&mut lib, "find", STR_FIND);
    register(&mut lib, "format", STR_FORMAT);
    register(&mut lib, "match", STR_MATCH);
    register(&mut lib, "gmatch", STR_GMATCH);
    register(&mut lib, "gsub", STR_GSUB);
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
        assert_eq!(get_end_pos(5, -1, 10), 5);
        assert_eq!(get_end_pos(0, -1, 10), 10); // 0 使用默认值 -1
    }

    #[test]
    fn test_get_end_pos_beyond_len() {
        assert_eq!(get_end_pos(20, -1, 10), 10);
    }

    #[test]
    fn test_get_end_pos_negative() {
        assert_eq!(get_end_pos(-1, -1, 10), 10);
        assert_eq!(get_end_pos(-3, -1, 10), 8);
    }

    #[test]
    fn test_get_end_pos_negative_out_of_range() {
        assert_eq!(get_end_pos(-20, -1, 10), 0);
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
        assert_eq!(str_sub("hello", 2, 0), "ello"); // 0 means default -1
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
    fn test_str_rep_basic() {
        assert_eq!(str_rep("ab", 3, ""), "ababab");
        assert_eq!(str_rep("ab", 1, ""), "ab");
        assert_eq!(str_rep("ab", 0, ""), "");
    }

    #[test]
    fn test_str_rep_with_sep() {
        assert_eq!(str_rep("ab", 3, ","), "ab,ab,ab");
        assert_eq!(str_rep("x", 3, "-"), "x-x-x");
    }

    #[test]
    fn test_str_rep_negative() {
        assert_eq!(str_rep("ab", -5, ""), "");
    }

    // ========================================================================
    // 模式匹配测试
    // ========================================================================

    #[test]
    fn test_str_find_plain() {
        let result = str_find("hello world", "world", 1, true).unwrap();
        match result {
            FindResult::Found { start, end, captures } => {
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
            FindResult::Found { start, end, captures } => {
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
        let args = vec![TValue::Str(crate::strings::LuaString::Short(
            Arc::new(crate::strings::ShortString { hash: 0, contents: "world".to_string() })
        ))];
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
            TValue::Str(crate::strings::LuaString::Short(
                Arc::new(crate::strings::ShortString { hash: 0, contents: "two".to_string() })
            )),
            TValue::Float(3.0),
        ];
        let result = str_format("%d %s %f", &args).unwrap();
        assert_eq!(result, "1 two 3");
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
        let mt = state.dmt.get(LuaType::String).expect("string metatable must exist");
        for tm in &[
            TagMethod::Add, TagMethod::Sub, TagMethod::Mul, TagMethod::Mod,
            TagMethod::Pow, TagMethod::Div, TagMethod::IDiv, TagMethod::Unm,
        ] {
            let key = make_tm_tvalue(*tm);
            assert!(mt.get(&key).is_some(), "metamethod {:?} must exist", tm);
        }
    }

    #[test]
    fn test_string_metatable_has_index() {
        let mut state = LuaState::new();
        create_string_metatable(&mut state);
        let mt = state.dmt.get(LuaType::String).expect("string metatable must exist");
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
            for name in &["upper", "lower", "len", "sub", "reverse", "byte", "char", "rep", "find", "format", "match", "gsub"] {
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
        let v = TValue::Str(crate::strings::LuaString::Short(
            Arc::new(crate::strings::ShortString { hash: 0, contents: "42".to_string() })
        ));
        let result = to_num(&v);
        assert_eq!(result, Some(TValue::Integer(42)));
    }

    #[test]
    fn test_to_num_string_float() {
        let v = TValue::Str(crate::strings::LuaString::Short(
            Arc::new(crate::strings::ShortString { hash: 0, contents: "3.14".to_string() })
        ));
        let result = to_num(&v);
        assert!(matches!(result, Some(TValue::Float(f)) if (f - 3.14).abs() < 1e-10));
    }

    #[test]
    fn test_to_num_invalid_string() {
        let v = TValue::Str(crate::strings::LuaString::Short(
            Arc::new(crate::strings::ShortString { hash: 0, contents: "abc".to_string() })
        ));
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
        let make_str = |s: &str| TValue::Str(crate::strings::LuaString::Short(
            Arc::new(crate::strings::ShortString { hash: 0, contents: s.to_string() })
        ));
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
        assert!(match_class(b'1', b'A'));  // '1' 不是字母，%A 应该匹配
    }

    #[test]
    fn test_match_class_space() {
        assert!(match_class(b' ', b's'));
        assert!(match_class(b'\t', b's'));
        assert!(!match_class(b'a', b's'));
    }

    // ========================================================================
    // call_string_function 测试
    // ========================================================================

    #[test]
    fn test_call_string_function_upper() {
        let mut state = LuaState::new();
        // 清空栈 (LuaState::new() 会预置一个 Nil)
        state.stack.clear();
        // 模拟栈: [func, "hello"]
        state.stack.push(TValue::LightUserData(STR_UPPER as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("hello")));
        let a = 0;
        let nargs = 1;
        let nresults = 1;
        call_string_function(STR_UPPER, &mut state, a, nargs, nresults).unwrap();
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "HELLO"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_string_function_len() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(STR_LEN as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("hello")));
        call_string_function(STR_LEN, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 5),
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_string_function_sub() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(STR_SUB as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("hello")));
        state.stack.push(TValue::Integer(2));
        state.stack.push(TValue::Integer(4));
        call_string_function(STR_SUB, &mut state, 0, 3, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "ell"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_string_function_reverse() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(STR_REVERSE as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("abc")));
        call_string_function(STR_REVERSE, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "cba"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_string_function_byte() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(STR_BYTE as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("AB")));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(2));
        call_string_function(STR_BYTE, &mut state, 0, 3, -1).unwrap();
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
    fn test_call_string_function_char() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(STR_CHAR as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(65));
        state.stack.push(TValue::Integer(66));
        call_string_function(STR_CHAR, &mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "AB"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_string_function_rep() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(STR_REP as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("ab")));
        state.stack.push(TValue::Integer(3));
        call_string_function(STR_REP, &mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "ababab"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_string_function_find() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(STR_FIND as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("hello world")));
        state.stack.push(TValue::Str(state.intern_str("world")));
        call_string_function(STR_FIND, &mut state, 0, 2, -1).unwrap();
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
    fn test_call_string_function_format() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(STR_FORMAT as *mut std::ffi::c_void));
        state.stack.push(TValue::Str(state.intern_str("hello %s")));
        state.stack.push(TValue::Str(state.intern_str("world")));
        call_string_function(STR_FORMAT, &mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "hello world"),
            _ => panic!("expected string result"),
        }
    }

    #[test]
    fn test_call_string_function_unknown_tag() {
        let mut state = LuaState::new();
        let result = call_string_function(999, &mut state, 0, 0, 0);
        assert!(result.is_err());
    }
}
