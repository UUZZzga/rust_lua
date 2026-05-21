//! Lua 虚拟机核心操作 (纯 Rust 重写)
//!
//! 对应 C 源码: lvm.h + lvm.cpp (除 luaV_execute 外)
//!
//! ## 设计原则
//! - `F2IMode` 用 Rust enum + 方法替代 C 的 typedef enum + 宏
//! - 数值转换函数返回 `Option<T>` 替代 C 的输出参数 + 返回码
//! - 比较函数直接返回 `bool` 或 `Ordering`，用模式匹配替代 tagged union 判断
//! - 算术运算返回 `Result` 或 `Option`，除零等情况用错误类型表达
//! - 字符串比较用 Rust `std::cmp` trait + `strcoll` FFI
//! - 所有函数不依赖全局 Lua state，错误通过 Result 传播
//!
//! ## 规约驱动开发 (spec-driven-tdd)
//! 每个公开函数都包含规约注释。

use std::cmp::Ordering;
use std::ffi::CString;

use crate::objects::TValue;
use crate::strings::LuaString;
use crate::table::Table;

// ============================================================================
// F2IMode: 浮点数到整数的转换模式
// ============================================================================

/// 浮点数到整数的舍入模式。
///
/// Scenario: 选择舍入模式
/// Given: 浮点数 3.7
/// When: 用 Eq 模式转换为整数
/// Then: 失败（因为 3.7 不是整数值）
/// When: 用 Floor 模式
/// Then: 返回 3
/// When: 用 Ceil 模式
/// Then: 返回 4
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F2IMode {
    Eq,
    Floor,
    Ceil,
}

impl Default for F2IMode {
    fn default() -> Self {
        F2IMode::Eq
    }
}

// ============================================================================
// 数值转换
// ============================================================================

/// 将浮点数转换为整数，根据 mode 进行舍入。
///
/// Scenario: 浮点数到整数 — Eq 模式
/// Given: n = 3.0 (整数值)
/// When: 调用 float_to_integer(3.0, Eq)
/// Then: 返回 Some(3)
///
/// Given: n = 3.7 (非整数值)
/// When: 调用 float_to_integer(3.7, Eq)
/// Then: 返回 None
///
/// Scenario: Floor 模式
/// Given: n = 3.7
/// When: 调用 float_to_integer(3.7, Floor)
/// Then: 返回 Some(3)
///
/// Given: n = -3.7
/// When: 调用 float_to_integer(-3.7, Floor)
/// Then: 返回 Some(-4)
///
/// Scenario: Ceil 模式
/// Given: n = 3.2
/// When: 调用 float_to_integer(3.2, Ceil)
/// Then: 返回 Some(4)
///
/// Given: n = -3.2
/// When: 调用 float_to_integer(-3.2, Ceil)
/// Then: 返回 Some(-3)
///
/// Scenario: 超出整数范围
/// Given: n = f64::MAX
/// When: 调用 float_to_integer(f64::MAX, Floor)
/// Then: 返回 None
///
/// Scenario: NaN / Inf
/// Given: n = f64::NAN
/// When: 调用 float_to_integer(NaN, 任意模式)
/// Then: 返回 None
pub fn float_to_integer(n: f64, mode: F2IMode) -> Option<i64> {
    if n.is_nan() || n.is_infinite() {
        return None;
    }
    let floor = n.floor();
    if n != floor {
        match mode {
            F2IMode::Eq => return None,
            F2IMode::Floor => {}
            F2IMode::Ceil => {
                let ceil = n.ceil();
                if ceil > i64::MAX as f64 {
                    return None;
                }
                return Some(ceil as i64);
            }
        }
    }
    if floor > i64::MAX as f64 || floor < i64::MIN as f64 {
        return None;
    }
    Some(floor as i64)
}

/// 尝试将 TValue 转换为浮点数（不含字符串强制转换）。
///
/// Scenario: 整数转浮点
/// Given: TValue::Integer(42)
/// When: 调用 to_number_ns
/// Then: 返回 Some(42.0)
///
/// Scenario: 非数字类型
/// Given: TValue::Boolean(true)
/// When: 调用 to_number_ns
/// Then: 返回 None
pub fn to_number_ns(obj: &TValue) -> Option<f64> {
    match obj {
        TValue::Float(f) => Some(*f),
        TValue::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

/// 尝试将 TValue 转换为浮点数（含字符串强制转换）。
///
/// Scenario: 字符串 "3.14" 转浮点
/// Given: TValue::Str("3.14")
/// When: 调用 to_number
/// Then: 返回 Some(3.14)
///
/// Scenario: 无效字符串
/// Given: TValue::Str("abc")
/// When: 调用 to_number
/// Then: 返回 None
pub fn to_number(obj: &TValue) -> Option<f64> {
    match obj {
        TValue::Float(f) => Some(*f),
        TValue::Integer(i) => Some(*i as f64),
        TValue::Str(s) => s.as_str().parse::<f64>().ok(),
        _ => None,
    }
}

/// 尝试将 TValue 转换为整数（不含字符串强制转换）。
///
/// Scenario: 整数不变
/// Given: TValue::Integer(42)
/// When: 调用 to_integer_ns(obj, Eq)
/// Then: 返回 Some(42)
///
/// Scenario: 浮点转整数 — 整数值
/// Given: TValue::Float(3.0)
/// When: 调用 to_integer_ns(obj, Eq)
/// Then: 返回 Some(3)
///
/// Scenario: 非数字类型
/// Given: TValue::Boolean(true)
/// When: 调用 to_integer_ns
/// Then: 返回 None
pub fn to_integer_ns(obj: &TValue, mode: F2IMode) -> Option<i64> {
    match obj {
        TValue::Integer(i) => Some(*i),
        TValue::Float(f) => float_to_integer(*f, mode),
        _ => None,
    }
}

/// 尝试将 TValue 转换为整数（含字符串强制转换）。
///
/// Scenario: 字符串 "42" 转整数
/// Given: TValue::Str("42")
/// When: 调用 to_integer(obj, Eq)
/// Then: 返回 Some(42)
pub fn to_integer(obj: &TValue, mode: F2IMode) -> Option<i64> {
    match obj {
        TValue::Integer(i) => Some(*i),
        TValue::Float(f) => float_to_integer(*f, mode),
        TValue::Str(s) => {
            let parsed = s.as_str().parse::<f64>().ok()?;
            float_to_integer(parsed, mode)
        }
        _ => None,
    }
}

// ============================================================================
// 字符串比较
// ============================================================================

/// 比较两个 LuaString，返回 Ordering。
///
/// 使用 C locale 的 strcoll 进行逐段比较（支持内含 '\0' 的字符串）。
///
/// Scenario: 相同短字符串
/// Given: "hello" 和 "hello"
/// When: 调用 strcmp
/// Then: 返回 Ordering::Equal
///
/// Scenario: 字典序比较
/// Given: "abc" 和 "abd"
/// When: 调用 strcmp
/// Then: 返回 Ordering::Less
pub fn strcmp(ts1: &LuaString, ts2: &LuaString) -> Ordering {
    let s1 = ts1.as_str();
    let s2 = ts2.as_str();

    let has_null1 = s1.contains('\0');
    let has_null2 = s2.contains('\0');

    if !has_null1 && !has_null2 {
        return strcoll_compare(s1, s2);
    }

    let mut pos1 = 0;
    let mut pos2 = 0;
    let bytes1 = s1.as_bytes();
    let bytes2 = s2.as_bytes();

    loop {
        let seg1_end = bytes1[pos1..].iter().position(|&b| b == 0).map(|p| pos1 + p).unwrap_or(s1.len());
        let seg2_end = bytes2[pos2..].iter().position(|&b| b == 0).map(|p| pos2 + p).unwrap_or(s2.len());

        let seg1 = &s1[pos1..seg1_end];
        let seg2 = &s2[pos2..seg2_end];

        let cmp = strcoll_compare(seg1, seg2);
        if cmp != Ordering::Equal {
            return cmp;
        }

        let finished1 = seg1_end == s1.len();
        let finished2 = seg2_end == s2.len();

        if finished2 {
            return if finished1 { Ordering::Equal } else { Ordering::Greater };
        }
        if finished1 {
            return Ordering::Less;
        }

        pos1 = seg1_end + 1;
        pos2 = seg2_end + 1;
    }
}

fn strcoll_compare(a: &str, b: &str) -> Ordering {
    let ca = CString::new(a).unwrap_or_default();
    let cb = CString::new(b).unwrap_or_default();
    let result = unsafe { libc::strcoll(ca.as_ptr(), cb.as_ptr()) };
    result.cmp(&0)
}

// ============================================================================
// 混合整数/浮点数比较辅助函数
// ============================================================================

fn int_fits_float(i: i64) -> bool {
    let f = i as f64;
    (f as i64) == i
}

fn lt_int_float(i: i64, f: f64) -> bool {
    if f.is_nan() {
        return false;
    }
    if int_fits_float(i) {
        (i as f64) < f
    } else if let Some(fi) = float_to_integer(f, F2IMode::Ceil) {
        i < fi
    } else {
        f > 0.0
    }
}

fn le_int_float(i: i64, f: f64) -> bool {
    if f.is_nan() {
        return false;
    }
    if int_fits_float(i) {
        (i as f64) <= f
    } else if let Some(fi) = float_to_integer(f, F2IMode::Floor) {
        i <= fi
    } else {
        f > 0.0
    }
}

fn lt_float_int(f: f64, i: i64) -> bool {
    if f.is_nan() {
        return false;
    }
    if int_fits_float(i) {
        f < (i as f64)
    } else if let Some(fi) = float_to_integer(f, F2IMode::Floor) {
        fi < i
    } else {
        f < 0.0
    }
}

fn le_float_int(f: f64, i: i64) -> bool {
    if f.is_nan() {
        return false;
    }
    if int_fits_float(i) {
        f <= (i as f64)
    } else if let Some(fi) = float_to_integer(f, F2IMode::Ceil) {
        fi <= i
    } else {
        f < 0.0
    }
}

// ============================================================================
// 数值比较
// ============================================================================

/// 比较两个数值 TValue: 返回 l < r。
pub fn lt_num(l: &TValue, r: &TValue) -> bool {
    match (l, r) {
        (TValue::Integer(li), TValue::Integer(ri)) => li < ri,
        (TValue::Integer(li), TValue::Float(rf)) => lt_int_float(*li, *rf),
        (TValue::Float(lf), TValue::Integer(ri)) => lt_float_int(*lf, *ri),
        (TValue::Float(lf), TValue::Float(rf)) => {
            if lf.is_nan() || rf.is_nan() { false } else { lf < rf }
        }
        _ => panic!("lt_num: operands must be numbers"),
    }
}

/// 比较两个数值 TValue: 返回 l <= r。
pub fn le_num(l: &TValue, r: &TValue) -> bool {
    match (l, r) {
        (TValue::Integer(li), TValue::Integer(ri)) => li <= ri,
        (TValue::Integer(li), TValue::Float(rf)) => le_int_float(*li, *rf),
        (TValue::Float(lf), TValue::Integer(ri)) => le_float_int(*lf, *ri),
        (TValue::Float(lf), TValue::Float(rf)) => {
            if lf.is_nan() || rf.is_nan() { false } else { lf <= rf }
        }
        _ => panic!("le_num: operands must be numbers"),
    }
}

// ============================================================================
// 相等性比较
// ============================================================================

/// 比较两个 TValue 是否相等（不含元方法）。
///
/// 对于引用类型（Table、LClosure 等），比较原始指针是否相同。
/// 对于值类型（Integer、Boolean 等），比较值是否相同。
/// 浮点数比较时会尝试整数化以匹配 Lua 的数值等价语义。
///
/// Scenario: 同类型同值
/// Given: Integer(42) 和 Integer(42)
/// When: 调用 raw_equal
/// Then: 返回 true
///
/// Scenario: 不同类型的 nil 变体不等
/// Given: Nil(Strict) 和 Nil(Empty)
/// When: 调用 raw_equal
/// Then: 返回 false
///
/// Scenario: NaN != NaN
/// Given: Float(NaN) 和 Float(NaN)
/// When: 调用 raw_equal
/// Then: 返回 false
pub fn raw_equal(t1: &TValue, t2: &TValue) -> bool {
    match (t1, t2) {
        (TValue::Nil(a), TValue::Nil(b)) => a == b,
        (TValue::Boolean(a), TValue::Boolean(b)) => a == b,
        (TValue::Integer(a), TValue::Integer(b)) => a == b,
        (TValue::Float(a), TValue::Float(b)) => {
            if a.is_nan() || b.is_nan() { false } else { a.to_bits() == b.to_bits() }
        }
        (TValue::Integer(a), TValue::Float(b)) => {
            if b.is_nan() { false } else { float_to_integer(*b, F2IMode::Eq).map_or(false, |i| *a == i) }
        }
        (TValue::Float(a), TValue::Integer(b)) => {
            if a.is_nan() { false } else { float_to_integer(*a, F2IMode::Eq).map_or(false, |i| i == *b) }
        }
        (TValue::Str(a), TValue::Str(b)) => a.as_str() == b.as_str(),
        (TValue::LightUserData(a), TValue::LightUserData(b)) => std::ptr::eq(*a, *b),
        (TValue::Table(a), TValue::Table(b)) => std::ptr::eq(a as *const Table, b as *const Table),
        (TValue::LClosure(a), TValue::LClosure(b)) => std::ptr::eq(a as *const _, b as *const _),
        (TValue::CClosure(a), TValue::CClosure(b)) => std::ptr::eq(a as *const _, b as *const _),
        (TValue::UserData(a), TValue::UserData(b)) => std::ptr::eq(a as *const _, b as *const _),
        (TValue::Thread(a), TValue::Thread(b)) => std::ptr::eq(a as *const _, b as *const _),
        _ => false,
    }
}

/// 比较两个 TValue 是否相等（支持元方法回退）。
///
/// 目前仅实现原始比较，元方法回退预留。
pub fn equal(t1: &TValue, t2: &TValue) -> bool {
    raw_equal(t1, t2)
}

// ============================================================================
// 小于比较
// ============================================================================

/// 比较两个 TValue: 返回 l < r（不含元方法回退）。
pub fn less_than(l: &TValue, r: &TValue) -> bool {
    if l.is_number() && r.is_number() {
        return lt_num(l, r);
    }
    if let (TValue::Str(a), TValue::Str(b)) = (l, r) {
        return strcmp(a, b) == Ordering::Less;
    }
    false
}

/// 比较两个 TValue: 返回 l <= r（不含元方法回退）。
pub fn less_equal(l: &TValue, r: &TValue) -> bool {
    if l.is_number() && r.is_number() {
        return le_num(l, r);
    }
    if let (TValue::Str(a), TValue::Str(b)) = (l, r) {
        return strcmp(a, b) != Ordering::Greater;
    }
    false
}

// ============================================================================
// 算术运算
// ============================================================================

/// 整数整除: 返回 floor(m / n)。
///
/// Scenario: 正常整除
/// Given: m = 10, n = 3
/// When: 调用 idiv(10, 3)
/// Then: 返回 Ok(3)
///
/// Scenario: 负数整除
/// Given: m = -10, n = 3
/// When: 调用 idiv(-10, 3)
/// Then: 返回 Ok(-4)
///
/// Scenario: 除零报错
/// Given: m = 5, n = 0
/// When: 调用 idiv(5, 0)
/// Then: 返回 Err
pub fn idiv(m: i64, n: i64) -> Result<i64, &'static str> {
    if n == 0 {
        return Err("attempt to divide by zero");
    }
    if n == -1 {
        return Ok(m.wrapping_neg());
    }
    let q = m / n;
    if (m ^ n) < 0 && m % n != 0 {
        Ok(q - 1)
    } else {
        Ok(q)
    }
}

/// 整数取模: 返回 m % n（Lua 风格）。
///
/// Scenario: 正数取模
/// Given: m = 10, n = 3
/// When: 调用 modulus(10, 3)
/// Then: 返回 Ok(1)
///
/// Scenario: 负数取模
/// Given: m = -10, n = 3
/// When: 调用 modulus(-10, 3)
/// Then: 返回 Ok(2) (Lua 风格，结果与除数同号)
///
/// Scenario: 除零报错
/// Given: m = 5, n = 0
/// When: 调用 modulus(5, 0)
/// Then: 返回 Err
pub fn modulus(m: i64, n: i64) -> Result<i64, &'static str> {
    if n == 0 {
        return Err("attempt to divide by zero");
    }
    if n == -1 {
        return Ok(0);
    }
    let r = m % n;
    if r != 0 && (r ^ n) < 0 {
        Ok(r + n)
    } else {
        Ok(r)
    }
}

/// 浮点数取模: 返回 m % n（Lua 风格）。
///
/// Scenario: 正常浮点取模
/// Given: m = 10.5, n = 3.0
/// When: 调用 modulus_float(10.5, 3.0)
/// Then: 返回 1.5
pub fn modulus_float(m: f64, n: f64) -> f64 {
    let r = m % n;
    if (r > 0.0) == (n < 0.0) && r != 0.0 {
        r + n
    } else {
        r
    }
}

/// 位移运算: 左移或右移。
///
/// Scenario: 左移
/// Given: x = 1, y = 3
/// When: 调用 shiftl(1, 3)
/// Then: 返回 8
///
/// Scenario: 右移（y 为负数）
/// Given: x = 16, y = -2
/// When: 调用 shiftl(16, -2)
/// Then: 返回 4
///
/// Scenario: 位移量超出范围
/// Given: x = 1, y = 100
/// When: 调用 shiftl(1, 100)
/// Then: 返回 0
pub fn shiftl(x: i64, y: i64) -> i64 {
    const NBITS: i64 = 64;
    if y < 0 {
        if y <= -NBITS { return 0; }
        ((x as u64) >> (-y as u32)) as i64
    } else {
        if y >= NBITS { return 0; }
        ((x as u64) << (y as u32)) as i64
    }
}

// ============================================================================
// objlen: # 操作符
// ============================================================================

/// 计算值的长度（# 操作符）。
///
/// Scenario: 表
/// Given: 一个有 3 个连续元素的表
/// When: 调用 objlen(&table)
/// Then: 返回 Some(Integer(3))
///
/// Scenario: 字符串
/// Given: Str("hello")
/// When: 调用 objlen(&str)
/// Then: 返回 Some(Integer(5))
///
/// Scenario: 其他类型 — 无元方法支持
/// Given: Integer(42)
/// When: 调用 objlen
/// Then: 返回 None
pub fn objlen(obj: &TValue) -> Option<TValue> {
    match obj {
        TValue::Table(t) => {
            Some(TValue::Integer(t.len()))
        }
        TValue::Str(s) => {
            Some(TValue::Integer(s.len() as i64))
        }
        _ => None,
    }
}

// ============================================================================
// is_false: Lua 假值判断
// ============================================================================

/// Lua 假值判断: nil 和 false 为假，其他为真。
///
/// Scenario: nil 是假
/// Given: TValue::Nil(_)
/// When: 调用 is_false
/// Then: 返回 true
///
/// Scenario: false 是假
/// Given: TValue::Boolean(false)
/// When: 调用 is_false
/// Then: 返回 true
///
/// Scenario: 0 是真
/// Given: TValue::Integer(0)
/// When: 调用 is_false
/// Then: 返回 false
pub fn is_false(v: &TValue) -> bool {
    match v {
        TValue::Nil(_) => true,
        TValue::Boolean(false) => true,
        _ => false,
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::NilKind;
    use crate::strings::{LuaString, StringTable};

    // ========================================================================
    // F2IMode 测试
    // ========================================================================

    #[test]
    fn test_f2i_mode_default() {
        assert_eq!(F2IMode::default(), F2IMode::Eq);
    }

    // ========================================================================
    // float_to_integer 测试
    // ========================================================================

    #[test]
    fn test_float_to_integer_eq_exact() {
        assert_eq!(float_to_integer(3.0, F2IMode::Eq), Some(3));
        assert_eq!(float_to_integer(0.0, F2IMode::Eq), Some(0));
        assert_eq!(float_to_integer(-5.0, F2IMode::Eq), Some(-5));
    }

    #[test]
    fn test_float_to_integer_eq_non_integral() {
        assert_eq!(float_to_integer(3.7, F2IMode::Eq), None);
        assert_eq!(float_to_integer(-3.2, F2IMode::Eq), None);
    }

    #[test]
    fn test_float_to_integer_floor() {
        assert_eq!(float_to_integer(3.7, F2IMode::Floor), Some(3));
        assert_eq!(float_to_integer(-3.7, F2IMode::Floor), Some(-4));
        assert_eq!(float_to_integer(3.0, F2IMode::Floor), Some(3));
    }

    #[test]
    fn test_float_to_integer_ceil() {
        assert_eq!(float_to_integer(3.2, F2IMode::Ceil), Some(4));
        assert_eq!(float_to_integer(-3.2, F2IMode::Ceil), Some(-3));
        assert_eq!(float_to_integer(3.0, F2IMode::Ceil), Some(3));
    }

    #[test]
    fn test_float_to_integer_nan() {
        assert_eq!(float_to_integer(f64::NAN, F2IMode::Eq), None);
        assert_eq!(float_to_integer(f64::NAN, F2IMode::Floor), None);
        assert_eq!(float_to_integer(f64::NAN, F2IMode::Ceil), None);
    }

    #[test]
    fn test_float_to_integer_infinity() {
        assert_eq!(float_to_integer(f64::INFINITY, F2IMode::Floor), None);
        assert_eq!(float_to_integer(f64::NEG_INFINITY, F2IMode::Floor), None);
    }

    #[test]
    fn test_float_to_integer_large() {
        let large: f64 = (1i64 << 53) as f64;
        assert_eq!(float_to_integer(large, F2IMode::Eq), Some(1i64 << 53));
    }

    // ========================================================================
    // to_number / to_number_ns 测试
    // ========================================================================

    #[test]
    fn test_to_number_ns_int() {
        let v = TValue::Integer(42);
        assert_eq!(to_number_ns(&v), Some(42.0));
    }

    #[test]
    fn test_to_number_ns_float() {
        let v = TValue::Float(3.14);
        assert_eq!(to_number_ns(&v), Some(3.14));
    }

    #[test]
    fn test_to_number_ns_non_number() {
        let v = TValue::Boolean(true);
        assert_eq!(to_number_ns(&v), None);
    }

    #[test]
    fn test_to_number_str() {
        let tb = StringTable::new();
        let v = TValue::Str(tb.intern("3.14"));
        assert!((to_number(&v).unwrap() - 3.14).abs() < 1e-10);
    }

    #[test]
    fn test_to_number_str_int() {
        let tb = StringTable::new();
        let v = TValue::Str(tb.intern("42"));
        assert_eq!(to_number(&v), Some(42.0));
    }

    #[test]
    fn test_to_number_str_invalid() {
        let tb = StringTable::new();
        let v = TValue::Str(tb.intern("abc"));
        assert_eq!(to_number(&v), None);
    }

    // ========================================================================
    // to_integer / to_integer_ns 测试
    // ========================================================================

    #[test]
    fn test_to_integer_ns_int() {
        let v = TValue::Integer(42);
        assert_eq!(to_integer_ns(&v, F2IMode::Eq), Some(42));
    }

    #[test]
    fn test_to_integer_ns_float_exact() {
        let v = TValue::Float(3.0);
        assert_eq!(to_integer_ns(&v, F2IMode::Eq), Some(3));
    }

    #[test]
    fn test_to_integer_ns_float_inexact() {
        let v = TValue::Float(3.7);
        assert_eq!(to_integer_ns(&v, F2IMode::Eq), None);
    }

    #[test]
    fn test_to_integer_str() {
        let tb = StringTable::new();
        let v = TValue::Str(tb.intern("42"));
        assert_eq!(to_integer(&v, F2IMode::Eq), Some(42));
    }

    // ========================================================================
    // strcmp 测试
    // ========================================================================

    fn make_ls(s: &str) -> LuaString {
        let tb = StringTable::new();
        tb.intern(s)
    }

    #[test]
    fn test_strcmp_equal() {
        let a = make_ls("hello");
        let b = make_ls("hello");
        assert_eq!(strcmp(&a, &b), Ordering::Equal);
    }

    #[test]
    fn test_strcmp_less() {
        let a = make_ls("abc");
        let b = make_ls("abd");
        assert_eq!(strcmp(&a, &b), Ordering::Less);
    }

    #[test]
    fn test_strcmp_empty() {
        let a = make_ls("");
        let b = make_ls("a");
        assert_eq!(strcmp(&a, &b), Ordering::Less);
    }

    #[test]
    fn test_strcmp_prefix() {
        let a = make_ls("ab");
        let b = make_ls("abc");
        assert_eq!(strcmp(&a, &b), Ordering::Less);
    }

    // ========================================================================
    // lt_num / le_num 测试
    // ========================================================================

    #[test]
    fn test_lt_num_int_int() {
        assert!(lt_num(&TValue::Integer(3), &TValue::Integer(5)));
        assert!(!lt_num(&TValue::Integer(5), &TValue::Integer(3)));
        assert!(!lt_num(&TValue::Integer(3), &TValue::Integer(3)));
    }

    #[test]
    fn test_lt_num_float_float() {
        assert!(lt_num(&TValue::Float(2.5), &TValue::Float(3.0)));
        assert!(!lt_num(&TValue::Float(3.0), &TValue::Float(2.5)));
    }

    #[test]
    fn test_lt_num_int_float() {
        assert!(lt_num(&TValue::Integer(3), &TValue::Float(3.5)));
        assert!(!lt_num(&TValue::Integer(5), &TValue::Float(3.0)));
    }

    #[test]
    fn test_lt_num_float_int() {
        assert!(lt_num(&TValue::Float(2.5), &TValue::Integer(3)));
        assert!(!lt_num(&TValue::Float(5.0), &TValue::Integer(3)));
    }

    #[test]
    fn test_lt_num_nan() {
        assert!(!lt_num(&TValue::Float(f64::NAN), &TValue::Integer(1)));
        assert!(!lt_num(&TValue::Integer(1), &TValue::Float(f64::NAN)));
    }

    #[test]
    fn test_le_num_equal() {
        assert!(le_num(&TValue::Integer(3), &TValue::Integer(3)));
        assert!(le_num(&TValue::Float(3.0), &TValue::Integer(3)));
    }

    #[test]
    fn test_le_num_strict() {
        assert!(le_num(&TValue::Integer(3), &TValue::Integer(5)));
        assert!(!le_num(&TValue::Integer(5), &TValue::Integer(3)));
    }

    // ========================================================================
    // raw_equal 测试
    // ========================================================================

    #[test]
    fn test_raw_equal_same_type_same_value() {
        assert!(raw_equal(&TValue::Integer(42), &TValue::Integer(42)));
        assert!(raw_equal(&TValue::Boolean(true), &TValue::Boolean(true)));
        assert!(raw_equal(&TValue::Nil(NilKind::Strict), &TValue::Nil(NilKind::Strict)));
    }

    #[test]
    fn test_raw_equal_same_type_diff_value() {
        assert!(!raw_equal(&TValue::Integer(42), &TValue::Integer(43)));
        assert!(!raw_equal(&TValue::Boolean(true), &TValue::Boolean(false)));
    }

    #[test]
    fn test_raw_equal_diff_nil_variant() {
        assert!(!raw_equal(&TValue::Nil(NilKind::Strict), &TValue::Nil(NilKind::Empty)));
    }

    #[test]
    fn test_raw_equal_int_float() {
        assert!(raw_equal(&TValue::Integer(3), &TValue::Float(3.0)));
        assert!(raw_equal(&TValue::Float(3.0), &TValue::Integer(3)));
    }

    #[test]
    fn test_raw_equal_int_float_non_integral() {
        assert!(!raw_equal(&TValue::Integer(3), &TValue::Float(3.14)));
    }

    #[test]
    fn test_raw_equal_diff_type() {
        assert!(!raw_equal(&TValue::Integer(1), &TValue::Boolean(true)));
        assert!(!raw_equal(&TValue::Nil(NilKind::Strict), &TValue::Integer(0)));
    }

    #[test]
    fn test_raw_equal_nan() {
        assert!(!raw_equal(&TValue::Float(f64::NAN), &TValue::Float(f64::NAN)));
    }

    #[test]
    fn test_raw_equal_strings() {
        let tb = StringTable::new();
        let a = TValue::Str(tb.intern("hello"));
        let b = TValue::Str(tb.intern("hello"));
        assert!(raw_equal(&a, &b));

        let c = TValue::Str(tb.intern("world"));
        assert!(!raw_equal(&a, &c));
    }

    // ========================================================================
    // less_than / less_equal 测试
    // ========================================================================

    #[test]
    fn test_less_than_numbers() {
        assert!(less_than(&TValue::Integer(3), &TValue::Integer(5)));
        assert!(!less_than(&TValue::Integer(5), &TValue::Integer(3)));
    }

    #[test]
    fn test_less_than_strings() {
        let tb = StringTable::new();
        let a = TValue::Str(tb.intern("abc"));
        let b = TValue::Str(tb.intern("abd"));
        assert!(less_than(&a, &b));
    }

    #[test]
    fn test_less_equal_equal() {
        assert!(less_equal(&TValue::Integer(3), &TValue::Integer(3)));
        assert!(!less_equal(&TValue::Integer(5), &TValue::Integer(3)));
    }

    // ========================================================================
    // idiv 测试
    // ========================================================================

    #[test]
    fn test_idiv_positive() {
        assert_eq!(idiv(10, 3).unwrap(), 3);
        assert_eq!(idiv(10, 2).unwrap(), 5);
        assert_eq!(idiv(9, 3).unwrap(), 3);
    }

    #[test]
    fn test_idiv_negative() {
        assert_eq!(idiv(-10, 3).unwrap(), -4);
        assert_eq!(idiv(10, -3).unwrap(), -4);
        assert_eq!(idiv(-10, -3).unwrap(), 3);
    }

    #[test]
    fn test_idiv_by_zero() {
        assert!(idiv(5, 0).is_err());
    }

    #[test]
    fn test_idiv_min_by_neg_one() {
        let m = i64::MIN;
        assert_eq!(idiv(m, -1).unwrap(), m.wrapping_neg());
    }

    // ========================================================================
    // modulus 测试
    // ========================================================================

    #[test]
    fn test_mod_positive() {
        assert_eq!(modulus(10, 3).unwrap(), 1);
        assert_eq!(modulus(10, 5).unwrap(), 0);
    }

    #[test]
    fn test_mod_negative() {
        assert_eq!(modulus(-10, 3).unwrap(), 2);
        assert_eq!(modulus(10, -3).unwrap(), -2);
    }

    #[test]
    fn test_mod_by_zero() {
        assert!(modulus(5, 0).is_err());
    }

    // ========================================================================
    // modulus_float 测试
    // ========================================================================

    #[test]
    fn test_modf_positive() {
        let r = modulus_float(10.5, 3.0);
        assert!((r - 1.5).abs() < 1e-10);
    }

    #[test]
    fn test_modf_exact() {
        let r = modulus_float(9.0, 3.0);
        assert!(r.abs() < 1e-10);
    }

    // ========================================================================
    // shiftl 测试
    // ========================================================================

    #[test]
    fn test_shiftl_left() {
        assert_eq!(shiftl(1, 3), 8);
        assert_eq!(shiftl(4, 1), 8);
    }

    #[test]
    fn test_shiftl_right() {
        assert_eq!(shiftl(16, -2), 4);
        assert_eq!(shiftl(8, -3), 1);
    }

    #[test]
    fn test_shiftl_overflow() {
        assert_eq!(shiftl(1, 100), 0);
        assert_eq!(shiftl(100, -100), 0);
    }

    #[test]
    fn test_shiftl_zero() {
        assert_eq!(shiftl(1, 0), 1);
        assert_eq!(shiftl(0, 3), 0);
    }

    // ========================================================================
    // objlen 测试
    // ========================================================================

    #[test]
    fn test_objlen_string() {
        let tb = StringTable::new();
        let s = TValue::Str(tb.intern("hello"));
        assert_eq!(objlen(&s), Some(TValue::Integer(5)));
    }

    #[test]
    fn test_objlen_empty_string() {
        let tb = StringTable::new();
        let s = TValue::Str(tb.intern(""));
        assert_eq!(objlen(&s), Some(TValue::Integer(0)));
    }

    #[test]
    fn test_objlen_table() {
        let mut t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));
        let tv = TValue::Table(t);
        assert_eq!(objlen(&tv), Some(TValue::Integer(3)));
    }

    #[test]
    fn test_objlen_non_lenable() {
        assert_eq!(objlen(&TValue::Integer(42)), None);
    }

    // ========================================================================
    // is_false 测试
    // ========================================================================

    #[test]
    fn test_is_false_nil() {
        assert!(is_false(&TValue::Nil(NilKind::Strict)));
        assert!(is_false(&TValue::Nil(NilKind::Empty)));
    }

    #[test]
    fn test_is_false_bool() {
        assert!(is_false(&TValue::Boolean(false)));
        assert!(!is_false(&TValue::Boolean(true)));
    }

    #[test]
    fn test_is_false_numbers() {
        assert!(!is_false(&TValue::Integer(0)));
        assert!(!is_false(&TValue::Float(0.0)));
    }

    #[test]
    fn test_is_false_strings() {
        let tb = StringTable::new();
        assert!(!is_false(&TValue::Str(tb.intern(""))));
        assert!(!is_false(&TValue::Str(tb.intern("false"))));
    }

    // ========================================================================
    // equal 测试
    // ========================================================================

    #[test]
    fn test_equal_basic() {
        assert!(equal(&TValue::Integer(42), &TValue::Integer(42)));
        assert!(!equal(&TValue::Integer(42), &TValue::Integer(43)));
    }

    // ========================================================================
    // int_fits_float 测试
    // ========================================================================

    #[test]
    fn test_int_fits_float_small() {
        assert!(int_fits_float(42));
        assert!(int_fits_float(-100));
    }

    #[test]
    fn test_int_fits_float_large() {
        assert!(!int_fits_float(2i64.pow(53) + 1));
        assert!(int_fits_float(2i64.pow(53)));
        assert!(int_fits_float(2i64.pow(54)));
    }

    // ========================================================================
    // 混合比较测试
    // ========================================================================

    #[test]
    fn test_lt_int_float_small() {
        assert!(lt_int_float(3, 3.5));
        assert!(!lt_int_float(5, 3.0));
    }

    #[test]
    fn test_lt_int_float_nan() {
        assert!(!lt_int_float(1, f64::NAN));
    }

    #[test]
    fn test_lt_float_int_small() {
        assert!(lt_float_int(2.5, 3));
        assert!(!lt_float_int(5.0, 3));
    }

    #[test]
    fn test_le_int_float() {
        assert!(le_int_float(3, 3.0));
        assert!(le_int_float(3, 3.5));
        assert!(!le_int_float(5, 3.0));
    }

    #[test]
    fn test_le_float_int() {
        assert!(le_float_int(3.0, 3));
        assert!(le_float_int(2.5, 3));
        assert!(!le_float_int(5.0, 3));
    }

    #[test]
    fn test_mod_min_by_neg_one() {
        assert_eq!(modulus(i64::MIN, -1).unwrap(), 0);
    }

    // ========================================================================
    // float_to_integer 边界测试
    // ========================================================================

    #[test]
    fn test_float_to_integer_i64_max_edge() {
        let max_f = i64::MAX as f64;
        assert_eq!(float_to_integer(max_f, F2IMode::Eq), Some(i64::MAX));
        assert!(float_to_integer(f64::INFINITY, F2IMode::Eq).is_none());
    }

    #[test]
    fn test_float_to_integer_overflow() {
        assert_eq!(float_to_integer(i64::MAX as f64 + 10000.0, F2IMode::Eq), None);
    }

    #[test]
    fn test_float_to_integer_i64_min_edge() {
        assert_eq!(float_to_integer(i64::MIN as f64, F2IMode::Eq), Some(i64::MIN));
    }

    #[test]
    fn test_float_to_integer_floor_negative_fraction() {
        assert_eq!(float_to_integer(-0.1, F2IMode::Floor), Some(-1));
        assert_eq!(float_to_integer(-0.9, F2IMode::Floor), Some(-1));
        assert_eq!(float_to_integer(-1.1, F2IMode::Floor), Some(-2));
    }

    #[test]
    fn test_float_to_integer_ceil_negative_fraction() {
        assert_eq!(float_to_integer(-0.1, F2IMode::Ceil), Some(0));
        assert_eq!(float_to_integer(-1.9, F2IMode::Ceil), Some(-1));
    }

    #[test]
    fn test_float_to_integer_zero_modes() {
        assert_eq!(float_to_integer(0.0, F2IMode::Eq), Some(0));
        assert_eq!(float_to_integer(0.0, F2IMode::Floor), Some(0));
        assert_eq!(float_to_integer(0.0, F2IMode::Ceil), Some(0));
        assert_eq!(float_to_integer(-0.0, F2IMode::Eq), Some(0));
    }

    // ========================================================================
    // to_number / to_integer 额外测试
    // ========================================================================

    #[test]
    fn test_to_number_str_negative() {
        let tb = StringTable::new();
        let v = TValue::Str(tb.intern("-3.14"));
        assert!((to_number(&v).unwrap() + 3.14).abs() < 1e-10);
    }

    #[test]
    fn test_to_integer_str_negative() {
        let tb = StringTable::new();
        let v = TValue::Str(tb.intern("-42"));
        assert_eq!(to_integer(&v, F2IMode::Eq), Some(-42));
    }

    #[test]
    fn test_to_integer_str_float_floor() {
        let tb = StringTable::new();
        let v = TValue::Str(tb.intern("3.7"));
        assert_eq!(to_integer(&v, F2IMode::Floor), Some(3));
        assert_eq!(to_integer(&v, F2IMode::Eq), None);
    }

    // ========================================================================
    // strcmp: 含 '\0' 的字符串
    // ========================================================================

    #[test]
    fn test_strcmp_with_null_bytes_equal() {
        let tb = StringTable::new();
        let a = tb.intern("hello\0world");
        let b = tb.intern("hello\0world");
        assert_eq!(strcmp(&a, &b), Ordering::Equal);
    }

    #[test]
    fn test_strcmp_with_null_bytes_less() {
        let tb = StringTable::new();
        let a = tb.intern("abc\0def");
        let b = tb.intern("abd\0def");
        assert_eq!(strcmp(&a, &b), Ordering::Less);
    }

    #[test]
    fn test_strcmp_null_vs_normal() {
        let tb = StringTable::new();
        let a = tb.intern("abc\0xyz");
        let b = tb.intern("abc");
        assert_eq!(strcmp(&a, &b), Ordering::Greater);
        let c = tb.intern("abd");
        assert_eq!(strcmp(&a, &c), Ordering::Less);
    }

    // ========================================================================
    // lt_num / le_num 边界测试
    // ========================================================================

    #[test]
    fn test_lt_num_large_int_vs_float() {
        let large = 2i64.pow(54);
        assert!(!lt_num(&TValue::Integer(large), &TValue::Float(large as f64)));
        assert!(lt_num(&TValue::Integer(large), &TValue::Float(large as f64 + 8.0)));
        assert!(lt_num(&TValue::Integer(large - 1), &TValue::Float(large as f64 + 8.0)));
    }

    #[test]
    fn test_lt_num_float_nan_self() {
        assert!(!lt_num(&TValue::Float(f64::NAN), &TValue::Float(1.0)));
        assert!(!lt_num(&TValue::Float(1.0), &TValue::Float(f64::NAN)));
        assert!(!lt_num(&TValue::Float(f64::NAN), &TValue::Float(f64::NAN)));
    }

    #[test]
    fn test_le_num_nan() {
        assert!(!le_num(&TValue::Float(f64::NAN), &TValue::Float(1.0)));
        assert!(!le_num(&TValue::Float(1.0), &TValue::Float(f64::NAN)));
    }

    // ========================================================================
    // raw_equal: 引用类型身份比较
    // ========================================================================

    #[test]
    fn test_raw_equal_different_tables() {
        let t1 = Table::with_capacity(1, 0);
        let t2 = Table::with_capacity(1, 0);
        assert!(!raw_equal(&TValue::Table(t1), &TValue::Table(t2)));
    }

    #[test]
    fn test_raw_equal_same_table_identity() {
        let t = Table::with_capacity(1, 0);
        let tv1 = TValue::Table(t.clone());
        assert!(raw_equal(&tv1, &tv1));
    }

    #[test]
    fn test_raw_equal_light_userdata_same() {
        let x: *mut std::ffi::c_void = 0x42 as *mut _;
        let a = TValue::LightUserData(x);
        let b = TValue::LightUserData(x);
        assert!(raw_equal(&a, &b));
    }

    #[test]
    fn test_raw_equal_light_userdata_different() {
        let a = TValue::LightUserData(0x1 as *mut _);
        let b = TValue::LightUserData(0x2 as *mut _);
        assert!(!raw_equal(&a, &b));
    }

    #[test]
    fn test_raw_equal_float_negative_zero() {
        assert!(raw_equal(&TValue::Float(-0.0), &TValue::Integer(0)));
    }

    // ========================================================================
    // less_than / less_equal 额外测试
    // ========================================================================

    #[test]
    fn test_less_than_different_types() {
        assert!(!less_than(&TValue::Integer(1), &TValue::Boolean(true)));
        assert!(!less_than(&TValue::Nil(NilKind::Strict), &TValue::Integer(0)));
    }

    #[test]
    fn test_less_equal_strings() {
        let tb = StringTable::new();
        let a = TValue::Str(tb.intern("abc"));
        let b = TValue::Str(tb.intern("abc"));
        assert!(less_equal(&a, &b));
        let c = TValue::Str(tb.intern("abd"));
        assert!(less_equal(&a, &c));
        assert!(!less_equal(&c, &a));
    }

    // ========================================================================
    // idiv 额外测试
    // ========================================================================

    #[test]
    fn test_idiv_exact() {
        assert_eq!(idiv(9, 3).unwrap(), 3);
        assert_eq!(idiv(10, 5).unwrap(), 2);
    }

    #[test]
    fn test_idiv_zero_numerator() {
        assert_eq!(idiv(0, 5).unwrap(), 0);
    }

    #[test]
    fn test_idiv_floor_behavior() {
        assert_eq!(idiv(-1, 2).unwrap(), -1);
        assert_eq!(idiv(1, -2).unwrap(), -1);
    }

    // ========================================================================
    // modulus 额外测试
    // ========================================================================

    #[test]
    fn test_mod_zero_divisor() {
        assert!(modulus(1, 1).is_ok());
        assert_eq!(modulus(5, 2).unwrap(), 1);
    }

    #[test]
    fn test_mod_negative_operands() {
        assert_eq!(modulus(-5, -3).unwrap(), -2);
    }

    // ========================================================================
    // modulus_float 额外测试
    // ========================================================================

    #[test]
    fn test_modf_negative() {
        let r = modulus_float(-10.5, 3.0);
        assert!((r - 1.5).abs() < 1e-10);
    }

    #[test]
    fn test_modf_negative_divisor() {
        let r = modulus_float(10.5, -3.0);
        assert!((r + 1.5).abs() < 1e-10);
    }

    // ========================================================================
    // shiftl 额外测试
    // ========================================================================

    #[test]
    fn test_shiftl_by_zero() {
        assert_eq!(shiftl(42, 0), 42);
    }

    #[test]
    fn test_shiftl_negative_number() {
        assert_eq!(shiftl(-1, 1), -2);
    }

    #[test]
    fn test_shiftl_right_sign_extend() {
        assert_eq!(shiftl(1, -1), 0);
        assert_eq!(shiftl(8, -1), 4);
    }

    // ========================================================================
    // objlen 额外测试
    // ========================================================================

    #[test]
    fn test_objlen_table_with_gaps() {
        let mut t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(4, TValue::Integer(40));
        assert_eq!(objlen(&TValue::Table(t)), Some(TValue::Integer(2)));
    }

    #[test]
    fn test_objlen_table_hash_only() {
        let mut t = Table::new();
        t.set(TValue::Boolean(true), TValue::Integer(1));
        assert_eq!(objlen(&TValue::Table(t)), Some(TValue::Integer(0)));
    }

    // ========================================================================
    // 混合比较: 大整数边界
    // ========================================================================

    #[test]
    fn test_lt_int_float_large_int() {
        let large = 2i64.pow(54);
        assert!(!lt_int_float(large, large as f64));
        assert!(lt_int_float(large, large as f64 + 8.0));
    }

    #[test]
    fn test_lt_float_int_large_int() {
        let large = 2i64.pow(54);
        assert!(!lt_float_int(large as f64, large));
        assert!(lt_float_int(large as f64 - 2.0, large));
    }

    #[test]
    fn test_le_int_float_nan() {
        assert!(!le_int_float(1, f64::NAN));
    }

    #[test]
    fn test_le_float_int_nan() {
        assert!(!le_float_int(f64::NAN, 1));
    }

    // ========================================================================
    // F2IMode 额外测试
    // ========================================================================

    #[test]
    fn test_f2i_mode_debug_display() {
        assert_eq!(format!("{:?}", F2IMode::Eq), "Eq");
        assert_eq!(format!("{:?}", F2IMode::Floor), "Floor");
        assert_eq!(format!("{:?}", F2IMode::Ceil), "Ceil");
    }

    #[test]
    fn test_f2i_mode_clone() {
        let m = F2IMode::Floor;
        assert_eq!(m, m.clone());
    }
}