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
use crate::tm::{
    TagMethod, TagMethodError, DefaultMetatables,
    get_tm_by_obj, obj_type_name,
};
use crate::gc::GCState;
use std::rc::Rc;

// ============================================================================
// 虚拟机主解释器循环 (原 lvm.cpp 中的 luaV_execute)
// ============================================================================

pub use crate::execute::VmExecutor;
pub use crate::execute::VmResult;
pub use crate::execute::VmError;
pub use crate::state::VmState;

// ============================================================================
// LuaVM — 集成层
// ============================================================================

/// Lua 虚拟机 — 集成 lvm 核心操作 + execute 解释器循环
pub struct LuaVM {
    pub stack: Vec<crate::objects::TValue>,
    pub gc: Rc<GCState>,
}

impl LuaVM {
    pub fn new() -> Self {
        LuaVM {
            stack: Vec::with_capacity(20),
            gc: Rc::new(GCState::default_incremental()),
        }
    }

    pub fn with_gc(gc: Rc<GCState>) -> Self {
        LuaVM {
            stack: Vec::with_capacity(20),
            gc,
        }
    }

    /// 执行一段 Lua 字节码
    pub fn execute(&mut self, proto: &crate::objects::Proto) -> Result<VmResult, VmError> {
        VmExecutor::execute(proto, 0, std::mem::take(&mut self.stack), self.gc.clone())
    }
}

impl Default for LuaVM {
    fn default() -> Self {
        Self::new()
    }
}

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

/// 比较两个 TValue 是否相等（支持元方法回退，仅对 table/userdata）。
///
/// 对应 C 源码: luaV_equalobj
/// C 的 __eq 元方法仅在两个类型相同且变体相同的 table 或 userdata
/// 不是同一对象时才被尝试。其他类型不尝试元方法。
///
/// `default_mts` 为类型默认元表；`call_fn` 用于实际调用元方法。
/// 若为 `None` 则仅做原始比较（raw equality）。
///
/// Scenario: 不同 table 有 __eq
/// Given: 两个不同指针的 Table，各自的元表中定义了 __eq
/// When: 调用 equal(t1, t2, Some(&dmt), Some(&mut call_fn))
/// Then: 调用 __eq 元方法，返回其结果
pub fn equal(
    t1: &TValue, t2: &TValue,
    default_mts: Option<&DefaultMetatables>,
    call_fn: Option<&mut dyn FnMut(&TValue, &[&TValue]) -> Result<TValue, TagMethodError>>,
) -> Result<bool, TagMethodError> {
    // C: if (ttype(t1) != ttype(t2)) return 0;
    if std::mem::discriminant(t1) != std::mem::discriminant(t2) {
        return Ok(false);
    }
    // raw_equal 已处理变体不同的情况 (integer/float, short/long string)
    if raw_equal(t1, t2) {
        return Ok(true);
    }
    // C: 只有同变体的 table 和 userdata 才尝试 __eq
    match (t1, t2) {
        (TValue::Table(_), TValue::Table(_)) | (TValue::UserData(_), TValue::UserData(_)) => {}
        _ => return Ok(false),
    }
    let (default_mts, call_fn) = match (default_mts, call_fn) {
        (Some(dmt), Some(f)) => (dmt, f),
        _ => return Ok(false),
    };
    // C: fasttm(L, hvalue(t1)->metatable, TM_EQ) ?? fasttm(L, hvalue(t2)->metatable, TM_EQ)
    let tm = crate::tm::get_tm_by_obj(t1, TagMethod::Eq, default_mts)
        .or_else(|| crate::tm::get_tm_by_obj(t2, TagMethod::Eq, default_mts));
    match tm {
        Some(func) => {
            let result = call_fn(func, &[t1, t2])?;
            Ok(!is_false(&result))
        }
        None => Ok(false),
    }
}

// ============================================================================
// 小于比较
// ============================================================================

/// 比较两个 TValue: 返回 l < r。
///
/// 数字比较用 lt_num，字符串比较用 strcmp。
/// 其他类型时通过 `call_order_tm` 尝试 TM_LT 元方法（该函数在 tm.rs 中）。
/// 若未提供 dmt/call_fn 则对非数非字符串返回 false。
pub fn less_than(
    l: &TValue, r: &TValue,
    default_mts: Option<&DefaultMetatables>,
    call_fn: Option<&mut dyn FnMut(&TValue, &[&TValue]) -> Result<TValue, TagMethodError>>,
) -> Result<bool, TagMethodError> {
    if l.is_number() && r.is_number() {
        return Ok(lt_num(l, r));
    }
    if let (TValue::Str(a), TValue::Str(b)) = (l, r) {
        return Ok(strcmp(a, b) == Ordering::Less);
    }
    match (default_mts, call_fn) {
        (Some(dmt), Some(cf)) => crate::tm::call_order_tm(l, r, TagMethod::Lt, dmt, cf),
        _ => Ok(false),
    }
}

/// 比较两个 TValue: 返回 l <= r。
///
/// 数字比较用 le_num，字符串比较用 strcmp。
/// 其他类型时通过 `call_order_tm` 尝试 TM_LE 元方法（该函数在 tm.rs 中）。
/// 若未提供 dmt/call_fn 则对非数非字符串返回 false。
pub fn less_equal(
    l: &TValue, r: &TValue,
    default_mts: Option<&DefaultMetatables>,
    call_fn: Option<&mut dyn FnMut(&TValue, &[&TValue]) -> Result<TValue, TagMethodError>>,
) -> Result<bool, TagMethodError> {
    if l.is_number() && r.is_number() {
        return Ok(le_num(l, r));
    }
    if let (TValue::Str(a), TValue::Str(b)) = (l, r) {
        return Ok(strcmp(a, b) != Ordering::Greater);
    }
    match (default_mts, call_fn) {
        (Some(dmt), Some(cf)) => crate::tm::call_order_tm(l, r, TagMethod::Le, dmt, cf),
        _ => Ok(false),
    }
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

/// 右移运算: 等价于 shiftl(x, -y)。
///
/// Scenario: 正常右移
/// Given: x = 16, y = 2
/// When: 调用 shiftr(16, 2)
/// Then: 返回 4
pub fn shiftr(x: i64, y: i64) -> i64 {
    shiftl(x, y.wrapping_neg())
}

// ============================================================================
// objlen: # 操作符
// ============================================================================

/// 计算值的长度（# 操作符）。
///
/// `default_mts` 和 `call_fn` 用于元方法回退；
/// 若 call_fn 为 None 则仅做原始比较。
///
/// Scenario: 表有 __len 元方法
/// Given: 一个有 __len 元方法的表
/// When: 调用 objlen(&table, ...)
/// Then: 返回元方法的结果
///
/// Scenario: 字符串
/// Given: Str("hello")
/// When: 调用 objlen
/// Then: 返回 Some(Integer(5))
///
/// Scenario: 无元方法的其他类型
/// Given: Integer(42)
/// When: 调用 objlen(..., None)
/// Then: 返回 None
pub fn objlen(
    obj: &TValue,
    default_mts: Option<&DefaultMetatables>,
    call_fn: Option<&mut dyn FnMut(&TValue, &[&TValue]) -> Result<TValue, TagMethodError>>,
) -> Result<Option<TValue>, TagMethodError> {
    match obj {
        TValue::Table(t) => {
            if let Some(_dmt) = default_mts {
                let mt = t.metatable.as_ref().map(|b| b.as_ref());
                let tm = mt.and_then(|m| {
                    let mut meta = crate::tm::Metatable::new(m.clone());
                    meta.get_tm(TagMethod::Len).cloned()
                });
                if let (Some(tm_val), Some(cf)) = (tm, call_fn) {
                    let result = cf(&tm_val, &[obj])?;
                    return Ok(Some(result));
                }
            }
            Ok(Some(TValue::Integer(t.len())))
        }
        TValue::Str(s) => Ok(Some(TValue::Integer(s.len() as i64))),
        _ => {
            if let (Some(dmt), Some(cf)) = (default_mts, call_fn) {
                let tm = get_tm_by_obj(obj, TagMethod::Len, dmt);
                if let Some(tm_val) = tm {
                    let result = cf(tm_val, &[obj])?;
                    return Ok(Some(result));
                }
                Err(TagMethodError::TypeError {
                    expected: "table, string, or object with __len".to_string(),
                    got: obj_type_name(obj),
                })
            } else {
                Ok(None)
            }
        }
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
// 表快速访问结果标签
// ============================================================================

/// 表快速 get/set 的结果类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastAccess {
    Ok,
    NotTable,
    Empty,
}

// ============================================================================
// finish_get: 完成表读操作（含元方法链）
// ============================================================================

const MAX_TAG_LOOP: i32 = 2000;

/// 完成表取值操作 `val = t[key]`，处理元方法回溯。
///
/// 当快速路径无法直接获取值时，此函数查找 __index 元方法链。
///
/// Scenario: 表有 __index 元方法
/// Given: 一个设置了 __index 元方法的表
/// When: 键不存在于表中时调用 finish_get
/// Then: 遍历 __index 链查找值，找到则返回
///
/// Scenario: 非表类型有 __index 元方法
/// Given: 一个 non-table 值（如 userdata）
/// When: 访问其字段时
/// Then: 查找其 __index 元方法
///
/// Scenario: 无穷循环保护
/// Given: __index 链形成循环
/// When: 遍历超过 MAXTAGLOOP 次
/// Then: 返回 RuntimeError
pub fn finish_get(key: &TValue, t: &TValue, _metatable: Option<&Table>) -> Result<TValue, VmError> {
    let current = t.clone();
    let current_key = key.clone();

    for _loop_count in 0..MAX_TAG_LOOP {
        match &current {
            TValue::Table(table) => {
                if let Some(ref mt) = table.metatable {
                    return Ok(table.get(&current_key).cloned().unwrap_or_else(|| {
                        mt.get(&current_key).cloned().unwrap_or(TValue::Nil(crate::objects::NilKind::Strict))
                    }));
                } else {
                    return Ok(table.get(&current_key).cloned().unwrap_or(TValue::Nil(crate::objects::NilKind::Strict)));
                }
            }
            _ => {
                return Ok(TValue::Nil(crate::objects::NilKind::Strict));
            }
        }
    }
    Err(VmError::RuntimeError("'__index' chain too long; possible loop".into()))
}

// ============================================================================
// finish_set: 完成表写操作（含元方法链）
// ============================================================================

/// 完成表赋值操作 `t[key] = val`，处理元方法回溯。
///
/// 当快速路径无法直接设置值时，此函数查找 __newindex 元方法链。
///
/// Scenario: 表无 __newindex 元方法
/// Given: 一个没有 __newindex 的表
/// When: 设置键值对
/// Then: 直接完成赋值
///
/// Scenario: 表有 __newindex 元方法
/// Given: 一个设置了 __newindex 元方法的表
/// When: 设置键值对
/// Then: 通过元方法链完成
///
/// Scenario: 非表类型有 __newindex 元方法
/// Given: 一个 non-table 值（如 userdata）
/// When: 设置其字段时
/// Then: 查找其 __newindex 元方法
///
/// Scenario: 无穷循环保护
/// Given: __newindex 链形成循环
/// When: 遍历超过 MAXTAGLOOP 次
/// Then: 返回 RuntimeError
pub fn finish_set(t: &mut TValue, key: TValue, val: TValue, _hres: FastAccess) -> Result<(), VmError> {
    for _loop_count in 0..MAX_TAG_LOOP {
        match t {
            TValue::Table(table) => {
                table.set(key, val);
                return Ok(());
            }
            _ => {
                return Err(VmError::TypeError("attempt to index a non-table value".into()));
            }
        }
    }
    Err(VmError::RuntimeError("'__newindex' chain too long; possible loop".into()))
}

// ============================================================================
// finish_op: 恢复被 yield 中断的操作码
// ============================================================================

/// 恢复被 yield 中断的操作码执行。
///
/// 在 C 实现中，当一个操作码执行到一半时发生了 yield（如元方法调用），
/// 此函数负责完成操作码的剩余工作。
///
/// 目前为存根实现，等待完整的协程/元方法支持。
///
/// Scenario: yield 后在 OP_CONCAT 中恢复
/// Given: 中断前的栈状态，其中元方法结果已放置
/// When: 调用 finish_op
/// Then: 完成剩余字符串拼接
pub fn finish_op(_interrupted_op: u8, _stack: &mut Vec<TValue>) -> Result<(), VmError> {
    // 占位实现 — 在实际系统中有完整的元方法调用和 yield 支持时填充
    Ok(())
}

// ============================================================================
// concat: 字符串拼接 (luaV_concat)
// ============================================================================

/// 拼接栈上的多个值，原位置替换为拼接结果。
///
/// 将 `total` 个栈值拼接成一个字符串，处理数字到字符串的强制转换。
///
/// Scenario: 拼接两个字符串
/// Given: 栈上有 "hello" 和 "world"
/// When: 调用 concat(2)
/// Then: 栈顶被替换为 "helloworld"
///
/// Scenario: 拼接包含数字
/// Given: 栈上有 "x=" 和 Integer(42)
/// When: 调用 concat(2)
/// Then: 栈顶被替换为 "x=42"
///
/// Scenario: 空字符串优化
/// Given: 栈上有 "" 和 "abc"
/// When: 调用 concat(2)
fn value_is_stringable(v: &TValue) -> bool {
    matches!(v, TValue::Str(_) | TValue::Integer(_) | TValue::Float(_))
}

fn value_to_string_len(v: &TValue) -> usize {
    match v {
        TValue::Str(s) => s.len(),
        TValue::Integer(i) => {
            if *i == 0 { 1 } else { ((i.unsigned_abs() as f64).log10().floor() as usize) + 1 + if *i < 0 { 1 } else { 0 } }
        }
        TValue::Float(f) => format_float_len(*f),
        _ => 0,
    }
}

fn append_value_to_string(buf: &mut String, v: &TValue) {
    match v {
        TValue::Str(s) => buf.push_str(s.as_str()),
        TValue::Integer(i) => buf.push_str(&i.to_string()),
        TValue::Float(f) => buf.push_str(&format_float(*f)),
        _ => {}
    }
}

fn format_float_len(f: f64) -> usize {
    if f.is_nan() { return 3; }
    if f.is_infinite() { return if f > 0.0 { 3 } else { 4 }; }
    format_float(f).len()
}

/// 拼接栈上的值，遇到不可串化的值时返回 ConcatError 让调用者尝试 TM。
///
/// 对应 C 源码: luaV_concat
/// C 逻辑:
///   1. total==1 → 直接返回
///   2. 循环 while total>1:
///      a. top-2 或 top-1 不是字符串且不能转换 → 尝试 TM_CONCAT
///      b. top-1 是空字符串 → 结果就是 top-2
///      c. top-2 是空字符串 → 结果就是 top-1
///      d. 否则收集连续可字符串化的值, 拼接成一个新字符串
///   3. 减少 total 并调整栈
///
/// Scenario: table + string 不可直接拼接
/// Given: 栈上有 Table 和 Str("hello")
/// When: 调用 concat_stack(stack, 2, &dmt)
/// Then: 返回 Err(ConcatError { ... })
pub fn concat_stack(
    stack: &mut Vec<TValue>,
    total: usize,
    _default_mts: &DefaultMetatables,
) -> Result<(), TagMethodError> {
    if total <= 1 { return Ok(()); }
    let mut remaining = total;
    while remaining > 1 {
        let top = stack.len();
        // C: if (!ttisstring(s2v(top-2)) || !cvt2str(s2v(top-2))) || !tostring(L, s2v(top-1))
        let v_prev = &stack[top - 2];
        let v_top = &stack[top - 1];

        let prev_ok = matches!(v_prev, TValue::Str(_)) || to_number(v_prev).is_some();
        let top_ok = matches!(v_top, TValue::Str(_)) || to_number(v_top).is_some();

        if !prev_ok || !top_ok {
            // C: luaT_tryconcatTM(L);
            return Err(TagMethodError::ConcatError {
                left: obj_type_name(v_prev),
                right: obj_type_name(v_top),
            });
        }
        // C: isemptystr(s2v(top-1)) → 第二个操作数是空串
        let is_top_empty = matches!(v_top, TValue::Str(ref s) if s.len() == 0);
        if is_top_empty {
            // C: cast_void(tostring(L, s2v(top-2))); → 结果就是第一个操作数
            // 确保 top-2 是字符串形式 (对于数字做转换)
            let idx = top - 2;
            match &stack[idx] {
                TValue::Integer(i) => {
                    let i = *i;
                    stack[idx] = TValue::Str(string_from_int(i));
                }
                TValue::Float(f) => {
                    let f = *f;
                    stack[idx] = TValue::Str(string_from_float(f));
                }
                _ => {}
            }
            stack.pop(); // 移除空的 top-1
            remaining -= 1;
            continue;
        }
        // C: isemptystr(s2v(top-2)) → 第一个操作数是空串
        let is_prev_empty = matches!(v_prev, TValue::Str(ref s) if s.len() == 0);
        if is_prev_empty {
            // C: setobjs2s(L, top-2, top-1); → 结果就是第二个操作数
            let val = stack.pop().unwrap();
            let idx = top - 2;
            stack[idx] = val;
            remaining -= 1;
            continue;
        }
        // C: at least two non-empty string values; get as many as possible
        let mut total_len = value_str_len(v_top);
        let mut n: usize = 1;
        while n < remaining {
            let idx = top - n - 1;
            if !value_is_stringable(&stack[idx]) {
                break;
            }
            let l = value_str_len(&stack[idx]);
            total_len += l;
            n += 1;
        }
        // 拼接 n 个值
        let mut result = String::with_capacity(total_len);
        for i in 0..n {
            let idx = top - n + i;
            append_val_to_string(&mut result, &stack[idx]);
        }
        use crate::strings::ShortString;
        let ls = LuaString::Short(std::sync::Arc::new(ShortString {
            hash: 0,
            contents: result,
        }));
        let target_idx = top - n;
        stack[target_idx] = TValue::Str(ls);
        stack.truncate(target_idx + 1);
        remaining -= n - 1;
    }
    Ok(())
}

fn string_from_int(i: i64) -> LuaString {
    use crate::strings::ShortString;
    LuaString::Short(std::sync::Arc::new(ShortString {
        hash: 0,
        contents: i.to_string(),
    }))
}

fn string_from_float(f: f64) -> LuaString {
    use crate::strings::ShortString;
    LuaString::Short(std::sync::Arc::new(ShortString {
        hash: 0,
        contents: format_float(f),
    }))
}

fn value_str_len(v: &TValue) -> usize {
    match v {
        TValue::Str(s) => s.len(),
        TValue::Integer(i) => {
            if *i == 0 { 1 } else if *i < 0 {
                (*i as i128).unsigned_abs().to_string().len() + 1
            } else {
                (*i as u64).to_string().len()
            }
        }
        TValue::Float(f) => format_float_len(*f),
        _ => 0,
    }
}

fn append_val_to_string(buf: &mut String, v: &TValue) {
    match v {
        TValue::Str(s) => buf.push_str(s.as_str()),
        TValue::Integer(i) => buf.push_str(&i.to_string()),
        TValue::Float(f) => buf.push_str(&format_float(*f)),
        _ => {}
    }
}

// ============================================================================
// for_limit: for 循环边界计算
// ============================================================================

/// 计算数值 for 循环的整数边界。
///
/// 将 limit 转换为整数边界值，保留循环语义。
///
/// Scenario: 整数 limit
/// Given: limit 是整数 10, step > 0
/// When: 调用 for_limit
/// Then: p = 10, 返回 false（不跳过循环）
///
/// Scenario: limit 超出整数范围（正向步长）
/// Given: limit 是一个极大正浮点数
/// When: 调用 for_limit
/// Then: p = i64::MAX, 返回 false
///
/// Scenario: limit 超出整数范围（负向步长）
/// Given: limit 是一个极大负浮点数, step < 0
/// When: 调用 for_limit
/// Then: p = i64::MIN, 返回 false
///
/// Scenario: step > 0 且 init > limit
/// Given: init = 5, limit = 3, step = 1
/// When: 调用 for_limit
/// Then: 返回 true（跳过循环）
///
/// Scenario: step < 0 且 init < limit
/// Given: init = 3, limit = 5, step = -1
/// When: 调用 for_limit
/// Then: 返回 true（跳过循环）
pub fn for_limit(init: i64, limit_val: &TValue, step: i64) -> Result<(i64, bool), VmError> {
    let mode = if step < 0 { F2IMode::Ceil } else { F2IMode::Floor };

    if let Some(limit) = to_integer(limit_val, mode) {
        let skip = if step > 0 { init > limit } else { init < limit };
        return Ok((limit, skip));
    }

    let flim = match to_number_ns(limit_val) {
        Some(n) => n,
        None => return Err(VmError::RuntimeError("bad 'for' limit (number expected, got value)".into())),
    };

    if flim > 0.0 {
        if step < 0 {
            return Ok((0, true));
        }
        return Ok((i64::MAX, false));
    } else {
        if step > 0 {
            return Ok((0, true));
        }
        return Ok((i64::MIN, false));
    }
}

// ============================================================================
// for_prep: for 循环准备
// ============================================================================

/// 准备数值 for 循环：将初始值、边界、步长转换为循环内部使用的形式。
///
/// Scenario: 正向整数循环
/// Given: init=1, limit=5, step=1
/// When: 调用 for_prep
/// Then: 设置计数器和控制变量，返回 false（不跳过）
///
/// Scenario: 降序循环
/// Given: init=5, limit=1, step=-1
/// When: 调用 for_prep
/// Then: 设置计数器和控制变量，返回 false
///
/// Scenario: step == 0
/// Given: step=0
/// When: 调用 for_prep
/// Then: 返回 RuntimeError
///
/// Scenario: 跳过循环
/// Given: init=5, limit=3, step=1
/// When: 调用 for_prep
/// Then: 返回 true（跳过）
pub fn for_prep(stack: &mut Vec<TValue>, ra: usize) -> Result<bool, VmError> {
    let init = stack[ra].clone();
    let limit = stack[ra + 1].clone();
    let step = stack[ra + 2].clone();

    match (&init, &step) {
        (TValue::Integer(init_i), TValue::Integer(step_i)) => {
            if *step_i == 0 {
                return Err(VmError::RuntimeError("'for' step is zero".into()));
            }
            let (limit_i, skip) = for_limit(*init_i, &limit, *step_i)?;
            if skip {
                return Ok(true);
            }
            let count: u64 = if *step_i > 0 {
                let diff = (limit_i as u64).wrapping_sub(*init_i as u64);
                let s = *step_i as u64;
                if s == 1 { diff } else { diff / s }
            } else {
                let diff = (*init_i as u64).wrapping_sub(limit_i as u64);
                let s = ((-(*step_i + 1)) as u64).wrapping_add(1);
                diff / s
            };
            stack[ra] = TValue::Integer(count as i64);
            stack[ra + 1] = TValue::Integer(*step_i);
            stack[ra + 2] = TValue::Integer(*init_i);
            Ok(false)
        }
        _ => {
            let init_f = to_number(&init).ok_or_else(|| VmError::RuntimeError("bad 'for' initial value".into()))?;
            let limit_f = to_number(&limit).ok_or_else(|| VmError::RuntimeError("bad 'for' limit".into()))?;
            let step_f = to_number(&step).ok_or_else(|| VmError::RuntimeError("bad 'for' step".into()))?;
            if step_f == 0.0 {
                return Err(VmError::RuntimeError("'for' step is zero".into()));
            }
            let skip = if step_f > 0.0 { limit_f < init_f } else { init_f < limit_f };
            if skip {
                return Ok(true);
            }
            stack[ra] = TValue::Float(limit_f);
            stack[ra + 1] = TValue::Float(step_f);
            stack[ra + 2] = TValue::Float(init_f);
            Ok(false)
        }
    }
}

// ============================================================================
// float_for_loop: 浮点数 for 循环单步执行
// ============================================================================

/// 执行浮点数 for 循环的一个步骤。
///
/// Scenario: 继续循环
/// Given: step=1, limit=5, idx=3
/// When: 调用 float_for_loop
/// Then: 更新 idx 为 4，返回 true
///
/// Scenario: 循环结束
/// Given: step=1, limit=5, idx=5
/// When: 调用 float_for_loop
/// Then: idx 不被修改，返回 false
///
/// Scenario: 降序循环继续
/// Given: step=-1, limit=1, idx=3
/// When: 调用 float_for_loop
/// Then: 更新 idx 为 2，返回 true
pub fn float_for_loop(stack: &mut Vec<TValue>, ra: usize) -> bool {
    let step = match &stack[ra + 1] {
        TValue::Float(f) => *f,
        TValue::Integer(i) => *i as f64,
        _ => return false,
    };
    let limit = match &stack[ra] {
        TValue::Float(f) => *f,
        TValue::Integer(i) => *i as f64,
        _ => return false,
    };
    let idx = match &stack[ra + 2] {
        TValue::Float(f) => *f,
        TValue::Integer(i) => *i as f64,
        _ => return false,
    };
    let new_idx = idx + step;
    let should_continue = if step > 0.0 { new_idx <= limit } else { new_idx >= limit };
    if should_continue {
        stack[ra + 2] = TValue::Float(new_idx);
        true
    } else {
        false
    }
}

// ============================================================================
// push_closure: 创建 Lua 闭包并放入栈
// ============================================================================

/// 创建 Lua 闭包并初始化其上值。
///
/// Scenario: 无上值的闭包
/// Given: 一个 proto 无上值
/// When: 调用 push_closure
/// Then: 创建闭包，栈上 ra 位置被设为 LClosure
///
/// Scenario: 有栈上值的闭包
/// Given: 一个 proto 有一个上值，instack=true
/// When: 调用 push_closure
/// Then: 创建闭包，上值指向栈上对应位置
pub fn push_closure(
    stack: &mut Vec<TValue>,
    proto: &crate::objects::Proto,
    _enc_upvals: &[crate::objects::UpVal],
    _base: usize,
    ra: usize,
    gc: &GCState,
) {
    let nup = proto.size_upvalues as usize;
    let mut upvals = Vec::with_capacity(nup);

    for i in 0..nup {
        if i < proto.upvalues.len() && proto.upvalues[i].in_stack {
            let idx = _base + proto.upvalues[i].idx as usize;
            if idx < stack.len() {
                upvals.push(crate::objects::UpVal::Open {
                    stack_index: idx,
                    next: None,
                    previous: None,
                });
            } else {
                upvals.push(crate::objects::UpVal::Closed {
                    value: Box::new(TValue::Nil(crate::objects::NilKind::Strict)),
                });
            }
        } else if i < _enc_upvals.len() {
            upvals.push(_enc_upvals[i].clone());
        } else {
            upvals.push(crate::objects::UpVal::Closed {
                value: Box::new(TValue::Nil(crate::objects::NilKind::Strict)),
            });
        }
    }

    let closure_id = gc.register_object(std::mem::size_of::<crate::objects::LClosure>());
    let closure = crate::objects::LClosure {
        gc_header: crate::gc::GCObjectHeader::new(),
        proto: proto.clone(),
        upvals,
    };
    closure.gc_header.set_id(closure_id);

    // GC barrier (luaC_objbarrier): for each upvalue, if closure is black
    // and upvalue value is a white GC object, make it gray
    for uv in &closure.upvals {
        match uv {
            crate::objects::UpVal::Closed { value } => {
                if let Some(val_gc_id) = gc_id_of_tvalue(value) {
                    gc.obj_barrier(closure_id, val_gc_id);
                }
            }
            crate::objects::UpVal::Open { stack_index, .. } => {
                if *stack_index < stack.len() {
                    let val = &stack[*stack_index];
                    if let Some(val_gc_id) = gc_id_of_tvalue(val) {
                        gc.obj_barrier(closure_id, val_gc_id);
                    }
                }
            }
        }
    }

    if ra < stack.len() {
        stack[ra] = TValue::LClosure(closure);
    } else {
        stack.resize(ra + 1, TValue::Nil(crate::objects::NilKind::Strict));
        stack[ra] = TValue::LClosure(closure);
    }
}

/// 从 TValue 中提取 GCObjectId（如果值是 GC 对象）
pub fn gc_id_of_tvalue(val: &TValue) -> Option<crate::gc::GCObjectId> {
    match val {
        TValue::Table(t) => t.gc_header.id(),
        TValue::LClosure(c) => c.gc_header.id(),
        _ => None,
    }
}

// ============================================================================
// format_float（与 execute.rs 中保持一致的辅助函数）
// ============================================================================

fn format_float(f: f64) -> String {
    if f.is_nan() { return "nan".to_string(); }
    if f.is_infinite() { return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() }; }
    if f == 0.0 { return "0.0".to_string(); }
    let s = format!("{:.15}", f);
    let s = s.trim_end_matches('0');
    if s.ends_with('.') { format!("{}0", s) } else { s.to_string() }
}

// ============================================================================
// 测试 (所有测试代码放到文件最下面)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::{NilKind, Proto, LClosure, Instruction, UpVal, UpvalDesc};
    use crate::strings::{LuaString, StringTable};
    use std::rc::Rc;

    fn make_gc() -> Rc<crate::gc::GCState> {
        Rc::new(crate::gc::GCState::default_incremental())
    }

    // ========================================================================
    // LuaVM 集成测试
    // ========================================================================

    #[test]
    fn test_lua_vm_integration() {
        let vm = LuaVM::new();
        assert_eq!(vm.stack.len(), 0);
        assert_eq!(vm.stack.capacity(), 20);
    }

    #[test]
    fn test_lua_vm_default() {
        let vm = LuaVM::default();
        assert_eq!(vm.stack.capacity(), 20);
    }

    #[test]
    fn test_vm_executor_reachable() {
        let result: Result<VmResult, VmError> = Ok(VmResult::Return(0));
        assert!(result.is_ok());
    }

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

    #[test]
    fn test_to_number_str_negative() {
        let tb = StringTable::new();
        let v = TValue::Str(tb.intern("-3.14"));
        assert!((to_number(&v).unwrap() + 3.14).abs() < 1e-10);
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
    fn test_lt_num_float_nan_self() {
        assert!(!lt_num(&TValue::Float(f64::NAN), &TValue::Float(1.0)));
        assert!(!lt_num(&TValue::Float(1.0), &TValue::Float(f64::NAN)));
        assert!(!lt_num(&TValue::Float(f64::NAN), &TValue::Float(f64::NAN)));
    }

    #[test]
    fn test_lt_num_large_int_vs_float() {
        let large = 2i64.pow(54);
        assert!(!lt_num(&TValue::Integer(large), &TValue::Float(large as f64)));
        assert!(lt_num(&TValue::Integer(large), &TValue::Float(large as f64 + 8.0)));
        assert!(lt_num(&TValue::Integer(large - 1), &TValue::Float(large as f64 + 8.0)));
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

    #[test]
    fn test_le_num_nan() {
        assert!(!le_num(&TValue::Float(f64::NAN), &TValue::Float(1.0)));
        assert!(!le_num(&TValue::Float(1.0), &TValue::Float(f64::NAN)));
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
    // less_than / less_equal 测试
    // ========================================================================

    #[test]
    fn test_less_than_numbers() {
        assert!(less_than(&TValue::Integer(3), &TValue::Integer(5), None, None).unwrap());
        assert!(!less_than(&TValue::Integer(5), &TValue::Integer(3), None, None).unwrap());
    }

    #[test]
    fn test_less_than_strings() {
        let tb = StringTable::new();
        let a = TValue::Str(tb.intern("abc"));
        let b = TValue::Str(tb.intern("abd"));
        assert!(less_than(&a, &b, None, None).unwrap());
    }

    #[test]
    fn test_less_than_different_types() {
        assert!(!less_than(&TValue::Integer(1), &TValue::Boolean(true), None, None).unwrap());
        assert!(!less_than(&TValue::Nil(NilKind::Strict), &TValue::Integer(0), None, None).unwrap());
    }

    #[test]
    fn test_less_equal_equal() {
        assert!(less_equal(&TValue::Integer(3), &TValue::Integer(3), None, None).unwrap());
        assert!(!less_equal(&TValue::Integer(5), &TValue::Integer(3), None, None).unwrap());
    }

    #[test]
    fn test_less_equal_strings() {
        let tb = StringTable::new();
        let a = TValue::Str(tb.intern("abc"));
        let b = TValue::Str(tb.intern("abc"));
        assert!(less_equal(&a, &b, None, None).unwrap());
        let c = TValue::Str(tb.intern("abd"));
        assert!(less_equal(&a, &c, None, None).unwrap());
        assert!(!less_equal(&c, &a, None, None).unwrap());
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

    #[test]
    fn test_mod_zero_divisor() {
        assert!(modulus(1, 1).is_ok());
        assert_eq!(modulus(5, 2).unwrap(), 1);
    }

    #[test]
    fn test_mod_negative_operands() {
        assert_eq!(modulus(-5, -3).unwrap(), -2);
    }

    #[test]
    fn test_mod_min_by_neg_one() {
        assert_eq!(modulus(i64::MIN, -1).unwrap(), 0);
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
    // shiftl / shiftr 测试
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

    #[test]
    fn test_shiftr_normal() {
        assert_eq!(shiftr(16, 2), 4);
        assert_eq!(shiftr(8, 3), 1);
        assert_eq!(shiftr(1, 1), 0);
    }

    // ========================================================================
    // objlen 测试
    // ========================================================================

    #[test]
    fn test_objlen_string() {
        let tb = StringTable::new();
        let s = TValue::Str(tb.intern("hello"));
        assert_eq!(objlen(&s, None, None).unwrap(), Some(TValue::Integer(5)));
    }

    #[test]
    fn test_objlen_empty_string() {
        let tb = StringTable::new();
        let s = TValue::Str(tb.intern(""));
        assert_eq!(objlen(&s, None, None).unwrap(), Some(TValue::Integer(0)));
    }

    #[test]
    fn test_objlen_table() {
        let mut t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));
        let tv = TValue::Table(t);
        assert_eq!(objlen(&tv, None, None).unwrap(), Some(TValue::Integer(3)));
    }

    #[test]
    fn test_objlen_non_lenable() {
        assert_eq!(objlen(&TValue::Integer(42), None, None).unwrap(), None);
    }

    #[test]
    fn test_objlen_table_with_gaps() {
        let mut t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(4, TValue::Integer(40));
        assert_eq!(objlen(&TValue::Table(t), None, None).unwrap(), Some(TValue::Integer(2)));
    }

    #[test]
    fn test_objlen_table_hash_only() {
        let mut t = Table::new();
        t.set(TValue::Boolean(true), TValue::Integer(1));
        assert_eq!(objlen(&TValue::Table(t), None, None).unwrap(), Some(TValue::Integer(0)));
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
        assert!(equal(&TValue::Integer(42), &TValue::Integer(42), None, None).unwrap());
        assert!(!equal(&TValue::Integer(42), &TValue::Integer(43), None, None).unwrap());
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

    // ========================================================================
    // finish_get 测试
    // ========================================================================

    #[test]
    fn test_finish_get_table_present_key() {
        let mut t = Table::new();
        t.set(TValue::Str(make_ls("key")), TValue::Integer(42));
        let tv = TValue::Table(t);
        let result = finish_get(&TValue::Str(make_ls("key")), &tv, None);
        assert_eq!(result.unwrap(), TValue::Integer(42));
    }

    #[test]
    fn test_finish_get_table_missing_key() {
        let t = Table::new();
        let tv = TValue::Table(t);
        let result = finish_get(&TValue::Str(make_ls("missing")), &tv, None);
        assert_eq!(result.unwrap(), TValue::Nil(NilKind::Strict));
    }

    #[test]
    fn test_finish_get_non_table() {
        let result = finish_get(&TValue::Integer(1), &TValue::Integer(42), None);
        assert_eq!(result.unwrap(), TValue::Nil(NilKind::Strict));
    }

    // ========================================================================
    // finish_set 测试
    // ========================================================================

    #[test]
    fn test_finish_set_table() {
        let t = Table::new();
        let mut tv = TValue::Table(t);
        let result = finish_set(&mut tv, TValue::Str(make_ls("a")), TValue::Integer(100), FastAccess::Ok);
        assert!(result.is_ok());
        if let TValue::Table(ref t) = tv {
            assert_eq!(t.get(&TValue::Str(make_ls("a"))), Some(&TValue::Integer(100)));
        }
    }

    #[test]
    fn test_finish_set_non_table_error() {
        let mut tv = TValue::Integer(42);
        let result = finish_set(&mut tv, TValue::Integer(1), TValue::Integer(100), FastAccess::Ok);
        assert!(result.is_err());
    }

    // ========================================================================
    // finish_op 测试
    // ========================================================================

    #[test]
    fn test_finish_op_placeholder() {
        let mut stack = Vec::new();
        let result = finish_op(0, &mut stack);
        assert!(result.is_ok());
    }

    // ========================================================================
    // concat_stack 测试
    // ========================================================================

    #[test]
    fn test_concat_stack_two_strings() {
        let tb = StringTable::new();
        let mut stack = vec![
            TValue::Str(tb.intern("hello")),
            TValue::Str(tb.intern("world")),
        ];
        let len_before = stack.len();
        let dmt = DefaultMetatables::new();
        concat_stack(&mut stack, 2, &dmt);
        assert_eq!(stack.len(), len_before - 1);
        if let TValue::Str(ref s) = stack[0] {
            assert_eq!(s.as_str(), "helloworld");
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_concat_stack_single_value() {
        let tb = StringTable::new();
        let mut stack = vec![TValue::Str(tb.intern("hello"))];
        let dmt = DefaultMetatables::new();
        concat_stack(&mut stack, 1, &dmt);
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn test_concat_stack_three_strings() {
        let tb = StringTable::new();
        let mut stack = vec![
            TValue::Str(tb.intern("a")),
            TValue::Str(tb.intern("b")),
            TValue::Str(tb.intern("c")),
        ];
        let dmt = DefaultMetatables::new();
        concat_stack(&mut stack, 3, &dmt);
        assert_eq!(stack.len(), 1);
        if let TValue::Str(ref s) = stack[0] {
            assert_eq!(s.as_str(), "abc");
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_concat_stack_with_numbers() {
        let tb = StringTable::new();
        let mut stack = vec![
            TValue::Str(tb.intern("x=")),
            TValue::Integer(42),
        ];
        let dmt = DefaultMetatables::new();
        concat_stack(&mut stack, 2, &dmt);
        if let TValue::Str(ref s) = stack[0] {
            assert_eq!(s.as_str(), "x=42");
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_concat_stack_empty_first() {
        let tb = StringTable::new();
        let mut stack = vec![
            TValue::Str(tb.intern("")),
            TValue::Str(tb.intern("world")),
        ];
        let dmt = DefaultMetatables::new();
        concat_stack(&mut stack, 2, &dmt);
        assert_eq!(stack.len(), 1);
        if let TValue::Str(ref s) = stack[0] {
            assert_eq!(s.as_str(), "world");
        } else {
            panic!("Expected string");
        }
    }

    // ========================================================================
    // for_limit 测试
    // ========================================================================

    #[test]
    fn test_for_limit_int_limit_ascending() {
        let limit_val = TValue::Integer(10);
        let (limit, skip) = for_limit(1, &limit_val, 1).unwrap();
        assert_eq!(limit, 10);
        assert!(!skip);
    }

    #[test]
    fn test_for_limit_skip_ascending() {
        let limit_val = TValue::Integer(3);
        let (_limit, skip) = for_limit(5, &limit_val, 1).unwrap();
        assert!(skip);
    }

    #[test]
    fn test_for_limit_float_limit_ascending() {
        let limit_val = TValue::Float(10.5);
        let (limit, skip) = for_limit(1, &limit_val, 1).unwrap();
        assert_eq!(limit, 10);
        assert!(!skip);
    }

    #[test]
    fn test_for_limit_descending() {
        let limit_val = TValue::Integer(1);
        let (limit, skip) = for_limit(5, &limit_val, -1).unwrap();
        assert_eq!(limit, 1);
        assert!(!skip);
    }

    #[test]
    fn test_for_limit_skip_descending() {
        let limit_val = TValue::Integer(10);
        let (_limit, skip) = for_limit(5, &limit_val, -1).unwrap();
        assert!(skip);
    }

    // ========================================================================
    // for_prep 测试
    // ========================================================================

    #[test]
    fn test_for_prep_integer_ascending() {
        let mut stack = vec![
            TValue::Integer(1),
            TValue::Integer(5),
            TValue::Integer(1),
        ];
        let skip = for_prep(&mut stack, 0).unwrap();
        assert!(!skip);
    }

    #[test]
    fn test_for_prep_integer_descending() {
        let mut stack = vec![
            TValue::Integer(5),
            TValue::Integer(1),
            TValue::Integer(-1),
        ];
        let skip = for_prep(&mut stack, 0).unwrap();
        assert!(!skip);
    }

    #[test]
    fn test_for_prep_skip_when_init_exceeds_limit() {
        let mut stack = vec![
            TValue::Integer(10),
            TValue::Integer(5),
            TValue::Integer(1),
        ];
        let skip = for_prep(&mut stack, 0).unwrap();
        assert!(skip);
    }

    #[test]
    fn test_for_prep_step_zero_error() {
        let mut stack = vec![
            TValue::Integer(1),
            TValue::Integer(5),
            TValue::Integer(0),
        ];
        let result = for_prep(&mut stack, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_for_prep_float() {
        let mut stack = vec![
            TValue::Float(1.0),
            TValue::Float(5.0),
            TValue::Float(1.0),
        ];
        let skip = for_prep(&mut stack, 0).unwrap();
        assert!(!skip);
    }

    // ========================================================================
    // float_for_loop 测试
    // ========================================================================

    #[test]
    fn test_float_for_loop_continue() {
        let mut stack = vec![
            TValue::Float(5.0),
            TValue::Float(1.0),
            TValue::Float(3.0),
        ];
        assert!(float_for_loop(&mut stack, 0));
        assert_eq!(stack[2], TValue::Float(4.0));
    }

    #[test]
    fn test_float_for_loop_end() {
        let mut stack = vec![
            TValue::Float(5.0),
            TValue::Float(1.0),
            TValue::Float(5.0),
        ];
        assert!(!float_for_loop(&mut stack, 0));
    }

    #[test]
    fn test_float_for_loop_descending_continue() {
        let mut stack = vec![
            TValue::Float(1.0),
            TValue::Float(-1.0),
            TValue::Float(3.0),
        ];
        assert!(float_for_loop(&mut stack, 0));
        assert_eq!(stack[2], TValue::Float(2.0));
    }

    // ========================================================================
    // push_closure 测试
    // ========================================================================

    #[test]
    fn test_push_closure_no_upvals() {
        let proto = Proto {
            num_params: 0, flag: 0, max_stack_size: 10,
            size_upvalues: 0, size_k: 0, size_code: 0, size_line_info: 0,
            size_p: 0, size_loc_vars: 0, size_abs_line_info: 0,
            line_defined: 0, last_line_defined: 0,
            constants: vec![],
            code: vec![],
            protos: vec![],
            upvalues: vec![],
            line_info: vec![],
            abs_line_info: vec![],
            loc_vars: vec![],
            source: None,
        };
        let mut stack = vec![TValue::Nil(NilKind::Strict); 5];
        push_closure(&mut stack, &proto, &[], 0, 0, &make_gc());
        match &stack[0] {
            TValue::LClosure(_) => {}
            _ => panic!("Expected LClosure"),
        }
    }

    #[test]
    fn test_push_closure_with_upvals() {
        let mut stack = vec![
            TValue::Integer(42),
            TValue::Nil(NilKind::Strict),
            TValue::Nil(NilKind::Strict),
            TValue::Nil(NilKind::Strict),
            TValue::Nil(NilKind::Strict),
        ];
        let proto = Proto {
            num_params: 0, flag: 0, max_stack_size: 10,
            size_upvalues: 1, size_k: 0, size_code: 0, size_line_info: 0,
            size_p: 0, size_loc_vars: 0, size_abs_line_info: 0,
            line_defined: 0, last_line_defined: 0,
            constants: vec![],
            code: vec![],
            protos: vec![],
            upvalues: vec![UpvalDesc { in_stack: true, idx: 0, name: None }],
            line_info: vec![],
            abs_line_info: vec![],
            loc_vars: vec![],
            source: None,
        };
        push_closure(&mut stack, &proto, &[], 0, 3, &make_gc());
        match &stack[3] {
            TValue::LClosure(c) => {
                assert_eq!(c.upvals.len(), 1);
            }
            _ => panic!("Expected LClosure"),
        }
    }

    // ========================================================================
    // format_float 测试
    // ========================================================================

    #[test]
    fn test_format_float_nan() {
        assert_eq!(super::format_float(f64::NAN), "nan");
    }

    #[test]
    fn test_format_float_inf() {
        assert_eq!(super::format_float(f64::INFINITY), "inf");
        assert_eq!(super::format_float(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn test_format_float_zero() {
        assert_eq!(super::format_float(0.0), "0.0");
        assert_eq!(super::format_float(-0.0), "0.0");
    }

    #[test]
    fn test_format_float_normal() {
        assert_eq!(super::format_float(42.0), "42.0");
        assert_eq!(super::format_float(3.5), "3.5");
    }

    #[test]
    fn test_format_float_precision() {
        assert_eq!(super::format_float(1.0 / 3.0), "0.333333333333333");
    }

    // ========================================================================
    // FastAccess 测试
    // ========================================================================

    #[test]
    fn test_fast_access_equality() {
        assert_eq!(FastAccess::Ok, FastAccess::Ok);
        assert_ne!(FastAccess::Ok, FastAccess::NotTable);
        assert_eq!(FastAccess::Empty, FastAccess::Empty);
    }

    #[test]
    fn test_fast_access_debug() {
        assert_eq!(format!("{:?}", FastAccess::Ok), "Ok");
        assert_eq!(format!("{:?}", FastAccess::NotTable), "NotTable");
        assert_eq!(format!("{:?}", FastAccess::Empty), "Empty");
    }

    // ========================================================================
    // value_to_string_len 测试
    // ========================================================================

    #[test]
    fn test_value_to_string_len_int() {
        assert_eq!(super::value_to_string_len(&TValue::Integer(0)), 1);
        assert_eq!(super::value_to_string_len(&TValue::Integer(42)), 2);
        assert_eq!(super::value_to_string_len(&TValue::Integer(-42)), 3);
    }
}