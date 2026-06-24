//! Table 库 (ltablib.cpp → Rust)
//!
//! 对应 C 源码: ltablib.cpp
//!
//! ## 主要功能
//! - 注册 table 全局表，包含表操作函数
//! - 提供 table.concat, table.unpack, table.pack, table.insert, table.remove 函数
//!
//! ## 标签分配
//! - 标签 400+: Table 库

use crate::objects::{NilKind, TValue};
use crate::state::LuaState;
use crate::execute::VmError;

// ============================================================================
// 函数标签 (LightUserData 占位符值)
// ============================================================================

pub const TABLE_CONCAT: usize = 400;
pub const TABLE_UNPACK: usize = 401;
pub const TABLE_PACK: usize = 402;
pub const TABLE_INSERT: usize = 403;
pub const TABLE_REMOVE: usize = 404;

/// Table 库标签范围: [400, 410)
pub fn is_table_tag(tag: usize) -> bool {
    (400..410).contains(&tag)
}

/// 将 table 库函数 tag 映射到函数名（用于 traceback）
pub fn table_function_name(tag: usize) -> Option<&'static str> {
    match tag {
        TABLE_CONCAT => Some("concat"),
        TABLE_UNPACK => Some("unpack"),
        TABLE_PACK => Some("pack"),
        TABLE_INSERT => Some("insert"),
        TABLE_REMOVE => Some("remove"),
        _ => None,
    }
}

// ============================================================================
// 栈操作辅助函数
// ============================================================================

/// 从栈中读取参数
fn get_arg(state: &LuaState, a: usize, idx: usize) -> TValue {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return TValue::Nil(NilKind::Strict);
    }
    state.stack[stack_idx].clone()
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
        _ => default,
    }
}

/// 获取表的长度 (使用 Table 自身的 len 实现，正确处理 array+hash 边界)
fn table_len(table: &crate::table::Table) -> i64 {
    table.len()
}

/// 获取表中指定整数键的值
fn table_get_int(table: &crate::table::Table, key: i64) -> TValue {
    table.get_int(key).unwrap_or(TValue::Nil(NilKind::Strict))
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

// ============================================================================
// 函数实现
// ============================================================================

/// table.concat(list [, sep [, i [, j]]]) — 对应 C 的 tconcat
fn table_concat_impl(
    table: &crate::table::Table,
    sep: &str,
    i: i64,
    j: i64,
) -> Result<String, String> {
    if i > j {
        return Ok(String::new());
    }

    let mut result = String::new();
    let mut idx = i;
    while idx < j {
        let val = table_get_int(table, idx);
        match &val {
            TValue::Str(s) => result.push_str(s.as_str()),
            TValue::Integer(n) => result.push_str(&n.to_string()),
            TValue::Float(f) => result.push_str(&format!("{}", f)),
            _ => return Err(format!(
                "invalid value (at index {}) in table for 'concat'", idx
            )),
        }
        result.push_str(sep);
        idx += 1;
    }
    // 添加最后一个元素 (不加分隔符)
    if idx == j {
        let val = table_get_int(table, idx);
        match &val {
            TValue::Str(s) => result.push_str(s.as_str()),
            TValue::Integer(n) => result.push_str(&n.to_string()),
            TValue::Float(f) => result.push_str(&format!("{}", f)),
            _ => return Err(format!(
                "invalid value (at index {}) in table for 'concat'", idx
            )),
        }
    }
    Ok(result)
}

/// table.unpack(list [, i [, j]]) — 对应 C 的 tunpack
fn table_unpack_impl(
    table: &crate::table::Table,
    i: i64,
    j: i64,
) -> Vec<TValue> {
    let mut result = Vec::new();
    let mut idx = i;
    while idx <= j {
        result.push(table_get_int(table, idx));
        idx += 1;
    }
    result
}

// ============================================================================
// 函数派发 — 从 execute.rs 调用
// ============================================================================

/// Table 库函数派发
pub fn call_table_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let prev_c_func = state.last_c_function.take();
    state.last_c_function = table_function_name(tag).map(|s| s.to_string());

    let result = match tag {
        TABLE_CONCAT => call_concat(state, a, nargs, nresults),
        TABLE_UNPACK => call_unpack(state, a, nargs, nresults),
        TABLE_PACK => call_pack(state, a, nargs, nresults),
        TABLE_INSERT => call_insert(state, a, nargs, nresults),
        TABLE_REMOVE => call_remove(state, a, nargs, nresults),
        _ => Err(VmError::RuntimeError(format!(
            "unknown table function tag: {}", tag
        ))),
    };

    if result.is_ok() {
        state.last_c_function = prev_c_func;
    }
    result
}

/// table.concat(list [, sep [, i [, j]]])
fn call_concat(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let list_val = get_arg(state, a, 0);
    let table = match &list_val {
        TValue::Table(t) => t.clone(),
        _ => return Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'concat' (table expected, got {})", list_val.ty()
        ))),
    };

    let sep = if nargs >= 2 {
        let sep_val = get_arg(state, a, 1);
        match &sep_val {
            TValue::Str(s) => s.as_str().to_string(),
            _ => String::new(),
        }
    } else {
        String::new()
    };

    let default_len = table_len(&table);
    let i = if nargs >= 3 { get_opt_int_arg(state, a, 2, 1) } else { 1 };
    let j = if nargs >= 4 { get_opt_int_arg(state, a, 3, default_len) } else { default_len };

    match table_concat_impl(&table, &sep, i, j) {
        Ok(result) => {
            push_results(state, a, nresults, vec![TValue::Str(state.intern_str(&result))]);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// table.unpack(list [, i [, j]])
fn call_unpack(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let list_val = get_arg(state, a, 0);
    let table = match &list_val {
        TValue::Table(t) => t.clone(),
        _ => return Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'unpack' (table expected, got {})", list_val.ty()
        ))),
    };

    let default_len = table_len(&table);
    let i = if nargs >= 2 { get_opt_int_arg(state, a, 1, 1) } else { 1 };
    let j = if nargs >= 3 { get_opt_int_arg(state, a, 2, default_len) } else { default_len };

    let results = table_unpack_impl(&table, i, j);
    push_results(state, a, nresults, results);
    Ok(())
}

/// table.pack(...)
fn call_pack(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let t = crate::table::Table::new();
    for i in 0..nargs {
        let val = get_arg(state, a, i);
        t.set_int((i + 1) as i64, val);
    }
    t.set(
        TValue::Str(state.intern_str("n")),
        TValue::Integer(nargs as i64),
    );
    push_results(state, a, nresults, vec![TValue::Table(t)]);
    Ok(())
}

/// table.insert(list, [pos,] value)
fn call_insert(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let list_val = get_arg(state, a, 0);
    let mut table = match &list_val {
        TValue::Table(t) => t.clone(),
        _ => return Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'insert' (table expected, got {})", list_val.ty()
        ))),
    };

    let len = table_len(&table);
    if nargs == 2 {
        // insert at end
        let val = get_arg(state, a, 1);
        table.set_int(len + 1, val);
    } else if nargs >= 3 {
        // insert at position
        let pos = get_opt_int_arg(state, a, 1, 0);
        if pos < 1 || pos > len + 1 {
            return Err(VmError::RuntimeError("bad argument #2 to 'insert' (position out of bounds)".to_string()));
        }
        let val = get_arg(state, a, 2);
        // shift elements up
        let mut i = len;
        while i >= pos {
            let v = table_get_int(&table, i);
            table.set_int(i + 1, v);
            i -= 1;
        }
        table.set_int(pos, val);
    }
    push_results(state, a, nresults, vec![TValue::Table(table)]);
    Ok(())
}

/// table.remove(list [, pos])
fn call_remove(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let list_val = get_arg(state, a, 0);
    let mut table = match &list_val {
        TValue::Table(t) => t.clone(),
        _ => return Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'remove' (table expected, got {})", list_val.ty()
        ))),
    };

    let len = table_len(&table);
    if len == 0 {
        push_results(state, a, nresults, vec![TValue::Nil(NilKind::Strict)]);
        return Ok(());
    }

    let pos = if nargs >= 2 { get_opt_int_arg(state, a, 1, len) } else { len };
    if pos < 1 || pos > len {
        return Err(VmError::RuntimeError("bad argument #2 to 'remove' (position out of bounds)".to_string()));
    }

    let removed = table_get_int(&table, pos);
    // shift elements down
    let mut i = pos;
    while i < len {
        let v = table_get_int(&table, i + 1);
        table.set_int(i, v);
        i += 1;
    }
    table.set_int(len, TValue::Nil(NilKind::Strict));

    push_results(state, a, nresults, vec![removed]);
    Ok(())
}

// ============================================================================
// 打开 Table 库 — 对应 C 的 luaopen_table
// ============================================================================

/// 打开 Table 库并注册到全局变量 table
pub fn open_table_lib(state: &mut LuaState) {
    let mut lib = crate::table::Table::new();

    let register = |lib: &mut crate::table::Table, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        lib.set(key, TValue::LightUserData(tag as *mut std::ffi::c_void));
    };

    register(&mut lib, "concat", TABLE_CONCAT);
    register(&mut lib, "unpack", TABLE_UNPACK);
    register(&mut lib, "pack", TABLE_PACK);
    register(&mut lib, "insert", TABLE_INSERT);
    register(&mut lib, "remove", TABLE_REMOVE);

    let key = TValue::Str(state.intern_str("table"));
    state.globals.set(key, TValue::Table(lib));
}
