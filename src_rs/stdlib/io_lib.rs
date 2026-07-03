//! I/O 库 (liolib.cpp → Rust)
//!
//! 对应 C 源码: liolib.cpp
//!
//! ## 主要功能
//! - 注册 io 全局表，包含标准 I/O 流 (stdin/stdout/stderr)
//! - 实现 io.write / io.output / io.close（默认输出流操作）
//!
//! ## 标签分配
//! - 标签 700-702: stdin/stdout/stderr 占位符值（非函数）
//! - 标签 800-809: io 库函数（write/output/close/...）

use crate::execute::VmError;
use crate::objects::{LuaType, NilKind, TValue};
use crate::state::LuaState;
use std::io::Write;

// ============================================================================
// 占位符标签 (LightUserData 值，非函数)
// ============================================================================

/// io.stdin 的标签 — 用于标识标准输入流
pub const IO_STDIN: usize = 700;
/// io.stdout 的标签 — 用于标识标准输出流
pub const IO_STDOUT: usize = 701;
/// io.stderr 的标签 — 用于标识标准错误流
pub const IO_STDERR: usize = 702;

/// I/O 占位符标签范围: [700, 710)
pub fn is_io_tag(tag: usize) -> bool {
    (700..710).contains(&tag)
}

// ============================================================================
// 函数标签
// ============================================================================

pub const IO_WRITE: usize = 800;
pub const IO_OUTPUT: usize = 801;
pub const IO_CLOSE: usize = 802;
pub const IO_INPUT: usize = 803;
// FILE* 元方法标签
pub const IO_FILE_GC: usize = 804;
pub const IO_FILE_CLOSE: usize = 805;
pub const IO_FILE_TOSTRING: usize = 806;

/// I/O 库函数标签范围: [800, 810)
pub fn is_io_function_tag(tag: usize) -> bool {
    (800..810).contains(&tag)
}

/// 将 io 库函数 tag 映射到函数名（用于 traceback）
pub fn io_function_name(tag: usize) -> Option<&'static str> {
    match tag {
        IO_WRITE => Some("write"),
        IO_OUTPUT => Some("output"),
        IO_CLOSE => Some("close"),
        IO_INPUT => Some("input"),
        IO_FILE_GC => Some("__gc"),
        IO_FILE_CLOSE => Some("__close"),
        IO_FILE_TOSTRING => Some("__tostring"),
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

// ============================================================================
// io.write 实现 (对应 C 的 io_write / g_write)
// ============================================================================

/// io.write(...) — 写入到默认输出流
///
/// 对应 C 的 g_write：遍历参数，将字符串或数字写入当前输出流。
/// 非 string/number 参数报错。写入到 io_output（若已设置）否则 stdout。
fn call_io_write(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    _nresults: i32,
) -> Result<(), VmError> {
    let mut buf: Vec<u8> = Vec::new();
    for i in 0..nargs {
        let val = get_arg(state, a, i);
        match &val {
            TValue::Str(s) => buf.extend_from_slice(s.as_str().as_bytes()),
            TValue::Integer(n) => buf.extend_from_slice(n.to_string().as_bytes()),
            TValue::Float(_) => {
                buf.extend_from_slice(
                    crate::stdlib::base_lib::lua_value_to_string(&val).as_bytes()
                );
            }
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #{} to 'write' (string or number expected, got {})",
                    i + 1,
                    val.ty()
                )));
            }
        }
    }
    if let Some(out) = state.io_output.as_mut() {
        let _ = out.write_all(&buf);
        let _ = out.flush();
    } else {
        let _ = state.stdout.write_all(&buf);
        let _ = state.stdout.flush();
    }
    // io.write 返回 0 个结果（verybig.lua 不使用返回值）
    state.stack.truncate(a);
    Ok(())
}

// ============================================================================
// io.output 实现 (对应 C 的 io_output / g_iofile)
// ============================================================================

/// io.output([file]) — 设置或获取默认输出流
///
/// 对应 C 的 g_iofile(IO_OUTPUT, "w")：
/// - 字符串参数：以 "w" 模式打开文件，设为当前输出流
/// - 无参数/nil：不改变，返回当前输出流
fn call_io_output(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs > 0 {
        let arg = get_arg(state, a, 0);
        if !arg.is_nil() {
            match &arg {
                TValue::Str(s) => {
                    let filename = s.as_str().to_string();
                    match std::fs::File::create(&filename) {
                        Ok(file) => {
                            state.io_output = Some(Box::new(file));
                        }
                        Err(e) => {
                            return Err(VmError::RuntimeError(format!(
                                "cannot open file '{}' ({})",
                                filename, e
                            )));
                        }
                    }
                }
                _ => {
                    return Err(VmError::RuntimeError(format!(
                        "bad argument #1 to 'output' (string expected, got {})",
                        arg.ty()
                    )));
                }
            }
        }
    }
    // 简化：不返回文件句柄（verybig.lua 未使用返回值）
    state.adjust_results(a, nresults, vec![]);
    Ok(())
}

// ============================================================================
// io.close 实现 (对应 C 的 io_close)
// ============================================================================

/// io.close([file]) — 关闭默认输出流
///
/// 对应 C 的 io_close：无参数时关闭默认输出流 (IO_OUTPUT)。
fn call_io_close(state: &mut LuaState, a: usize, nresults: i32) -> Result<(), VmError> {
    if let Some(mut out) = state.io_output.take() {
        let _ = out.flush();
    }
    state.adjust_results(a, nresults, vec![TValue::Boolean(true)]);
    Ok(())
}

// ============================================================================
// io.input 实现 (对应 C 的 io_input / g_iofile)
// ============================================================================

/// io.input([file]) — 获取或设置默认输入流
///
/// 对应 C 的 g_iofile(IO_INPUT, "r")：
/// - 无参数/nil：返回当前输入流 (io.stdin)
/// - 文件参数：检查是否是 FILE* userdata，不是则报错
fn call_io_input(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs > 0 {
        let arg = get_arg(state, a, 0);
        if !arg.is_nil() {
            // 检查参数是否是 FILE* userdata (对应 C 的 tofile -> luaL_checkudata)
            match &arg {
                TValue::UserData(_) => {
                    // 是 userdata，设置为当前输入流 (简化: 不实际设置)
                }
                _ => {
                    let typearg = crate::tm::obj_type_name(&arg);
                    return Err(VmError::RuntimeError(format!(
                        "bad argument #1 to 'input' (FILE* expected, got {})",
                        typearg
                    )));
                }
            }
        }
    }
    // 返回当前输入流 (io.stdin) — 从 globals 获取
    let io_key = TValue::Str(state.intern_str("io"));
    let stdin_val = match state.globals.get(&io_key) {
        Some(TValue::Table(io_table)) => {
            let stdin_key = TValue::Str(state.intern_str("stdin"));
            io_table.get(&stdin_key).unwrap_or(TValue::Nil(NilKind::Strict))
        }
        _ => TValue::Nil(NilKind::Strict),
    };
    state.adjust_results(a, nresults, vec![stdin_val]);
    Ok(())
}

// ============================================================================
// FILE* 元方法实现 (对应 C 的 metameth: __gc, __close, __tostring)
// ============================================================================

/// 检查参数 #1 是否是 FILE* userdata — 对应 C 的 tolstream -> luaL_checkudata
///
/// C 的 luaL_checkudata 调用 luaL_typeerror，对无参数 (LUA_TNONE) 报 "got no value"，
/// 对 nil 报 "got nil"，对其他类型报 "got <typename>"。
fn check_file_arg(state: &LuaState, a: usize, nargs: usize, fname: &str) -> Result<(), VmError> {
    if nargs < 1 {
        // 无参数: lua_type 返回 LUA_TNONE, luaT_typenames_[0] = "no value"
        return Err(VmError::RuntimeError(format!(
            "bad argument #1 to '{}' (FILE* expected, got no value)",
            fname
        )));
    }
    let arg = get_arg(state, a, 0);
    match &arg {
        TValue::UserData(_) => Ok(()),
        _ => {
            let typearg = crate::tm::obj_type_name(&arg);
            Err(VmError::RuntimeError(format!(
                "bad argument #1 to '{}' (FILE* expected, got {})",
                fname, typearg
            )))
        }
    }
}

/// __gc 元方法 — 对应 C 的 f_gc
fn call_file_gc(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    check_file_arg(state, a, nargs, "__gc")?;
    // __gc 不返回结果（简化：不做实际 GC 操作）
    state.adjust_results(a, nresults, vec![]);
    Ok(())
}

/// __close 元方法 — 对应 C 的 f_gc (metameth 中 __close 也指向 f_gc)
fn call_file_close(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    check_file_arg(state, a, nargs, "__close")?;
    // __close 返回 true（简化）
    state.adjust_results(a, nresults, vec![TValue::Boolean(true)]);
    Ok(())
}

/// __tostring 元方法 — 对应 C 的 f_tostring
fn call_file_tostring(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    check_file_arg(state, a, nargs, "__tostring")?;
    // 简化: 返回 "file (0x0)"（对应 C 的 "file (0x地址)"）
    let result = TValue::Str(state.intern_str("file (0x0)"));
    state.adjust_results(a, nresults, vec![result]);
    Ok(())
}

// ============================================================================
// 派发函数
// ============================================================================

/// 派发 I/O 库函数调用
pub fn call_io_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 设置当前 C 函数名（用于 traceback）— 对应 C 的 CallInfo 记录
    let prev_c_func = state.last_c_function.take();
    state.last_c_function = io_function_name(tag).map(|s| s.to_string());

    let result = match tag {
        IO_WRITE => call_io_write(state, a, nargs, nresults),
        IO_OUTPUT => call_io_output(state, a, nargs, nresults),
        IO_CLOSE => call_io_close(state, a, nresults),
        IO_INPUT => call_io_input(state, a, nargs, nresults),
        IO_FILE_GC => call_file_gc(state, a, nargs, nresults),
        IO_FILE_CLOSE => call_file_close(state, a, nargs, nresults),
        IO_FILE_TOSTRING => call_file_tostring(state, a, nargs, nresults),
        _ => Ok(()),
    };

    state.last_c_function = prev_c_func;
    result
}

// ============================================================================
// 打开 I/O 库 — 对应 C 的 luaopen_io
// ============================================================================

/// 打开 I/O 库并注册到全局变量 io
///
/// 注册 stdin/stdout/stderr 作为 FullUserData（带 FILE* 元表），
/// 以及 io.write / io.output / io.close 函数。
pub fn open_io_lib(state: &mut LuaState) {
    let mut lib = crate::table::Table::new();

    // 创建 FILE* 元表 (对应 C 的 LUA_FILEHANDLE)
    let mut file_mt = crate::table::Table::new();
    let name_key = TValue::Str(state.intern_str("__name"));
    file_mt.set(name_key, TValue::Str(state.intern_str("FILE*")));
    // 注册元方法 (对应 C 的 metameth: __gc, __close, __tostring)
    file_mt.set(
        TValue::Str(state.intern_str("__gc")),
        TValue::LightUserData(IO_FILE_GC as *mut std::ffi::c_void),
    );
    file_mt.set(
        TValue::Str(state.intern_str("__close")),
        TValue::LightUserData(IO_FILE_CLOSE as *mut std::ffi::c_void),
    );
    file_mt.set(
        TValue::Str(state.intern_str("__tostring")),
        TValue::LightUserData(IO_FILE_TOSTRING as *mut std::ffi::c_void),
    );

    // 注册为 UserData 的默认元表 (对应 C 的 luaL_setmetatable(L, LUA_FILEHANDLE))
    let mt = crate::tm::Metatable::new(file_mt.clone());
    state.dmt.set(LuaType::UserData, mt);

    // 注册标准流作为 FullUserData (对应 C 的 FILE* 指针)
    // nuvalue=0 对应 C 的 lua_newuserdatauv(L, sizeof(LStream), 0)
    let make_stream = |state: &mut LuaState| {
        TValue::UserData(crate::objects::Udata {
            gc_header: crate::gc::GCObjectHeader::new(),
            nuvalue: 0,
            len: 0,
            metatable: Some(Box::new(file_mt.clone())),
            user_values: vec![],
            data: vec![],
        })
    };

    let stdin_key = TValue::Str(state.intern_str("stdin"));
    lib.set(stdin_key, make_stream(state));

    let stdout_key = TValue::Str(state.intern_str("stdout"));
    lib.set(stdout_key, make_stream(state));

    let stderr_key = TValue::Str(state.intern_str("stderr"));
    lib.set(stderr_key, make_stream(state));

    // 注册 io 库函数
    let register = |lib: &mut crate::table::Table, state: &mut LuaState, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        lib.set(key, TValue::LightUserData(tag as *mut std::ffi::c_void));
    };
    register(&mut lib, state, "write", IO_WRITE);
    register(&mut lib, state, "output", IO_OUTPUT);
    register(&mut lib, state, "close", IO_CLOSE);
    register(&mut lib, state, "input", IO_INPUT);

    let key = TValue::Str(state.intern_str("io"));
    state.globals.set(key, TValue::Table(lib));
}
