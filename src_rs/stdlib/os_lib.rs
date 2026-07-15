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

use crate::execute::VmError;
use crate::objects::{NilKind, TValue};
use crate::state::LuaState;
use crate::strings::LuaString;
use std::ffi::{CStr, CString};

// ============================================================================
// 函数标签 (LightUserData 占位符值)
// ============================================================================

pub const OS_SETLOCALE: usize = 600;
pub const OS_CLOCK: usize = 601;
pub const OS_TMPNAME: usize = 602;
pub const OS_REMOVE: usize = 603;
pub const OS_GETENV: usize = 604;
pub const OS_RENAME: usize = 605;
pub const OS_EXECUTE: usize = 606;
pub const OS_EXIT: usize = 607;
pub const OS_DATE: usize = 608;
pub const OS_TIME: usize = 609;
pub const OS_DIFFTIME: usize = 610;

/// OS 库标签范围: [600, 611)
pub fn is_os_tag(tag: usize) -> bool {
    (600..611).contains(&tag)
}

/// 将 os 库函数 tag 映射到函数名（用于 traceback）
pub fn os_function_name(tag: usize) -> Option<&'static str> {
    match tag {
        OS_SETLOCALE => Some("setlocale"),
        OS_CLOCK => Some("clock"),
        OS_TMPNAME => Some("tmpname"),
        OS_REMOVE => Some("remove"),
        OS_GETENV => Some("getenv"),
        OS_RENAME => Some("rename"),
        OS_EXECUTE => Some("execute"),
        OS_EXIT => Some("exit"),
        OS_DATE => Some("date"),
        OS_TIME => Some("time"),
        OS_DIFFTIME => Some("difftime"),
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
    let locale_val = if nargs > 0 {
        get_arg(state, a, 0)
    } else {
        TValue::Nil(NilKind::Strict)
    };
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
    let category_val = if nargs > 1 {
        get_arg(state, a, 1)
    } else {
        TValue::Nil(NilKind::Strict)
    };
    let cat = match &category_val {
        TValue::Str(s) => match s.as_str() {
            "all" => libc::LC_ALL,
            "collate" => libc::LC_COLLATE,
            "ctype" => libc::LC_CTYPE,
            "monetary" => libc::LC_MONETARY,
            "numeric" => libc::LC_NUMERIC,
            "time" => libc::LC_TIME,
            other => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #2 to 'setlocale' (invalid option '{}')",
                    other
                )));
            }
        },
        TValue::Nil(_) => libc::LC_ALL, // 默认 "all"
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #2 to 'setlocale' (string expected)".to_string(),
            ));
        }
    };

    // 调用 C 的 setlocale
    let locale_ptr = locale_cstr
        .as_ref()
        .map(|s| s.as_ptr())
        .unwrap_or(std::ptr::null());
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
        push_single_result(
            state,
            a,
            nresults,
            TValue::Str(state.intern_str(&result_str)),
        );
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
        OS_GETENV => call_getenv(state, a, nargs, nresults),
        OS_RENAME => call_rename(state, a, nargs, nresults),
        OS_EXECUTE => call_os_execute(state, a, nargs, nresults),
        OS_EXIT => call_os_exit(state, a, nargs, nresults),
        OS_DATE => call_os_date(state, a, nargs, nresults),
        OS_TIME => call_os_time(state, a, nargs, nresults),
        OS_DIFFTIME => call_os_difftime(state, a, nargs, nresults),
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
    unsafe {
        libc::close(fd);
    }
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
fn call_remove(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let filename_val = if nargs > 0 {
        get_arg(state, a, 0)
    } else {
        TValue::Nil(NilKind::Strict)
    };
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
        state.adjust_results(
            a,
            nresults,
            vec![
                TValue::Nil(NilKind::Strict),
                TValue::Str(state.intern_str(&msg)),
                TValue::Integer(errno as i64),
            ],
        );
    }
    Ok(())
}

// ============================================================================
// os.getenv 实现 (对应 C 的 os_getenv)
// ============================================================================

/// os.getenv(varname) — 获取环境变量值
///
/// 对应 C 的 os_getenv:
/// ```c
/// static int os_getenv (lua_State *L) {
///   lua_pushstring(L, getenv(luaL_checkstring(L, 1)));  /* if NULL push nil */
///   return 1;
/// }
/// ```
/// 环境变量不存在时返回 nil。
fn call_getenv(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let name_val = if nargs > 0 {
        get_arg(state, a, 0)
    } else {
        TValue::Nil(NilKind::Strict)
    };
    let name_cstr = match &name_val {
        TValue::Str(s) => {
            let bytes = s.as_str().as_bytes();
            if bytes.contains(&0) {
                return Err(VmError::RuntimeError(
                    "bad argument #1 to 'getenv' (string contains embedded zeros)".to_string(),
                ));
            }
            CString::new(bytes).unwrap_or_else(|_| CString::new("").unwrap())
        }
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #1 to 'getenv' (string expected)".to_string(),
            ));
        }
    };

    let result = unsafe { libc::getenv(name_cstr.as_ptr()) };
    if result.is_null() {
        push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
    } else {
        let result_str = unsafe { CStr::from_ptr(result) }
            .to_str()
            .unwrap_or("")
            .to_string();
        push_single_result(
            state,
            a,
            nresults,
            TValue::Str(state.intern_str(&result_str)),
        );
    }
    Ok(())
}

// ============================================================================
// os.rename 实现 (对应 C 的 os_rename)
// ============================================================================

/// os.rename(oldname, newname) — 重命名文件
///
/// 对应 C 的 os_rename + luaL_fileresult：
/// 成功返回 true；失败返回 nil, error message, errno。
fn call_rename(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let oldname_val = if nargs > 0 {
        get_arg(state, a, 0)
    } else {
        TValue::Nil(NilKind::Strict)
    };
    let oldname_cstr = match &oldname_val {
        TValue::Str(s) => {
            let bytes = s.as_str().as_bytes();
            if bytes.contains(&0) {
                return Err(VmError::RuntimeError(
                    "bad argument #1 to 'rename' (string contains embedded zeros)".to_string(),
                ));
            }
            CString::new(bytes).unwrap_or_else(|_| CString::new("").unwrap())
        }
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #1 to 'rename' (string expected)".to_string(),
            ));
        }
    };

    let newname_val = if nargs > 1 {
        get_arg(state, a, 1)
    } else {
        TValue::Nil(NilKind::Strict)
    };
    let newname_cstr = match &newname_val {
        TValue::Str(s) => {
            let bytes = s.as_str().as_bytes();
            if bytes.contains(&0) {
                return Err(VmError::RuntimeError(
                    "bad argument #2 to 'rename' (string contains embedded zeros)".to_string(),
                ));
            }
            CString::new(bytes).unwrap_or_else(|_| CString::new("").unwrap())
        }
        _ => {
            return Err(VmError::RuntimeError(
                "bad argument #2 to 'rename' (string expected)".to_string(),
            ));
        }
    };

    let result = unsafe { libc::rename(oldname_cstr.as_ptr(), newname_cstr.as_ptr()) };
    if result == 0 {
        push_single_result(state, a, nresults, TValue::Boolean(true));
    } else {
        let err = std::io::Error::last_os_error();
        let msg = err.to_string();
        let errno = err.raw_os_error().unwrap_or(0);
        state.adjust_results(
            a,
            nresults,
            vec![
                TValue::Nil(NilKind::Strict),
                TValue::Str(state.intern_str(&msg)),
                TValue::Integer(errno as i64),
            ],
        );
    }
    Ok(())
}

// ============================================================================
// os.execute 实现 (对应 C 的 os_execute)
// ============================================================================

/// os.execute([command]) — 执行系统命令
///
/// 对应 C 的 os_execute:
/// - 有 command 参数: 用 system() 执行, 返回 execresult (true/nil, "exit"/"signal", code)
/// - 无 command 参数: 返回 boolean 表示 shell 是否可用
fn call_os_execute(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let cmd_val = if nargs > 0 {
        get_arg(state, a, 0)
    } else {
        TValue::Nil(NilKind::Strict)
    };
    let cmd = match &cmd_val {
        TValue::Str(s) => Some(s.as_str().to_string()),
        TValue::Nil(_) => None,
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'execute' (string expected, got {})",
                crate::tm::obj_type_name(&cmd_val)
            )));
        }
    };

    unsafe {
        *libc::__errno_location() = 0;
    }
    let c_cmd = cmd.as_ref().and_then(|s| CString::new(s.clone()).ok());
    let stat = unsafe { libc::system(c_cmd.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())) };

    let mut results = Vec::new();
    if cmd.is_some() {
        crate::stdlib::io_lib::exec_result(state, &mut results, stat);
    } else {
        // 无 command: 返回 boolean 表示 shell 是否可用
        results.push(TValue::Boolean(stat != 0));
    }
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// os.exit 实现 (对应 C 的 os_exit)
// ============================================================================

/// os.exit([code [, close]]) — 退出程序
///
/// 对应 C 的 os_exit:
/// - code 是 boolean: true → 0 (SUCCESS), false → 1 (FAILURE)
/// - code 是 number: 用作退出码 (默认 0)
/// - close 为 true: 调用 close_state 触发 finalizer，然后 exit
fn call_os_exit(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    _nresults: i32,
) -> Result<(), VmError> {
    let status = if nargs >= 1 {
        let v = get_arg(state, a, 0);
        match &v {
            TValue::Boolean(b) => {
                if *b {
                    0
                } else {
                    1
                }
            }
            TValue::Integer(n) => *n as i32,
            TValue::Nil(_) => 0,
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #1 to 'exit' (number or boolean expected, got {})",
                    crate::tm::obj_type_name(&v)
                )));
            }
        }
    } else {
        0
    };
    let close = nargs >= 2 && {
        let v = get_arg(state, a, 1);
        matches!(&v, TValue::Boolean(true))
    };
    if close {
        if state.gc_closing {
            // 已在 close_state 中（finalizer 调用 os.exit）：设置退出请求，
            // close_state 处理完剩余 finalizer 后据此退出
            state.exit_requested = Some(status);
            // 用 VmError 中断当前 finalizer 的 pcall，让 close_state 继续处理后续对象
            return Err(VmError::RuntimeError("__exit_requested__".to_string()));
        } else {
            // 普通脚本中调用 os.exit(code, true)：触发 close_state，内部会 exit
            state.close_state();
            std::process::exit(status);
        }
    } else {
        std::process::exit(status);
    }
}

// ============================================================================
// os.date / os.time / os.difftime 实现 (对应 C 的 os_date / os_time / os_difftime)
// ============================================================================

/// 从表读取整数字段 — 对应 C 的 getfield
///
/// key: 字段名, d: 默认值 (< 0 表示必须存在), delta: 偏移量
/// 接受 Integer 或可无损转换的 Float (对应 C 的 lua_tointegerx)
fn get_field(
    table: &crate::table::Table,
    state: &LuaState,
    key: &str,
    d: i32,
    delta: i32,
) -> Result<i32, VmError> {
    let k = TValue::Str(state.intern_str(key));
    match table.get(&k) {
        Some(TValue::Nil(_)) | None => {
            // 字段不存在或为 nil: 用默认值
            if d < 0 {
                Err(VmError::RuntimeError(format!(
                    "field '{}' missing in date table",
                    key
                )))
            } else {
                Ok(d)
            }
        }
        Some(ref v) => {
            // 尝试转换为整数 (Integer 直接用, Float 无损转换) — 对应 C 的 lua_tointegerx
            match crate::vm::to_integer_ns(v, crate::vm::F2IMode::Eq) {
                Some(res) => {
                    // 越界检查 — 对应 C: if (!(res >= 0 ? res - delta <= INT_MAX : INT_MIN + delta <= res))
                    let in_bounds = if res >= 0 {
                        res - delta as i64 <= i32::MAX as i64
                    } else {
                        i32::MIN as i64 + delta as i64 <= res
                    };
                    if !in_bounds {
                        return Err(VmError::RuntimeError(format!(
                            "field '{}' is out-of-bound",
                            key
                        )));
                    }
                    Ok((res - delta as i64) as i32)
                }
                None => {
                    // 非数字或不可转换的 Float
                    Err(VmError::RuntimeError(format!(
                        "field '{}' is not an integer",
                        key
                    )))
                }
            }
        }
    }
}

/// 设置表整数字段 — 对应 C 的 setfield
fn set_field(table: &crate::table::Table, state: &LuaState, key: &str, value: i32, delta: i32) {
    let k = TValue::Str(state.intern_str(key));
    table.set(k, TValue::Integer((value as i64) + delta as i64));
}

/// 设置表布尔字段 — 对应 C 的 setboolfield (value < 0 时不设置)
fn set_boolfield(table: &crate::table::Table, state: &LuaState, key: &str, value: i32) {
    if value < 0 {
        return;
    }
    let k = TValue::Str(state.intern_str(key));
    table.set(k, TValue::Boolean(value != 0));
}

/// os.date([format [, time]]) — 格式化日期/时间
///
/// 对应 C 的 os_date:
/// - format 默认 "%c", 以 "!" 开头表示 UTC
/// - format == "*t" 返回时间表
/// - 其他用 strftime 格式化
fn call_os_date(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 获取格式字符串 (默认 "%c") — 保留原始字节 (可能含 \0)
    let fmt: Vec<u8> = if nargs >= 1 {
        let v = get_arg(state, a, 0);
        match &v {
            TValue::Str(s) => s.as_str().as_bytes().to_vec(),
            TValue::Nil(_) => b"%c".to_vec(),
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #1 to 'date' (string expected, got {})",
                    crate::tm::obj_type_name(&v)
                )));
            }
        }
    } else {
        b"%c".to_vec()
    };

    // 获取时间 (默认当前时间) — 对应 C 的 l_checktime (luaL_checkinteger, 接受可转换的 Float)
    let t: libc::time_t = if nargs >= 2 {
        let v = get_arg(state, a, 1);
        match &v {
            TValue::Nil(_) => unsafe { libc::time(std::ptr::null_mut()) },
            _ => match crate::vm::to_integer_ns(&v, crate::vm::F2IMode::Eq) {
                Some(n) => n as libc::time_t,
                None => {
                    return Err(VmError::RuntimeError(format!(
                        "bad argument #2 to 'date' (number has no integer representation, got {})",
                        crate::tm::obj_type_name(&v)
                    )));
                }
            },
        }
    } else {
        unsafe { libc::time(std::ptr::null_mut()) }
    };

    // 处理 "!" 前缀 (UTC)
    let (is_utc, fmt_start): (bool, usize) = if fmt.first() == Some(&b'!') {
        (true, 1)
    } else {
        (false, 0)
    };

    // 获取 tm 结构
    let mut tmr: libc::tm = unsafe { std::mem::zeroed() };
    let stm: *mut libc::tm = if is_utc {
        unsafe { libc::gmtime_r(&t, &mut tmr) }
    } else {
        unsafe { libc::localtime_r(&t, &mut tmr) }
    };
    if stm.is_null() {
        return Err(VmError::RuntimeError(
            "date result cannot be represented in this installation".to_string(),
        ));
    }

    // 检查是否是 "*t" 模式
    let fmt_rest = &fmt[fmt_start..];
    if fmt_rest == b"*t" {
        let table = crate::table::Table::new();
        set_field(&table, state, "year", tmr.tm_year, 1900);
        set_field(&table, state, "month", tmr.tm_mon, 1);
        set_field(&table, state, "day", tmr.tm_mday, 0);
        set_field(&table, state, "hour", tmr.tm_hour, 0);
        set_field(&table, state, "min", tmr.tm_min, 0);
        set_field(&table, state, "sec", tmr.tm_sec, 0);
        set_field(&table, state, "yday", tmr.tm_yday, 1);
        set_field(&table, state, "wday", tmr.tm_wday, 1);
        set_boolfield(&table, state, "isdst", tmr.tm_isdst);
        state.adjust_results(a, nresults, vec![TValue::Table(table)]);
        return Ok(());
    }

    // 格式化: 按字节遍历, 遇到 % 用 strftime 处理
    // 合法的 strftime 转换说明符 — 对应 C 的 LUA_STRFTIMEOPTIONS (POSIX)
    const SINGLE_OPTS: &[u8] = b"aAbBcCdDeFgGhHIjmMnprRStTuUVwWxXyYzZ%";
    const E_OPTS: &[u8] = b"cExXyY"; // %E 后可跟的字符
    const O_OPTS: &[u8] = b"dDeFgGmMVwWy"; // %O 后可跟的字符

    let mut result: Vec<u8> = Vec::new();
    let mut i = fmt_start;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            result.push(fmt[i]);
            i += 1;
        } else {
            i += 1; // 跳过 %
            if i >= fmt.len() {
                // % 在末尾: 非法转换说明符
                return Err(VmError::RuntimeError(
                    "invalid conversion specifier '%'".to_string(),
                ));
            }
            // 收集转换说明符 (支持可选的 E/O 修饰符 + 单字符)
            let mut spec = Vec::new();
            spec.push(b'%');
            let modifier = fmt[i] == b'E' || fmt[i] == b'O';
            if modifier {
                spec.push(fmt[i]);
                i += 1;
                if i >= fmt.len() {
                    // 修饰符在末尾: 非法
                    let spec_str = String::from_utf8_lossy(&spec);
                    return Err(VmError::RuntimeError(format!(
                        "invalid conversion specifier '{}'",
                        spec_str
                    )));
                }
            }
            spec.push(fmt[i]);
            i += 1;
            // 验证转换说明符是否合法 — 对应 C 的 checkoption
            let valid = if modifier {
                if spec[1] == b'E' {
                    E_OPTS.contains(&spec[2])
                } else {
                    O_OPTS.contains(&spec[2])
                }
            } else {
                SINGLE_OPTS.contains(&spec[1])
            };
            if !valid {
                let spec_str = String::from_utf8_lossy(&spec);
                return Err(VmError::RuntimeError(format!(
                    "invalid conversion specifier '{}'",
                    spec_str
                )));
            }
            // 用 strftime 格式化 (spec 是合法的 C 格式字符串, 不含 \0)
            let cc = std::ffi::CString::new(spec.clone())
                .map_err(|_| VmError::RuntimeError("invalid conversion specifier".to_string()))?;
            let mut buf = [0u8; 250];
            let reslen =
                unsafe { libc::strftime(buf.as_mut_ptr() as *mut i8, 250, cc.as_ptr(), &tmr) };
            result.extend_from_slice(&buf[..reslen]);
        }
    }

    // 返回结果 (可能包含 \0, 用 new_long_bytes 保留原始字节)
    let result_str = crate::strings::new_long_bytes(result);
    state.adjust_results(a, nresults, vec![TValue::Str(result_str)]);
    Ok(())
}

/// os.time([table]) — 获取时间
///
/// 对应 C 的 os_time:
/// - 无参数: 返回当前时间
/// - 表参数: 从表读取字段构造时间
fn call_os_time(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let result = if nargs < 1 || matches!(get_arg(state, a, 0), TValue::Nil(_)) {
        // 无参数: 返回当前时间
        let t = unsafe { libc::time(std::ptr::null_mut()) };
        if t == -1 as libc::time_t {
            return Err(VmError::RuntimeError(
                "time result cannot be represented in this installation".to_string(),
            ));
        }
        TValue::Integer(t as i64)
    } else {
        // 表参数: 从表读取字段
        let v = get_arg(state, a, 0);
        let table = match &v {
            TValue::Table(t) => t.clone(),
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #1 to 'time' (table expected, got {})",
                    crate::tm::obj_type_name(&v)
                )));
            }
        };
        let mut ts: libc::tm = unsafe { std::mem::zeroed() };
        ts.tm_year = get_field(&table, state, "year", -1, 1900)?;
        ts.tm_mon = get_field(&table, state, "month", -1, 1)?;
        ts.tm_mday = get_field(&table, state, "day", -1, 0)?;
        ts.tm_hour = get_field(&table, state, "hour", 12, 0)?;
        ts.tm_min = get_field(&table, state, "min", 0, 0)?;
        ts.tm_sec = get_field(&table, state, "sec", 0, 0)?;
        // isdst: -1 表示未知 (让 mktime 自动判断)
        let isdst_key = TValue::Str(state.intern_str("isdst"));
        ts.tm_isdst = match table.get(&isdst_key) {
            Some(TValue::Boolean(b)) => {
                if b {
                    1
                } else {
                    0
                }
            }
            _ => -1,
        };
        let t = unsafe { libc::mktime(&mut ts) };
        if t == -1 as libc::time_t {
            return Err(VmError::RuntimeError(
                "time result cannot be represented in this installation".to_string(),
            ));
        }
        // 更新表的字段为规范化值 — 对应 C 的 setallfields
        set_field(&table, state, "year", ts.tm_year, 1900);
        set_field(&table, state, "month", ts.tm_mon, 1);
        set_field(&table, state, "day", ts.tm_mday, 0);
        set_field(&table, state, "hour", ts.tm_hour, 0);
        set_field(&table, state, "min", ts.tm_min, 0);
        set_field(&table, state, "sec", ts.tm_sec, 0);
        set_field(&table, state, "yday", ts.tm_yday, 1);
        set_field(&table, state, "wday", ts.tm_wday, 1);
        set_boolfield(&table, state, "isdst", ts.tm_isdst);
        TValue::Integer(t as i64)
    };
    state.adjust_results(a, nresults, vec![result]);
    Ok(())
}

/// os.difftime(t2, t1) — 返回时间差
///
/// 对应 C 的 os_difftime: 返回 (lua_Number)difftime(t1, t2)
fn call_os_difftime(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 2 {
        return Err(VmError::RuntimeError(
            "bad argument to 'difftime' (two numbers expected)".to_string(),
        ));
    }
    let v1 = get_arg(state, a, 0);
    let t1 = match crate::vm::to_integer_ns(&v1, crate::vm::F2IMode::Eq) {
        Some(n) => n as libc::time_t,
        None => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'difftime' (number has no integer representation, got {})",
                crate::tm::obj_type_name(&v1)
            )))
        }
    };
    let v2 = get_arg(state, a, 1);
    let t2 = match crate::vm::to_integer_ns(&v2, crate::vm::F2IMode::Eq) {
        Some(n) => n as libc::time_t,
        None => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #2 to 'difftime' (number has no integer representation, got {})",
                crate::tm::obj_type_name(&v2)
            )))
        }
    };
    let diff = unsafe { libc::difftime(t1, t2) };
    state.adjust_results(a, nresults, vec![TValue::Float(diff)]);
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
    register(&mut lib, "getenv", OS_GETENV);
    register(&mut lib, "rename", OS_RENAME);
    register(&mut lib, "execute", OS_EXECUTE);
    register(&mut lib, "exit", OS_EXIT);
    register(&mut lib, "date", OS_DATE);
    register(&mut lib, "time", OS_TIME);
    register(&mut lib, "difftime", OS_DIFFTIME);

    let key = TValue::Str(state.intern_str("os"));
    state.globals.set(key, TValue::Table(lib));
}
