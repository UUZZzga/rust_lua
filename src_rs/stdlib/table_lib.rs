//! Table 库 (ltablib.cpp → Rust)
//!
//! 对应 C 源码: ltablib.cpp
//!
//! ## 主要功能
//! - 注册 table 全局表，包含表操作函数
//! - 提供 table.concat, table.unpack, table.pack, table.insert, table.remove 函数
//!
//! ## 迁移说明
//! - 已从 LightUserData(tag) 迁移到 BuiltinFn 函数指针方案

use crate::execute::{arg_error, VmError};
use crate::objects::{BuiltinFn, NilKind, TValue};
use crate::state::LuaState;
use crate::tm::{call_order_tm, obj_type_name, TagMethod};
use crate::vm::VmExecutor;

// ============================================================================
// 函数标签 (已迁移到 BuiltinFn，不再使用 LightUserData tag)
// ============================================================================
// 标签 400+: Table 库（已迁移到 BuiltinFn，不再使用 tag）

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

/// 获取对象长度，支持 __len 元方法 (对应 C 的 luaL_len)
///
/// 调用 obj_len (会触发 __len 元方法)，然后转为整数。
/// 如果结果不是整数，报 "object length is not an integer"。
fn get_obj_len(state: &mut LuaState, obj: &TValue) -> Result<i64, VmError> {
    let tmp_ra = state.stack.len();
    crate::tm::obj_len(state, tmp_ra, obj, "")?;
    let result = state
        .stack
        .get(tmp_ra)
        .cloned()
        .unwrap_or(TValue::Nil(NilKind::Strict));
    state.stack.truncate(tmp_ra);
    match result {
        TValue::Integer(n) => Ok(n),
        TValue::Float(f) => crate::vm::float_to_integer(f, crate::vm::F2IMode::Eq)
            .ok_or_else(|| VmError::RuntimeError("object length is not an integer".to_string())),
        _ => Err(VmError::RuntimeError(
            "object length is not an integer".to_string(),
        )),
    }
}

/// 获取 t[i]，支持 __index 元方法 (对应 C lua_geti → luaV_finishget)
/// 用于 table 库函数对表元素访问时透明地调用元方法（如 proxy 表）
#[inline]
fn geti_meta(state: &mut LuaState, table_val: &TValue, i: i64) -> Result<TValue, VmError> {
    VmExecutor::table_get(
        state,
        table_val,
        &TValue::Integer(i),
        crate::execute::VarSource::None,
    )
}

/// 设置 t[i] = v，支持 __newindex 元方法 (对应 C lua_seti → luaV_finishset)
#[inline]
fn seti_meta(state: &mut LuaState, table_val: &TValue, i: i64, val: TValue) -> Result<(), VmError> {
    VmExecutor::table_set(
        state,
        table_val.clone(),
        TValue::Integer(i),
        val,
        crate::execute::VarSource::None,
    )
}

#[inline]
fn seti_meta_(state: &mut LuaState, table_val: TValue, i: i64, val: TValue) -> Result<(), VmError> {
    VmExecutor::table_set(
        state,
        table_val,
        TValue::Integer(i),
        val,
        crate::execute::VarSource::None,
    )
}

/// 将结果压入栈并调整栈顶
#[inline]
fn push_results(state: &mut LuaState, a: usize, nresults: i32, results: Vec<TValue>) {
    state.adjust_results(a, nresults, results);
}

// ============================================================================
// 函数实现
// ============================================================================

/// table.concat(list [, sep [, i [, j]]]) — 对应 C 的 tconcat
/// 通过 __index 元方法访问元素 (对应 C lua_geti)
fn table_concat_impl(
    state: &mut LuaState,
    table_val: &TValue,
    sep: &str,
    i: i64,
    j: i64,
) -> Result<String, VmError> {
    if i > j {
        return Ok(String::new());
    }

    let push_val = |result: &mut String, val: &TValue, idx: i64| -> Result<(), VmError> {
        match val {
            TValue::Str(s) => result.push_str(s.as_str()),
            TValue::Integer(n) => result.push_str(&n.to_string()),
            TValue::Float(f) => result.push_str(&format!("{}", f)),
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "invalid value (at index {}) in table for 'concat'",
                    idx
                )))
            }
        }
        Ok(())
    };

    let mut result = String::new();
    let mut idx = i;
    while idx < j {
        let val = geti_meta(state, table_val, idx)?;
        push_val(&mut result, &val, idx)?;
        result.push_str(sep);
        idx += 1;
    }
    if idx == j {
        let val = geti_meta(state, table_val, idx)?;
        push_val(&mut result, &val, idx)?;
    }
    Ok(result)
}

// ============================================================================
// 函数派发 — 已迁移到 BuiltinFn，直接通过函数指针调用
// ============================================================================

/// table.concat(list [, sep [, i [, j]]])
fn call_concat(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let list_val = get_arg(state, a, 0);
    match &list_val {
        TValue::Table(_) => {}
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'concat' (table expected, got {})",
                list_val.ty()
            )))
        }
    }

    let sep = if nargs >= 2 {
        let sep_val = get_arg(state, a, 1);
        match &sep_val {
            TValue::Str(s) => s.as_str().to_string(),
            _ => String::new(),
        }
    } else {
        String::new()
    };

    let default_len = get_obj_len(state, &list_val)?;
    let i = if nargs >= 3 {
        get_opt_int_arg(state, a, 2, 1)
    } else {
        1
    };
    let j = if nargs >= 4 {
        get_opt_int_arg(state, a, 3, default_len)
    } else {
        default_len
    };

    let result = table_concat_impl(state, &list_val, &sep, i, j)?;
    push_results(
        state,
        a,
        nresults,
        vec![TValue::Str(state.intern_str(&result))],
    );
    Ok(())
}

/// table.unpack(list [, i [, j]])
/// 对应 C Lua 的 tunpack: 直接 push 到栈,不创建中间 Vec
fn call_unpack(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let list_val = get_arg(state, a, 0);
    match &list_val {
        TValue::Table(_) => {}
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'unpack' (table expected, got {})",
                list_val.ty()
            )))
        }
    }

    let default_len = get_obj_len(state, &list_val)?;
    let i = if nargs >= 2 {
        get_opt_int_arg(state, a, 1, 1)
    } else {
        1
    };
    let j = if nargs >= 3 {
        get_opt_int_arg(state, a, 2, default_len)
    } else {
        default_len
    };

    if i > j {
        // 空范围: 返回 0 个值 (对应 C 的 return 0)
        push_results(state, a, nresults, vec![]);
        return Ok(());
    }
    // 对应 C Lua 的 lua_checkstack 检查: 元素数量超过 INT_MAX 或栈空间不足时报错
    // C: n = l_castS2U(e) - l_castS2U(i); ++n; (用 unsigned 算术避免溢出)
    let n_minus_1 = (j as u64).wrapping_sub(i as u64);
    if n_minus_1 >= i32::MAX as u64 {
        return Err(VmError::RuntimeError(
            "too many results to unpack".to_string(),
        ));
    }
    let n = n_minus_1 as usize + 1;
    if n >= i32::MAX as usize || state.stack.len().saturating_add(n) > crate::state::MAXSTACK {
        return Err(VmError::RuntimeError(
            "too many results to unpack".to_string(),
        ));
    }

    // 预留栈空间。Rust TValue（96 字节）比 C 的 16 字节大 6 倍，大 n 时分配可能 OOM。
    // 用 try_reserve_exact 避免 panic，将 OOM 转为可被 pcall/resume 捕获的运行时错误。
    // 沿用 "too many results to unpack" 消息（与 MAXSTACK 检查一致），让 errors.lua:615 的
    // checkerr("too many results", f) 能匹配。
    state
        .stack
        .try_reserve_exact(n)
        .map_err(|_| VmError::RuntimeError("too many results to unpack".to_string()))?;

    // 直接 push 到 state.stack，不创建中间 Vec
    // 对应 C 版 tunpack: while (i < e) { lua_geti(L, 1, i); i++; } lua_geti(L, 1, e);
    let first_result_pos = state.stack.len();
    let mut idx = i;
    while idx < j {
        let val = geti_meta(state, &list_val, idx)?;
        state.stack.push(val);
        idx += 1;
    }
    let val = geti_meta(state, &list_val, j)?;
    state.stack.push(val);

    state.adjust_results_on_stack(a, nresults, n, first_result_pos);
    Ok(())
}

/// table.pack(...)
fn call_pack(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
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
fn call_insert(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let list_val = get_arg(state, a, 0);
    match &list_val {
        TValue::Table(_) => {}
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'insert' (table expected, got {})",
                list_val.ty()
            )))
        }
    }

    let len = get_obj_len(state, &list_val)?;
    // 对应 C: e = aux_getn + 1 (luaL_intop(+, e, 1) — wrap-around 安全)
    let e = len.wrapping_add(1);
    if nargs == 2 {
        // insert at end — 对应 C: pos = e; lua_seti(L, 1, pos)
        let val = get_arg(state, a, 1);
        seti_meta_(state, list_val, e, val)?;
    } else if nargs == 3 {
        // insert at position
        let pos = get_opt_int_arg(state, a, 1, 0);
        // C: luaL_argcheck(L, (lua_Unsigned)pos - 1u < (lua_Unsigned)e, ...) → pos ∈ [1, e]
        if (pos as u64).wrapping_sub(1) >= (e as u64) {
            return Err(VmError::RuntimeError(
                "bad argument #2 to 'insert' (position out of bounds)".to_string(),
            ));
        }
        let val = get_arg(state, a, 2);
        // shift elements up — 对应 C: for (i=e; i>pos; i--) { t[i] = t[i-1] }
        let mut i = e;
        while i > pos {
            let v = geti_meta(state, &list_val, i - 1)?;
            seti_meta(state, &list_val, i, v)?;
            i -= 1;
        }
        seti_meta_(state, list_val, pos, val)?;
    } else {
        // 对应 C: default → "wrong number of arguments to 'insert'"
        return Err(VmError::RuntimeError(
            "wrong number of arguments to 'insert'".to_string(),
        ));
    }
    push_results(state, a, nresults, vec![]);
    Ok(())
}

/// table.remove(list [, pos])
fn call_remove(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let list_val = get_arg(state, a, 0);
    match &list_val {
        TValue::Table(_) => {}
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'remove' (table expected, got {})",
                list_val.ty()
            )))
        }
    }

    let len = get_obj_len(state, &list_val)?;

    // 对应 C ltablib.cpp tremove:
    //   pos 默认 = size; 仅当 pos != size 时才校验 pos ∈ [1, size+1]
    //   (无符号下溢让 pos<=0 变巨大值 > size, 自然失败)
    // 这允许 size=0, pos=0 时取 t[0] (如 a={[0]="ban"}, table.remove(a) 返回 "ban")
    let pos = if nargs >= 2 {
        get_opt_int_arg(state, a, 1, len)
    } else {
        len
    };
    if pos != len {
        let pos_u = pos as u64;
        if pos_u.wrapping_sub(1) > (len as u64) {
            return Err(VmError::RuntimeError(
                "bad argument #2 to 'remove' (position out of bounds)".to_string(),
            ));
        }
    }

    let removed = geti_meta(state, &list_val, pos)?;
    // shift elements down — 对应 C: for (; pos < size; pos++) t[pos] = t[pos+1]
    let mut i = pos;
    while i < len {
        let v = geti_meta(state, &list_val, i + 1)?;
        seti_meta(state, &list_val, i, v)?;
        i += 1;
    }
    // 清除 shift 后的最终位置 (i == min(pos, len)) — 对应 C 的 lua_seti(L, 1, pos)
    // 当 pos <= len: i == len, 清除 t[len] (原最后一个元素被 shift 覆盖后的位置)
    // 当 pos == len+1: shift 不执行, i == pos == len+1, 清除 t[len+1] (不影响 t[len])
    seti_meta_(state, list_val, i, TValue::Nil(NilKind::Strict))?;

    push_results(state, a, nresults, vec![removed]);
    Ok(())
}

// ============================================================================
// 快速排序 (对应 C 的 sort_comp / partition / auxsort)
// ============================================================================

const RANLIMIT: u32 = 100;

fn sort_comp(state: &mut LuaState, comp: &TValue, a: &TValue, b: &TValue) -> Result<bool, VmError> {
    if matches!(comp, TValue::Nil(_)) {
        if a.is_number() && b.is_number() {
            Ok(crate::vm::lt_num(a, b))
        } else if let (TValue::Str(s1), TValue::Str(s2)) = (a, b) {
            Ok(crate::vm::strcmp(s1, s2) == std::cmp::Ordering::Less)
        } else {
            call_order_tm(state, a, b, TagMethod::Lt)
        }
    } else {
        call_comp_function(state, comp, a, b)
    }
}

fn partition(
    state: &mut LuaState,
    elems: &mut [TValue],
    lo: i64,
    up: i64,
    comp: &TValue,
) -> Result<i64, VmError> {
    let pivot = elems[(up - 1) as usize].clone();
    let mut i = lo;
    let mut j = up - 1;
    loop {
        loop {
            i += 1;
            if !sort_comp(state, comp, &elems[i as usize], &pivot)? {
                break;
            }
            if i == up - 1 {
                return Err(VmError::RuntimeError(
                    "invalid order function for sorting".to_string(),
                ));
            }
        }
        loop {
            j -= 1;
            let ej = elems
                .get(j as usize)
                .cloned()
                .unwrap_or(TValue::Nil(NilKind::Strict));
            if !sort_comp(state, comp, &pivot, &ej)? {
                break;
            }
            if j < i {
                return Err(VmError::RuntimeError(
                    "invalid order function for sorting".to_string(),
                ));
            }
        }
        if j < i {
            elems.swap((up - 1) as usize, i as usize);
            return Ok(i);
        }
        elems.swap(i as usize, j as usize);
    }
}

fn auxsort(
    state: &mut LuaState,
    elems: &mut [TValue],
    lo: i64,
    up: i64,
    rnd: u32,
    comp: &TValue,
) -> Result<(), VmError> {
    let mut lo = lo;
    let mut up = up;
    let mut rnd = rnd;
    while lo < up {
        let p: i64;
        let n: i64;
        if sort_comp(state, comp, &elems[up as usize], &elems[lo as usize])? {
            elems.swap(lo as usize, up as usize);
        }
        if up - lo == 1 {
            return Ok(());
        }
        if ((up - lo) as u32) < RANLIMIT || rnd == 0 {
            p = (lo + up) / 2;
        } else {
            let r4 = (up - lo) / 4;
            let rnd_i = rnd as i64;
            p = ((rnd_i ^ lo ^ up) % (r4 * 2)) + lo + r4;
        }
        if sort_comp(state, comp, &elems[p as usize], &elems[lo as usize])? {
            elems.swap(p as usize, lo as usize);
        } else if sort_comp(state, comp, &elems[up as usize], &elems[p as usize])? {
            elems.swap(p as usize, up as usize);
        }
        if up - lo == 2 {
            return Ok(());
        }
        elems.swap(p as usize, (up - 1) as usize);
        let pp = partition(state, elems, lo, up, comp)?;
        if pp - lo < up - pp {
            auxsort(state, elems, lo, pp - 1, rnd, comp)?;
            n = pp - lo;
            lo = pp + 1;
        } else {
            auxsort(state, elems, pp + 1, up, rnd, comp)?;
            n = up - pp;
            up = pp - 1;
        }
        if (up - lo) / 128 > n {
            rnd = 0;
        }
    }
    Ok(())
}

/// table.sort(table [, comp]) — 对应 C 的 sort
///
/// 对 table 的数组部分进行原地排序。comp 是可选的比较函数，
/// 接受两个参数，返回 true 如果第一个小于第二个。
/// 无 comp 时使用 Lua 的 < 运算符。
fn call_sort(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let table_val = get_arg(state, a, 0);
    let comp_val = if nargs >= 2 {
        get_arg(state, a, 1)
    } else {
        TValue::Nil(NilKind::Strict)
    };

    match &table_val {
        TValue::Table(_) => {}
        _ => {
            return Err(arg_error(
                state,
                1,
                &format!("table expected, got {}", obj_type_name(&table_val)),
            ))
        }
    }

    let n = get_obj_len(state, &table_val)?;

    // 对应 C: if (n > 1) { luaL_argcheck(L, n < INT_MAX, 1, "array too big"); ... }
    if n <= 1 {
        push_results(state, a, nresults, vec![]);
        return Ok(());
    }
    if n >= i32::MAX as i64 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'sort' (array too big)".to_string(),
        ));
    }

    // 提取数组元素到 Vec — 通过 __index 元方法访问 (对应 C lua_geti)
    let mut elems: Vec<TValue> = Vec::with_capacity(n as usize);
    for i in 1..=n {
        elems.push(geti_meta(state, &table_val, i)?);
    }

    // 快速排序 (对应 C 的 auxsort, 初始 rnd=0 → 用中间元素作为 pivot)
    auxsort(state, &mut elems, 0, n - 1, 0, &comp_val)?;

    // 写回 table — 通过 __newindex 元方法 (对应 C lua_seti)
    for (i, val) in elems.iter().enumerate() {
        seti_meta(state, &table_val, i as i64 + 1, val.clone())?;
    }

    push_results(state, a, nresults, vec![]);
    Ok(())
}

/// table.create(sizeseq [, sizerest]) — 对应 C 的 tcreate
///
/// 创建预分配大小的表。sizeseq 是数组部分大小，sizerest 是哈希部分预留容量。
/// sizeseq > INT_MAX → "out of range" (arg #1)
/// sizerest > INT_MAX → "out of range" (arg #2)
/// sizerest > MAXHSIZE (2^30) → "table overflow"
fn call_create(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    // 参数 1: sizeseq (必需)
    let sizeseq = if nargs >= 1 {
        match &state.stack[a + 1] {
            TValue::Integer(n) => *n,
            TValue::Float(f) => {
                if let Some(i) = crate::vm::float_to_integer(*f, crate::vm::F2IMode::Eq) {
                    i
                } else {
                    return Err(VmError::RuntimeError(
                        "bad argument #1 to 'create' (number has no integer representation)"
                            .to_string(),
                    ));
                }
            }
            other => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #1 to 'create' (integer expected, got {})",
                    other.ty()
                )))
            }
        }
    } else {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'create' (integer expected, got no value)".to_string(),
        ));
    };

    // 参数 2: sizerest (可选, 默认 0)
    let sizerest = if nargs >= 2 {
        match &state.stack[a + 2] {
            TValue::Integer(n) => *n,
            TValue::Float(f) => {
                if let Some(i) = crate::vm::float_to_integer(*f, crate::vm::F2IMode::Eq) {
                    i
                } else {
                    return Err(VmError::RuntimeError(
                        "bad argument #2 to 'create' (number has no integer representation)"
                            .to_string(),
                    ));
                }
            }
            TValue::Nil(_) => 0,
            other => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #2 to 'create' (integer expected, got {})",
                    other.ty()
                )))
            }
        }
    } else {
        0
    };

    // argcheck: sizeseq <= INT_MAX (对应 C 的 luaL_argcheck)
    if sizeseq < 0 || sizeseq > i32::MAX as i64 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'create' (value out of range)".to_string(),
        ));
    }
    // argcheck: sizerest <= INT_MAX
    if sizerest < 0 || sizerest > i32::MAX as i64 {
        return Err(VmError::RuntimeError(
            "bad argument #2 to 'create' (value out of range)".to_string(),
        ));
    }
    // 检查哈希大小是否溢出 (对应 C 的 setnodevector 检查)
    // C: lsize = ceil(log2(size)); if lsize > MAXHBITS(30) || (1<<lsize) > MAXHSIZE → "table overflow"
    // MAXHBITS = 30, 即 sizerest > 2^30 = 1073741824 时报错
    const MAXHSIZE: i64 = 1 << 30;
    if sizerest > MAXHSIZE {
        return Err(VmError::RuntimeError("table overflow".to_string()));
    }

    let table = crate::table::Table::with_capacity(sizeseq as usize, sizerest as usize);
    // 注册到 GC 并估算大小 (对应 C 中 lua_createtable 触发的 GC 跟踪)
    // 估算: array 部分 sizeseq * sizeof(TValue) + hash 部分预留容量 * 节点大小
    let estimated_size = sizeseq as usize * std::mem::size_of::<TValue>()
        + sizerest as usize * (std::mem::size_of::<TValue>() * 2 + 16);
    let table_id = state.gc.register_object(estimated_size);
    table.gc_header.set_id(table_id);
    push_results(state, a, nresults, vec![TValue::Table(table)]);
    Ok(())
}

/// table.move(a1, f, e, t [, a2]) — 对应 C 的 tmove
///
/// 将 a1[f..e] 移动到 a2[t..t+e-f]，默认 a2 = a1。返回 a2。
/// 通过 VmExecutor::table_get/table_set 调用元方法 (__index/__newindex)。
fn call_move(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    // 参数 1: 源表 (必需)
    let src_val = get_arg(state, a, 0);
    if !matches!(src_val, TValue::Table(_)) {
        return Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'move' (table expected, got {})",
            src_val.ty()
        )));
    }

    // 参数 2: f (必需, 整数)
    let f = get_int_arg(state, a, 1, "move", 2)?;
    // 参数 3: e (必需, 整数)
    let e = get_int_arg(state, a, 2, "move", 3)?;
    // 参数 4: t (必需, 整数)
    let t = get_int_arg(state, a, 3, "move", 4)?;

    // 参数 5: 目标表 (可选, 默认为 a1; nil 表示使用 a1)
    let dst_val = if nargs >= 5 {
        let v = get_arg(state, a, 4);
        if matches!(v, TValue::Nil(_)) {
            src_val.clone()
        } else if !matches!(v, TValue::Table(_)) {
            return Err(VmError::RuntimeError(format!(
                "bad argument #5 to 'move' (table expected, got {})",
                v.ty()
            )));
        } else {
            v
        }
    } else {
        src_val.clone()
    };

    if e >= f {
        // "too many elements to move": n = e - f + 1 不能溢出
        // C: luaL_argcheck(L, f > 0 || e < LUA_MAXINTEGER + f, 3, "too many elements to move")
        let n = match e.checked_sub(f).and_then(|d| d.checked_add(1)) {
            Some(n) if n >= 0 => n,
            _ => {
                return Err(VmError::RuntimeError(
                    "bad argument #3 to 'move' (too many elements to move)".to_string(),
                ))
            }
        };

        // "destination wrap around": t + n - 1 不能超过 MAXINT
        // C: luaL_argcheck(L, t <= LUA_MAXINTEGER - n + 1, 4, "destination wrap around")
        if t > i64::MAX - n + 1 {
            return Err(VmError::RuntimeError(
                "bad argument #4 to 'move' (destination wrap around)".to_string(),
            ));
        }

        // 决定复制方向: 当源和目标重叠时反向复制避免覆盖未读取元素
        // C: t > e || t <= f || (tt != 1 && !lua_compare(L, 1, tt, LUA_OPEQ))
        let src_eq_dst = match (&src_val, &dst_val) {
            (TValue::Table(s), TValue::Table(d)) => std::rc::Rc::ptr_eq(&s.data, &d.data),
            _ => false,
        };
        let ascending = t > e || t <= f || !src_eq_dst;

        // 执行复制: 通过 VmExecutor::table_get/table_set 触发元方法
        // 注意: 不预分配 Vec,因为 n 可能非常大 (如 maxI),
        // 元方法可能在第一次访问时就抛出错误 (对应 C 的循环行为)
        if ascending {
            let mut i: i64 = 0;
            while i < n {
                let src_key = TValue::Integer(f.wrapping_add(i));
                let dst_key = TValue::Integer(t.wrapping_add(i));
                let val = crate::execute::VmExecutor::table_get(
                    state,
                    &src_val,
                    &src_key,
                    crate::execute::VarSource::None,
                )?;
                crate::execute::VmExecutor::table_set(
                    state,
                    dst_val.clone(),
                    dst_key,
                    val,
                    crate::execute::VarSource::None,
                )?;
                i += 1;
            }
        } else {
            let mut i: i64 = n - 1;
            loop {
                let src_key = TValue::Integer(f.wrapping_add(i));
                let dst_key = TValue::Integer(t.wrapping_add(i));
                let val = crate::execute::VmExecutor::table_get(
                    state,
                    &src_val,
                    &src_key,
                    crate::execute::VarSource::None,
                )?;
                crate::execute::VmExecutor::table_set(
                    state,
                    dst_val.clone(),
                    dst_key,
                    val,
                    crate::execute::VarSource::None,
                )?;
                if i == 0 {
                    break;
                }
                i -= 1;
            }
        }
    }

    push_results(state, a, nresults, vec![dst_val]);
    Ok(())
}

/// 从栈中读取必需整数参数 (对应 C 的 luaL_checkinteger)
fn get_int_arg(
    state: &LuaState,
    a: usize,
    idx: usize,
    fname: &str,
    arg_num: usize,
) -> Result<i64, VmError> {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return Err(VmError::RuntimeError(format!(
            "bad argument #{} to '{}' (integer expected, got no value)",
            arg_num, fname
        )));
    }
    match &state.stack[stack_idx] {
        TValue::Integer(n) => Ok(*n),
        TValue::Float(f) => {
            if let Some(i) = crate::vm::float_to_integer(*f, crate::vm::F2IMode::Eq) {
                Ok(i)
            } else {
                Err(VmError::RuntimeError(format!(
                    "bad argument #{} to '{}' (number has no integer representation)",
                    arg_num, fname
                )))
            }
        }
        other => Err(VmError::RuntimeError(format!(
            "bad argument #{} to '{}' (integer expected, got {})",
            arg_num,
            fname,
            other.ty()
        ))),
    }
}

/// 从 Rust 调用 Lua 比较函数 (对应 C 的 sort_comp 中的函数调用路径)
///
/// 推入 comp(a, b) 并通过 state.pcall 调用,返回布尔结果。
/// 注意: C Lua 使用 lua_call(不可 yield),这里通过递增 n_ny_calls 模拟,
/// 使比较函数内部不能 yield(对应 C Lua 的 nny 计数)。
fn call_comp_function(
    state: &mut LuaState,
    comp: &TValue,
    a: &TValue,
    b: &TValue,
) -> Result<bool, VmError> {
    let saved_len = state.stack.len();
    // 推入: comp, a, b
    state.stack.push(comp.clone());
    state.stack.push(a.clone());
    state.stack.push(b.clone());

    // 递增 n_ny_calls,使比较函数调用不可 yield(对应 C Lua 的 lua_call 行为)
    state.n_ny_calls += 1;
    let status = state.pcall(2, 1, 0);
    state.n_ny_calls = state.n_ny_calls.saturating_sub(1);

    if status != 0 {
        // 获取原始错误值并传播 (对应 C 的 lua_call 直接传播错误)
        let err_val = state
            .stack
            .last()
            .cloned()
            .unwrap_or_else(|| TValue::Nil(NilKind::Strict));
        state.stack.truncate(saved_len);
        return Err(VmError::RuntimeErrorValue(err_val));
    }

    // pcall 后: 栈截断到 saved_len, 推入 1 个结果
    let result = if saved_len < state.stack.len() {
        match &state.stack[saved_len] {
            TValue::Boolean(v) => *v,
            _ => false,
        }
    } else {
        false
    };

    // 恢复栈到调用前
    state.stack.truncate(saved_len);
    Ok(result)
}

// ============================================================================
// 打开 Table 库 — 对应 C 的 luaopen_table
// ============================================================================

/// 打开 Table 库并注册到全局变量 table
pub fn open_table_lib(state: &mut LuaState) {
    let mut lib = crate::table::Table::new();

    // 注册所有 Table 函数 (使用 BuiltinFn 函数指针)
    let register = |lib: &mut crate::table::Table,
                    name: &'static std::ffi::CStr,
                    func: crate::objects::BuiltinFnPtr| {
        let key = TValue::Str(state.intern_str(name.to_str().unwrap_or("")));
        let name_ptr = name.as_ptr() as *const u8;
        lib.set(key, TValue::BuiltinFn(BuiltinFn { func, name: name_ptr }));
    };

    register(&mut lib, c"concat", call_concat);
    register(&mut lib, c"unpack", call_unpack);
    register(&mut lib, c"pack", call_pack);
    register(&mut lib, c"insert", call_insert);
    register(&mut lib, c"remove", call_remove);
    register(&mut lib, c"sort", call_sort);
    register(&mut lib, c"move", call_move);
    register(&mut lib, c"create", call_create);

    let key = TValue::Str(state.intern_str("table"));
    state.globals.set(key, TValue::Table(lib));
}
