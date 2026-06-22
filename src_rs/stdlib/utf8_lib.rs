//! UTF-8 库 (lutf8lib.cpp → Rust)
//!
//! 对应 C 源码: lutf8lib.cpp
//!
//! ## 主要功能
//! - 注册 utf8 全局表，包含 UTF-8 操作函数
//! - 提供 utf8.offset, utf8.codepoint, utf8.char, utf8.len, utf8.codes 函数
//! - 提供 utf8.charpattern 模式字符串
//!
//! ## 标签分配
//! - 标签 1-19: 基础库
//! - 标签 100+: 字符串库
//! - 标签 200+: 数学库
//! - 标签 300+: UTF-8 库

use crate::objects::{NilKind, TValue};
use crate::state::LuaState;
use crate::execute::VmError;

// ============================================================================
// 常量 (对应 C 源码的宏定义)
// ============================================================================

/// 最大 Unicode 码点 — 对应 C 的 MAXUNICODE
const MAXUNICODE: u32 = 0x10FFFF;

/// 最大 UTF-8 可编码值 — 对应 C 的 MAXUTF
const MAXUTF: u32 = 0x7FFFFFFF;

/// 无效 UTF-8 码的错误消息 — 对应 C 的 MSGInvalid
const MSG_INVALID: &str = "invalid UTF-8 code";

/// UTF-8 字符模式 — 对应 C 的 UTF8PATT
///
/// 原始模式为 "[\0-\x7F\xC2-\xFD][\x80-\xBF]*"，包含非 ASCII 字节，
/// 因此存储为字节切片。使用时通过 unsafe 转换为 Lua 字符串。
const UTF8PATT_BYTES: &[u8] = &[
    b'[', 0, b'-', 0x7F, 0xC2, b'-', 0xFD, b']',
    b'[', 0x80, b'-', 0xBF, b']', b'*',
];

// ============================================================================
// 函数标签 (LightUserData 占位符值)
// ============================================================================

pub const UTF8_OFFSET: usize = 300;
pub const UTF8_CODEPOINT: usize = 301;
pub const UTF8_CHAR: usize = 302;
pub const UTF8_LEN: usize = 303;
pub const UTF8_CODES: usize = 304;
pub const UTF8_ITER_STRICT: usize = 305;
pub const UTF8_ITER_LAX: usize = 306;

/// UTF-8 库标签范围: [300, 310)
pub fn is_utf8_tag(tag: usize) -> bool {
    (300..310).contains(&tag)
}

/// 将 utf8 库函数 tag 映射到函数名（用于 traceback）
pub fn utf8_function_name(tag: usize) -> Option<&'static str> {
    match tag {
        UTF8_OFFSET => Some("offset"),
        UTF8_CODEPOINT => Some("codepoint"),
        UTF8_CHAR => Some("char"),
        UTF8_LEN => Some("len"),
        UTF8_CODES => Some("codes"),
        UTF8_ITER_STRICT | UTF8_ITER_LAX => Some("iter"),
        _ => None,
    }
}

// ============================================================================
// 辅助函数 (对应 C 源码的内联函数和宏)
// ============================================================================

/// 检查字节是否为续字节 — 对应 C 的 iscont 宏
#[inline]
fn iscont(c: u8) -> bool {
    (c & 0xC0) == 0x80
}

/// 检查指针位置的字节是否为续字节 — 对应 C 的 iscontp 宏
#[inline]
fn iscontp(s: &[u8], idx: usize) -> bool {
    idx < s.len() && iscont(s[idx])
}

/// 将相对位置转换为绝对位置 — 对应 C 的 u_posrelat
///
/// 与 C 版本一致:
/// - pos >= 0: 返回 pos
/// - pos < 0 且 |pos| > len: 返回 0
/// - pos < 0 且 |pos| <= len: 返回 len + pos + 1
fn u_posrelat(pos: i64, len: usize) -> i64 {
    if pos >= 0 {
        pos
    } else {
        let abs_pos = (-pos) as usize;
        if abs_pos > len {
            0
        } else {
            len as i64 + pos + 1
        }
    }
}

// ============================================================================
// UTF-8 解码 — 对应 C 的 utf8_decode
// ============================================================================

/// 解码一个 UTF-8 序列
///
/// 对应 C 的 utf8_decode 函数:
/// - 返回 Some((bytes_consumed, code_point)) 如果序列有效
/// - 返回 None 如果序列无效
///
/// `strict` 为 true 时，额外检查码点是否为代理区或超过 MAXUNICODE
fn utf8_decode(s: &[u8], strict: bool) -> Option<(usize, u32)> {
    if s.is_empty() {
        return None;
    }

    /// 各序列长度的最小值，用于检查过长表示 — 对应 C 的 limits 数组
    static LIMITS: [u32; 6] = [u32::MAX, 0x80, 0x800, 0x10000, 0x200000, 0x4000000];

    let c = s[0] as u32;
    let mut res: u32 = 0;

    if c < 0x80 {
        // ASCII 字符
        res = c;
        if strict && res > MAXUNICODE {
            return None;
        }
        return Some((1, res));
    }

    // 多字节序列
    let mut count = 0usize;
    let mut c_shift = c;
    while c_shift & 0x40 != 0 {
        count += 1;
        if count >= s.len() {
            return None; // 字符串太短
        }
        let cc = s[count] as u32;
        if !iscont(cc as u8) {
            return None; // 不是续字节
        }
        res = (res << 6) | (cc & 0x3F);
        c_shift <<= 1;
    }

    // 添加首字节的数据位
    res |= (c_shift & 0x7F) << (count * 5);

    // 验证
    if count > 5 || res > MAXUTF || res < LIMITS[count] {
        return None;
    }

    if strict {
        // 检查无效码点: 过大或代理区
        if res > MAXUNICODE || (0xD800..=0xDFFF).contains(&res) {
            return None;
        }
    }

    Some((count + 1, res))
}

// ============================================================================
// UTF-8 编码 — 对应 C 的 lua_pushfstring(L, "%U", code)
// ============================================================================

/// 将码点编码为 UTF-8 字节序列
///
/// 对应 C 中 lua_pushfstring 的 %U 格式说明符
fn utf8_encode(code: u32) -> Vec<u8> {
    if code < 0x80 {
        vec![code as u8]
    } else if code < 0x800 {
        vec![
            0xC0 | ((code >> 6) as u8),
            0x80 | ((code & 0x3F) as u8),
        ]
    } else if code < 0x10000 {
        vec![
            0xE0 | ((code >> 12) as u8),
            0x80 | (((code >> 6) & 0x3F) as u8),
            0x80 | ((code & 0x3F) as u8),
        ]
    } else if code < 0x200000 {
        vec![
            0xF0 | ((code >> 18) as u8),
            0x80 | (((code >> 12) & 0x3F) as u8),
            0x80 | (((code >> 6) & 0x3F) as u8),
            0x80 | ((code & 0x3F) as u8),
        ]
    } else if code < 0x4000000 {
        vec![
            0xF8 | ((code >> 24) as u8),
            0x80 | (((code >> 18) & 0x3F) as u8),
            0x80 | (((code >> 12) & 0x3F) as u8),
            0x80 | (((code >> 6) & 0x3F) as u8),
            0x80 | ((code & 0x3F) as u8),
        ]
    } else {
        vec![
            0xFC | ((code >> 30) as u8),
            0x80 | (((code >> 24) & 0x3F) as u8),
            0x80 | (((code >> 18) & 0x3F) as u8),
            0x80 | (((code >> 12) & 0x3F) as u8),
            0x80 | (((code >> 6) & 0x3F) as u8),
            0x80 | ((code & 0x3F) as u8),
        ]
    }
}

// ============================================================================
// 栈操作辅助函数
// ============================================================================

/// 从栈中读取字符串参数（返回字节切片）
fn get_str_bytes(state: &LuaState, a: usize, idx: usize) -> Result<Vec<u8>, VmError> {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return Err(VmError::RuntimeError(format!(
            "bad argument #{} (string expected, got no value)",
            idx + 1
        )));
    }
    match &state.stack[stack_idx] {
        TValue::Str(s) => Ok(s.as_str().as_bytes().to_vec()),
        TValue::Integer(n) => Ok(n.to_string().into_bytes()),
        TValue::Float(f) => Ok(format!("{}", f).into_bytes()),
        _ => Err(VmError::RuntimeError(format!(
            "bad argument #{} (string expected, got {})",
            idx + 1,
            state.stack[stack_idx].ty()
        ))),
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

/// 从栈中读取必需的整数参数（带错误消息）
fn get_required_int_arg(state: &LuaState, a: usize, idx: usize, fname: &str) -> Result<i64, VmError> {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return Err(VmError::RuntimeError(format!(
            "bad argument #{} to '{}' (number expected, got no value)",
            idx + 1, fname
        )));
    }
    match &state.stack[stack_idx] {
        TValue::Integer(n) => Ok(*n),
        TValue::Float(f) => Ok(*f as i64),
        TValue::Str(s) => s.as_str().parse::<i64>().map_err(|_| {
            VmError::RuntimeError(format!(
                "bad argument #{} to '{}' (number expected, got string)",
                idx + 1, fname
            ))
        }),
        _ => Err(VmError::RuntimeError(format!(
            "bad argument #{} to '{}' (number expected, got {})",
            idx + 1, fname, state.stack[stack_idx].ty()
        ))),
    }
}

/// 将结果压入栈并调整栈顶
fn push_results(state: &mut LuaState, a: usize, nresults: i32, results: Vec<TValue>) {
    state.stack.truncate(a);
    let n = if nresults < 0 {
        results.len()
    } else {
        nresults as usize
    };
    for i in 0..n {
        if i < results.len() {
            state.stack.push(results[i].clone());
        } else {
            state.stack.push(TValue::Nil(NilKind::Strict));
        }
    }
}

/// 将字节序列创建为 Lua 字符串（使用 unsafe 绕过 UTF-8 校验，与 string 库一致）
fn bytes_to_lua_str(state: &LuaState, bytes: &[u8]) -> TValue {
    let s = unsafe { String::from_utf8_unchecked(bytes.to_vec()) };
    TValue::Str(state.intern_str(&s))
}

// ============================================================================
// 函数实现 (对应 C 源码的各个函数)
// ============================================================================

/// utf8.codepoint(s, [i, [j [, lax]]]) — 对应 C 的 codepoint
fn utf8_codepoint_impl(s: &[u8], posi: i64, pose: i64, lax: bool) -> Result<Vec<u32>, String> {
    let len = s.len();
    let posi = u_posrelat(posi, len);
    let pose = u_posrelat(pose, len);

    if posi < 1 {
        return Err("out of bounds".to_string());
    }
    if pose > len as i64 {
        return Err("out of bounds".to_string());
    }
    if posi > pose {
        return Ok(Vec::new()); // 空区间
    }

    let mut result = Vec::new();
    let mut i = (posi - 1) as usize;
    let end = pose as usize;
    while i < end {
        match utf8_decode(&s[i..], !lax) {
            Some((consumed, code)) => {
                result.push(code);
                i += consumed;
            }
            None => {
                return Err(MSG_INVALID.to_string());
            }
        }
    }
    Ok(result)
}

/// utf8.char(n1, n2, ...) — 对应 C 的 utfchar
fn utf8_char_impl(codes: &[i64]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    for &c in codes {
        let code = c as u32;
        if code > MAXUTF {
            return Err("value out of range".to_string());
        }
        bytes.extend_from_slice(&utf8_encode(code));
    }
    Ok(bytes)
}

/// utf8.offset(s, n, [i]) — 对应 C 的 byteoffset
///
/// 返回第 n 个字符（从位置 i 开始计数）的起始和结束字节位置
/// n=0 表示位置 i 处的当前字符
fn utf8_offset_impl(s: &[u8], n: i64, posi_arg: i64) -> Result<Option<(i64, i64)>, String> {
    let len = s.len();

    // 默认位置: n >= 0 时为 1, n < 0 时为 len + 1
    let _default_posi = if n >= 0 { 1 } else { len as i64 + 1 };
    let posi = u_posrelat(posi_arg, len);

    // 边界检查: 1 <= posi <= len + 1
    if !(1 <= posi && posi <= len as i64 + 1) {
        return Err("position out of bounds".to_string());
    }
    let mut posi = (posi - 1) as usize; // 转为 0-based

    if n == 0 {
        // 找到当前字节序列的起始位置
        while posi > 0 && iscontp(s, posi) {
            posi -= 1;
        }
    } else {
        // 检查初始位置是否为续字节
        if iscontp(s, posi) {
            return Err("initial position is a continuation byte".to_string());
        }
        if n < 0 {
            let mut n = n;
            while n < 0 && posi > 0 {
                // 找到前一个字符的起始位置
                loop {
                    posi -= 1;
                    if !(posi > 0 && iscontp(s, posi)) {
                        break;
                    }
                }
                n += 1;
            }
            if n != 0 {
                return Ok(None); // 未找到
            }
        } else {
            let mut n = n - 1; // 第一个字符不需要移动
            while n > 0 && posi < len {
                // 找到下一个字符的起始位置
                loop {
                    posi += 1;
                    if !iscontp(s, posi) {
                        break;
                    }
                }
                n -= 1;
            }
            if n != 0 {
                return Ok(None); // 未找到
            }
        }
    }

    // 计算初始和结束位置
    let init_pos = posi + 1; // 1-based

    // C 中 s[len] 是 '\0'（null 终止符），Rust 中没有，需边界检查
    if posi < len && (s[posi] & 0x80) != 0 {
        // 多字节字符
        if iscont(s[posi]) {
            return Err("initial position is a continuation byte".to_string());
        }
        // 跳到最后一个续字节
        while posi + 1 < len && iscont(s[posi + 1]) {
            posi += 1;
        }
    }
    // 单字节字符: 结束位置 = 初始位置

    let final_pos = posi + 1; // 1-based
    Ok(Some((init_pos as i64, final_pos as i64)))
}

/// utf8 迭代器辅助函数 — 对应 C 的 iter_aux
///
/// 参数: s (字符串), n (当前位置, 0-based)
/// 返回: (next_position, codepoint) 或空 Vec (结束)
#[cfg(test)]
fn utf8_iter_aux_impl(s: &[u8], n_arg: i64, strict: bool) -> Vec<TValue> {
    let len = s.len();
    // 将 n 视为无符号 (对应 C 的 lua_Unsigned)
    let n = if n_arg < 0 {
        // 负数转为很大的正数，会被 n >= len 检查捕获
        len + 1
    } else {
        n_arg as usize
    };

    let mut n = n;
    if n < len {
        // 跳到下一个字符的起始位置（跳过续字节）
        while n < len && iscont(s[n]) {
            n += 1;
        }
    }
    if n >= len {
        return Vec::new(); // 没有更多码点
    }

    match utf8_decode(&s[n..], strict) {
        Some((_, code)) => {
            vec![
                TValue::Integer((n + 1) as i64), // 1-based 位置
                TValue::Integer(code as i64),
            ]
        }
        None => {
            // 无效 UTF-8 — 通过返回错误值让调用方处理
            // 在 C 中这是 luaL_error，这里我们返回特殊标记
            Vec::new()
        }
    }
}

// ============================================================================
// 函数派发 — 从 execute.rs 调用
// ============================================================================

/// UTF-8 库函数派发
///
/// 从 execute.rs 的 op_call 或 state.rs 的 pcall 调用,
/// 当 LightUserData 标签在 [300, 310) 范围内时。
pub fn call_utf8_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let prev_c_func = state.last_c_function.take();
    state.last_c_function = utf8_function_name(tag).map(|s| s.to_string());

    let result = match tag {
        UTF8_OFFSET => call_offset(state, a, nargs, nresults),
        UTF8_CODEPOINT => call_codepoint(state, a, nargs, nresults),
        UTF8_CHAR => call_char(state, a, nargs, nresults),
        UTF8_LEN => call_len(state, a, nargs, nresults),
        UTF8_CODES => call_codes(state, a, nargs, nresults),
        UTF8_ITER_STRICT => call_iter(state, a, nargs, nresults, true),
        UTF8_ITER_LAX => call_iter(state, a, nargs, nresults, false),
        _ => Err(VmError::RuntimeError(format!(
            "unknown utf8 function tag: {}",
            tag
        ))),
    };

    if result.is_ok() {
        state.last_c_function = prev_c_func;
    }
    result
}

/// utf8.offset(s, n, [i]) — 对应 C 的 byteoffset
fn call_offset(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_bytes(state, a, 0)?;
    let n = get_required_int_arg(state, a, 1, "offset")?;
    let default_posi = if n >= 0 { 1 } else { s.len() as i64 + 1 };
    let posi = if nargs >= 3 {
        get_opt_int_arg(state, a, 2, default_posi)
    } else {
        default_posi
    };

    match utf8_offset_impl(&s, n, posi) {
        Ok(Some((init_pos, final_pos))) => {
            push_results(state, a, nresults, vec![
                TValue::Integer(init_pos),
                TValue::Integer(final_pos),
            ]);
            Ok(())
        }
        Ok(None) => {
            // 未找到字符
            push_results(state, a, nresults, vec![TValue::Nil(NilKind::Strict)]);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// utf8.codepoint(s, [i, [j [, lax]]]) — 对应 C 的 codepoint
fn call_codepoint(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_bytes(state, a, 0)?;
    let posi = if nargs >= 2 { get_opt_int_arg(state, a, 1, 1) } else { 1 };
    let pose = if nargs >= 3 { get_opt_int_arg(state, a, 2, posi) } else { posi };
    let lax = nargs >= 4 && get_bool_arg(state, a, 3, false);

    match utf8_codepoint_impl(&s, posi, pose, lax) {
        Ok(codes) => {
            let results: Vec<TValue> = codes
                .into_iter()
                .map(|c| TValue::Integer(c as i64))
                .collect();
            push_results(state, a, nresults, results);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// utf8.char(n1, n2, ...) — 对应 C 的 utfchar
fn call_char(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let mut codes = Vec::with_capacity(nargs);
    for i in 0..nargs {
        let c = get_required_int_arg(state, a, i, "char")?;
        codes.push(c);
    }

    match utf8_char_impl(&codes) {
        Ok(bytes) => {
            let result = bytes_to_lua_str(state, &bytes);
            push_results(state, a, nresults, vec![result]);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// utf8.len(s [, i [, j [, lax]]]) — 对应 C 的 utflen
fn call_len(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_bytes(state, a, 0)?;
    let len = s.len();
    let posi = if nargs >= 2 { get_opt_int_arg(state, a, 1, 1) } else { 1 };
    let posj = if nargs >= 3 { get_opt_int_arg(state, a, 2, -1) } else { -1 };
    let lax = nargs >= 4 && get_bool_arg(state, a, 3, false);

    // 边界检查 (对应 C 的 argcheck)
    // C: 1 <= posi && --posi <= len  (posi 先转 0-based)
    // C: --posj < len  (posj 先转 0-based)
    let posi_rel = u_posrelat(posi, len);
    if !(1 <= posi_rel && posi_rel - 1 <= len as i64) {
        return Err(VmError::RuntimeError("initial position out of bounds".to_string()));
    }
    let posj_rel = u_posrelat(posj, len);
    if !(posj_rel - 1 < len as i64) {
        return Err(VmError::RuntimeError("final position out of bounds".to_string()));
    }

    // 转为 0-based (对应 C 的 --posi, --posj)
    // 使用 i64 避免空字符串时 posj_rel=0 导致 usize 下溢
    let mut i = posi_rel - 1;
    let j = posj_rel - 1;
    let mut n: i64 = 0;

    while i <= j {
        if i < 0 || i as usize >= len {
            break;
        }
        match utf8_decode(&s[i as usize..], !lax) {
            Some((consumed, _)) => {
                i += consumed as i64;
                n += 1;
            }
            None => {
                // 返回 nil + 当前位置 (1-based)
                push_results(state, a, nresults, vec![
                    TValue::Nil(NilKind::Strict),
                    TValue::Integer(i + 1),
                ]);
                return Ok(());
            }
        }
    }

    push_results(state, a, nresults, vec![TValue::Integer(n)]);
    Ok(())
}

/// utf8.codes(s, [lax]) — 对应 C 的 iter_codes
///
/// 返回 3 个值: 迭代器函数, 字符串 s, 初始位置 0
fn call_codes(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let s = get_str_bytes(state, a, 0)?;
    let lax = nargs >= 2 && get_bool_arg(state, a, 1, false);

    // 检查字符串首字节不是续字节
    if !s.is_empty() && iscont(s[0]) {
        return Err(VmError::RuntimeError(MSG_INVALID.to_string()));
    }

    let iter_tag = if lax { UTF8_ITER_LAX } else { UTF8_ITER_STRICT };
    let iter_val = TValue::LightUserData(iter_tag as *mut std::ffi::c_void);
    let s_val = state.stack[a + 1].clone();
    let init_pos = TValue::Integer(0);

    push_results(state, a, nresults, vec![iter_val, s_val, init_pos]);
    Ok(())
}

/// utf8 迭代器函数 — 对应 C 的 iter_auxstrict / iter_auxlax
///
/// 在 TFORCALL 中调用，参数: s (字符串), n (当前位置)
fn call_iter(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
    strict: bool,
) -> Result<(), VmError> {
    let s_val = if a + 1 < state.stack.len() {
        state.stack[a + 1].clone()
    } else {
        TValue::Nil(NilKind::Strict)
    };
    let n_val = if a + 2 < state.stack.len() {
        state.stack[a + 2].clone()
    } else {
        TValue::Nil(NilKind::Strict)
    };

    let s_bytes: Vec<u8> = match &s_val {
        TValue::Str(s) => s.as_str().as_bytes().to_vec(),
        _ => return Err(VmError::RuntimeError(
            "bad argument #1 to 'iter' (string expected)".to_string(),
        )),
    };
    let n: i64 = match &n_val {
        TValue::Integer(n) => *n,
        TValue::Float(f) => *f as i64,
        _ => return Err(VmError::RuntimeError(
            "bad argument #2 to 'iter' (number expected)".to_string(),
        )),
    };

    let len = s_bytes.len();
    let mut pos = if n < 0 {
        len + 1 // 负数会被 n >= len 检查捕获
    } else {
        n as usize
    };

    if pos < len {
        // 跳到下一个字符的起始位置（跳过续字节）
        while pos < len && iscont(s_bytes[pos]) {
            pos += 1;
        }
    }

    if pos >= len {
        // 没有更多码点
        push_results(state, a, nresults, vec![TValue::Nil(NilKind::Strict)]);
        return Ok(());
    }

    match utf8_decode(&s_bytes[pos..], strict) {
        Some((consumed, code)) => {
            // 对应 C 代码的 iscontp(next) 检查:
            // 解码后,下一个字节不应是续字节,否则说明存在游离的续字节
            // 例如 "in\x80valid": 解码 'n' 后,下一个字节是 \x80 (续字节),应报错
            let next_pos = pos + consumed;
            if next_pos < len && iscont(s_bytes[next_pos]) {
                return Err(VmError::RuntimeError(MSG_INVALID.to_string()));
            }
            push_results(state, a, nresults, vec![
                TValue::Integer((pos + 1) as i64), // 1-based 位置
                TValue::Integer(code as i64),
            ]);
            Ok(())
        }
        None => Err(VmError::RuntimeError(MSG_INVALID.to_string())),
    }
}

// ============================================================================
// 打开 UTF-8 库 — 对应 C 的 luaopen_utf8
// ============================================================================

/// 打开 UTF-8 库, 注册所有 UTF-8 函数和 charpattern
///
/// 对应 C 源码 lutf8lib.cpp 的 luaopen_utf8 函数:
/// 1. 创建 utf8 库函数表
/// 2. 设置 charpattern 字段
/// 3. 返回库表 (调用者负责注册到全局或 package.loaded)
pub fn create_utf8_lib_table(state: &LuaState) -> crate::table::Table {
    let mut lib = crate::table::Table::new();

    let register = |lib: &mut crate::table::Table, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        lib.set(key, TValue::LightUserData(tag as *mut std::ffi::c_void));
    };

    register(&mut lib, "offset", UTF8_OFFSET);
    register(&mut lib, "codepoint", UTF8_CODEPOINT);
    register(&mut lib, "char", UTF8_CHAR);
    register(&mut lib, "len", UTF8_LEN);
    register(&mut lib, "codes", UTF8_CODES);

    // 设置 charpattern 字段
    let pattern_str = unsafe { String::from_utf8_unchecked(UTF8PATT_BYTES.to_vec()) };
    lib.set(
        TValue::Str(state.intern_str("charpattern")),
        TValue::Str(state.intern_str(&pattern_str)),
    );

    lib
}

/// 打开 UTF-8 库并注册到全局变量 utf8
pub fn open_utf8_lib(state: &mut LuaState) {
    let lib = create_utf8_lib_table(state);
    let key = TValue::Str(state.intern_str("utf8"));
    state.globals.set(key, TValue::Table(lib));
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // utf8_decode 测试
    // ========================================================================

    #[test]
    fn test_utf8_decode_ascii() {
        assert_eq!(utf8_decode(b"A", false), Some((1, 0x41)));
        assert_eq!(utf8_decode(b"\x7F", false), Some((1, 0x7F)));
        assert_eq!(utf8_decode(b"\x00", false), Some((1, 0x00)));
    }

    #[test]
    fn test_utf8_decode_multibyte() {
        // 2-byte: U+0080
        assert_eq!(utf8_decode(&[0xC2, 0x80], false), Some((2, 0x80)));
        // 2-byte: U+07FF
        assert_eq!(utf8_decode(&[0xDF, 0xBF], false), Some((2, 0x7FF)));
        // 3-byte: U+0800
        assert_eq!(utf8_decode(&[0xE0, 0xA0, 0x80], false), Some((3, 0x800)));
        // 3-byte: U+FFFF
        assert_eq!(utf8_decode(&[0xEF, 0xBF, 0xBF], false), Some((3, 0xFFFF)));
        // 4-byte: U+10000
        assert_eq!(utf8_decode(&[0xF0, 0x90, 0x80, 0x80], false), Some((4, 0x10000)));
        // 4-byte: U+10FFFF
        assert_eq!(utf8_decode(&[0xF4, 0x8F, 0xBF, 0xBF], false), Some((4, 0x10FFFF)));
    }

    #[test]
    fn test_utf8_decode_strict_surrogates() {
        // 代理区码点在 strict 模式下应失败
        assert_eq!(utf8_decode(&[0xED, 0xA0, 0x80], true), None); // U+D800
        assert_eq!(utf8_decode(&[0xED, 0xBF, 0xBF], true), None); // U+DFFF
        // 在 lax 模式下应成功
        assert_eq!(utf8_decode(&[0xED, 0xA0, 0x80], false), Some((3, 0xD800)));
        assert_eq!(utf8_decode(&[0xED, 0xBF, 0xBF], false), Some((3, 0xDFFF)));
    }

    #[test]
    fn test_utf8_decode_overlong() {
        // 过长表示应失败
        assert_eq!(utf8_decode(&[0xC0, 0x80], false), None); // U+0000 用 2 字节
        assert_eq!(utf8_decode(&[0xC1, 0xBF], false), None); // U+007F 用 2 字节
        assert_eq!(utf8_decode(&[0xE0, 0x9F, 0xBF], false), None); // U+07FF 用 3 字节
        assert_eq!(utf8_decode(&[0xF0, 0x8F, 0xBF, 0xBF], false), None); // U+FFFF 用 4 字节
    }

    #[test]
    fn test_utf8_decode_invalid() {
        assert_eq!(utf8_decode(&[0x80], false), None); // 续字节
        assert_eq!(utf8_decode(&[0xBF], false), None); // 续字节
        assert_eq!(utf8_decode(&[0xFE], false), None); // 无效字节
        assert_eq!(utf8_decode(&[0xFF], false), None); // 无效字节
        assert_eq!(utf8_decode(&[0xE0], false), None); // 不完整的 3 字节序列
        assert_eq!(utf8_decode(&[0xE0, 0xA0], false), None); // 不完整的 3 字节序列
    }

    // ========================================================================
    // utf8_encode 测试
    // ========================================================================

    #[test]
    fn test_utf8_encode_ascii() {
        assert_eq!(utf8_encode(0x00), vec![0x00]);
        assert_eq!(utf8_encode(0x41), vec![0x41]);
        assert_eq!(utf8_encode(0x7F), vec![0x7F]);
    }

    #[test]
    fn test_utf8_encode_multibyte() {
        assert_eq!(utf8_encode(0x80), vec![0xC2, 0x80]);
        assert_eq!(utf8_encode(0x7FF), vec![0xDF, 0xBF]);
        assert_eq!(utf8_encode(0x800), vec![0xE0, 0xA0, 0x80]);
        assert_eq!(utf8_encode(0xFFFF), vec![0xEF, 0xBF, 0xBF]);
        assert_eq!(utf8_encode(0x10000), vec![0xF0, 0x90, 0x80, 0x80]);
        assert_eq!(utf8_encode(0x10FFFF), vec![0xF4, 0x8F, 0xBF, 0xBF]);
    }

    #[test]
    fn test_utf8_encode_large() {
        // 原始 UTF-8 值 (非 Unicode)
        assert_eq!(utf8_encode(0x4000000), vec![0xF8, 0x90, 0x80, 0x80, 0x80]);
        assert_eq!(utf8_encode(0x7FFFFFFF), vec![0xFC, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF]);
    }

    // ========================================================================
    // u_posrelat 测试
    // ========================================================================

    #[test]
    fn test_u_posrelat_positive() {
        assert_eq!(u_posrelat(3, 10), 3);
        assert_eq!(u_posrelat(1, 10), 1);
        assert_eq!(u_posrelat(0, 10), 0);
    }

    #[test]
    fn test_u_posrelat_negative() {
        assert_eq!(u_posrelat(-1, 10), 10);
        assert_eq!(u_posrelat(-3, 10), 8);
    }

    #[test]
    fn test_u_posrelat_negative_out_of_range() {
        assert_eq!(u_posrelat(-11, 10), 0);
        assert_eq!(u_posrelat(-20, 10), 0);
    }

    // ========================================================================
    // utf8_char_impl 测试
    // ========================================================================

    #[test]
    fn test_utf8_char_empty() {
        assert_eq!(utf8_char_impl(&[]).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn test_utf8_char_ascii() {
        assert_eq!(utf8_char_impl(&[0]).unwrap(), vec![0x00]);
        assert_eq!(utf8_char_impl(&[97, 98, 99]).unwrap(), b"abc");
    }

    #[test]
    fn test_utf8_char_multibyte() {
        assert_eq!(utf8_char_impl(&[0x10FFFF]).unwrap(), vec![0xF4, 0x8F, 0xBF, 0xBF]);
    }

    #[test]
    fn test_utf8_char_out_of_range() {
        assert!(utf8_char_impl(&[-1]).is_err());
    }

    // ========================================================================
    // utf8_offset_impl 测试
    // ========================================================================

    #[test]
    fn test_utf8_offset_basic() {
        let s = b"hello";
        // n=0, 返回当前字符的位置
        assert_eq!(utf8_offset_impl(s, 0, 1).unwrap(), Some((1, 1)));
        // n=1, 第1个字符
        assert_eq!(utf8_offset_impl(s, 1, 1).unwrap(), Some((1, 1)));
        // n=2, 第2个字符
        assert_eq!(utf8_offset_impl(s, 2, 1).unwrap(), Some((2, 2)));
        // n=5, 第5个字符
        assert_eq!(utf8_offset_impl(s, 5, 1).unwrap(), Some((5, 5)));
        // n=6, 超出范围
        assert_eq!(utf8_offset_impl(s, 6, 1).unwrap(), None);
    }

    #[test]
    fn test_utf8_offset_negative() {
        let s = b"hello";
        // n=-1 从末尾开始
        assert_eq!(utf8_offset_impl(s, -1, 6).unwrap(), Some((5, 5)));
        // n=-5
        assert_eq!(utf8_offset_impl(s, -5, 6).unwrap(), Some((1, 1)));
        // n=-6 超出范围
        assert_eq!(utf8_offset_impl(s, -6, 6).unwrap(), None);
    }

    #[test]
    fn test_utf8_offset_multibyte() {
        // "汉字" = 6 bytes (每个汉字 3 bytes)
        let s = "汉字".as_bytes();
        assert_eq!(utf8_offset_impl(s, 1, 1).unwrap(), Some((1, 3)));
        assert_eq!(utf8_offset_impl(s, 2, 1).unwrap(), Some((4, 6)));
        assert_eq!(utf8_offset_impl(s, 0, 4).unwrap(), Some((4, 6)));
        assert_eq!(utf8_offset_impl(s, -1, 7).unwrap(), Some((4, 6)));
    }

    #[test]
    fn test_utf8_offset_not_found() {
        assert_eq!(utf8_offset_impl(b"alo", 5, 1).unwrap(), None);
        assert_eq!(utf8_offset_impl(b"alo", -4, 4).unwrap(), None);
    }

    #[test]
    fn test_utf8_offset_out_of_bounds() {
        assert!(utf8_offset_impl(b"abc", 1, 5).is_err());
        assert!(utf8_offset_impl(b"abc", 1, -4).is_err());
        assert!(utf8_offset_impl(b"", 1, 2).is_err());
        assert!(utf8_offset_impl(b"", 1, -1).is_err());
    }

    #[test]
    fn test_utf8_offset_continuation_byte() {
        // "𦧺" 是 4 字节字符
        let s = "𦧺".as_bytes();
        // 位置 2 是续字节
        assert!(utf8_offset_impl(s, 1, 2).is_err());
    }

    #[test]
    fn test_utf8_offset_incomplete_sequence() {
        // 不完整的 3 字节序列
        let s = b"\xE0";
        assert_eq!(utf8_offset_impl(s, 1, 1).unwrap(), Some((1, 1)));

        let s = b"\xE0\x9e";
        assert_eq!(utf8_offset_impl(s, -1, 3).unwrap(), Some((1, 2)));
    }

    // ========================================================================
    // utf8_codepoint_impl 测试
    // ========================================================================

    #[test]
    fn test_utf8_codepoint_basic() {
        let s = b"abc";
        let result = utf8_codepoint_impl(s, 1, 3, false).unwrap();
        assert_eq!(result, vec![0x61, 0x62, 0x63]);
    }

    #[test]
    fn test_utf8_codepoint_multibyte() {
        let s = "汉字".as_bytes();
        let result = utf8_codepoint_impl(s, 1, -1, false).unwrap();
        assert_eq!(result, vec![27721, 23383]);
    }

    #[test]
    fn test_utf8_codepoint_empty_interval() {
        let s = b"abc";
        let result = utf8_codepoint_impl(s, 4, 3, false).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_utf8_codepoint_invalid() {
        let s = b"\x80";
        assert!(utf8_codepoint_impl(s, 1, 1, false).is_err());
    }

    #[test]
    fn test_utf8_codepoint_surrogates_strict() {
        // U+D800 在 strict 模式下应失败
        let s = &[0xED, 0xA0, 0x80];
        assert!(utf8_codepoint_impl(s, 1, 1, false).is_err());
        // 在 lax 模式下应成功
        let result = utf8_codepoint_impl(s, 1, 1, true).unwrap();
        assert_eq!(result, vec![0xD800]);
    }

    // ========================================================================
    // utf8_len_impl 测试 (通过内部逻辑测试)
    // ========================================================================

    #[test]
    fn test_iscont() {
        assert!(!iscont(0x00));
        assert!(!iscont(0x7F));
        assert!(!iscont(0xC0));
        assert!(iscont(0x80));
        assert!(iscont(0xBF));
        assert!(!iscont(0xC2));
    }

    #[test]
    fn test_utf8_iter_aux_empty() {
        let result = utf8_iter_aux_impl(b"", 0, true);
        assert!(result.is_empty());
    }

    #[test]
    fn test_utf8_iter_aux_basic() {
        let s = b"abc";
        let result = utf8_iter_aux_impl(s, 0, true);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], TValue::Integer(1));
        assert_eq!(result[1], TValue::Integer(0x61));

        let result = utf8_iter_aux_impl(s, 1, true);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], TValue::Integer(2));
        assert_eq!(result[1], TValue::Integer(0x62));

        let result = utf8_iter_aux_impl(s, 3, true);
        assert!(result.is_empty()); // 结束
    }

    #[test]
    fn test_utf8_iter_aux_negative() {
        let result = utf8_iter_aux_impl(b"abc", -1, true);
        assert!(result.is_empty());
    }
}
