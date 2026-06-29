//! OS 库 (loslib.cpp → Rust)
//!
//! 对应 C 源码: loslib.cpp
//!
//! ## 主要功能
//! - 注册 os 全局表，包含操作系统相关函数
//! - 当前实现: os.setlocale
//!
//! ## 标签分配
//! - 标签 600+: OS 库

use crate::objects::{NilKind, TValue};
use crate::state::LuaState;
use crate::execute::VmError;
use crate::strings::LuaString;
use std::ffi::{CStr, CString};

// ============================================================================
// 函数标签 (LightUserData 占位符值)
// ============================================================================

pub const OS_SETLOCALE: usize = 600;

/// OS 库标签范围: [600, 610)
pub fn is_os_tag(tag: usize) -> bool {
    (600..610).contains(&tag)
}

/// 将 os 库函数 tag 映射到函数名（用于 traceback）
pub fn os_function_name(tag: usize) -> Option<&'static str> {
    match tag {
        OS_SETLOCALE => Some("setlocale"),
        _ => None,
    }
}

// ============================================================================
// 栈操作辅助函数
// ============================================================================

fn get_arg(state: &LuaState, a: usize, idx: usize) -> TValue {
    let stack_idx = a + 1 + idx;
    if stack_idx >= state.stack.len() {
        return TValue::Nil(NilKind::Strict);
    }
    state.stack[stack_idx].clone()
}

fn push_single_result(state: &mut LuaState, a: usize, nresults: i32, result: TValue) {
    state.adjust_results(a, nresults, vec![result]);
}

// ============================================================================
// os.setlocale 实现 (对应 C 的 os_setlocale)
// ============================================================================

/// os.setlocale([locale [, category]]) — 设置或查询区域设置
///
/// 对应 C 的 os_setlocale:
/// ```c
/// static int os_setlocale (lua_State *L) {
///   static const int cat[] = {LC_ALL, LC_COLLATE, LC_CTYPE, LC_MONETARY,
///                            LC_NUMERIC, LC_TIME};
///   static const char *const catnames[] = {"all", "collate", "ctype", "monetary",
///      "numeric", "time", NULL};
///   const char *l = luaL_optstring(L, 1, NULL);
///   int op = luaL_checkoption(L, 2, "all", catnames);
///   lua_pushstring(L, setlocale(cat[op], l));
///   return 1;
/// }
/// ```
fn call_setlocale(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 获取 locale 参数 (可选, 默认 NULL = 查询)
    let locale_val = if nargs > 0 { get_arg(state, a, 0) } else { TValue::Nil(NilKind::Strict) };
    let locale_cstr = match &locale_val {
        TValue::Str(s) => {
            let bytes = s.as_str().as_bytes();
            // 确保字符串不包含内部 \0
            if bytes.contains(&0) {
                return Err(VmError::RuntimeError(
                    "bad argument #1 to 'setlocale' (string contains embedded zeros)".to_string(),
                ));
            }
            Some(CString::new(bytes).unwrap_or_else(|_| CString::new("").unwrap()))
        }
        TValue::Nil(_) => None, // NULL: 查询当前 locale
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #1 to 'setlocale' (string expected)".to_string(),
            ));
        }
    };

    // 获取 category 参数 (可选, 默认 "all")
    let category_val = if nargs > 1 { get_arg(state, a, 1) } else { TValue::Nil(NilKind::Strict) };
    let cat = match &category_val {
        TValue::Str(s) => {
            match s.as_str() {
                "all" => libc::LC_ALL,
                "collate" => libc::LC_COLLATE,
                "ctype" => libc::LC_CTYPE,
                "monetary" => libc::LC_MONETARY,
                "numeric" => libc::LC_NUMERIC,
                "time" => libc::LC_TIME,
                other => {
                    return Err(VmError::RuntimeError(format!(
                        "bad argument #2 to 'setlocale' (invalid option '{}')", other
                    )));
                }
            }
        }
        TValue::Nil(_) => libc::LC_ALL, // 默认 "all"
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #2 to 'setlocale' (string expected)".to_string(),
            ));
        }
    };

    // 调用 C 的 setlocale
    let locale_ptr = locale_cstr.as_ref().map(|s| s.as_ptr()).unwrap_or(std::ptr::null());
    let result = unsafe { libc::setlocale(cat, locale_ptr) };

    if result.is_null() {
        // 设置失败, 返回 nil
        push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
    } else {
        // 返回 locale 字符串
        let result_str = unsafe { CStr::from_ptr(result) }
            .to_str()
            .unwrap_or("")
            .to_string();
        push_single_result(state, a, nresults, TValue::Str(state.intern_str(&result_str)));
    }
    Ok(())
}

// ============================================================================
// 派发函数
// ============================================================================

/// 派发 OS 库函数调用
pub fn call_os_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    match tag {
        OS_SETLOCALE => call_setlocale(state, a, nargs, nresults),
        _ => Ok(()),
    }
}

// ============================================================================
// 打开 OS 库 — 对应 C 的 luaopen_os
// ============================================================================

/// 打开 OS 库并注册到全局变量 os
pub fn open_os_lib(state: &mut LuaState) {
    let mut lib = crate::table::Table::new();

    let register = |lib: &mut crate::table::Table, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        lib.set(key, TValue::LightUserData(tag as *mut std::ffi::c_void));
    };

    register(&mut lib, "setlocale", OS_SETLOCALE);

    let key = TValue::Str(state.intern_str("os"));
    state.globals.set(key, TValue::Table(lib));
}
