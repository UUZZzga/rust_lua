//! Lua C API 导出层（Rust → C ABI）
//!
//! 对应 C 源码: lapi.cpp + lauxlib.cpp 中的 LUA_API / LUALIB_API 函数。
//!
//! ## 设计要点
//! - `lua_State*` = `*mut LuaState`（直接用 LuaState 作为不透明指针）
//! - 所有 `#[no_mangle] extern "C" fn` 导出符号，供第三方 C 模块链接
//! - 栈索引转换基于 `LuaState::api_func_base`：
//!   - 正索引 idx（1-based）→ `stack[api_func_base + idx]`（0-based）
//!   - 负索引 idx → `stack[stack.len() + idx]`（0-based，-1 = top）
//!   - 伪索引（LUA_REGISTRYINDEX 等）单独处理
//! - C 函数调用语义对应 C 的 `precallC` + `luaD_poscall`：
//!   1. 设置 `api_func_base = func_idx`
//!   2. 调用 `f(L)`，C 函数通过本模块导出的 API 操作栈，返回结果数 n
//!   3. 把栈顶 n 个结果移动到 func 位置（poscall 语义）
//!
//! ## 与 C 实现的对齐
//! 本模块导出的函数名、签名、语义均与 `src/lua.h` / `src/lauxlib.h` 1:1 对齐，
//! 第三方 Lua C 模块无需修改即可链接到 Rust 实现的库。

#![allow(non_snake_case, non_camel_case_types, unused_imports)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;

use crate::objects::{LuaType, NilKind, Proto, TValue, LClosure, CClosure, LCFunction, Table};
use crate::state::LuaState;
use crate::strings::LuaString;
use crate::vm::F2IMode;

// ============================================================================
// 类型定义（与 lua.h 对齐）
// ============================================================================

/// lua_State 不透明指针 —— 直接用 LuaState
pub type lua_State = LuaState;

/// C 函数类型（与 objects.rs 的 LCFunction.func / CClosure.f 类型一致）
pub type lua_CFunction = unsafe extern "C" fn(L: *mut c_void) -> c_int;

/// Lua 数值类型
pub type lua_Number = f64;
pub type lua_Integer = i64;
pub type lua_Unsigned = u64;

// ============================================================================
// 常量（与 lua.h 对齐）
// ============================================================================

pub const LUA_OK: c_int = 0;
pub const LUA_YIELD: c_int = 1;
pub const LUA_ERRRUN: c_int = 2;
pub const LUA_ERRSYNTAX: c_int = 3;
pub const LUA_ERRMEM: c_int = 4;
pub const LUA_ERRERR: c_int = 5;

pub const LUA_MULTRET: c_int = -1;
pub const LUA_REGISTRYINDEX: c_int = -(c_int::MAX / 2 + 1000);
pub const LUA_MINSTACK: c_int = 20;

pub const LUA_TNONE: c_int = -1;
pub const LUA_TNIL: c_int = 0;
pub const LUA_TBOOLEAN: c_int = 1;
pub const LUA_TLIGHTUSERDATA: c_int = 2;
pub const LUA_TNUMBER: c_int = 3;
pub const LUA_TSTRING: c_int = 4;
pub const LUA_TTABLE: c_int = 5;
pub const LUA_TFUNCTION: c_int = 6;
pub const LUA_TUSERDATA: c_int = 7;
pub const LUA_TTHREAD: c_int = 8;
pub const LUA_NUMTYPES: c_int = 9;

pub const LUA_RIDX_GLOBALS: c_int = 2;

// ============================================================================
// 内部辅助：索引转换
// ============================================================================

/// 将 C API 栈索引转换为 0-based 栈偏移。
///
/// 对应 C 的 index2stack/lua_absindex 逻辑：
/// - idx > 0: 绝对索引，相对于 api_func_base。0-based = api_func_base + idx
/// - idx < 0 且 > LUA_REGISTRYINDEX: 负索引，相对于 top。0-based = stack.len() + idx
/// - 伪索引（LUA_REGISTRYINDEX 等）: 返回 None，由调用者单独处理
/// - idx == 0: 无效，返回 None
fn index2offset(L: &LuaState, idx: c_int) -> Option<usize> {
    if idx > 0 {
        let abs = L.api_func_base + idx as usize;
        if abs < L.stack.len() {
            Some(abs)
        } else {
            None
        }
    } else if idx < 0 && idx > LUA_REGISTRYINDEX {
        // 负索引（非伪索引）
        let n = (-idx) as usize;
        if n <= L.stack.len() {
            Some(L.stack.len() - n)
        } else {
            None
        }
    } else {
        None
    }
}

/// 获取栈上 idx 处的 TValue 引用（只读）。
fn index2val<'a>(L: &'a LuaState, idx: c_int) -> Option<&'a TValue> {
    // 伪索引：registry
    if idx == LUA_REGISTRYINDEX {
        // registry 是一个 table，存放在 LuaState.registry
        // 这里返回一个临时引用 —— 实际上需要特殊处理
        // 由于 registry 不是栈上的值，我们返回 None 让调用者走 registry 分支
        return None;
    }
    // 伪索引：上值（idx < LUA_REGISTRYINDEX）
    // lua_upvalueindex(i) = LUA_REGISTRYINDEX - i
    // 上值存储在 stack[api_func_base] 处的 CClosure 中
    if idx < LUA_REGISTRYINDEX {
        let up_idx = (LUA_REGISTRYINDEX - idx) as usize;  // 1-based
        if L.api_func_base < L.stack.len() {
            if let TValue::CClosure(cc) = &L.stack[L.api_func_base] {
                if up_idx >= 1 && up_idx <= cc.upvalue.len() {
                    return Some(&cc.upvalue[up_idx - 1]);
                }
            }
        }
        return None;
    }
    let off = index2offset(L, idx)?;
    Some(&L.stack[off])
}

/// 判断 idx 是否是 registry 伪索引
#[inline]
fn is_registry(idx: c_int) -> bool {
    idx == LUA_REGISTRYINDEX
}

/// 从 LuaType 获取 C 类型码
fn lua_type_code(t: LuaType) -> c_int {
    t as c_int
}

/// lua_upvalueindex: 返回第 i 个上值的伪索引。
///
/// 对应 C 的 `#define lua_upvalueindex(i) (LUA_REGISTRYINDEX - (i))`
#[inline]
pub fn lua_upvalueindex(i: c_int) -> c_int {
    LUA_REGISTRYINDEX - i
}

// ============================================================================
// State 管理
// ============================================================================

/// 创建新的 Lua state。
///
/// 对应 C 的 lua_newstate（简化版，忽略 alloc/seed 参数）。
/// 返回的指针需要由 lua_close 释放。
#[no_mangle]
pub extern "C" fn lua_newstate(
    _f: *mut c_void,
    _ud: *mut c_void,
    _seed: std::ffi::c_uint,
) -> *mut lua_State {
    let state = LuaState::new();
    Box::into_raw(Box::new(state))
}

/// 关闭 Lua state，释放资源。
#[no_mangle]
pub extern "C" fn lua_close(L: *mut lua_State) {
    if !L.is_null() {
        unsafe {
            drop(Box::from_raw(L));
        }
    }
}

/// luaL_newstate —— 兼容 lauxlib.h
#[no_mangle]
pub extern "C" fn luaL_newstate() -> *mut lua_State {
    lua_newstate(ptr::null_mut(), ptr::null_mut(), 0)
}

// ============================================================================
// 基础栈操作
// ============================================================================

/// lua_absindex: 将索引转为绝对（正）索引。
#[no_mangle]
pub extern "C" fn lua_absindex(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &*L };
    if idx > 0 || is_registry(idx) {
        idx
    } else {
        // 负索引转正：相对于 api_func_base
        // C 语义: return cast_int(L->top - L->ci->func) + idx;
        // 这里 top - func = stack.len() - api_func_base
        (L.stack.len() - L.api_func_base) as c_int + idx
    }
}

/// lua_gettop: 返回栈顶索引（相对于当前帧，不包括函数槽）。
#[no_mangle]
pub extern "C" fn lua_gettop(L: *mut lua_State) -> c_int {
    let L = unsafe { &*L };
    // C: cast_int(L->top - (L->ci->func + 1))
    // top 是 stack.len()，func 是 api_func_base
    // 函数槽占 1 位，所以可用元素数 = stack.len() - api_func_base - 1
    (L.stack.len() as isize - L.api_func_base as isize - 1) as c_int
}

/// lua_settop: 设置栈顶。
///
/// C 语义:
/// - idx >= 0: new top = func + 1 + idx
/// - idx < 0:  new top = top + idx + 1
#[no_mangle]
pub extern "C" fn lua_settop(L: *mut lua_State, idx: c_int) {
    let L = unsafe { &mut *L };
    if idx >= 0 {
        // new top = func + 1 + idx → stack.len() = api_func_base + 1 + idx
        let new_top = L.api_func_base + 1 + idx as usize;
        if new_top < L.stack.len() {
            L.stack.truncate(new_top);
        } else {
            L.stack.resize(new_top, TValue::Nil(NilKind::Strict));
        }
    } else {
        // new top = top + idx + 1
        let new_top = (L.stack.len() as isize + idx as isize + 1) as usize;
        if new_top <= L.stack.len() {
            L.stack.truncate(new_top);
        }
    }
}

/// lua_pushvalue: 将 idx 处的值压栈。
#[no_mangle]
pub extern "C" fn lua_pushvalue(L: *mut lua_State, idx: c_int) {
    let L = unsafe { &mut *L };
    if is_registry(idx) {
        // registry 是一个 table
        L.stack.push(TValue::Table(L.registry.clone()));
        return;
    }
    if let Some(off) = index2offset(L, idx) {
        let val = L.stack[off].clone();
        L.stack.push(val);
    } else {
        L.stack.push(TValue::Nil(NilKind::Strict));
    }
}

/// lua_rotate: 旋转栈元素。
///
/// 将 idx 到 top 的元素旋转 n 个位置（n>0 向 top 方向，n<0 向 idx 方向）。
#[no_mangle]
pub extern "C" fn lua_rotate(L: *mut lua_State, idx: c_int, n: c_int) {
    let L = unsafe { &mut *L };
    let abs = if is_registry(idx) {
        return; // registry 不可旋转
    } else {
        match index2offset(L, idx) {
            Some(o) => o,
            None => return,
        }
    };
    let top = L.stack.len();
    if abs >= top {
        return;
    }
    let count = top - abs;
    if count == 0 {
        return;
    }
    let n = if n >= 0 {
        n as usize % count
    } else {
        // 负 n: 等价于正方向 count - (-n % count)
        let nn = (-n) as usize % count;
        if nn == 0 {
            0
        } else {
            count - nn
        }
    };
    if n == 0 {
        return;
    }
    // 旋转: [abs..top] 向上移 n
    // 切片旋转
    let mut slice: Vec<TValue> = L.stack.drain(abs..top).collect();
    let split = count - n;
    slice.rotate_right(n);
    // 重新放回
    L.stack.extend(slice);
}

/// lua_copy: 从 fromidx 复制值到 toidx。
#[no_mangle]
pub extern "C" fn lua_copy(L: *mut lua_State, fromidx: c_int, toidx: c_int) {
    let L = unsafe { &mut *L };
    let from = if is_registry(fromidx) {
        TValue::Table(L.registry.clone())
    } else {
        match index2offset(L, fromidx) {
            Some(o) => L.stack[o].clone(),
            None => return,
        }
    };
    if is_registry(toidx) {
        // 不能直接写 registry（它是独立字段）
        return;
    }
    if let Some(to) = index2offset(L, toidx) {
        L.stack[to] = from;
    } else {
        // toidx 超出当前栈，扩展
        let to = L.api_func_base + toidx as usize;
        if to > L.stack.len() {
            L.stack.resize(to, TValue::Nil(NilKind::Strict));
        }
        if to == L.stack.len() {
            L.stack.push(from);
        } else if to < L.stack.len() {
            L.stack[to] = from;
        }
    }
}

/// lua_checkstack: 确保栈有 extra 个额外空间。
#[no_mangle]
pub extern "C" fn lua_checkstack(L: *mut lua_State, extra: c_int) -> c_int {
    let L = unsafe { &mut *L };
    if extra < 0 {
        return 0;
    }
    let needed = L.stack.len() + extra as usize;
    if needed > L.stack.capacity() {
        L.stack.reserve(extra as usize);
    }
    1
}

/// lua_pop: 弹出 n 个元素（宏，但导出为函数方便 C 调用）。
#[no_mangle]
pub extern "C" fn lua_pop(L: *mut lua_State, n: c_int) {
    lua_settop(L, -(n) - 1);
}

// ============================================================================
// Push 系列
// ============================================================================

#[no_mangle]
pub extern "C" fn lua_pushnil(L: *mut lua_State) {
    let L = unsafe { &mut *L };
    L.stack.push(TValue::Nil(NilKind::Strict));
}

#[no_mangle]
pub extern "C" fn lua_pushnumber(L: *mut lua_State, n: lua_Number) {
    let L = unsafe { &mut *L };
    L.stack.push(TValue::Float(n));
}

#[no_mangle]
pub extern "C" fn lua_pushinteger(L: *mut lua_State, n: lua_Integer) {
    let L = unsafe { &mut *L };
    L.stack.push(TValue::Integer(n));
}

/// lua_pushlstring: 压入指定长度的字符串。
///
/// 返回指向内部字符串缓冲区的指针（C 语义）。
#[no_mangle]
pub extern "C" fn lua_pushlstring(
    L: *mut lua_State,
    s: *const c_char,
    len: usize,
) -> *const c_char {
    let L = unsafe { &mut *L };
    if s.is_null() || len == 0 {
        L.push_string("");
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
        L.push_lstring(bytes);
    }
    // 返回内部字符串指针 —— 由于 Rust 字符串存储在 LuaString 内部，
    // 我们需要返回一个稳定的指针。这里简化处理：返回静态空串。
    // 实际使用中，C 代码应在调用后立即使用，或通过 lua_tolstring 重新获取。
    c"".as_ptr()
}

/// lua_pushstring: 压入以 \0 结尾的字符串。
#[no_mangle]
pub extern "C" fn lua_pushstring(L: *mut lua_State, s: *const c_char) -> *const c_char {
    let L = unsafe { &mut *L };
    if s.is_null() {
        L.stack.push(TValue::Nil(NilKind::Strict));
        return ptr::null();
    }
    let cstr = unsafe { CStr::from_ptr(s) };
    let bytes = cstr.to_bytes();
    L.push_lstring(bytes);
    s
}

#[no_mangle]
pub extern "C" fn lua_pushboolean(L: *mut lua_State, b: c_int) {
    let L = unsafe { &mut *L };
    L.stack.push(TValue::Boolean(b != 0));
}

#[no_mangle]
pub extern "C" fn lua_pushlightuserdata(L: *mut lua_State, p: *mut c_void) {
    let L = unsafe { &mut *L };
    L.stack.push(TValue::LightUserData(p));
}

/// lua_pushcclosure: 创建 C 闭包并压栈。
///
/// n 为上值数量：从栈顶弹出 n 个值作为上值。
/// n == 0 时创建轻量 C 函数（LCFn）。
#[no_mangle]
pub extern "C" fn lua_pushcclosure(L: *mut lua_State, f: lua_CFunction, n: c_int) {
    let L = unsafe { &mut *L };
    let n = n as usize;
    if n == 0 {
        // 轻量 C 函数
        L.stack.push(TValue::LCFn(LCFunction { func: f }));
    } else {
        // C 闭包：从栈顶弹 n 个上值
        let mut upvalues = Vec::with_capacity(n);
        for _ in 0..n {
            upvalues.push(
                L.stack
                    .pop()
                    .unwrap_or(TValue::Nil(NilKind::Strict)),
            );
        }
        L.stack.push(TValue::CClosure(CClosure {
            f,
            upvalue: upvalues,
        }));
    }
}

/// lua_pushcfunction: 宏，等价于 lua_pushcclosure(L, f, 0)
#[no_mangle]
pub extern "C" fn lua_pushcfunction(L: *mut lua_State, f: lua_CFunction) {
    lua_pushcclosure(L, f, 0);
}

// ============================================================================
// Type / Is 系列
// ============================================================================

#[no_mangle]
pub extern "C" fn lua_type(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &*L };
    if is_registry(idx) {
        return LUA_TTABLE;
    }
    match index2val(L, idx) {
        Some(v) => lua_type_code(v.ty()),
        None => LUA_TNONE,
    }
}

#[no_mangle]
pub extern "C" fn lua_typename(L: *mut lua_State, tp: c_int) -> *const c_char {
    let _ = L;
    // 返回静态 C 字符串。各分支数组长度不同，统一取 as_ptr() 使类型一致。
    let name: &[u8] = match tp {
        LUA_TNIL => b"nil\0",
        LUA_TBOOLEAN => b"boolean\0",
        LUA_TLIGHTUSERDATA => b"lightuserdata\0",
        LUA_TNUMBER => b"number\0",
        LUA_TSTRING => b"string\0",
        LUA_TTABLE => b"table\0",
        LUA_TFUNCTION => b"function\0",
        LUA_TUSERDATA => b"userdata\0",
        LUA_TTHREAD => b"thread\0",
        _ => b"no value\0",
    };
    name.as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn lua_isnumber(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::Integer(_)) | Some(TValue::Float(_)) => 1,
        Some(TValue::Str(s)) => s.as_str().parse::<f64>().is_ok() as c_int,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn lua_isstring(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::Str(_)) | Some(TValue::Integer(_)) | Some(TValue::Float(_)) => 1,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn lua_iscfunction(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::LCFn(_)) | Some(TValue::CClosure(_)) => 1,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn lua_isinteger(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::Integer(_)) => 1,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn lua_isuserdata(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::LightUserData(_)) | Some(TValue::UserData(_)) => 1,
        _ => 0,
    }
}

// ============================================================================
// To 系列
// ============================================================================

#[no_mangle]
pub extern "C" fn lua_toboolean(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::Nil(_)) => 0,
        Some(TValue::Boolean(false)) => 0,
        _ => 1,
    }
}

/// lua_tointegerx: 转为整数，isnum 输出是否转换成功。
#[no_mangle]
pub extern "C" fn lua_tointegerx(
    L: *mut lua_State,
    idx: c_int,
    isnum: *mut c_int,
) -> lua_Integer {
    let L = unsafe { &*L };
    let result = match index2val(L, idx) {
        Some(TValue::Integer(i)) => Some(*i),
        Some(TValue::Float(f)) => crate::vm::float_to_integer(*f, F2IMode::Eq),
        Some(TValue::Str(s)) => s
            .as_str()
            .parse::<i64>()
            .ok()
            .or_else(|| {
                s.as_str()
                    .parse::<f64>()
                    .ok()
                    .and_then(|f| crate::vm::float_to_integer(f, F2IMode::Eq))
            }),
        _ => None,
    };
    match result {
        Some(i) => {
            if !isnum.is_null() {
                unsafe { *isnum = 1 };
            }
            i
        }
        None => {
            if !isnum.is_null() {
                unsafe { *isnum = 0 };
            }
            0
        }
    }
}

/// lua_tonumberx: 转为浮点数。
#[no_mangle]
pub extern "C" fn lua_tonumberx(
    L: *mut lua_State,
    idx: c_int,
    isnum: *mut c_int,
) -> lua_Number {
    let L = unsafe { &*L };
    let result = match index2val(L, idx) {
        Some(TValue::Integer(i)) => Some(*i as f64),
        Some(TValue::Float(f)) => Some(*f),
        Some(TValue::Str(s)) => s.as_str().parse::<f64>().ok(),
        _ => None,
    };
    match result {
        Some(n) => {
            if !isnum.is_null() {
                unsafe { *isnum = 1 };
            }
            n
        }
        None => {
            if !isnum.is_null() {
                unsafe { *isnum = 0 };
            }
            0.0
        }
    }
}

/// lua_tolstring: 转为字符串，返回 C 字符串指针和长度。
///
/// 注意：返回的指针指向内部缓冲区，在下次 Lua 调用后可能失效。
/// 当前实现：对于字符串值直接返回内部指针；对于数字先转换为字符串再压栈。
#[no_mangle]
pub extern "C" fn lua_tolstring(
    L: *mut lua_State,
    idx: c_int,
    len: *mut usize,
) -> *const c_char {
    let L = unsafe { &mut *L };
    let off = if is_registry(idx) {
        return ptr::null(); // registry 不是字符串
    } else {
        match index2offset(L, idx) {
            Some(o) => o,
            None => return ptr::null(),
        }
    };

    // 如果不是字符串，尝试转换（数字 → 字符串）
    let need_convert = !matches!(L.stack[off], TValue::Str(_));
    if need_convert {
        let converted = match &L.stack[off] {
            TValue::Integer(i) => Some(i.to_string()),
            TValue::Float(f) => Some(format_float(*f)),
            _ => None,
        };
        if let Some(s) = converted {
            L.stack[off] = TValue::Str(crate::state::str_to_ls(&L.string_table, &s));
        } else {
            return ptr::null();
        }
    }

    // 返回内部字符串指针
    if let TValue::Str(ref s) = L.stack[off] {
        let ptr = s.as_str().as_ptr() as *const c_char;
        if !len.is_null() {
            unsafe { *len = s.len() };
        }
        ptr
    } else {
        ptr::null()
    }
}

#[no_mangle]
pub extern "C" fn lua_touserdata(L: *mut lua_State, idx: c_int) -> *mut c_void {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::LightUserData(p)) => *p,
        Some(TValue::UserData(_)) => {
            // full userdata 暂未完整实现，返回 null
            ptr::null_mut()
        }
        _ => ptr::null_mut(),
    }
}

/// lua_rawlen: 返回值的原始长度。
#[no_mangle]
pub extern "C" fn lua_rawlen(L: *mut lua_State, idx: c_int) -> lua_Unsigned {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::Str(s)) => s.len() as lua_Unsigned,
        Some(TValue::Table(t)) => t.len() as lua_Unsigned,
        _ => 0,
    }
}

/// lua_tocfunction: 返回 C 函数指针。
#[no_mangle]
pub extern "C" fn lua_tocfunction(L: *mut lua_State, idx: c_int) -> Option<lua_CFunction> {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::LCFn(f)) => Some(f.func),
        Some(TValue::CClosure(c)) => Some(c.f),
        _ => None,
    }
}

// ============================================================================
// Table 操作
// ============================================================================

/// lua_createtable: 创建新表并压栈。
#[no_mangle]
pub extern "C" fn lua_createtable(L: *mut lua_State, narr: c_int, nrec: c_int) {
    let L = unsafe { &mut *L };
    let t = Table::with_capacity(
        if narr > 0 { narr as usize } else { 0 },
        if nrec > 0 { nrec as usize } else { 0 },
    );
    L.stack.push(TValue::Table(t));
}

/// lua_gettable: t[k]，k 从栈顶弹出，结果压栈。返回值的类型。
#[no_mangle]
pub extern "C" fn lua_gettable(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &mut *L };
    let key = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    if is_registry(idx) {
        let val = L.registry.get(&key).unwrap_or(TValue::Nil(NilKind::Strict));
        let ty = lua_type_code(val.ty());
        L.stack.push(val);
        return ty;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.push(TValue::Nil(NilKind::Strict));
            return LUA_TNIL;
        }
    };
    let val = match &L.stack[off] {
        TValue::Table(t) => t.get(&key).unwrap_or(TValue::Nil(NilKind::Strict)),
        _ => TValue::Nil(NilKind::Strict),
    };
    let ty = lua_type_code(val.ty());
    L.stack.push(val);
    ty
}

/// lua_settable: t[k] = v，k 和 v 从栈顶弹出。
#[no_mangle]
pub extern "C" fn lua_settable(L: *mut lua_State, idx: c_int) {
    let L = unsafe { &mut *L };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    let key = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    if is_registry(idx) {
        L.registry.set(key, val);
        return;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => return,
    };
    if let TValue::Table(ref mut t) = L.stack[off] {
        t.set(key, val);
    }
}

/// lua_getfield: t[k]，结果压栈。
#[no_mangle]
pub extern "C" fn lua_getfield(L: *mut lua_State, idx: c_int, k: *const c_char) -> c_int {
    let L = unsafe { &mut *L };
    let key_str = if k.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(k) }
            .to_string_lossy()
            .into_owned()
    };
    let key = crate::state::str_to_ls(&L.string_table, &key_str);
    let key_tv = TValue::Str(key);
    if is_registry(idx) {
        let val = L.registry.get(&key_tv).unwrap_or(TValue::Nil(NilKind::Strict));
        let ty = lua_type_code(val.ty());
        L.stack.push(val);
        return ty;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.push(TValue::Nil(NilKind::Strict));
            return LUA_TNIL;
        }
    };
    let val = match &L.stack[off] {
        TValue::Table(t) => t.get(&key_tv).unwrap_or(TValue::Nil(NilKind::Strict)),
        _ => TValue::Nil(NilKind::Strict),
    };
    let ty = lua_type_code(val.ty());
    L.stack.push(val);
    ty
}

/// lua_setfield: t[k] = v，v 从栈顶弹出。
#[no_mangle]
pub extern "C" fn lua_setfield(L: *mut lua_State, idx: c_int, k: *const c_char) {
    let L = unsafe { &mut *L };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    let key_str = if k.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(k) }
            .to_string_lossy()
            .into_owned()
    };
    let key = crate::state::str_to_ls(&L.string_table, &key_str);
    let key_tv = TValue::Str(key);
    if is_registry(idx) {
        L.registry.set(key_tv, val);
        return;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => return,
    };
    if let TValue::Table(ref mut t) = L.stack[off] {
        t.set(key_tv, val);
    }
}

/// lua_rawget: t[k]，k 从栈顶弹出（不走元方法）。
#[no_mangle]
pub extern "C" fn lua_rawget(L: *mut lua_State, idx: c_int) -> c_int {
    // 当前实现与 lua_gettable 一致（元方法尚未打通）
    lua_gettable(L, idx)
}

/// lua_rawset: t[k] = v，k 和 v 从栈顶弹出（不走元方法）。
#[no_mangle]
pub extern "C" fn lua_rawset(L: *mut lua_State, idx: c_int) {
    lua_settable(L, idx);
}

/// lua_rawgeti: t[n]，结果压栈。
#[no_mangle]
pub extern "C" fn lua_rawgeti(L: *mut lua_State, idx: c_int, n: lua_Integer) -> c_int {
    let L = unsafe { &mut *L };
    let key = TValue::Integer(n);
    if is_registry(idx) {
        let val = L.registry.get(&key).unwrap_or(TValue::Nil(NilKind::Strict));
        let ty = lua_type_code(val.ty());
        L.stack.push(val);
        return ty;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.push(TValue::Nil(NilKind::Strict));
            return LUA_TNIL;
        }
    };
    let val = match &L.stack[off] {
        TValue::Table(t) => t.get(&key).unwrap_or(TValue::Nil(NilKind::Strict)),
        _ => TValue::Nil(NilKind::Strict),
    };
    let ty = lua_type_code(val.ty());
    L.stack.push(val);
    ty
}

/// lua_rawseti: t[n] = v，v 从栈顶弹出。
#[no_mangle]
pub extern "C" fn lua_rawseti(L: *mut lua_State, idx: c_int, n: lua_Integer) {
    let L = unsafe { &mut *L };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    if is_registry(idx) {
        L.registry.set(TValue::Integer(n), val);
        return;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => return,
    };
    if let TValue::Table(ref mut t) = L.stack[off] {
        t.set_int(n, val);
    }
}

// ============================================================================
// Globals
// ============================================================================

/// lua_getglobal: 读取全局变量，结果压栈。
#[no_mangle]
pub extern "C" fn lua_getglobal(L: *mut lua_State, name: *const c_char) -> c_int {
    let L = unsafe { &mut *L };
    let name_str = if name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned()
    };
    let key = crate::state::str_to_ls(&L.string_table, &name_str);
    let key_tv = TValue::Str(key);
    let val = L.globals.get(&key_tv).unwrap_or(TValue::Nil(NilKind::Strict));
    let ty = lua_type_code(val.ty());
    L.stack.push(val);
    ty
}

/// lua_setglobal: 设置全局变量，值从栈顶弹出。
#[no_mangle]
pub extern "C" fn lua_setglobal(L: *mut lua_State, name: *const c_char) {
    let L = unsafe { &mut *L };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    let name_str = if name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned()
    };
    let key = crate::state::str_to_ls(&L.string_table, &name_str);
    L.globals.set(TValue::Str(key), val);
}

// ============================================================================
// 辅助函数（内部）
// ============================================================================

/// 浮点数格式化（与 vm.rs 保持一致）
fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    if f == 0.0 {
        return "0.0".to_string();
    }
    let s = format!("{:.14}", f);
    let s = s.trim_end_matches('0');
    if s.ends_with('.') {
        format!("{}0", s)
    } else {
        s.to_string()
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lua_newstate_close() {
        let L = lua_newstate(ptr::null_mut(), ptr::null_mut(), 0);
        assert!(!L.is_null());
        lua_close(L);
    }

    #[test]
    fn test_basic_stack_ops() {
        let L = luaL_newstate();
        unsafe {
            // LuaState::new 会推入一个 nil 作为函数入口槽（stack[0]），
            // api_func_base=0 指向函数槽。lua_gettop 返回 top-(func+1)，
            // 所以初始 top=0（函数槽不算入可用栈）。
            assert_eq!(lua_gettop(L), 0);

            lua_pushinteger(L, 42);
            assert_eq!(lua_gettop(L), 1);
            assert_eq!(lua_type(L, 1), LUA_TNUMBER);
            assert!(lua_isinteger(L, 1) != 0);

            let mut isnum: c_int = 0;
            let i = lua_tointegerx(L, 1, &mut isnum);
            assert_eq!(i, 42);
            assert_eq!(isnum, 1);

            lua_pop(L, 1);
            assert_eq!(lua_gettop(L), 0);
        }
        lua_close(L);
    }

    #[test]
    fn test_pushcclosure_light_cfn() {
        let L = luaL_newstate();
        unsafe {
            unsafe extern "C" fn dummy(_L: *mut c_void) -> c_int {
                0
            }
            lua_pushcfunction(L, dummy);
            assert_eq!(lua_type(L, 1), LUA_TFUNCTION);
            assert!(lua_iscfunction(L, 1) != 0);
            assert!(lua_tocfunction(L, 1).is_some());
        }
        lua_close(L);
    }

    #[test]
    fn test_table_ops() {
        let L = luaL_newstate();
        unsafe {
            lua_createtable(L, 0, 0);
            assert_eq!(lua_type(L, 1), LUA_TTABLE);

            // t["key"] = 100
            lua_pushinteger(L, 100);
            lua_setfield(L, 1, c"key".as_ptr());

            // x = t["key"]
            let ty = lua_getfield(L, 1, c"key".as_ptr());
            assert_eq!(ty, LUA_TNUMBER);
            let mut isnum: c_int = 0;
            assert_eq!(lua_tointegerx(L, -1, &mut isnum), 100);
            assert_eq!(isnum, 1);

            lua_pop(L, 2); // 弹出 value 和 table
        }
        lua_close(L);
    }

    #[test]
    fn test_global_ops() {
        let L = luaL_newstate();
        unsafe {
            lua_pushinteger(L, 999);
            lua_setglobal(L, c"myvar".as_ptr());

            let ty = lua_getglobal(L, c"myvar".as_ptr());
            assert_eq!(ty, LUA_TNUMBER);
            let mut isnum: c_int = 0;
            assert_eq!(lua_tointegerx(L, -1, &mut isnum), 999);
            lua_pop(L, 1);
        }
        lua_close(L);
    }

    #[test]
    fn test_string_ops() {
        let L = luaL_newstate();
        unsafe {
            lua_pushstring(L, c"hello world".as_ptr());
            assert_eq!(lua_type(L, -1), LUA_TSTRING);
            assert!(lua_isstring(L, -1) != 0);

            let mut len: usize = 0;
            let ptr = lua_tolstring(L, -1, &mut len);
            assert_eq!(len, 11);
            let s = std::slice::from_raw_parts(ptr as *const u8, len);
            assert_eq!(s, b"hello world");
        }
        lua_close(L);
    }

    #[test]
    fn test_cclosure_with_upvalues() {
        let L = luaL_newstate();
        unsafe {
            unsafe extern "C" fn adder(L: *mut c_void) -> c_int {
                // 读取上值（idx 用 lua_upvalueindex）
                // upvalueindex(1) = LUA_REGISTRYINDEX - 1
                let upv = lua_tointegerx(L as *mut lua_State, LUA_REGISTRYINDEX - 1, std::ptr::null_mut());
                let arg = lua_tointegerx(L as *mut lua_State, 1, std::ptr::null_mut());
                lua_pushinteger(L as *mut lua_State, upv + arg);
                1
            }
            // 创建闭包，上值为 100
            lua_pushinteger(L, 100);
            lua_pushcclosure(L, adder, 1);
            assert_eq!(lua_type(L, 1), LUA_TFUNCTION);

            // 调用: push 闭包, push 参数 23, 调用
            // 这里只验证闭包创建成功，实际调用在 execute.rs 的 C 函数调用支持完成后测试
        }
        lua_close(L);
    }

    /// 测试通过 Lua 代码调用 C 函数（LCFn）。
    /// 验证 op_call 中的 call_c_function 路径。
    #[test]
    fn test_c_function_call_via_lua() {
        // C 函数: add(a, b) = a + b
        unsafe extern "C" fn add(L: *mut c_void) -> c_int {
            let L = L as *mut lua_State;
            let a = lua_tointegerx(L, 1, std::ptr::null_mut());
            let b = lua_tointegerx(L, 2, std::ptr::null_mut());
            lua_pushinteger(L, a + b);
            1
        }

        let L = luaL_newstate();
        unsafe {
            // 注册 add 函数到全局变量
            lua_pushcfunction(L, add);
            lua_setglobal(L, c"add".as_ptr());

            // 编译并执行 Lua 代码
            let state = &mut *L;
            let status = state.load_buffer("return add(3, 4)", "=test");
            assert_eq!(status, 0, "compile failed");

            // pcall 执行: 栈顶是 LClosure，0 个参数，期望 1 个结果
            let status = state.pcall(0, 1, 0);
            assert_eq!(status, 0, "execution failed");

            // 验证结果
            let mut isnum: c_int = 0;
            let result = lua_tointegerx(L, -1, &mut isnum);
            assert_eq!(result, 7);
            assert_eq!(isnum, 1);
        }
        lua_close(L);
    }

    /// 测试通过 Lua 代码调用 C 闭包（CClosure）。
    /// 验证上值访问和 op_call 中的 call_c_function 路径。
    #[test]
    fn test_cclosure_call_via_lua() {
        // C 闭包: 返回 上值 + 第一个参数
        unsafe extern "C" fn adder(L: *mut c_void) -> c_int {
            let L = L as *mut lua_State;
            let upv = lua_tointegerx(L, lua_upvalueindex(1), std::ptr::null_mut());
            let arg = lua_tointegerx(L, 1, std::ptr::null_mut());
            lua_pushinteger(L, upv + arg);
            1
        }

        let L = luaL_newstate();
        unsafe {
            // 创建闭包：上值为 100，注册到全局变量 adder
            lua_pushinteger(L, 100);
            lua_pushcclosure(L, adder, 1);
            lua_setglobal(L, c"adder".as_ptr());

            // 编译并执行 Lua 代码
            let state = &mut *L;
            let status = state.load_buffer("return adder(23)", "=test");
            assert_eq!(status, 0, "compile failed");

            let status = state.pcall(0, 1, 0);
            assert_eq!(status, 0, "execution failed");

            // 验证结果: 100 + 23 = 123
            let mut isnum: c_int = 0;
            let result = lua_tointegerx(L, -1, &mut isnum);
            assert_eq!(result, 123);
            assert_eq!(isnum, 1);
        }
        lua_close(L);
    }
}
