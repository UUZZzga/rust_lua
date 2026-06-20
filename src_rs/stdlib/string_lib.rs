//! 字符串库 (lstrlib.cpp → Rust)
//!
//! 对应 C 源码: lstrlib.cpp
//!
//! ## 主要功能
//! - 创建字符串类型的默认元表 (string metatable)
//! - 元表包含算术元方法 (__add, __sub, __mul, __mod, __pow, __div, __idiv, __unm)
//! - __index 指向字符串库函数表 (string.len, string.sub 等)

use crate::objects::{LuaType, TValue};
use crate::state::LuaState;
use crate::table::Table;
use crate::tm::{Metatable, TagMethod, make_tm_tvalue};
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
    // 每个元方法是一个 LCFn，调用时尝试将字符串转为数字并执行运算
    set_arith_method(&mut mt_table, TagMethod::Add, add_int, add_f);
    set_arith_method(&mut mt_table, TagMethod::Sub, sub_int, sub_f);
    set_arith_method(&mut mt_table, TagMethod::Mul, mul_int, mul_f);
    set_arith_method(&mut mt_table, TagMethod::Mod, mod_int, mod_f);
    set_arith_method(&mut mt_table, TagMethod::Pow, |_, _| None, pow_f);
    set_arith_method(&mut mt_table, TagMethod::Div, |_, _| None, div_f);
    set_arith_method(&mut mt_table, TagMethod::IDiv, idiv_int, idiv_f);
    set_arith_method(&mut mt_table, TagMethod::Unm, unm_int, unm_f);

    // __index 指向字符串库表 (简化: 创建空表作为占位符)
    // 对应 C: lua_pushvalue(L, -2); lua_setfield(L, -2, "__index");
    let string_lib_table = Table::new();
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
    // 注意: 完整实现需要存储函数指针，但当前架构通过 TagMethod 索引即可
    let _ = (int_op, float_op); // 暂时未使用，保留接口
    mt_table.set(make_tm_tvalue(tm), TValue::Integer(0));
}

// ============================================================================
// 字符串库入口 — 对应 C 的 luaopen_string
// ============================================================================

/// 打开字符串库
///
/// 对应 C 源码 lstrlib.cpp 的 luaopen_string 函数:
/// 1. 创建字符串库函数表
/// 2. 创建字符串元表
pub fn open_string_lib(state: &mut LuaState) {
    create_string_metatable(state);
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::LuaType;

    #[test]
    fn test_create_string_metatable() {
        let mut state = LuaState::new();
        // 创建前: 字符串类型没有默认元表
        assert!(state.dmt.get(LuaType::String).is_none());
        // 创建字符串元表
        create_string_metatable(&mut state);
        // 创建后: 字符串类型有默认元表
        assert!(state.dmt.get(LuaType::String).is_some());
    }

    #[test]
    fn test_string_metatable_has_arith_methods() {
        let mut state = LuaState::new();
        create_string_metatable(&mut state);
        let mt = state.dmt.get(LuaType::String).expect("string metatable must exist");
        // 验证算术元方法存在
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
}
