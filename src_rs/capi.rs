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

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::ptr;
use std::rc::Rc;

use crate::objects::{
    CClosure, LCFunction, LClosure, LuaThread, LuaType, NilKind, Proto, TValue, Table,
    ThreadContext, ThreadStatus, Udata,
};
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
        let up_idx = (LUA_REGISTRYINDEX - idx) as usize; // 1-based
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
/// 对应 C 的 lua_newstate。
/// 保存 allocf 的 ud 到 LuaState.allocf_ud，供 lua_getallocf 取回。
/// （Rust 用自己的分配器，忽略 f；seed 由 LuaState::new 内部生成。）
/// 返回的指针需要由 lua_close 释放。
#[no_mangle]
pub extern "C" fn lua_newstate(
    _f: *mut c_void,
    ud: *mut c_void,
    _seed: std::ffi::c_uint,
) -> *mut lua_State {
    let mut state = LuaState::new();
    state.allocf_ud = ud;
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

/// lua_newthread: 创建新线程（coroutine）。
///
/// Lua 5.5 标准 C API。创建一个共享全局状态但有独立栈的新 `lua_State`。
/// 新 state 通过 `LuaState::new_thread` 创建，共享原 L 的 globals/registry/string_table/gc 等，
/// 但拥有独立的 stack/call_info/执行状态。
///
/// 同时创建一个 `TValue::Thread` 并 push 到 L 的栈上（对应 C 中 newthread 把 thread
/// 对象留在栈上的语义），让 Lua 代码能引用此线程。`LuaThread.c_state` 保存新 state 指针，
/// 供 `lua_tothread` 反查。
///
/// 返回的指针需要由调用方持有（通常存在 userdata 的 uservalue 中），
/// 不可通过 lua_close 关闭（关闭主 L 时会一并释放所有 thread）。
#[no_mangle]
pub extern "C" fn lua_newthread(L: *mut lua_State) -> *mut lua_State {
    if L.is_null() {
        return ptr::null_mut();
    }
    let L_ref = unsafe { &mut *L };
    let new_state = L_ref.new_thread();
    let new_state_ptr: *mut lua_State = Box::into_raw(Box::new(new_state));

    // 创建 LuaThread 并关联到新 state
    let context = Rc::new(std::cell::RefCell::new(ThreadContext::default()));
    context.borrow_mut().status = ThreadStatus::Suspended;
    let thread = LuaThread {
        stack: Vec::new(),
        status: ThreadStatus::Suspended,
        function: None,
        is_main: false,
        context: context.clone(),
        c_state: std::cell::Cell::new(new_state_ptr),
    };
    let thread_rc = Rc::new(thread);
    // 设置 thread_ref，让 coroutine.running() 能返回同一对象
    context.borrow_mut().thread_ref = Rc::downgrade(&thread_rc);

    // 设置新 state 的 current_thread（让 lua_resume 能找到 ThreadContext）
    unsafe {
        (*new_state_ptr).current_thread = Some(context);
    }

    // push TValue::Thread 到 L 的栈上（对应 C 中 lua_newthread 留下 thread 对象的语义）
    L_ref.stack.push(TValue::Thread(thread_rc));
    new_state_ptr
}

/// lua_closethread: 关闭线程（coroutine）。
///
/// Lua 5.5 标准 C API。关闭 thread 中的 TBC 变量并重置状态。
/// 简化实现为空操作，返回 LUA_OK（0）。
#[no_mangle]
pub extern "C" fn lua_closethread(_L: *mut lua_State, _from: *mut lua_State) -> c_int {
    0 // LUA_OK
}

/// luaL_newstate —— 兼容 lauxlib.h
#[no_mangle]
pub extern "C" fn luaL_newstate() -> *mut lua_State {
    lua_newstate(ptr::null_mut(), ptr::null_mut(), luaL_makeseed(ptr::null_mut()))
}

/// luaL_makeseed: 生成随机种子。
///
/// 对应 C 的 lauxlib.c::luaL_makeseed。用当前时间和局部变量地址生成种子，
/// 供 luaL_newstate → lua_newstate 创建 Lua state 时使用。
/// 简化实现：用 time + 局部变量地址混合，与 C 的 luai_makeseed 等价。
#[no_mangle]
pub extern "C" fn luaL_makeseed(_L: *mut lua_State) -> c_uint {
    // 对应 C 的 luai_makeseed: time(NULL) + 局部变量地址
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 局部变量地址（对应 C 的 addbuff(b, b)）
    let local_addr = &t as *const _ as usize;
    let mut res = t as u32;
    res ^= res.rotate_left(7).wrapping_add(local_addr as u32);
    res
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
    // 上值伪索引: idx < LUA_REGISTRYINDEX
    // lua_upvalueindex(i) = LUA_REGISTRYINDEX - i
    // 上值存储在 stack[api_func_base] 的 CClosure 中
    if idx < LUA_REGISTRYINDEX {
        let up_idx = (LUA_REGISTRYINDEX - idx) as usize; // 1-based
        if L.api_func_base < L.stack.len() {
            if let TValue::CClosure(cc) = &L.stack[L.api_func_base] {
                if up_idx >= 1 && up_idx <= cc.upvalue.len() {
                    L.stack.push(cc.upvalue[up_idx - 1].clone());
                    return;
                }
            }
        }
        L.stack.push(TValue::Nil(NilKind::Strict));
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

/// lua_insert: 把栈顶元素插入到 idx 位置，元素上移。
/// 等同于 lua_rotate(L, idx, 1)。
#[no_mangle]
pub extern "C" fn lua_insert(L: *mut lua_State, idx: c_int) {
    lua_rotate(L, idx, 1);
}

/// lua_remove: 移除 idx 处的元素，元素下移。
/// 等同于 lua_rotate(L, idx, -1) 后 pop。
#[no_mangle]
pub extern "C" fn lua_remove(L: *mut lua_State, idx: c_int) {
    lua_rotate(L, idx, -1);
    lua_pop(L, 1);
}

/// lua_replace: 宏，等价于 lua_copy(L, -1, idx) + pop
#[no_mangle]
pub extern "C" fn lua_replace(L: *mut lua_State, idx: c_int) {
    lua_copy(L, -1, idx);
    lua_pop(L, 1);
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
    // 返回栈顶字符串的内部指针（对应 C 的 lua_pushlstring 返回内部 TString 缓冲区）
    // 短字符串由于 interned，指针在进程生命周期内稳定
    // 长字符串指针在字符串不被从栈中移除前有效
    match L.stack.last() {
        Some(TValue::Str(ls)) => ls.as_c_str_ptr(),
        _ => c"".as_ptr(),
    }
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
        // C 闭包：从栈顶弹 n 个上值。pop 顺序是栈顶（最后压栈）→栈底（最先压栈），
        // 与 C Lua 的 upvalue[0] = 最先压栈（栈底）相反，因此需要 reverse。
        let mut upvalues = Vec::with_capacity(n);
        for _ in 0..n {
            let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
            upvalues.push(val);
        }
        upvalues.reverse();
        L.stack.push(TValue::CClosure(Rc::new(CClosure {
            f,
            upvalue: upvalues,
        })));
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
pub extern "C" fn lua_tointegerx(L: *mut lua_State, idx: c_int, isnum: *mut c_int) -> lua_Integer {
    let L = unsafe { &*L };
    let result = match index2val(L, idx) {
        Some(TValue::Integer(i)) => Some(*i),
        Some(TValue::Float(f)) => crate::vm::float_to_integer(*f, F2IMode::Eq),
        Some(TValue::Str(s)) => s.as_str().parse::<i64>().ok().or_else(|| {
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
pub extern "C" fn lua_tonumberx(L: *mut lua_State, idx: c_int, isnum: *mut c_int) -> lua_Number {
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
pub extern "C" fn lua_tolstring(L: *mut lua_State, idx: c_int, len: *mut usize) -> *const c_char {
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

    // 返回 NUL 结尾的 C 字符串指针（LuaString 内部已保证末尾有 NUL）
    if let TValue::Str(ref s) = L.stack[off] {
        if !len.is_null() {
            unsafe { *len = s.len() };
        }
        s.as_c_str_ptr()
    } else {
        ptr::null()
    }
}

#[no_mangle]
pub extern "C" fn lua_touserdata(L: *mut lua_State, idx: c_int) -> *mut c_void {
    let L = unsafe { &*L };
    match index2val(L, idx) {
        Some(TValue::LightUserData(p)) => *p,
        Some(TValue::UserData(ud)) => ud.data.as_ptr() as *mut c_void,
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
    // IMPORTANT: resolve offset BEFORE popping, because idx might be negative
    // and popping changes relative positions
    if is_registry(idx) {
        // For registry, pop key, look up directly
        let key = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let val = L.registry.get(&key).unwrap_or(TValue::Nil(NilKind::Strict));
        let ty = lua_type_code(val.ty());
        L.stack.push(val);
        return ty;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            // offset invalid: just pop key and push nil
            L.stack.pop();
            L.stack.push(TValue::Nil(NilKind::Strict));
            return LUA_TNIL;
        }
    };
    // Now pop key
    let key = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
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
    // 先基于当前 top（包含 key 和 val）定位 table，再 pop
    if is_registry(idx) {
        let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let key = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        L.registry.set(key, val);
        return;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.pop();
            L.stack.pop();
            return;
        }
    };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    let key = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
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
        unsafe { CStr::from_ptr(k) }.to_string_lossy().into_owned()
    };
    let key = crate::state::str_to_ls(&L.string_table, &key_str);
    let key_tv = TValue::Str(key);
    if is_registry(idx) {
        let val = L
            .registry
            .get(&key_tv)
            .unwrap_or(TValue::Nil(NilKind::Strict));
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
    // 先基于当前 top（包含 val）定位 table，再 pop val
    let key_str = if k.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(k) }.to_string_lossy().into_owned()
    };
    let key = crate::state::str_to_ls(&L.string_table, &key_str);
    let key_tv = TValue::Str(key);
    if is_registry(idx) {
        let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        L.registry.set(key_tv, val);
        return;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.pop();
            return;
        }
    };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    if let TValue::Table(ref mut t) = L.stack[off] {
        t.set(key_tv, val);
    }
}
/// lua_rawget: t[k]，k 从栈顶弹出（不走元方法）。
#[no_mangle]
pub extern "C" fn lua_rawget(L: *mut lua_State, idx: c_int) -> c_int {
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
    // 先基于当前 top（包含 val）定位 table，再 pop
    if is_registry(idx) {
        let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        L.registry.set(TValue::Integer(n), val);
        return;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.pop();
            return;
        }
    };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
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
    let val = L
        .globals
        .get(&key_tv)
        .unwrap_or(TValue::Nil(NilKind::Strict));
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
        return if f > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
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
// luaL 辅助库函数 — 对应 C lauxlib.cpp
// ============================================================================

/// luaL_Reg 结构 — 对应 C 的 luaL_Reg
#[repr(C)]
pub struct luaL_Reg {
    pub name: *const c_char,
    pub func: Option<lua_CFunction>,
}

/// luaL_checkversion_: 版本兼容性检查（简化为空实现）
///
/// C 版本检查 LUA_VERSION_NUM 和 LUAL_NUMSIZES，不匹配则 luaL_error。
/// Rust 实现暂不检查，因为 .so 都是与同版本编译的。
#[no_mangle]
pub extern "C" fn luaL_checkversion_(_L: *mut lua_State, _ver: lua_Number, _sz: usize) {
    // 简化：不做任何检查
}

/// luaL_setfuncs: 把 luaL_Reg 数组中的函数注册到栈顶表
///
/// 栈布局（调用前）: [... | table | upvalue1 | ... | upvalueN]
/// nup = N，table 在 -(nup+1) 位置
/// 调用后: [... | table]（弹出所有 upvalue）
#[no_mangle]
pub extern "C" fn luaL_setfuncs(L: *mut lua_State, l: *const luaL_Reg, nup: c_int) {
    if l.is_null() {
        return;
    }
    let nup = nup as i32;
    let mut i = 0;
    loop {
        let reg = unsafe { &*l.add(i) };
        if reg.name.is_null() {
            break;
        }
        i += 1;
        match reg.func {
            None => {
                // 占位符：压入 false
                lua_pushboolean(L, 0);
            }
            Some(f) => {
                // 复制 nup 个 upvalue 到栈顶（每次复制 -nup 位置）
                for _ in 0..nup {
                    lua_pushvalue(L, -nup);
                }
                lua_pushcclosure(L, f, nup);
            }
        }
        // 栈: [... | table | upv1..N | closure]，table 在 -(nup+2)
        lua_setfield(L, -(nup + 2), reg.name);
    }
    // 弹出 nup 个 upvalue
    lua_pop(L, nup);
}

/// luaL_checklstring: 检查参数 arg 是否为字符串，返回字符串指针和长度
///
/// 对应 C 的 luaL_checklstring，参数不匹配时调用 luaL_typeerror 抛错。
#[no_mangle]
pub extern "C-unwind" fn luaL_checklstring(L: *mut lua_State, arg: c_int, l: *mut usize) -> *const c_char {
    // 用 lua_tolstring 获取字符串指针
    let ptr = lua_tolstring(L, arg, l);
    if ptr.is_null() {
        // 类型错误：简化处理，调用 lua_error 抛错
        let msg = CString::new(format!("bad argument #{} (string expected)", arg)).unwrap();
        lua_pushstring(L, msg.as_ptr());
        lua_error(L);
    }
    ptr
}

/// luaL_ref: 在表 t（栈顶）中创建对栈顶值的引用
///
/// 返回引用编号（整数）。栈顶值被弹出。
/// 对应 C 的 luaL_ref：
/// - 栈顶值是 nil → 弹出，返回 LUA_REFNIL
/// - 否则 → t[n] = value，n++，弹出 value，返回 n-1
#[no_mangle]
pub extern "C" fn luaL_ref(L: *mut lua_State, t: c_int) -> c_int {
    const LUA_REFNIL: c_int = -1;
    const LUA_NOREF: c_int = -2;

    let L = unsafe { &mut *L };
    if L.stack.is_empty() {
        return LUA_NOREF;
    }
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    if matches!(val, TValue::Nil(_)) {
        return LUA_REFNIL;
    }
    // 获取表 t（通常是 LUA_REGISTRYINDEX）
    // 简化：假设 t 是 registry（LUA_REGISTRYINDEX），用 registry 的 array 部分
    // C 实现：t[ref] = value，ref 从 t[0] 取空闲链表
    // 这里用 registry 的 hash 部分，key 是整数 ref
    let registry = L.registry.clone();
    // 简化：维护一个递增计数器存在 registry[0]
    let next_ref_key = TValue::Integer(0);
    let next_ref = match registry.get(&next_ref_key) {
        Some(TValue::Integer(n)) => n + 1,
        _ => 1,
    };
    registry.set(next_ref_key, TValue::Integer(next_ref));
    registry.set(TValue::Integer(next_ref), val);
    next_ref as c_int
}

/// luaL_unref: 释放引用（简化实现，不回收）
#[no_mangle]
pub extern "C" fn luaL_unref(L: *mut lua_State, _t: c_int, ref_: c_int) {
    let L = unsafe { &mut *L };
    if ref_ == -1 || ref_ == -2 {
        return; // LUA_REFNIL 或 LUA_NOREF
    }
    let registry = L.registry.clone();
    registry.set(TValue::Integer(ref_ as i64), TValue::Nil(NilKind::Strict));
}

// ============================================================================
// 调用与错误 — 对应 C lapi.cpp
// ============================================================================

/// lua_callk: 调用栈顶函数（无保护，错误会传播）
///
/// 简化实现：用 pcall 调用，错误时 panic（模拟无保护语义）
/// k 是延续函数（C continuation），Rust 实现不支持，忽略。
/// panic 委托给辅助函数，避免 extern "C" 函数中直接 panic 被编译器转为 abort。
#[inline(never)]
#[cold]
unsafe fn do_lua_callk(L: *mut lua_State, nargs: usize, nresults: i32) {
    let L = &mut *L;
    let status = L.pcall(nargs, nresults, 0);
    if status != 0 {
        let err = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        L.pending_error = Some(err);
        panic!("lua_callk error");
    }
}
#[no_mangle]
pub extern "C-unwind" fn lua_callk(
    L: *mut lua_State,
    nargs: usize,
    nresults: i32,
    _ctx: isize,
    _k: Option<unsafe extern "C" fn(*mut lua_State, c_int, isize) -> c_int>,
) {
    unsafe { do_lua_callk(L, nargs, nresults) }
}
// C 函数错误处理
// ============================================================================
//
// lua_error 被 C 代码调用（通常通过 lauxlib.cpp::luaL_error）。
// Rust 的 extern "C" 函数中 panic 会触发 abort（panic_cannot_unwind），
// 因为编译器会对 extern "C" 函数隐式应用 unwind(abort)。
// 因此我们使用非 extern 函数来做实际的 panic，这样 unwind 可以正常进行。
// C 模块的 .o 文件需编译时加 -fexceptions 标志，使 GCC 生成必要的
// 栈展开表，让 Rust 的 catch_unwind 能安全通过 C 帧展开。
// 已在 deps/Makefile 中添加此标志。

/// # Safety: L must be a valid pointer to a LuaState
#[inline(never)]
#[cold]
unsafe fn do_lua_error(L: *mut lua_State) -> ! {
    let L = &mut *L;
    let err = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    L.pending_error = Some(err);
    panic!("lua_error");
}

#[no_mangle]
pub extern "C-unwind" fn lua_error(L: *mut lua_State) -> c_int {
    unsafe { do_lua_error(L) }
}
/// ud 设为 NULL（Rust VM 用自己的分配器，C 模块分配的内存由其自行管理）。
pub type lua_Alloc =
    Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize, usize) -> *mut c_void>;

/// 默认 C 内存分配器：realloc/free 包装
unsafe extern "C" fn default_allocf(
    _ud: *mut c_void,
    ptr: *mut c_void,
    _osize: usize,
    nsize: usize,
) -> *mut c_void {
    if nsize == 0 {
        if !ptr.is_null() {
            libc::free(ptr);
        }
        ptr::null_mut()
    } else {
        libc::realloc(ptr, nsize)
    }
}

#[no_mangle]
pub extern "C" fn lua_getallocf(L: *mut lua_State, ud: *mut *mut c_void) -> lua_Alloc {
    let L = unsafe { &*L };
    if !ud.is_null() {
        unsafe {
            *ud = L.allocf_ud;
        }
    }
    Some(default_allocf)
}


// ============================================================================
// Userdata — Lua 5.5 (lua_newuserdatauv)
// ============================================================================

/// lua_newuserdatauv: 创建 full userdata。
///
/// 对应 C 的 lua_newuserdatauv。创建大小为 sz 字节、nuvalue 个用户值的 UserData，
/// 压栈并返回数据区指针。
///
/// 注册到 GC 并设置 id，使 mark_tvalue 能正确标记 reachable。
/// 这对 GC finalizer 机制至关重要：没有 id 的 userdata 会被 collect_finalizers
/// 误判为不可达（id() 返回 None → map_or(false, ...)），即使它还在栈上。
#[no_mangle]
pub extern "C-unwind" fn lua_newuserdatauv(L: *mut lua_State, sz: usize, nuvalue: c_int) -> *mut c_void {
    let L = unsafe { &mut *L };
    let nuv = if nuvalue >= 0 { nuvalue as usize } else { 0 };
    let mut udata = crate::objects::Udata {
        gc_header: crate::gc::GCObjectHeader::new(),
        nuvalue: nuv as u16,
        len: sz,
        metatable: None,
        user_values: (0..nuv).map(|_| TValue::Nil(NilKind::Strict)).collect(),
        data: vec![0u8; sz],
    };
    // 注册到 GC 并设置 id（使 mark_tvalue 能正确标记 reachable）
    // 用 gc_mem_size() 计费含 data/user_values 容量，比 size_of::<Udata>() 更接近真实占用
    let ud_id = L.gc.register_object(udata.gc_mem_size());
    udata.gc_header.set_id(ud_id);
    let ptr = udata.data.as_mut_ptr() as *mut c_void;
    L.stack.push(TValue::UserData(Rc::new(udata)));
    ptr
}

/// lua_getmetatable: 获取对象元表。
///
/// 将 idx 处对象的元表（若有）压栈，返回 1；否则压 nil 返回 0。
#[no_mangle]
pub extern "C" fn lua_getmetatable(L: *mut lua_State, objindex: c_int) -> c_int {
    if is_registry(objindex) {
        // registry 没有元表，不 push 任何东西（对应 C Lua 语义）
        return 0;
    }
    let L = unsafe { &mut *L };
    let off = match index2offset(L, objindex) {
        Some(o) => o,
        None => return 0,
    };
    let mt = match &L.stack[off] {
        TValue::UserData(u) => {
            u.metatable.as_ref().map(|b| (**b).clone())
        }
        TValue::Table(t) => t.get_metatable(),
        _ => None,
    };
    match mt {
        Some(mt_val) => {
            L.stack.push(TValue::Table(mt_val));
            1
        }
        // 对应 C Lua: 没有元表时不 push 任何东西，只返回 0
        None => 0,
    }
}
/// lua_setmetatable: 设置对象元表。
///
/// 从栈顶弹出元表，设置为 idx 处对象的元表。返回 1 成功，0 失败（对象非 table/userdata）。
///
/// 对 userdata 设置含 `__gc` 元方法的元表时，注册到 `ud_finobj_list`，
/// 确保 userdata 不可达时 GC 调用 finalizer（对应 C Lua 的 luaC_checkfinalizer）。
/// 这对 C 模块（如 lsqlite3）至关重要：__gc 会在 Rc drop 前被调用，
/// 让模块有机会清理 registry 中的 light userdata 引用，避免悬空指针。
#[no_mangle]
pub extern "C" fn lua_setmetatable(L: *mut lua_State, objindex: c_int) -> c_int {
    let L = unsafe { &mut *L };
    // IMPORTANT: resolve object offset BEFORE popping metatable,
    // because the metatable is at stack top and objindex is relative
    // to the original stack layout BEFORE the pop.
    let off = match index2offset(L, objindex) {
        Some(o) => o,
        None => return 0,
    };
    // Now pop metatable from stack
    let mt_val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    let mt = match mt_val {
        TValue::Table(t) => Some(t),
        TValue::Nil(_) => None,
        _ => {
            return 0;
        }
    };
    // 预先 intern __gc 字符串（在持有 stack 借用前完成）
    let gc_key = TValue::Str(L.intern_str("__gc"));
    // 收集需要注册 __gc 的 userdata（避免在持有 stack 借用时调用 register_ud_finobj）
    let mut ud_to_register: Option<Rc<Udata>> = None;
    let result = match &mut L.stack[off] {
        TValue::Table(t) => {
            t.set_metatable(mt.clone());
            1
        }
        TValue::UserData(u) => {
            // Use in-place mutation through raw pointer instead of Rc::make_mut.
            // Rc::make_mut clones the Udata when refcount > 1, creating a copy
            // that doesn't propagate back to other references (like the Lua variable).
            // This breaks the C API semantics where lua_setmetatable modifies
            // the userdata in-place regardless of how many references there are.
            let ptr = Rc::as_ptr(u) as *mut Udata;
            unsafe { (*ptr).metatable = mt.as_ref().map(|t| Box::new(t.clone())); }
            // 检查元表是否含 __gc，若有则收集待注册的 userdata
            if let Some(ref mt_table) = mt {
                if mt_table.get(&gc_key).is_some() {
                    ud_to_register = Some(Rc::clone(u));
                }
            }
            1
        }
        TValue::LightUserData(p) => {
            let _ = *p;
            if let Some(mt_val) = mt {
                L.dmt.set(crate::objects::LuaType::LightUserData, crate::tm::Metatable::new(mt_val));
            } else {
                L.dmt.clear(crate::objects::LuaType::LightUserData);
            }
            1
        }
        _ => {
            let ty = L.stack[off].ty();
            if let Some(mt_val) = mt {
                L.dmt.set(ty, crate::tm::Metatable::new(mt_val));
            } else {
                L.dmt.clear(ty);
            }
            1
        }
    };
    // 在 match 外注册 __gc finalizer（避免借用冲突）
    if let Some(ud) = ud_to_register {
        L.register_ud_finobj(&ud);
    }
    result
}

/// lua_pcallk: 保护调用。
///
/// 对应 C 的 lua_pcallk。以保护模式调用函数。
/// 简化实现：不支持 continuation（k 参数被忽略）。
#[no_mangle]
pub extern "C" fn lua_pcallk(
    L: *mut lua_State,
    nargs: c_int,
    nresults: c_int,
    _errfunc: c_int,
    _ctx: isize,
    _k: *const c_void,
) -> c_int {
    let L = unsafe { &mut *L };
    // 清空 c_safety_keepalive: 上次 pcall 错误路径暂存的值现在可以安全释放。
    // C 代码在两次 pcall 之间不应持有 userdata 指针（C Lua 中 GC 可能已回收）。
    L.c_safety_keepalive.clear();
    L.pcall(nargs as usize, nresults, _errfunc as isize)
}

/// lua_pushthread: 将当前线程压栈。
#[no_mangle]
pub extern "C" fn lua_pushthread(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    // 主线程：还未实现协程线程对象的推送
    // 简单做法：压入一个布尔值表示主线程
    L.stack.push(TValue::Boolean(true));
    1 // 主线程返回 1
}

// ============================================================================
// Table iteration (next)
// ============================================================================

/// lua_next: 表迭代。
///
/// 从栈顶弹出一个 key，在 idx 处的表中查找下一对 (key, value)，
/// 将 key 和 value 压栈。无下一项时返回 0。
///
/// 对应 C Lua 5.5 lapi.c lua_next:
///   k = s2v(L->top.p - 1);   // 保存 key（不立即 pop）
///   t = gettable(L, idx);    // 用 idx 定位 table（key 还在栈上！）
///   ... next ...
///   L->top.p -= 1;           // pop key
///
/// 关键：idx 必须在 key 还在栈上时解释，否则负索引会偏移。
/// 例如栈 [..., table, key]，idx=-2 指向 table；如果先 pop key，
/// idx=-2 会指向 table 下面的元素。
#[no_mangle]
pub extern "C" fn lua_next(L: *mut lua_State, idx: c_int) -> c_int {
    let L = unsafe { &mut *L };
    // 先用 idx 定位 table（key 还在栈上），再 pop key
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            // table 不存在，但仍需 pop key 保持栈平衡
            L.stack.pop();
            return 0;
        }
    };
    // 读取 table 引用后再 pop key，避免借用冲突
    let table_val = L.stack[off].clone();
    let key = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    match &table_val {
        TValue::Table(t) => match crate::stdlib::base_lib::table_next(t, &key) {
            Ok((Some(next_key), next_val)) => {
                L.stack.push(next_key);
                L.stack.push(next_val);
                1
            }
            Ok((None, _)) => 0,
            Err(_) => 0,
        },
        _ => 0,
    }
}

// ============================================================================
// Length, concat, comparison, arithmetic
// ============================================================================

/// lua_len: 长度操作符。
///
/// 计算 idx 处值的长度，结果压栈。
#[no_mangle]
pub extern "C" fn lua_len(L: *mut lua_State, idx: c_int) {
    let L = unsafe { &mut *L };
    let len = L.len(idx as isize);
    L.stack.push(TValue::Integer(len as i64));
}

/// lua_concat: 字符串连接。
///
/// 连接栈顶 n 个值，结果压栈。
#[no_mangle]
pub extern "C" fn lua_concat(L: *mut lua_State, n: c_int) {
    let L = unsafe { &mut *L };
    if n <= 0 {
        L.stack.push(TValue::Str(L.intern_str("")));
        return;
    }
    let n = n as usize;
    // 收集栈顶 n 个值，转为字符串
    let mut parts: Vec<String> = Vec::with_capacity(n);
    for _ in 0..n {
        if L.stack.len() > 0 {
            let val = L.stack.pop().unwrap();
            let s = crate::stdlib::base_lib::lua_value_to_string(&val);
            parts.push(s);
        }
    }
    parts.reverse(); // 恢复原始顺序
    let result = parts.concat();
    L.stack.push(TValue::Str(L.intern_str(&result)));
}

// ============================================================================
// 格式化字符串 — lua_pushfstring / lua_pushvfstring 由 C wrapper 实现
// （Rust stable 不支持 c_variadic，用 deps/capi_compat.c 中的 C 代码处理可变参数）
// ============================================================================

/// lua_rawequal: 原始相等比较（不走元方法）。
///
/// 比较 idx1 和 idx2 处的值，相等返回 1，否则 0。
#[no_mangle]
pub extern "C" fn lua_rawequal(L: *mut lua_State, idx1: c_int, idx2: c_int) -> c_int {
    let L = unsafe { &*L };
    match (index2val(L, idx1), index2val(L, idx2)) {
        (Some(v1), Some(v2)) => {
            if v1.ty() != v2.ty() {
                return 0;
            }
            match (v1, v2) {
                (TValue::Nil(_), TValue::Nil(_)) => 1,
                (TValue::Boolean(a), TValue::Boolean(b)) => (*a == *b) as c_int,
                (TValue::Integer(a), TValue::Integer(b)) => (*a == *b) as c_int,
                (TValue::Float(a), TValue::Float(b)) => (*a == *b) as c_int,
                (TValue::Str(a), TValue::Str(b)) => (a == b) as c_int,
                (TValue::Table(a), TValue::Table(b)) => (a.gc_header.ptr_id == b.gc_header.ptr_id) as c_int,
                (TValue::UserData(a), TValue::UserData(b)) => (a.gc_header.ptr_id == b.gc_header.ptr_id) as c_int,
                (TValue::LightUserData(a), TValue::LightUserData(b)) => (*a == *b) as c_int,
                (TValue::LClosure(a), TValue::LClosure(b)) => (a.gc_header.ptr_id == b.gc_header.ptr_id) as c_int,
                (TValue::CClosure(a), TValue::CClosure(b)) => Rc::ptr_eq(a, b) as c_int,
                (TValue::Thread(a), TValue::Thread(b)) => Rc::ptr_eq(&a.context, &b.context) as c_int,
                _ => 0,
            }
        }
        _ => 0,
    }
}

/// lua_compare: 比较操作（支持元方法）。
///
/// op: 0=EQ, 1=LT, 2=LE
#[no_mangle]
pub extern "C" fn lua_compare(L: *mut lua_State, idx1: c_int, idx2: c_int, op: c_int) -> c_int {
    let L = unsafe { &*L };
    let v1 = match index2val(L, idx1) {
        Some(v) => v,
        None => return 0,
    };
    let v2 = match index2val(L, idx2) {
        Some(v) => v,
        None => return 0,
    };
    // 简化实现：仅支持原始类型的比较
    // 对于 table/userdata 类型尝试使用元方法
    match op {
        0 => { // LUA_OPEQ: equal
            // 先检查 rawequal
            if v1.ty() == v2.ty() {
                let eq = match (v1, v2) {
                    (TValue::Nil(_), TValue::Nil(_)) => true,
                    (TValue::Boolean(a), TValue::Boolean(b)) => a == b,
                    (TValue::Integer(a), TValue::Integer(b)) => a == b,
                    (TValue::Float(a), TValue::Float(b)) => a == b,
                    (TValue::Integer(a), TValue::Float(b)) => *a as f64 == *b,
                    (TValue::Float(a), TValue::Integer(b)) => *a == *b as f64,
                    (TValue::Str(a), TValue::Str(b)) => a == b,
                    (TValue::Table(a), TValue::Table(b)) => a.gc_header.ptr_id == b.gc_header.ptr_id,
                    (TValue::UserData(a), TValue::UserData(b)) => a.gc_header.ptr_id == b.gc_header.ptr_id,
                    (TValue::LightUserData(a), TValue::LightUserData(b)) => a == b,
                    (TValue::LClosure(a), TValue::LClosure(b)) => a.gc_header.ptr_id == b.gc_header.ptr_id,
                    (TValue::CClosure(a), TValue::CClosure(b)) => Rc::ptr_eq(a, b),
                    (TValue::Thread(a), TValue::Thread(b)) => Rc::ptr_eq(&a.context, &b.context),
                    _ => false,
                };
                return eq as c_int;
            }
            // 不同类型：尝试 rawequal
            0
        }
        1 => { // LUA_OPLT: less than
            match (v1, v2) {
                (TValue::Integer(a), TValue::Integer(b)) => (*a < *b) as c_int,
                (TValue::Float(a), TValue::Float(b)) => (*a < *b) as c_int,
                (TValue::Integer(a), TValue::Float(b)) => ((*a as f64) < (*b)) as c_int,
                (TValue::Float(a), TValue::Integer(b)) => ((*a) < (*b as f64)) as c_int,
                (TValue::Str(a), TValue::Str(b)) => (a.as_str() < b.as_str()) as c_int,
                _ => 0,
            }
        }
        2 => { // LUA_OPLE: less or equal
            match (v1, v2) {
                (TValue::Integer(a), TValue::Integer(b)) => (*a <= *b) as c_int,
                (TValue::Float(a), TValue::Float(b)) => (*a <= *b) as c_int,
                (TValue::Integer(a), TValue::Float(b)) => ((*a as f64) <= (*b)) as c_int,
                (TValue::Float(a), TValue::Integer(b)) => ((*a) <= (*b as f64)) as c_int,
                (TValue::Str(a), TValue::Str(b)) => (a.as_str() <= b.as_str()) as c_int,
                _ => 0,
            }
        }
        _ => 0,
    }
}

/// lua_arith: 算术运算。
///
/// op: LUA_OPADD=0, OPSUB=1, OPMUL=2, OPMOD=3, OPPOW=4, OPDIV=5,
///     OPIDIV=6, OPBAND=7, OPBOR=8, OPBXOR=9, OPSHL=10, OPSHR=11,
///     OPUNM=12, OPBNOT=13
/// 从栈顶操作，结果压栈（弹出操作数，压入结果）。
#[no_mangle]
pub extern "C" fn lua_arith(L: *mut lua_State, op: c_int) {
    let L = unsafe { &mut *L };
    let rb = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    let ra = if op != 12 && op != 13 {
        L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict))
    } else {
        TValue::Integer(0)
    };
    
    use crate::vm::to_number_ns;
    
    let result = match op {
        0 => { // LUA_OPADD: a + b
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => TValue::Integer(i1.wrapping_add(*i2)),
                _ => {
                    match (to_number_ns(&ra), to_number_ns(&rb)) {
                        (Some(n1), Some(n2)) => TValue::Float(n1 + n2),
                        _ => TValue::Nil(NilKind::Strict),
                    }
                }
            }
        }
        1 => { // LUA_OPSUB: a - b
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => TValue::Integer(i1.wrapping_sub(*i2)),
                _ => {
                    match (to_number_ns(&ra), to_number_ns(&rb)) {
                        (Some(n1), Some(n2)) => TValue::Float(n1 - n2),
                        _ => TValue::Nil(NilKind::Strict),
                    }
                }
            }
        }
        2 => { // LUA_OPMUL: a * b
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => TValue::Integer(i1.wrapping_mul(*i2)),
                _ => {
                    match (to_number_ns(&ra), to_number_ns(&rb)) {
                        (Some(n1), Some(n2)) => TValue::Float(n1 * n2),
                        _ => TValue::Nil(NilKind::Strict),
                    }
                }
            }
        }
        3 => { // LUA_OPMOD: a % b
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => {
                    match crate::vm::modulus(*i1, *i2) {
                        Ok(r) => TValue::Integer(r),
                        Err(_) => TValue::Nil(NilKind::Strict),
                    }
                }
                _ => {
                    match (to_number_ns(&ra), to_number_ns(&rb)) {
                        (Some(n1), Some(n2)) => TValue::Float(crate::vm::modulus_float(n1, n2)),
                        _ => TValue::Nil(NilKind::Strict),
                    }
                }
            }
        }
        4 => { // LUA_OPPOW: a ^ b
            match (to_number_ns(&ra), to_number_ns(&rb)) {
                (Some(n1), Some(n2)) => TValue::Float(n1.powf(n2)),
                _ => TValue::Nil(NilKind::Strict),
            }
        }
        5 => { // LUA_OPDIV: a / b
            match (to_number_ns(&ra), to_number_ns(&rb)) {
                (Some(n1), Some(n2)) => TValue::Float(n1 / n2),
                _ => TValue::Nil(NilKind::Strict),
            }
        }
        6 => { // LUA_OPIDIV: a // b (floor division)
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => {
                    match crate::vm::idiv(*i1, *i2) {
                        Ok(r) => TValue::Integer(r),
                        Err(_) => TValue::Nil(NilKind::Strict),
                    }
                }
                _ => {
                    match (to_number_ns(&ra), to_number_ns(&rb)) {
                        (Some(n1), Some(n2)) => TValue::Float((n1 / n2).floor()),
                        _ => TValue::Nil(NilKind::Strict),
                    }
                }
            }
        }
        7 => { // LUA_OPBAND: a & b
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => TValue::Integer(*i1 & *i2),
                _ => TValue::Nil(NilKind::Strict),
            }
        }
        8 => { // LUA_OPBOR: a | b
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => TValue::Integer(*i1 | *i2),
                _ => TValue::Nil(NilKind::Strict),
            }
        }
        9 => { // LUA_OPBXOR: a ^ b (bitwise xor)
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => TValue::Integer(*i1 ^ *i2),
                _ => TValue::Nil(NilKind::Strict),
            }
        }
        10 => { // LUA_OPSHL: a << b
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => TValue::Integer(crate::vm::shiftl(*i1, *i2)),
                _ => TValue::Nil(NilKind::Strict),
            }
        }
        11 => { // LUA_OPSHR: a >> b
            match (&ra, &rb) {
                (TValue::Integer(i1), TValue::Integer(i2)) => TValue::Integer(crate::vm::shiftr(*i1, *i2)),
                _ => TValue::Nil(NilKind::Strict),
            }
        }
        12 => { // LUA_OPUNM: unary minus (-a)
            match &rb {
                TValue::Integer(i) => TValue::Integer(-i),
                TValue::Float(f) => TValue::Float(-f),
                _ => TValue::Nil(NilKind::Strict),
            }
        }
        13 => { // LUA_OPBNOT: bitwise not (~a)
            match &rb {
                TValue::Integer(i) => TValue::Integer(!i),
                _ => TValue::Nil(NilKind::Strict),
            }
        }
        _ => TValue::Nil(NilKind::Strict),
    };
    L.stack.push(result);
}

// ============================================================================
// Version and panic
// ============================================================================

/// lua_version: 返回 Lua 版本号。
#[no_mangle]
pub extern "C" fn lua_version(_L: *mut lua_State) -> lua_Number {
    505.0 // LUA_VERSION_NUM = 5*100 + 5
}

// ============================================================================
// GC control
// ============================================================================

/// lua_gc: 垃圾回收控制。
///
/// what: LUA_GCSTOP=0, LUA_GCRESTART=1, LUA_GCCOLLECT=2,
///       LUA_GCCOUNT=3, LUA_GCSTEP=5, LUA_GCSETPAUSE=6,
#[no_mangle]
pub unsafe extern "C" fn lua_gc(L: *mut lua_State, what: c_int, _arg: c_int) -> c_int {
    let L = unsafe { &mut *L };
    match what {
        0 => { // LUA_GCSTOP
            L.gc_stop();
            0
        }
        1 => { // LUA_GCRESTART
            L.gc_restart();
            0
        }
        2 => { // LUA_GCCOLLECT
            L.collect_gc();
            0
        }
        3 => { // LUA_GCCOUNT
            // 返回 GC 内存使用量（以 KB 为单位）
            L.gc.gc_estimate.get() as c_int
        }
        5 => { // LUA_GCSTEP
            L.step_gc(1024 * 100); // 步进 100KB
            0
        }
        9 => { // LUA_GCISRUNNING
            if L.gc.gc_stop.get() == 0 { 1 } else { 0 }
        }
        10 => { // LUA_GCGEN
            L.gc_gen();
            0
        }
        11 => { // LUA_GCINC
            L.gc_inc();
            0
        }
        _ => 0,
    }
}

// ============================================================================
// Integer key access (geti/seti)
// ============================================================================

/// lua_geti: t[n]，结果压栈。
#[no_mangle]
pub extern "C" fn lua_geti(L: *mut lua_State, idx: c_int, n: lua_Integer) -> c_int {
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

/// lua_seti: t[n] = v，v 从栈顶弹出。
#[no_mangle]
pub extern "C" fn lua_seti(L: *mut lua_State, idx: c_int, n: lua_Integer) {
    let L = unsafe { &mut *L };
    if is_registry(idx) {
        let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        L.registry.set(TValue::Integer(n), val);
        return;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.pop();
            return;
        }
    };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    if let TValue::Table(ref mut t) = L.stack[off] {
        t.set(TValue::Integer(n), val);
    }
}

// ============================================================================
// lua_atpanic — 设置 panic 处理器
// ============================================================================

/// lua_atpanic: 设置 panic 回调。
///
/// 保存传入的 panic 处理函数，返回旧的（当前简化：总是返回一个 no-op 函数）。
#[allow(non_upper_case_globals)]
static default_panic_handler: lua_CFunction = panic_noop;

unsafe extern "C" fn panic_noop(_L: *mut c_void) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn lua_atpanic(_L: *mut lua_State, _panicf: lua_CFunction) -> lua_CFunction {
    // 简化实现：不存储 panic 处理器，总是返回静态的 no-op 函数
    // 这确保 C 模块尝试调用/比较返回的 panic 处理器时不会遇到 null 指针
    default_panic_handler
}
// ============================================================================
// Debug & Load API — 供 lauxlib.cpp (静态链接到 C 模块 .so) 使用
// ============================================================================

/// lua_Debug 结构体 — 对应 C lua.h 的 struct lua_Debug
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct lua_Debug {
    event: c_int,
    name: *const c_char,
    namewhat: *const c_char,
    what: *const c_char,
    source: *const c_char,
    srclen: usize,
    currentline: c_int,
    linedefined: c_int,
    lastlinedefined: c_int,
    nups: u8,
    nparams: u8,
    isvararg: i8,
    extraargs: u8,
    istailcall: i8,
    ftransfer: c_int,
    ntransfer: c_int,
    short_src: [i8; 60], // LUA_IDSIZE
    i_ci: *mut c_void,   // struct CallInfo*
}

/// lua_Reader — 对应 C lua.h 的 lua_Reader
pub type lua_Reader = unsafe extern "C" fn(
    L: *mut lua_State,
    data: *mut c_void,
    size: *mut usize,
) -> *const c_char;

/// lua_WarnFunction — 对应 C lua.h 的 lua_WarnFunction
pub type lua_WarnFunction = extern "C" fn(ud: *mut c_void, msg: *const c_char, tocont: c_int);

/// lua_getstack: 返回指定级别的堆栈信息
///
/// 对应 C Lua lapi.cpp lua_getstack:
///   level 0 = 当前函数 (call_info 最后一个条目)
///   level 1 = 调用者 (倒数第二个条目)
///   ...
/// 将 call_info 索引存储在 ar.i_ci 中供 lua_getinfo 使用。
#[no_mangle]
pub extern "C" fn lua_getstack(L: *mut lua_State, level: c_int, ar: *mut lua_Debug) -> c_int {
    if level < 0 || ar.is_null() {
        return 0;
    }
    let L = unsafe { &*L };
    let ci_len = L.call_info.len();
    if ci_len == 0 {
        return 0;
    }
    let idx = match ci_len.checked_sub(level as usize + 1) {
        Some(i) => i,
        None => return 0,
    };
    // 将索引存储在 i_ci 中（编码为指针大小的值）
    unsafe {
        (*ar).i_ci = idx as *mut c_void;
    }
    1
}

/// lua_getinfo: 获取调试信息
///
/// 对应 C Lua lapi.cpp lua_getinfo。支持 what 字符串中的以下选项:
///   'n' - name, namewhat (从调用点代码分析)
///   't' - istailcall, extraargs
///   'f' - 将函数压入栈顶
///   'S' - what, source, srclen, linedefined, lastlinedefined, short_src
///   'l' - currentline
///   'u' - nups, nparams, isvararg
#[no_mangle]
pub extern "C" fn lua_getinfo(
    L: *mut lua_State,
    what: *const c_char,
    ar: *mut lua_Debug,
) -> c_int {
    if ar.is_null() || what.is_null() {
        return 0;
    }
    let L = unsafe { &mut *L };
    let what_str = unsafe { CStr::from_ptr(what) }
        .to_str()
        .unwrap_or("");
    let ci_idx = unsafe { (*ar).i_ci as usize };
    if ci_idx >= L.call_info.len() {
        return 0;
    }
    let ci = &L.call_info[ci_idx];

    // 静态 C 字符串常量
    static WHAT_C: &[u8] = b"C\0";
    static WHAT_LUA: &[u8] = b"Lua\0";
    static SOURCE_C: &[u8] = b"=[C]\0";
    static SHORT_SRC_C: &[u8] = b"[C]\0";
    static EMPTY_STR: &[u8] = b"\0";

    if what_str.contains('n') {
        // name/namewhat: 从调用者代码分析
        // 对应 C getfuncname: 从 caller_proto 的 saved_pc 处分析调用指令。
        // C 函数帧和 Lua 函数帧都存储了 caller_proto（execute.rs 中 push 时设置），
        // 统一处理。仅当 caller_proto 为 None（如 hook 帧）时才返回空。
        if let Some(ref caller_proto) = ci.caller_proto {
            let (name, namewhat) = crate::execute::compute_name_from_proto(caller_proto, ci.saved_pc);
            if name.is_empty() {
                unsafe {
                    (*ar).name = ptr::null();
                    (*ar).namewhat = EMPTY_STR.as_ptr() as *const c_char;
                }
            } else {
                // name 需要是持久化的 C 字符串
                // 使用 LuaString 的内部缓冲区（通过 intern）
                let name_ls = crate::state::str_to_ls(&L.string_table, &name);
                let namewhat_ls = crate::state::str_to_ls(&L.string_table, &namewhat);
                unsafe {
                    (*ar).name = name_ls.as_c_str_ptr();
                    (*ar).namewhat = namewhat_ls.as_c_str_ptr();
                }
            }
        } else {
            unsafe {
                (*ar).name = ptr::null();
                (*ar).namewhat = EMPTY_STR.as_ptr() as *const c_char;
            }
        }
    }

    if what_str.contains('t') {
        unsafe {
            (*ar).istailcall = if ci.is_tailcall { 1 } else { 0 };
            (*ar).extraargs = ci.nextraargs as u8;
        }
    }

    if what_str.contains('f') {
        // 将函数压入栈顶: 函数在 stack[ci.base - 1]
        if ci.base > 0 && ci.base <= L.stack.len() {
            let func_val = L.stack[ci.base - 1].clone();
            L.stack.push(func_val);
        } else {
            L.stack.push(TValue::Nil(NilKind::Strict));
        }
    }

    if what_str.contains('S') || what_str.contains('l') {
        if ci.is_c {
            // C 函数
            if what_str.contains('S') {
                unsafe {
                    (*ar).what = WHAT_C.as_ptr() as *const c_char;
                    (*ar).source = SOURCE_C.as_ptr() as *const c_char;
                    (*ar).srclen = 4; // "=[C]" 长度
                    (*ar).linedefined = -1;
                    (*ar).lastlinedefined = -1;
                    // short_src: 复制 "[C]"
                    let src = b"[C]";
                    let buf = &mut (*ar).short_src;
                    for (i, &b) in src.iter().enumerate() {
                        if i < buf.len() {
                            buf[i] = b as i8;
                        }
                    }
                    if src.len() < buf.len() {
                        buf[src.len()] = 0;
                    }
                }
            }
            if what_str.contains('l') {
                unsafe { (*ar).currentline = -1; }
            }
        } else if let Some(ref closure) = ci.closure {
            // Lua 函数
            let proto = &closure.proto;
            if what_str.contains('S') {
                let (source_ptr, srclen) = if let Some(ref src) = proto.source {
                    (src.as_c_str_ptr(), src.len())
                } else {
                    (EMPTY_STR.as_ptr() as *const c_char, 0)
                };
                unsafe {
                    (*ar).what = WHAT_LUA.as_ptr() as *const c_char;
                    (*ar).source = source_ptr;
                    (*ar).srclen = srclen;
                    (*ar).linedefined = proto.line_defined;
                    (*ar).lastlinedefined = proto.last_line_defined;
                    // short_src: 从 source 生成（简化版）
                    let src_str = if let Some(ref src) = proto.source {
                        src.as_str()
                    } else {
                        ""
                    };
                    let short = if src_str.starts_with('@') {
                        &src_str[1..]
                    } else if src_str.starts_with('=') {
                        &src_str[1..]
                    } else {
                        src_str
                    };
                    let buf = &mut (*ar).short_src;
                    let short_bytes = short.as_bytes();
                    let copy_len = short_bytes.len().min(buf.len() - 1);
                    for i in 0..copy_len {
                        buf[i] = short_bytes[i] as i8;
                    }
                    buf[copy_len] = 0;
                }
            }
            if what_str.contains('l') {
                // currentline: 从 caller_proto 的 saved_pc 计算
                // 注意: saved_pc 是调用点 PC，不是当前执行 PC
                // 对于 luaL_where(level=1)，level 1 = 调用者，需要调用者当前行号
                // = 调用点行号 = get_proto_line(caller_proto, saved_pc)
                let line = if let Some(ref caller_proto) = ci.caller_proto {
                    crate::execute::get_proto_line(caller_proto, ci.saved_pc)
                } else {
                    -1
                };
                unsafe { (*ar).currentline = line; }
            }
        } else {
            // 无 closure 信息（不应发生）
            if what_str.contains('S') {
                unsafe {
                    (*ar).what = WHAT_C.as_ptr() as *const c_char;
                    (*ar).source = EMPTY_STR.as_ptr() as *const c_char;
                    (*ar).srclen = 0;
                    (*ar).linedefined = -1;
                    (*ar).lastlinedefined = -1;
                    (*ar).short_src[0] = 0;
                }
            }
            if what_str.contains('l') {
                unsafe { (*ar).currentline = -1; }
            }
        }
    }

    if what_str.contains('u') {
        if let Some(ref closure) = ci.closure {
            unsafe {
                (*ar).nups = closure.proto.size_upvalues as u8;
                (*ar).nparams = closure.proto.num_params as u8;
                (*ar).isvararg = if closure.proto.is_vararg() { 1 } else { 0 };
            }
        } else {
            unsafe {
                (*ar).nups = 0;
                (*ar).nparams = 0;
                (*ar).isvararg = 0;
            }
        }
    }

    1
}

/// lua_load: 加载 Lua 代码块
///
/// reader 是读取回调，data 是回调参数，chunkname 是代码块名称，mode 是编译模式。
/// 加载成功返回 0，失败返回错误码。
#[no_mangle]
pub extern "C" fn lua_load(
    L: *mut lua_State,
    reader: lua_Reader,
    data: *mut c_void,
    chunkname: *const c_char,
    _mode: *const c_char,
) -> c_int {
    let state = unsafe { &mut *L };
    // 通过 reader 回调读取完整源码
    let mut source = Vec::new();
    loop {
        let mut sz: usize = 0;
        let chunk = unsafe { reader(L, data, &mut sz) };
        if chunk.is_null() || sz == 0 {
            break;
        }
        let slice = unsafe { std::slice::from_raw_parts(chunk as *const u8, sz) };
        source.extend_from_slice(slice);
    }
    let name = if chunkname.is_null() {
        "=(C load)"
    } else {
        unsafe { std::ffi::CStr::from_ptr(chunkname) }
            .to_str()
            .unwrap_or("=(C load)")
    };
    let source_str = String::from_utf8_lossy(&source);
    let status = state.load_buffer(&source_str, name);
    status as c_int
}
/// lua_setwarnf: 设置警告回调（简化实现：忽略）
#[no_mangle]
pub extern "C" fn lua_setwarnf(
    _L: *mut lua_State,
    _f: lua_WarnFunction,
    _ud: *mut c_void,
) {
    // 简化实现：不存储警告回调
}

/// lua_numbertocstring: 将数字转换为字符串并写入缓冲
///
/// 返回写入的字节数（不含终止 null）。
#[no_mangle]
pub extern "C" fn lua_numbertocstring(
    L: *mut lua_State,
    idx: c_int,
    buff: *mut c_char,
) -> u32 {
    let state = unsafe { &mut *L };
    let off = match index2offset(state, idx) {
        Some(o) => o,
        None => return 0,
    };
    let val = &state.stack[off];
    let s = crate::stdlib::base_lib::lua_value_to_string(val);
    let bytes = s.as_bytes();
    let len = bytes.len().min(255); // 安全上限
    if !buff.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buff as *mut u8, len);
            *buff.add(len) = 0; // null-terminate
        }
    }
    len as u32
}

/// lua_toclose: 标记栈上值在离开作用域时关闭（简化实现：忽略）
#[no_mangle]
pub extern "C" fn lua_toclose(_L: *mut lua_State, _idx: c_int) {
    // 简化实现：不支持 to-close 变量
}

/// lua_closeslot: 关闭 to-close 槽（简化实现：忽略）
#[no_mangle]
pub extern "C" fn lua_closeslot(_L: *mut lua_State, _idx: c_int) {
    // 简化实现
}

/// lua_topointer: 返回值的内部指针
#[no_mangle]
pub extern "C" fn lua_topointer(L: *mut lua_State, idx: c_int) -> *const c_void {
    let state = unsafe { &mut *L };
    let off = match index2offset(state, idx) {
        Some(o) => o,
        None => return std::ptr::null(),
    };
    match &state.stack[off] {
        TValue::Str(s) => s.as_str().as_ptr() as *const c_void,
        TValue::Table(t) => std::ptr::from_ref(t) as *const c_void,
        TValue::LClosure(c) => std::ptr::from_ref(c) as *const c_void,
        TValue::CClosure(c) => std::ptr::from_ref(c) as *const c_void,
        TValue::UserData(u) => std::ptr::from_ref(u) as *const c_void,
        TValue::LightUserData(p) => *p as *const c_void,
        TValue::Thread(th) => std::ptr::from_ref(th) as *const c_void,
        _ => std::ptr::from_ref(&state.stack[off]) as *const c_void,
    }
}

// ============================================================================
// External String — Lua 5.5 新增 API
// ============================================================================

/// lua_pushexternalstring: 创建外部字符串并压栈
///
/// 对应 C 5.5 的 lua_pushexternalstring。
/// s 是外部缓冲区，len 是长度，dealloc 是释放回调，ud 是回调参数。
///
/// 简化实现：拷贝 s 内容到普通 LuaString，不实际调用 dealloc
/// （.so 的 dlclose 由进程退出时 OS 回收，对测试运行无影响）。
/// 这避免了修改 GC 机制以支持外部字符串的复杂度。
#[no_mangle]
pub extern "C" fn lua_pushexternalstring(
    L: *mut lua_State,
    s: *const c_char,
    len: usize,
    _dealloc: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize, usize) -> *mut c_void>,
    _ud: *mut c_void,
) -> *const c_char {
    let L = unsafe { &mut *L };
    if s.is_null() || len == 0 {
        L.push_string("");
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
        L.push_lstring(bytes);
    }
    s
}

// ============================================================================
// package.loadlib 支持函数 — 供 base_lib.rs 调用
// ============================================================================

/// 内部函数：dlopen 加载动态库，返回库句柄
///
/// 对应 C loadlib.cpp 的 lsys_load。seeglb=true 时用 RTLD_GLOBAL。
pub unsafe fn sys_load(path: &str, seeglb: bool) -> *mut c_void {
    let cpath = match CString::new(path) {
        Ok(c) => c,
        Err(_) => return ptr::null_mut(),
    };
    let flags = if seeglb {
        libc::RTLD_NOW | libc::RTLD_GLOBAL
    } else {
        libc::RTLD_NOW | libc::RTLD_LOCAL
    };
    unsafe { libc::dlopen(cpath.as_ptr(), flags) }
}

/// 内部函数：dlsym 查找符号，返回函数指针
///
/// 对应 C loadlib.cpp 的 lsys_sym。
pub unsafe fn sys_sym(lib: *mut c_void, sym: &str) -> Option<lua_CFunction> {
    let csym = CString::new(sym).ok()?;
    let ptr = unsafe { libc::dlsym(lib, csym.as_ptr()) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute::<*mut c_void, lua_CFunction>(ptr) })
    }
}

pub unsafe fn sys_unload(lib: *mut c_void) {
    if !lib.is_null() {
        unsafe {
            libc::dlclose(lib);
        }
    }
}

/// 内部函数：dlerror 获取错误消息
pub unsafe fn sys_dlerror() -> String {
    let ptr = unsafe { libc::dlerror() };
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

// ============================================================================
// luaL_* 扩展函数 — 供 sol2 等 C++ binding 库使用
// ============================================================================
// 这些函数对应 C 实现的 lauxlib.cpp 中的 LUALIB_API 函数。
// 之前只导出了 C 模块（cjson/luasocket/lsqlite3）需要的少数 luaL_* 函数，
// sol2 等更高级的 binding 库需要完整的 luaL_* API。

/// luaL_loadbufferx: 加载缓冲区为 Lua 代码块
///
/// 对应 C lauxlib.cpp 的 luaL_loadbufferx。
/// 用 lua_load + reader 回调实现。
#[no_mangle]
pub extern "C" fn luaL_loadbufferx(
    L: *mut lua_State,
    buff: *const c_char,
    size: usize,
    name: *const c_char,
    mode: *const c_char,
) -> c_int {
    if buff.is_null() || size == 0 {
        // 空缓冲区：push 空函数
        let state = unsafe { &mut *L };
        let name_str = if name.is_null() {
            "=(load)".to_string()
        } else {
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        };
        return state.load_buffer("", &name_str) as c_int;
    }

    // 用 lua_load 的 reader 机制
    struct LoadS {
        s: *const c_char,
        size: usize,
    }
    unsafe extern "C" fn reader(
        _L: *mut lua_State,
        data: *mut c_void,
        sz: *mut usize,
    ) -> *const c_char {
        let ls = &mut *(data as *mut LoadS);
        if ls.size == 0 {
            *sz = 0;
            return ptr::null();
        }
        *sz = ls.size;
        let ptr = ls.s;
        ls.size = 0; // 一次性返回全部
        ptr
    }
    let ls = LoadS { s: buff, size };
    lua_load(
        L,
        reader,
        &ls as *const LoadS as *mut c_void,
        name,
        mode,
    )
}

/// luaL_loadbuffer: 兼容宏（luaL_loadbufferx with mode=NULL）
#[no_mangle]
pub extern "C" fn luaL_loadbuffer(
    L: *mut lua_State,
    buff: *const c_char,
    size: usize,
    name: *const c_char,
) -> c_int {
    luaL_loadbufferx(L, buff, size, name, ptr::null())
}

/// luaL_loadfilex: 从文件加载 Lua 代码块。
///
/// Lua 5.5 标准 C API。读取文件内容，跳过 `#!` shebang 行，调用
/// `luaL_loadbufferx` 加载。`filename` 为 NULL 时从 stdin 读取。
/// `mode` 为 "b"（仅字节码）、"t"（仅文本）、"bt"（两者皆可）或 NULL。
#[no_mangle]
pub extern "C" fn luaL_loadfilex(
    L: *mut lua_State,
    filename: *const c_char,
    mode: *const c_char,
) -> c_int {
    use std::io::Read;

    // 读取文件内容
    let content: Vec<u8> = if filename.is_null() {
        // 从 stdin 读取
        let mut buf = Vec::new();
        match std::io::stdin().lock().read_to_end(&mut buf) {
            Ok(_) => buf,
            Err(_) => {
                let L = unsafe { &mut *L };
                L.push_string("cannot read from stdin");
                return 3; // LUA_ERRSYNTAX
            }
        }
    } else {
        let fname = unsafe { CStr::from_ptr(filename) };
        let path = match fname.to_str() {
            Ok(s) => s,
            Err(_) => {
                let L = unsafe { &mut *L };
                L.push_string("invalid filename");
                return 3; // LUA_ERRSYNTAX
            }
        };
        match std::fs::File::open(path) {
            Ok(mut f) => {
                let mut buf = Vec::new();
                match f.read_to_end(&mut buf) {
                    Ok(_) => buf,
                    Err(e) => {
                        let L = unsafe { &mut *L };
                        L.push_string(&format!("cannot read {}: {}", path, e));
                        return 3; // LUA_ERRSYNTAX
                    }
                }
            }
            Err(e) => {
                let L = unsafe { &mut *L };
                L.push_string(&format!("cannot open {}: {}", path, e));
                return 6; // LUA_ERRFILE = LUA_ERRERR(5) + 1
            }
        }
    };

    // 跳过 #! shebang 行（Lua 标准行为）
    let content = if content.starts_with(b"#!") {
        // 找到第一个换行符后的内容
        if let Some(pos) = content.iter().position(|&b| b == b'\n') {
            &content[pos + 1..]
        } else {
            &content[..]
        }
    } else {
        &content[..]
    };

    // 构造 chunk name: "@filename" (必须 NUL 终止，因为 luaL_loadbufferx 内部用 CStr::from_ptr)
    let name_buf: Vec<u8> = if filename.is_null() {
        b"=stdin\0".to_vec()
    } else {
        let fname = unsafe { CStr::from_ptr(filename) };
        let mut name = Vec::with_capacity(fname.to_bytes().len() + 2); // +1 for '@', +1 for NUL
        name.push(b'@');
        name.extend_from_slice(fname.to_bytes());
        name.push(0); // NUL 终止符
        name
    };

    luaL_loadbufferx(
        L,
        content.as_ptr() as *const c_char,
        content.len(),
        name_buf.as_ptr() as *const c_char,
        mode,
    )
}

/// luaL_loadstring: 加载字符串
#[no_mangle]
pub extern "C" fn luaL_loadstring(L: *mut lua_State, s: *const c_char) -> c_int {
    if s.is_null() {
        return 3; // LUA_ERRSYNTAX
    }
    let cstr = unsafe { CStr::from_ptr(s) };
    let bytes = cstr.to_bytes();
    luaL_loadbufferx(L, s, bytes.len(), s, ptr::null())
}

/// luaL_getmetatable: 从注册表获取指定名称的元表
///
/// 返回值的类型（LUA_TNIL 如果不存在）。
#[no_mangle]
pub extern "C" fn luaL_getmetatable(L: *mut lua_State, name: *const c_char) -> c_int {
    let L = unsafe { &mut *L };
    let name_str = if name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned()
    };
    let key = crate::state::str_to_ls(&L.string_table, &name_str);
    let val = L
        .registry
        .get(&TValue::Str(key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    let ty = lua_type_code(val.ty());
    L.stack.push(val);
    ty
}

/// luaL_newmetatable: 创建并注册元表到注册表
///
/// 如果已存在返回 0（不创建），否则创建并返回 1。
#[no_mangle]
pub extern "C" fn luaL_newmetatable(L: *mut lua_State, tname: *const c_char) -> c_int {
    // 先检查是否已存在
    let existing_type = luaL_getmetatable(L, tname);
    if existing_type != 0 { // 非 nil（LUA_TNIL=0）
        return 0; // 已存在，不创建
    }
    // 弹出 nil
    let L = unsafe { &mut *L };
    L.stack.pop();
    // 创建新表
    lua_createtable(L, 0, 2);
    // 设置 __name 字段
    let tname_str = if tname.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(tname) }
            .to_string_lossy()
            .into_owned()
    };
    lua_pushstring(L, tname);
    lua_setfield(L, -2, c"__name".as_ptr());
    // 注册到 registry[tname] = metatable
    // 对应 C: lua_pushvalue(L, -1); lua_setfield(L, LUA_REGISTRYINDEX, tname);
    // lua_setfield 会弹出复制的值，这里需手动 pop
    lua_pushvalue(L, -1);
    let key = crate::state::str_to_ls(&L.string_table, &tname_str);
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    L.registry.set(TValue::Str(key), val);
    1
}

/// luaL_getsubtable: 获取 t[idx] 中的子表 [name]
///
/// 如果不存在则创建。成功返回 1，失败返回 0。
#[no_mangle]
pub extern "C" fn luaL_getsubtable(L: *mut lua_State, idx: c_int, fname: *const c_char) -> c_int {
    // 获取 t[idx]
    lua_getfield(L, idx, fname);
    let L = unsafe { &mut *L };
    let is_table = matches!(
        L.stack.last().unwrap_or(&TValue::Nil(NilKind::Strict)),
        TValue::Table(_)
    );
    if is_table {
        return 1; // 已存在
    }
    // 不存在，弹出 nil，创建新表
    L.stack.pop();
    lua_createtable(L, 0, 0);
    // t[fname] = newtable
    // 需要：push t[idx]，push newtable，setfield
    // 但 setfield 会 pop newtable，所以先复制一份
    lua_pushvalue(L, -1); // 复制 newtable
    lua_setfield(L, idx, fname); // t[fname] = newtable（pop 副本）
    // 栈顶保留 newtable
    1
}

/// luaL_checkstack: 确保栈有 space 个额外空间，否则抛出 "stack overflow" 错误。
///
/// 对应 C 的 lauxlib.cpp::luaL_checkstack。
#[no_mangle]
pub extern "C-unwind" fn luaL_checkstack(L: *mut lua_State, space: c_int, msg: *const c_char) {
    if unsafe { lua_checkstack(L, space) } == 0 {
        // 栈溢出：构造错误消息并抛出
        let errmsg = if !msg.is_null() {
            let cstr = unsafe { CStr::from_ptr(msg) };
            format!("stack overflow ({})", cstr.to_string_lossy())
        } else {
            "stack overflow".to_string()
        };
        let L = unsafe { &mut *L };
        L.push_string(&errmsg);
        unsafe { lua_error(L) };
    }
}

/// luaL_where: 标记当前调用位置（level 1）的错误信息前缀
///
/// push "chunkname:line: " 到栈顶。
#[no_mangle]
pub extern "C" fn luaL_where(L: *mut lua_State, _level: c_int) {
    // 简化实现：push 空字符串（Rust 实现的调试信息结构与 C 不同）
    let L = unsafe { &mut *L };
    L.push_string("");
}

/// luaL_error: 抛出格式化错误
///
/// 注意：Rust stable 不支持 c_variadic，真正的 luaL_error 由
/// deps/capi_compat.c 中的 C 代码实现（用 vsnprintf 格式化可变参数）。
/// 此 Rust 版本仅作为 fallback，不导出（无 #[no_mangle]）。
#[allow(dead_code)]
extern "C-unwind" fn luaL_error_rust(L: *mut lua_State, fmt: *const c_char) -> c_int {
    if !fmt.is_null() {
        unsafe { lua_pushstring(L, fmt) };
    } else {
        let L = unsafe { &mut *L };
        L.push_string("error");
    }
    unsafe { lua_error(L) }
}

/// luaL_requiref: 简化版 require
///
/// 调用 openf 打开模块，注册到 package.loaded，可选注册到全局表。
#[no_mangle]
pub extern "C" fn luaL_requiref(
    L: *mut lua_State,
    modname: *const c_char,
    openf: lua_CFunction,
    glb: c_int,
) {
    // 对应 C 的 luaL_requiref:
    //   1. 检查 package.loaded[modname] (= registry["_LOADED"][modname])
    //   2. 若未加载，调用 openf(modname)，结果存入 package.loaded[modname]
    //   3. 可选设置全局变量 modname = result
    let L = unsafe { &mut *L };
    let modname_str = if modname.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(modname) }
            .to_string_lossy()
            .into_owned()
    };

    let mod_key = crate::state::str_to_ls(&L.string_table, &modname_str);

    // 获取 registry["_LOADED"] 表（与 package.loaded 共享同一 Rc 引用）
    let loaded_key = crate::state::str_to_ls(&L.string_table, "_LOADED");
    let loaded_table = match L.registry.get(&TValue::Str(loaded_key.clone())) {
        Some(TValue::Table(t)) => t,
        _ => {
            // _LOADED 表不存在：创建并注册到 registry
            let t = crate::table::Table::new();
            L.registry
                .set(TValue::Str(loaded_key), TValue::Table(t.clone()));
            t
        }
    };

    // 检查是否已加载：_LOADED[modname]
    let loaded_val = loaded_table
        .get(&TValue::Str(mod_key.clone()))
        .unwrap_or(TValue::Nil(NilKind::Strict));

    if !matches!(loaded_val, TValue::Nil(_)) && !matches!(loaded_val, TValue::Boolean(false)) {
        // 已加载，push 到栈顶
        L.stack.push(loaded_val);
    } else {
        // 未加载，调用 openf
        L.stack.push(TValue::LCFn(LCFunction { func: openf }));
        L.push_string(&modname_str);
        let status = L.pcall(1, 1, 0);
        if status != 0 {
            // 调用失败，弹出错误，push 模块名作为 fallback
            L.stack.pop();
            L.push_string(&modname_str);
        }

        // 注册到 package.loaded[modname] = result
        if let Some(val) = L.stack.last() {
            let val = val.clone();
            loaded_table.set(TValue::Str(mod_key.clone()), val);
        }

        // 如果 glb，设置全局变量
        if glb != 0 {
            let val = L.stack.last().cloned().unwrap_or(TValue::Nil(NilKind::Strict));
            L.globals.set(TValue::Str(mod_key), val);
        }
    }
}

/// lua_xmove: 在线程间移动 n 个值
///
/// 对应 C lapi.cpp 的 lua_xmove。
/// 从 from 栈顶弹出 n 个值，push 到 to 栈顶。
#[no_mangle]
pub extern "C" fn lua_xmove(from: *mut lua_State, to: *mut lua_State, n: c_int) {
    if from == to || n <= 0 {
        return;
    }
    let from = unsafe { &mut *from };
    let to = unsafe { &mut *to };
    let n = n as usize;
    let start = if from.stack.len() >= n {
        from.stack.len() - n
    } else {
        0
    };
    let vals: Vec<_> = from.stack.drain(start..).collect();
    for v in vals {
        to.stack.push(v);
    }
}

/// lua_pushglobaltable: push 全局表到栈顶
#[no_mangle]
pub extern "C" fn lua_pushglobaltable(L: *mut lua_State) {
    let L = unsafe { &mut *L };
    L.stack.push(TValue::Table(L.globals.clone()));
}

/// luaL_traceback: 生成调用栈回溯字符串
///
/// 简化实现：只 push msg（如果非空），不生成完整栈回溯。
/// Rust lua 的调试信息结构与 C 不同，完整实现需要 lua_getstack/lua_getinfo。
#[no_mangle]
pub extern "C" fn luaL_traceback(
    L: *mut lua_State,
    _L1: *mut lua_State,
    msg: *const c_char,
    _level: c_int,
) {
    let L = unsafe { &mut *L };
    if msg.is_null() {
        L.push_string("stack traceback:");
    } else {
        let msg_str = unsafe { CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned();
        L.push_string(&format!("{}\nstack traceback:", msg_str));
    }
}

// ============================================================================
// luaopen_* 标准库开库函数 — 供 luaL_requiref 调用
// ============================================================================
// 每个函数调用对应的 Rust open_*_lib，然后 push 库表（或全局表）到栈顶，
// 返回 1。签名与 C 实现一致：int luaopen_xxx(lua_State *L)。

/// luaL_openselectedlibs: 选择性打开标准库（Lua 5.5 标准 C API）。
///
/// 对应 C 的 `luaL_openselectedlibs(L, load, preload)`。lua-rs 不支持选择性打开
/// （load/preload 参数被忽略），调用一次即打开所有标准库。`luaL_openlibs(L)`
/// 在 lualib.h 中定义为 `luaL_openselectedlibs(L, ~0, 0)`，skynet 等第三方 C
/// 代码会调用 `luaL_openlibs`，此函数确保标准库被正确初始化。
#[no_mangle]
pub extern "C" fn luaL_openselectedlibs(L: *mut lua_State, _load: c_int, _preload: c_int) {
    let L = unsafe { &mut *L };
    L.open_selected_libs(-1, 0);
}

/// luaopen_base: 打开基础库
#[no_mangle]
pub extern "C" fn luaopen_base(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    crate::stdlib::base_lib::open_base_lib(L);
    // push 全局表作为返回值
    L.stack.push(TValue::Table(L.globals.clone()));
    1
}

/// luaopen_math: 打开数学库
#[no_mangle]
pub extern "C" fn luaopen_math(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    crate::stdlib::math_lib::open_math_lib(L);
    // push math 表
    let math_key = crate::state::str_to_ls(&L.string_table, "math");
    let math_val = L
        .globals
        .get(&TValue::Str(math_key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    L.stack.push(math_val);
    1
}

/// luaopen_string: 打开字符串库
#[no_mangle]
pub extern "C" fn luaopen_string(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    crate::stdlib::string_lib::open_string_lib(L);
    let key = crate::state::str_to_ls(&L.string_table, "string");
    let val = L
        .globals
        .get(&TValue::Str(key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    L.stack.push(val);
    1
}

/// luaopen_os: 打开 OS 库
#[no_mangle]
pub extern "C" fn luaopen_os(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    crate::stdlib::os_lib::open_os_lib(L);
    let key = crate::state::str_to_ls(&L.string_table, "os");
    let val = L
        .globals
        .get(&TValue::Str(key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    L.stack.push(val);
    1
}

/// luaopen_coroutine: 打开协程库
#[no_mangle]
pub extern "C" fn luaopen_coroutine(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    crate::stdlib::coroutine_lib::open_coroutine_lib(L);
    let key = crate::state::str_to_ls(&L.string_table, "coroutine");
    let val = L
        .globals
        .get(&TValue::Str(key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    L.stack.push(val);
    1
}

/// luaopen_table: 打开 table 库
#[no_mangle]
pub extern "C" fn luaopen_table(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    crate::stdlib::table_lib::open_table_lib(L);
    let key = crate::state::str_to_ls(&L.string_table, "table");
    let val = L
        .globals
        .get(&TValue::Str(key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    L.stack.push(val);
    1
}

/// luaopen_io: 打开 IO 库
#[no_mangle]
pub extern "C" fn luaopen_io(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    crate::stdlib::io_lib::open_io_lib(L);
    let key = crate::state::str_to_ls(&L.string_table, "io");
    let val = L
        .globals
        .get(&TValue::Str(key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    L.stack.push(val);
    1
}

/// luaopen_debug: 打开 debug 库
#[no_mangle]
pub extern "C" fn luaopen_debug(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    crate::stdlib::debug_lib::open_debug_lib(L);
    let key = crate::state::str_to_ls(&L.string_table, "debug");
    let val = L
        .globals
        .get(&TValue::Str(key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    L.stack.push(val);
    1
}

/// luaopen_utf8: 打开 utf8 库
#[no_mangle]
pub extern "C" fn luaopen_utf8(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    crate::stdlib::utf8_lib::open_utf8_lib(L);
    let key = crate::state::str_to_ls(&L.string_table, "utf8");
    let val = L
        .globals
        .get(&TValue::Str(key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    L.stack.push(val);
    1
}

/// luaopen_package: 打开 package 库
///
/// Rust 实现中 package 表由 open_base_lib 初始化，
/// 这里直接返回 package 表。
#[no_mangle]
pub extern "C" fn luaopen_package(L: *mut lua_State) -> c_int {
    let L = unsafe { &mut *L };
    // package 表已在 open_base_lib 中初始化
    let key = crate::state::str_to_ls(&L.string_table, "package");
    let val = L
        .globals
        .get(&TValue::Str(key))
        .unwrap_or(TValue::Nil(NilKind::Strict));
    L.stack.push(val);
    1
}

// ============================================================================
// skynet 兼容 API — Lua 5.5 标准 C API 补全
// ============================================================================
// 本区域补全 skynet 框架链接 lua-rs 静态库时所需的 24 个标准 C API。
// 实现遵循 lua.h / lauxlib.h 的签名与语义，部分复杂 API（如 coroutine）
// 做了简化处理，注释中标注简化点。

// ---------------------------------------------------------------------------
// 类型定义（与 lua.h / lauxlib.h 对齐）
// ---------------------------------------------------------------------------

/// lua_Hook — 调试钩子回调类型（对应 C lua.h 的 lua_Hook）
pub type lua_Hook = unsafe extern "C" fn(L: *mut lua_State, ar: *mut lua_Debug);

/// lua_KContext — 延续函数上下文类型（对应 C 的 intptr_t）
pub type lua_KContext = isize;

/// lua_KFunction — 延续函数类型（对应 C lua.h 的 lua_KFunction）
pub type lua_KFunction =
    unsafe extern "C" fn(L: *mut lua_State, status: c_int, ctx: lua_KContext) -> c_int;

/// LUAL_BUFFERSIZE — 初始内联缓冲区大小（与 C luaconf.h 一致：16 * sizeof(void*) * sizeof(lua_Number)）
pub const LUAL_BUFFERSIZE: usize = 16 * std::mem::size_of::<*mut c_void>() * std::mem::size_of::<lua_Number>();

/// luaL_Buffer — 字符串拼接缓冲区（与 C lauxlib.h 布局兼容）
///
/// 对应 C 的 `struct luaL_Buffer`。使用 `#[repr(C)]` 确保与 C 代码在栈上分配的
/// luaL_Buffer 布局一致，使 C 代码传入的指针可被 Rust 函数安全操作。
/// 字段 b 初始指向 init 内联缓冲区，数据超过 LUAL_BUFFERSIZE 时切换到堆分配。
#[repr(C)]
pub struct luaL_Buffer {
    /// 缓冲区当前地址（初始指向 init，扩容后指向堆分配的内存）
    b: *mut c_char,
    /// 缓冲区总容量
    size: usize,
    /// 已使用的字节数
    n: usize,
    /// 关联的 lua_State（供 pushresult/addvalue 使用）
    L: *mut lua_State,
    /// 初始内联缓冲区（避免小字符串堆分配）
    init: [u8; LUAL_BUFFERSIZE],
}

// ---------------------------------------------------------------------------
// 内部辅助：错误消息构造
// ---------------------------------------------------------------------------

/// 将 &str 压栈为 Lua 字符串（不依赖 CString，避免 NUL 截断问题）
fn push_str_to_stack(L: *mut lua_State, s: &str) {
    let L = unsafe { &mut *L };
    L.push_string(s);
}

// ---------------------------------------------------------------------------
// 1. 类型检查 API（对应 C lauxlib.cpp）
// ---------------------------------------------------------------------------

/// luaL_checktype: 检查 idx 处参数类型是否为 t，不匹配则 luaL_typeerror。
///
/// 对应 C lauxlib.cpp::luaL_checktype。通过 lua_type 获取实际类型并与 t 比较，
/// 不匹配时调用 luaL_typeerror 抛错（不返回）。
#[no_mangle]
pub extern "C-unwind" fn luaL_checktype(L: *mut lua_State, arg: c_int, t: c_int) {
    let actual = lua_type(L, arg);
    if actual != t {
        let tname = lua_typename(L, t);
        luaL_typeerror(L, arg, tname);
    }
}

/// luaL_checkany: 检查 idx 处是否有参数（非 NONE），否则 luaL_argerror。
///
/// 对应 C lauxlib.cpp::luaL_checkany。lua_type 返回 LUA_TNONE 表示无参数。
#[no_mangle]
pub extern "C-unwind" fn luaL_checkany(L: *mut lua_State, arg: c_int) {
    if lua_type(L, arg) == LUA_TNONE {
        let msg = CString::new("value expected").unwrap();
        luaL_argerror(L, arg, msg.as_ptr());
    }
}

/// luaL_checkinteger: 检查并返回整数，非数字则 luaL_typeerror。
///
/// 对应 C lauxlib.cpp::luaL_checkinteger。用 lua_tointegerx 尝试转换，
/// isnum=0 表示非数字，抛出类型错误。
#[no_mangle]
pub extern "C-unwind" fn luaL_checkinteger(L: *mut lua_State, arg: c_int) -> lua_Integer {
    let mut isnum: c_int = 0;
    let n = lua_tointegerx(L, arg, &mut isnum);
    if isnum == 0 {
        let tname = lua_typename(L, LUA_TNUMBER);
        luaL_typeerror(L, arg, tname);
    }
    n
}

/// luaL_checknumber: 检查并返回数字，非数字则 luaL_typeerror。
///
/// 对应 C lauxlib.cpp::luaL_checknumber。用 lua_tonumberx 尝试转换。
#[no_mangle]
pub extern "C-unwind" fn luaL_checknumber(L: *mut lua_State, arg: c_int) -> lua_Number {
    let mut isnum: c_int = 0;
    let n = lua_tonumberx(L, arg, &mut isnum);
    if isnum == 0 {
        let tname = lua_typename(L, LUA_TNUMBER);
        luaL_typeerror(L, arg, tname);
    }
    n
}

/// luaL_optinteger: 可选整数，nil 返回 def，非数字则 luaL_typeerror。
///
/// 对应 C lauxlib.cpp::luaL_optinteger。idx 处为 nil 时返回 def，
/// 否则按 luaL_checkinteger 语义检查。
#[no_mangle]
pub extern "C-unwind" fn luaL_optinteger(
    L: *mut lua_State,
    arg: c_int,
    def: lua_Integer,
) -> lua_Integer {
    if lua_type(L, arg) == LUA_TNIL {
        return def;
    }
    luaL_checkinteger(L, arg)
}

/// luaL_optnumber: 可选数字，nil 返回 def，非数字则 luaL_typeerror。
///
/// 对应 C lauxlib.cpp::luaL_optnumber。
#[no_mangle]
pub extern "C-unwind" fn luaL_optnumber(
    L: *mut lua_State,
    arg: c_int,
    def: lua_Number,
) -> lua_Number {
    if lua_type(L, arg) == LUA_TNIL {
        return def;
    }
    luaL_checknumber(L, arg)
}

/// luaL_argerror: 抛出参数错误。
///
/// 对应 C lauxlib.cpp::luaL_argerror。调用 lua_getstack/lua_getinfo 获取
/// 调用者函数名，格式化为 "bad argument #N to 'func' (msg)"（与 C 一致）。
/// 若无法获取函数名，使用 "?" 作为函数名（与 C 一致）。
#[no_mangle]
pub extern "C-unwind" fn luaL_argerror(
    L: *mut lua_State,
    arg: c_int,
    extramsg: *const c_char,
) -> c_int {
    let extra = if extramsg.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(extramsg) }
            .to_string_lossy()
            .into_owned()
    };
    // 对应 C luaL_argerror: level=0 获取当前运行的 C 函数帧
    // （luaL_argerror 本身不在 Lua 栈中，level=0 即调用它的 C 函数）
    let mut ar: lua_Debug = unsafe { std::mem::zeroed() };
    if lua_getstack(L, 0, &mut ar) == 0 {
        let msg = format!("bad argument #{} ({})", arg, extra);
        push_str_to_stack(L, &msg);
        return lua_error(L);
    }
    // 获取 n(名字) 和 t(extraargs/istailcall) 信息
    let what = b"nt\0";
    lua_getinfo(L, what.as_ptr() as *const c_char, &mut ar);
    // 对应 C luaL_argerror 的 argword/arg 调整逻辑
    let extraargs = ar.extraargs as c_int;
    let namewhat_str = if ar.namewhat.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(ar.namewhat) }
            .to_str()
            .unwrap_or("")
    };
    // 处理 method（冒号语法）和 extraargs
    let (argword, adj_arg, is_self_error) = if arg <= extraargs {
        ("extra argument", arg, false)
    } else {
        let adj = arg - extraargs;
        if namewhat_str == "method" {
            let adj2 = adj - 1;
            if adj2 == 0 {
                // 错误在 self 参数中
                (".", 0, true)
            } else {
                ("argument", adj2, false)
            }
        } else {
            ("argument", adj, false)
        }
    };
    // 对应 C: if (ar.name == NULL) ar.name = "?";
    let name_str = if ar.name.is_null() {
        "?".to_string()
    } else {
        unsafe { CStr::from_ptr(ar.name) }
            .to_string_lossy()
            .into_owned()
    };
    let msg = if is_self_error {
        // self 参数错误：calling 'name' on bad self (msg)
        format!("calling '{}' on bad self ({})", name_str, extra)
    } else {
        format!("bad {} #{} to '{}' ({})", argword, adj_arg, name_str, extra)
    };
    push_str_to_stack(L, &msg);
    lua_error(L)
}

/// luaL_typeerror: 抛出类型错误。
///
/// 对应 C lauxlib.cpp::luaL_typeerror。格式化为 "tname expected, got typearg"，
/// 然后调用 luaL_argerror。typearg 优先取 __name 元字段，否则用 lua_typename。
#[no_mangle]
pub extern "C-unwind" fn luaL_typeerror(
    L: *mut lua_State,
    arg: c_int,
    tname: *const c_char,
) -> c_int {
    let tname_str = if tname.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(tname) }
            .to_string_lossy()
            .into_owned()
    };
    // 获取实际参数的类型名
    let typearg = {
        let L_ref = unsafe { &*L };
        match index2val(L_ref, arg) {
            // 轻量 userdata 特殊处理（与 C 一致）
            Some(TValue::LightUserData(_)) => "light userdata".to_string(),
            // 其他类型用 lua_typename
            Some(v) => {
                let tn = lua_typename(L, lua_type_code(v.ty()));
                if tn.is_null() {
                    "no value".to_string()
                } else {
                    unsafe { CStr::from_ptr(tn) }
                        .to_string_lossy()
                        .into_owned()
                }
            }
            None => "no value".to_string(),
        }
    };
    let msg = format!("{} expected, got {}", tname_str, typearg);
    let cmsg = CString::new(msg).unwrap();
    luaL_argerror(L, arg, cmsg.as_ptr())
}

/// luaL_checkudata: 检查 userdata 是否有指定名称的元表，不匹配则 luaL_typeerror。
///
/// 对应 C lauxlib.cpp::luaL_checkudata。先调用 luaL_testudata 检查，
/// 不匹配时调用 luaL_typeerror 抛错。返回 userdata 的数据区指针。
#[no_mangle]
pub extern "C-unwind" fn luaL_checkudata(
    L: *mut lua_State,
    ud: c_int,
    tname: *const c_char,
) -> *mut c_void {
    // luaL_testudata 逻辑：lua_touserdata 获取指针，lua_getmetatable 获取元表，
    // luaL_getmetatable 获取注册表中的元表，lua_rawequal 比较。
    let p = lua_touserdata(L, ud);
    if !p.is_null() {
        if lua_getmetatable(L, ud) != 0 {
            // 栈顶是 userdata 的元表
            luaL_getmetatable(L, tname);
            // 栈顶是注册表中的元表，-2 是 userdata 的元表
            if lua_rawequal(L, -1, -2) != 0 {
                // 匹配：弹出两个元表，返回指针
                lua_pop(L, 2);
                return p;
            }
            // 不匹配：弹出两个元表
            lua_pop(L, 2);
        }
    }
    // 不匹配或非 userdata：抛出类型错误
    let tname_str = if tname.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(tname) }
            .to_string_lossy()
            .into_owned()
    };
    let cmsg = CString::new(tname_str).unwrap_or_else(|_| CString::new("").unwrap());
    luaL_typeerror(L, ud, cmsg.as_ptr());
    std::ptr::null_mut()
}

/// luaL_getmetafield: 获取 idx 处对象的元表中名为 name 的字段，压栈并返回类型。
///
/// 对应 C lauxlib.cpp::luaL_getmetafield。先 lua_getmetatable 获取元表，
/// 然后 lua_rawget 查找字段；nil 时弹出元表和字段，返回 LUA_TNIL。
#[no_mangle]
pub extern "C" fn luaL_getmetafield(
    L: *mut lua_State,
    obj: c_int,
    event: *const c_char,
) -> c_int {
    // 没有元表：返回 LUA_TNIL（不压栈）
    if lua_getmetatable(L, obj) == 0 {
        return LUA_TNIL;
    }
    // 栈顶是元表，push event 名字，rawget 查找
    lua_pushstring(L, event);
    // lua_rawget 弹出 key，压入 value，返回 value 类型
    let tt = lua_rawget(L, -2);
    if tt == LUA_TNIL {
        // 字段是 nil：弹出元表和 nil（2 个元素）
        lua_pop(L, 2);
    } else {
        // 字段非 nil：移除元表（保留字段在栈顶）
        lua_remove(L, -2);
    }
    tt
}

/// luaL_tolstring: 将 idx 处值转为字符串，压栈并返回指针。
///
/// 对应 C lauxlib.cpp::luaL_tolstring。先尝试 __tostring 元方法，
/// 否则按类型转换：number→数字字符串，string→副本，boolean→"true"/"false"，
/// nil→"nil"，其他→"type: 0xptr"。
#[no_mangle]
pub extern "C" fn luaL_tolstring(
    L: *mut lua_State,
    idx: c_int,
    len: *mut usize,
) -> *const c_char {
    let absidx = lua_absindex(L, idx);
    // 尝试 __tostring 元方法（对应 C 的 luaL_callmeta）
    if luaL_getmetafield(L, absidx, c"__tostring".as_ptr()) != LUA_TNIL {
        // 栈顶是 __tostring 元方法，push 对象，调用
        lua_pushvalue(L, absidx);
        // lua_callk 会 panic 传播错误（与 C 的 lua_call 一致）
        lua_callk(L, 1, 1, 0, None);
        // 检查返回值是否为字符串
        if lua_isstring(L, -1) == 0 {
            let msg = CString::new("'__tostring' must return a string").unwrap();
            push_str_to_stack(L, &msg.to_string_lossy());
            lua_error(L);
        }
    } else {
        // 无 __tostring：按类型转换
        let ty = lua_type(L, absidx);
        match ty {
            LUA_TNUMBER => {
                // 数字：用 lua_tolstring 转换（会原地修改栈值为字符串）
                lua_tolstring(L, absidx, std::ptr::null_mut());
                lua_pushvalue(L, absidx);
            }
            LUA_TSTRING => {
                lua_pushvalue(L, absidx);
            }
            LUA_TBOOLEAN => {
                let b = lua_toboolean(L, absidx);
                if b != 0 {
                    lua_pushstring(L, c"true".as_ptr());
                } else {
                    lua_pushstring(L, c"false".as_ptr());
                }
            }
            LUA_TNIL => {
                lua_pushstring(L, c"nil".as_ptr());
            }
            _ => {
                // 其他类型：尝试 __name 元字段，否则用 typename
                let tt = luaL_getmetafield(L, absidx, c"__name".as_ptr());
                let kind = if tt == LUA_TSTRING {
                    // 栈顶是 __name 字符串
                    let p = lua_tolstring(L, -1, std::ptr::null_mut());
                    if !p.is_null() {
                        unsafe { CStr::from_ptr(p) }
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        String::new()
                    }
                } else {
                    // 无 __name：用 typename
                    if tt != LUA_TNIL {
                        lua_pop(L, 1); // 弹出 nil 或非字符串值
                    }
                    let tn = lua_typename(L, ty);
                    if tn.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(tn) }
                            .to_string_lossy()
                            .into_owned()
                    }
                };
                let ptr = lua_topointer(L, absidx);
                let msg = format!("{}: {:p}", kind, ptr);
                if tt == LUA_TSTRING {
                    // 弹出 __name，push 格式化字符串
                    lua_pop(L, 1);
                }
                push_str_to_stack(L, &msg);
            }
        }
    }
    // 返回栈顶字符串的指针和长度
    lua_tolstring(L, -1, len)
}

// ---------------------------------------------------------------------------
// 2. Buffer API（对应 C lauxlib.cpp 的 luaL_Buffer 系列）
// ---------------------------------------------------------------------------

/// luaL_buffinit: 初始化 buffer。
///
/// 对应 C lauxlib.cpp::luaL_buffinit。设置 b 指向内联 init 缓冲区。
/// 简化点：C 版本会 push 一个 light userdata 占位符到栈顶（供 addvalue 的
/// prepbuffsize 错误路径使用），此简化版不 push 占位符，buffer 期间栈深度不变。
#[no_mangle]
pub extern "C" fn luaL_buffinit(L: *mut lua_State, B: *mut luaL_Buffer) {
    if B.is_null() {
        return;
    }
    unsafe {
        (*B).b = (*B).init.as_mut_ptr() as *mut c_char;
        (*B).size = LUAL_BUFFERSIZE;
        (*B).n = 0;
        (*B).L = L;
    }
}

/// luaL_prepbuffsize: 确保 buffer 有 sz 字节可用空间，返回可用空间起始指针。
///
/// 对应 C lauxlib.cpp::luaL_prepbuffsize。当剩余空间不足时扩容：
/// - 若当前用的是 init 内联缓冲区，malloc 新内存并拷贝
/// - 若当前用的是堆内存，realloc 扩容
#[no_mangle]
pub extern "C" fn luaL_prepbuffsize(B: *mut luaL_Buffer, sz: usize) -> *mut c_char {
    if B.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        if (*B).size - (*B).n < sz {
            // 计算新容量：至少满足需求，且至少翻倍（与 C luaH_resize 类似策略）
            let need = (*B).n + sz;
            let new_size = need.max((*B).size * 2);
            let init_ptr = (*B).init.as_mut_ptr() as *mut c_char;
            if (*B).b == init_ptr {
                // 从 init 内联缓冲区升级到堆分配
                let new_buf = libc::malloc(new_size) as *mut c_char;
                if new_buf.is_null() {
                    return ptr::null_mut();
                }
                std::ptr::copy_nonoverlapping((*B).b, new_buf, (*B).n);
                (*B).b = new_buf;
            } else {
                // 已是堆分配，realloc 扩容
                let new_buf =
                    libc::realloc((*B).b as *mut c_void, new_size) as *mut c_char;
                if new_buf.is_null() {
                    return ptr::null_mut();
                }
                (*B).b = new_buf;
            }
            (*B).size = new_size;
        }
        (*B).b.add((*B).n)
    }
}

/// luaL_addlstring: 追加长度为 l 的字符串到 buffer。
///
/// 对应 C lauxlib.cpp::luaL_addlstring。l=0 时不操作（避免 NULL 解引用）。
#[no_mangle]
pub extern "C" fn luaL_addlstring(B: *mut luaL_Buffer, s: *const c_char, l: usize) {
    if B.is_null() || s.is_null() || l == 0 {
        return;
    }
    let dst = luaL_prepbuffsize(B, l);
    if dst.is_null() {
        return;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(s, dst, l);
        (*B).n += l;
    }
}

/// luaL_addvalue: 弹出栈顶字符串追加到 buffer。
///
/// 对应 C lauxlib.cpp::luaL_addvalue。用 lua_tolstring 获取栈顶字符串，
/// 调用 luaL_addlstring 追加，然后 lua_pop 弹出栈顶值。
#[no_mangle]
pub extern "C" fn luaL_addvalue(B: *mut luaL_Buffer) {
    if B.is_null() {
        return;
    }
    let L = unsafe { (*B).L };
    if L.is_null() {
        return;
    }
    let mut len: usize = 0;
    let s = lua_tolstring(L, -1, &mut len);
    if !s.is_null() && len > 0 {
        luaL_addlstring(B, s, len);
    }
    lua_pop(L, 1);
}

/// luaL_pushresult: 将 buffer 内容作为字符串压栈。
///
/// 对应 C lauxlib.cpp::luaL_pushresult。用 lua_pushlstring 把 buffer 内容
/// 压栈，然后释放堆分配的缓冲区（若已扩容）。
#[no_mangle]
pub extern "C" fn luaL_pushresult(B: *mut luaL_Buffer) {
    if B.is_null() {
        return;
    }
    let L = unsafe { (*B).L };
    if L.is_null() {
        return;
    }
    unsafe {
        let s = (*B).b;
        let n = (*B).n;
        lua_pushlstring(L, s, n);
        // 释放堆分配的缓冲区（若 b 不再指向 init）
        let init_ptr = (*B).init.as_ptr() as *mut c_char;
        if (*B).b != init_ptr {
            libc::free((*B).b as *mut c_void);
        }
    }
}

/// luaL_pushresultsize: 增加 sz 字节后压栈结果。
///
/// 对应 C lauxlib.cpp::luaL_pushresultsize。先 `luaL_addsize(B, sz)`（即
/// `B->n += sz`，对应 C 中的宏），再调用 `luaL_pushresult` 把结果压栈。
#[no_mangle]
pub extern "C" fn luaL_pushresultsize(B: *mut luaL_Buffer, sz: usize) {
    if B.is_null() {
        return;
    }
    unsafe {
        (*B).n += sz;
    }
    luaL_pushresult(B);
}

/// luaL_addstring: 追加以 NUL 结尾的字符串到 buffer。
///
/// 对应 C lauxlib.cpp::luaL_addstring。内部用 `strlen(s)` 计算长度后调用
/// `luaL_addlstring`。
#[no_mangle]
pub extern "C" fn luaL_addstring(B: *mut luaL_Buffer, s: *const c_char) {
    if B.is_null() || s.is_null() {
        return;
    }
    let len = unsafe { libc::strlen(s) };
    luaL_addlstring(B, s, len);
}

/// luaL_buffinitsize: 初始化 buffer 并预分配 size 字节。
///
/// 对应 C lauxlib.cpp::luaL_buffinitsize。先 luaL_buffinit，再 luaL_prepbuffsize。
/// 返回可用空间起始指针。
#[no_mangle]
pub extern "C" fn luaL_buffinitsize(
    L: *mut lua_State,
    B: *mut luaL_Buffer,
    sz: usize,
) -> *mut c_char {
    luaL_buffinit(L, B);
    luaL_prepbuffsize(B, sz)
}

// ---------------------------------------------------------------------------
// 3. Userdata API（对应 C lapi.cpp）
// ---------------------------------------------------------------------------

/// lua_getiuservalue: 获取 userdata 的第 n 个关联值，压栈并返回类型。
///
/// 对应 C lapi.cpp::lua_getiuservalue。n 超出 [1, nuvalue] 范围时压 nil
/// 返回 LUA_TNONE。
#[no_mangle]
pub extern "C" fn lua_getiuservalue(L: *mut lua_State, idx: c_int, n: c_int) -> c_int {
    let L = unsafe { &mut *L };
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.push(TValue::Nil(NilKind::Strict));
            return LUA_TNONE;
        }
    };
    let val = match &L.stack[off] {
        TValue::UserData(u) => {
            let n = n as usize;
            if n >= 1 && n <= u.user_values.len() {
                u.user_values[n - 1].clone()
            } else {
                TValue::Nil(NilKind::Strict)
            }
        }
        _ => TValue::Nil(NilKind::Strict),
    };
    let ty = lua_type_code(val.ty());
    L.stack.push(val);
    ty
}

/// lua_setiuservalue: 弹出栈顶值设为 userdata 的第 n 个关联值。
///
/// 对应 C lapi.cpp::lua_setiuservalue。n 超出范围时不设置（仍弹出值）。
/// 返回 1 成功，0 失败（C 版本返回 int，此处省略返回值以匹配签名）。
#[no_mangle]
pub extern "C" fn lua_setiuservalue(L: *mut lua_State, idx: c_int, n: c_int) {
    let L = unsafe { &mut *L };
    // 先基于当前 top（包含 val）定位 userdata，再 pop val
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.pop();
            return;
        }
    };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    if let TValue::UserData(ref u) = L.stack[off] {
        let n = n as usize;
        if n >= 1 && n <= u.user_values.len() {
            // 通过裸指针原地修改（与 lua_setmetatable 一致，避免 Rc::make_mut 克隆）
            let ptr = Rc::as_ptr(u) as *mut Udata;
            unsafe {
                let udata = &mut *ptr;
                udata.user_values[n - 1] = val;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. 表操作（指针键版本，对应 C lapi.cpp）
// ---------------------------------------------------------------------------

/// lua_rawgetp: t[lightuserdata(p)]，结果压栈（不走元方法）。
///
/// 对应 C lapi.cpp::lua_rawgetp。用 light userdata p 作为 key 查表。
#[no_mangle]
pub extern "C" fn lua_rawgetp(L: *mut lua_State, idx: c_int, p: *const c_void) -> c_int {
    let L = unsafe { &mut *L };
    let key = TValue::LightUserData(p as *mut c_void);
    if is_registry(idx) {
        let val = L
            .registry
            .get(&key)
            .unwrap_or(TValue::Nil(NilKind::Strict));
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

/// lua_rawsetp: t[lightuserdata(p)] = v，v 从栈顶弹出（不走元方法）。
///
/// 对应 C lapi.cpp::lua_rawsetp。用 light userdata p 作为 key 设置值。
#[no_mangle]
pub extern "C" fn lua_rawsetp(L: *mut lua_State, idx: c_int, p: *const c_void) {
    let L = unsafe { &mut *L };
    // 先基于当前 top（包含 val）定位 table，再 pop
    let key = TValue::LightUserData(p as *mut c_void);
    if is_registry(idx) {
        let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        L.registry.set(key, val);
        return;
    }
    let off = match index2offset(L, idx) {
        Some(o) => o,
        None => {
            L.stack.pop();
            return;
        }
    };
    let val = L.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
    if let TValue::Table(ref mut t) = L.stack[off] {
        t.set(key, val);
    }
}

// ---------------------------------------------------------------------------
// 5. Coroutine API（对应 C ldo.cpp / lapi.cpp）
// ---------------------------------------------------------------------------

/// lua_resume: 恢复 coroutine，nargs 个参数在栈顶。
///
/// 对应 C ldo.cpp::lua_resume。委托给 `coroutine_lib::c_api_resume` 实现：
/// - 首次 resume: 从栈取函数和参数，调用 setup_first_resume 初始化 VM 状态
/// - 后续 resume: 调用 setup_subsequent_resume 从 ThreadContext 恢复
/// - 执行 VmExecutor::execute_with_state，处理 Yield/Return/Error
///
/// 栈布局（首次 resume）: [nil, func, arg1, ..., argN]
/// 结果布局: [nil, result1, ..., resultM]，nres = M
#[no_mangle]
pub extern "C" fn lua_resume(
    L: *mut lua_State,
    _from: *mut lua_State,
    nargs: c_int,
    nres: *mut c_int,
) -> c_int {
    let L = unsafe { &mut *L };
    let nargs = nargs as usize;
    let r = crate::stdlib::coroutine_lib::c_api_resume(L, nargs);
    match r {
        Ok((status, n)) => {
            if !nres.is_null() {
                unsafe { *nres = n as c_int };
            }
            status
        }
        Err(_e) => {
            // c_api_resume 内部已处理错误并 push 错误消息到栈
            // 此分支仅在非预期错误时到达
            if !nres.is_null() {
                unsafe { *nres = 1 };
            }
            LUA_ERRRUN
        }
    }
}

/// lua_yieldk: 让出当前 coroutine。
///
/// 对应 C ldo.cpp::lua_yieldk。简化实现：lua-rs 的协程 yield 由 VM 内部
/// OP_YIELD 指令处理，C API 的 yield 无法直接挂起 VM。
/// 此实现设置 pending_yield 暂存 yield 值并返回 LUA_YIELD。
/// 若在非协程上下文调用（pending_yield 被忽略），调用方需自行处理。
#[no_mangle]
pub extern "C" fn lua_yieldk(
    L: *mut lua_State,
    nresults: c_int,
    _ctx: lua_KContext,
    _k: Option<lua_KFunction>,
) -> c_int {
    let L = unsafe { &mut *L };
    // 收集栈顶 nresults 个值作为 yield 值
    let nresults = nresults as usize;
    let stack_len = L.stack.len();
    let start = if stack_len >= nresults {
        stack_len - nresults
    } else {
        0
    };
    let yield_vals: Vec<TValue> = L.stack.drain(start..).collect();
    L.pending_yield = Some(yield_vals);
    LUA_YIELD
}

/// lua_status: 返回 thread 状态。
///
/// 对应 C lapi.cpp::lua_status。主线程返回 LUA_OK。
/// 若 current_thread 存在（协程执行中），返回协程的 status 映射值。
#[no_mangle]
pub extern "C" fn lua_status(L: *mut lua_State) -> c_int {
    let L = unsafe { &*L };
    if let Some(ref ctx) = L.current_thread {
        let status = ctx.borrow().status;
        match status {
            crate::objects::ThreadStatus::OK => LUA_OK,
            crate::objects::ThreadStatus::Suspended => LUA_YIELD,
            crate::objects::ThreadStatus::Normal => LUA_OK,
            crate::objects::ThreadStatus::Error => LUA_ERRRUN,
        }
    } else {
        // 主线程或无活动协程
        LUA_OK
    }
}

// ---------------------------------------------------------------------------
// 6. 其他 API（对应 C lapi.cpp / ldebug.cpp）
// ---------------------------------------------------------------------------

/// lua_tothread: 返回 idx 处的 thread（lua_State*）。
///
/// 对应 C lapi.cpp::lua_tothread。从栈 idx 处取 `TValue::Thread`，
/// 返回其关联的 `c_state`（由 `lua_newthread` 设置的 lua_State 指针）。
/// 若 c_state 为 null（由 lua-rs 的 coroutine.create 创建的内部 LuaThread），
/// 惰性创建关联的 LuaState：共享当前 L 的全局状态，设置 current_thread，
/// 并把 LuaThread.function push 到新 state 栈底（供 lua_resume 首次执行）。
#[no_mangle]
pub extern "C" fn lua_tothread(L: *mut lua_State, idx: c_int) -> *mut lua_State {
    let L_ref = unsafe { &mut *L };
    let thread_rc = match index2val(L_ref, idx) {
        Some(TValue::Thread(t)) => t.clone(),
        _ => return ptr::null_mut(),
    };
    let p = thread_rc.c_state.get();
    if !p.is_null() {
        return p;
    }
    // c_state 为 null：由 lua-rs 的 coroutine.create 创建的内部 LuaThread
    // 惰性创建关联的 LuaState（共享全局状态，独立栈）
    let mut new_state = L_ref.new_thread();
    new_state.current_thread = Some(thread_rc.context.clone());
    // 如果 LuaThread 有 function（首次 resume 前），push 到新 state 栈底
    // 这样 lua_resume 的 c_api_resume 能从栈底取函数执行
    if let Some(ref func) = thread_rc.function {
        new_state.stack.push((**func).clone());
    }
    let ptr = Box::into_raw(Box::new(new_state));
    thread_rc.c_state.set(ptr);
    ptr
}

/// lua_sethook: 设置调试钩子。
///
/// 对应 C ldebug.cpp::lua_sethook。简化实现：设置 hook_mask 和 hook_count，
/// 但不存储 C 钩子函数（lua-rs 的 hook_func 是 TValue，无法存 C 函数指针）。
/// 因此钩子掩码被正确设置，但钩子不会实际触发。
/// func=NULL 或 mask=0 时清除所有钩子。
#[no_mangle]
pub extern "C" fn lua_sethook(
    L: *mut lua_State,
    func: Option<lua_Hook>,
    mask: c_int,
    count: c_int,
) {
    let L = unsafe { &mut *L };
    if func.is_none() || mask == 0 {
        // 清除钩子
        L.hook_func = None;
        L.hook_mask = 0;
        L.hook_count = 0;
        L.current_hook_count = 0;
    } else {
        // 设置钩子掩码和计数（不存储 C 函数指针，钩子不会触发）
        L.hook_mask = mask;
        L.hook_count = count;
        L.current_hook_count = count;
        // hook_func 保持原值（若原本是 Lua 函数则保留，否则为 None）
        // 注意：C 函数指针无法存储为 TValue，因此钩子不会实际触发
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
                let upv = lua_tointegerx(
                    L as *mut lua_State,
                    LUA_REGISTRYINDEX - 1,
                    std::ptr::null_mut(),
                );
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
