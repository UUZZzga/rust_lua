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
pub const OS_CLOCK: usize = 601;
pub const OS_TMPNAME: usize = 602;
pub const OS_REMOVE: usize = 603;

/// OS 库标签范围: [600, 610)
pub fn is_os_tag(tag: usize) -> bool {
    (600..610).contains(&tag)
}

/// 将 os 库函数 tag 映射到函数名（用于 traceback）
pub fn os_function_name(tag: usize) -> Option<&'static str> {
    match tag {
        OS_SETLOCALE => Some("setlocale"),
        OS_CLOCK => Some("clock"),
        OS_TMPNAME => Some("tmpname"),
        OS_REMOVE => Some("remove"),
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
        OS_CLOCK => call_clock(state, a, nresults),
        OS_TMPNAME => call_tmpname(state, a, nresults),
        OS_REMOVE => call_remove(state, a, nargs, nresults),
        _ => Ok(()),
    }
}

/// os.clock() — 返回程序使用的 CPU 时间（秒）
/// 对应 C: lua_pushnumber(L, ((lua_Number)clock())/(lua_Number)CLOCKS_PER_SEC);
extern "C" {
    fn clock() -> isize;
}
const CLOCKS_PER_SEC: f64 = 1_000_000.0;

fn call_clock(state: &mut LuaState, a: usize, nresults: i32) -> Result<(), VmError> {
    let ticks = unsafe { clock() };
    let seconds = ticks as f64 / CLOCKS_PER_SEC;
    push_single_result(state, a, nresults, TValue::Float(seconds));
    Ok(())
}

// ============================================================================
// os.tmpname 实现 (对应 C 的 os_tmpname)
// ============================================================================

/// os.tmpname() — 返回一个临时文件名
///
/// 对应 C 的 os_tmpname (POSIX 路径)：使用 mkstemp 创建临时文件，
/// 关闭后返回文件名。模板为 "/tmp/lua_XXXXXX"。
fn call_tmpname(state: &mut LuaState, a: usize, nresults: i32) -> Result<(), VmError> {
    let mut buf: [u8; 32] = *b"/tmp/lua_XXXXXX\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
    let ptr = buf.as_mut_ptr() as *mut libc::c_char;
    let fd = unsafe { libc::mkstemp(ptr) };
    if fd == -1 {
        return Err(VmError::RuntimeError(
            "unable to generate a unique filename".to_string(),
        ));
    }
    unsafe { libc::close(fd); }
    let cstr = unsafe { CStr::from_ptr(ptr) };
    let s = cstr.to_str().unwrap_or("").to_string();
    push_single_result(state, a, nresults, TValue::Str(state.intern_str(&s)));
    Ok(())
}

// ============================================================================
// os.remove 实现 (对应 C 的 os_remove)
// ============================================================================

/// os.remove(filename) — 删除文件
///
/// 对应 C 的 os_remove + luaL_fileresult：
/// 成功返回 true；失败返回 nil, error message, errno。
fn call_remove(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let filename_val = if nargs > 0 { get_arg(state, a, 0) } else { TValue::Nil(NilKind::Strict) };
    let filename_cstr = match &filename_val {
        TValue::Str(s) => {
            let bytes = s.as_str().as_bytes();
            if bytes.contains(&0) {
                return Err(VmError::RuntimeError(
                    "bad argument #1 to 'remove' (string contains embedded zeros)".to_string(),
                ));
            }
            CString::new(bytes).unwrap_or_else(|_| CString::new("").unwrap())
        }
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #1 to 'remove' (string expected)".to_string(),
            ));
        }
    };

    let result = unsafe { libc::remove(filename_cstr.as_ptr()) };
    if result == 0 {
        push_single_result(state, a, nresults, TValue::Boolean(true));
    } else {
        let err = std::io::Error::last_os_error();
        let fname = filename_cstr.to_str().unwrap_or("");
        let msg = format!("{}: {}", fname, err);
        let errno = err.raw_os_error().unwrap_or(0);
        state.adjust_results(a, nresults, vec![
            TValue::Nil(NilKind::Strict),
            TValue::Str(state.intern_str(&msg)),
            TValue::Integer(errno as i64),
        ]);
    }
    Ok(())
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
    register(&mut lib, "clock", OS_CLOCK);
    register(&mut lib, "tmpname", OS_TMPNAME);
    register(&mut lib, "remove", OS_REMOVE);

    let key = TValue::Str(state.intern_str("os"));
    state.globals.set(key, TValue::Table(lib));
}
