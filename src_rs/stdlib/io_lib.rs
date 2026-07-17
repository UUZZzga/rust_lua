//! I/O 库 (liolib.cpp → Rust)
//!
//! 对应 C 源码: liolib.cpp
//!
//! ## 主要功能
//! - 注册 io 全局表，包含标准 I/O 流 (stdin/stdout/stderr)
//! - 实现 io.write / io.output / io.close / io.input / io.type / io.open /
//!   io.read / io.lines / io.flush / io.tmpfile
//! - 实现 file:read / file:write / file:seek / file:lines / file:flush /
//!   file:setvbuf / file:close
//!
//! ## 标签分配
//! - 标签 700-702: stdin/stdout/stderr 占位符值（非函数）
//! - 标签 800-819: io 库函数和文件方法

use crate::execute::VmError;
use crate::objects::{LuaType, NilKind, TValue};
use crate::state::LuaState;
use crate::table::Table;
use std::io::Write;
use std::rc::Rc;
use std::os::raw::c_int;

// C 标准库的 stdin/stdout/stderr — libc crate 不直接导出，用 extern 声明
extern "C" {
    #[link_name = "stdin"]
    static C_STDIN: *mut libc::FILE;
    #[link_name = "stdout"]
    static C_STDOUT: *mut libc::FILE;
    #[link_name = "stderr"]
    static C_STDERR: *mut libc::FILE;
}

/// 获取 C 的 stdin
fn c_stdin() -> *mut libc::FILE {
    unsafe { C_STDIN }
}
/// 获取 C 的 stdout
fn c_stdout() -> *mut libc::FILE {
    unsafe { C_STDOUT }
}
/// 获取 C 的 stderr
fn c_stderr() -> *mut libc::FILE {
    unsafe { C_STDERR }
}

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
// FILE* 方法表标签 (对应 C 的 meth: file:close 等)
pub const IO_FILE_CLOSE_METHOD: usize = 807;
pub const IO_TYPE: usize = 808;
pub const IO_OPEN: usize = 809;
pub const IO_FILE_READ: usize = 810;
pub const IO_FILE_WRITE: usize = 811;
pub const IO_FILE_SEEK: usize = 812;
pub const IO_FILE_LINES: usize = 813;
pub const IO_FILE_FLUSH: usize = 814;
pub const IO_FILE_SETVBUF: usize = 815;
pub const IO_READ: usize = 816;
pub const IO_LINES: usize = 817;
pub const IO_FLUSH: usize = 818;
pub const IO_TMPFILE: usize = 819;
pub const IO_POPEN: usize = 820;

/// io.lines / file:lines 的最大参数数量（对应 C 的 MAXARGLINE）
const MAXARGLINE: usize = 250;

/// I/O 库函数标签范围: [800, 821)
pub fn is_io_function_tag(tag: usize) -> bool {
    (800..821).contains(&tag)
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
        IO_FILE_CLOSE_METHOD => Some("close"),
        IO_TYPE => Some("type"),
        IO_OPEN => Some("open"),
        IO_FILE_READ => Some("read"),
        IO_FILE_WRITE => Some("write"),
        IO_FILE_SEEK => Some("seek"),
        IO_FILE_LINES => Some("lines"),
        IO_FILE_FLUSH => Some("flush"),
        IO_FILE_SETVBUF => Some("setvbuf"),
        IO_READ => Some("read"),
        IO_LINES => Some("lines"),
        IO_FLUSH => Some("flush"),
        IO_TMPFILE => Some("tmpfile"),
        IO_POPEN => Some("popen"),
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
// 文件句柄辅助函数
// ============================================================================

/// 检查参数是否是 FILE* userdata 并返回其 ptr_id
/// 对应 C 的 tolstream -> luaL_checkudata
fn check_file_arg(state: &LuaState, a: usize, nargs: usize, fname: &str) -> Result<u32, VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(format!(
            "bad argument #1 to '{}' (FILE* expected, got no value)",
            fname
        )));
    }
    let arg = get_arg(state, a, 0);
    match &arg {
        TValue::UserData(u) => {
            // 检查元表 __name == "FILE*"
            let is_file = u.metatable.as_ref().map_or(false, |mt| {
                let name_key = TValue::Str(state.intern_str("__name"));
                mt.get(&name_key) == Some(TValue::Str(state.intern_str("FILE*")))
            });
            if !is_file {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #1 to '{}' (FILE* expected, got userdata)",
                    fname
                )));
            }
            Ok(u.gc_header.ptr_id)
        }
        _ => {
            let typearg = crate::tm::obj_type_name(&arg);
            Err(VmError::RuntimeError(format!(
                "bad argument #1 to '{}' (FILE* expected, got {})",
                fname, typearg
            )))
        }
    }
}

/// 从 UserData ptr_id 获取 FILE* — 不检查是否已关闭
fn get_file_ptr(state: &LuaState, ptr_id: u32) -> Option<*mut libc::FILE> {
    state.file_handles.get(&ptr_id).copied()
}

/// 判断文件是否已关闭 (file_handles 中无对应 ptr_id)
fn is_closed(state: &LuaState, ptr_id: u32) -> bool {
    !state.file_handles.contains_key(&ptr_id)
}

/// 创建 FILE* userdata — 对应 C 的 newfile
///
/// 创建带 FILE* 元表的 UserData，并把 FILE* 存入 state.file_handles。
/// 注册到 GC（设置 id）和 ud_finobj_list（如果有 __gc 元方法）。
fn new_file_userdata(state: &mut LuaState, file: *mut libc::FILE, file_mt: &Table) -> TValue {
    let mut udata = crate::objects::Udata {
        gc_header: crate::gc::GCObjectHeader::new(),
        nuvalue: 0,
        len: 0,
        metatable: Some(Box::new(file_mt.clone())),
        user_values: vec![],
        data: vec![],
    };
    // 注册到 GC 并设置 id（使 mark_tvalue 能正确标记 reachable）
    let ud_id = state
        .gc
        .register_object(std::mem::size_of::<crate::objects::Udata>());
    udata.gc_header.set_id(ud_id);
    let ptr_id = udata.gc_header.ptr_id;
    state.file_handles.insert(ptr_id, file);
    // 如果元表有 __gc，注册到 ud_finobj_list
    let gc_key = TValue::Str(state.intern_str("__gc"));
    let ud_rc = Rc::new(udata);
    if file_mt.get(&gc_key).is_some() {
        state.register_ud_finobj(&ud_rc);
    }
    TValue::UserData(ud_rc)
}

/// 创建已关闭的 FILE* userdata — 用于 io.type 检查已关闭文件
fn new_closed_userdata(file_mt: &Table) -> TValue {
    TValue::UserData(Rc::new(crate::objects::Udata {
        gc_header: crate::gc::GCObjectHeader::new(),
        nuvalue: 0,
        len: 0,
        metatable: Some(Box::new(file_mt.clone())),
        user_values: vec![],
        data: vec![],
    }))
}

/// 检查文件模式是否合法 — 对应 C 的 l_checkmode
///
/// 模式必须匹配 `[rwa]%+?[b]*`
fn check_mode(mode: &str) -> bool {
    let bytes = mode.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    // 第一个字符必须是 r/w/a
    if bytes[0] != b'r' && bytes[0] != b'w' && bytes[0] != b'a' {
        return false;
    }
    let mut i = 1;
    // 可选的 '+'
    if i < bytes.len() && bytes[i] == b'+' {
        i += 1;
    }
    // 后续只能是 'b'
    while i < bytes.len() {
        if bytes[i] != b'b' {
            return false;
        }
        i += 1;
    }
    true
}

/// 推入文件结果 — 对应 C 的 luaL_fileresult
///
/// 成功: 推入 true，返回 1
/// 失败: 推入 nil, "filename: error" 或 "error", errno，返回 3
fn file_result(
    state: &mut LuaState,
    results: &mut Vec<TValue>,
    stat: bool,
    fname: Option<&str>,
) -> usize {
    if stat {
        results.push(TValue::Boolean(true));
        1
    } else {
        let en = unsafe { *libc::__errno_location() };
        let msg = if en != 0 {
            unsafe { std::ffi::CStr::from_ptr(libc::strerror(en)) }
                .to_string_lossy()
                .into_owned()
        } else {
            "(no extra info)".to_string()
        };
        results.push(TValue::Nil(NilKind::Strict));
        let full_msg = if let Some(f) = fname {
            format!("{}: {}", f, msg)
        } else {
            msg
        };
        results.push(TValue::Str(state.intern_str(&full_msg)));
        results.push(TValue::Integer(en as i64));
        3
    }
}

/// 检查 popen 模式是否合法 — 对应 C 的 l_checkmodep
///
/// 只接受 "r" 或 "w"
fn check_modep(mode: &str) -> bool {
    let bytes = mode.as_bytes();
    bytes.len() == 1 && (bytes[0] == b'r' || bytes[0] == b'w')
}

/// 推入命令执行结果 — 对应 C 的 luaL_execresult
///
/// 解析 system/pclose 返回的状态码:
/// - 成功退出 (exit 0): 返回 true, "exit", 0
/// - 非零退出: 返回 nil, "exit", exitcode
/// - 被信号终止: 返回 nil, "signal", signo
/// - errno 错误: 返回 nil, error_msg, errno
pub fn exec_result(state: &mut LuaState, results: &mut Vec<TValue>, stat: i32) -> usize {
    let en = unsafe { *libc::__errno_location() };
    if stat != 0 && en != 0 {
        // errno 错误 — 对应 luaL_fileresult(L, 0, NULL)
        let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(en)) }
            .to_string_lossy()
            .into_owned();
        results.push(TValue::Nil(NilKind::Strict));
        results.push(TValue::Str(state.intern_str(&msg)));
        results.push(TValue::Integer(en as i64));
        return 3;
    }
    // 解析 wait status — 对应 C 的 l_inspectstat
    let mut what = "exit";
    let mut code = stat;
    if libc::WIFEXITED(stat) {
        code = libc::WEXITSTATUS(stat);
    } else if libc::WIFSIGNALED(stat) {
        code = libc::WTERMSIG(stat);
        what = "signal";
    }
    if what == "exit" && code == 0 {
        results.push(TValue::Boolean(true));
    } else {
        results.push(TValue::Nil(NilKind::Strict));
    }
    results.push(TValue::Str(state.intern_str(what)));
    results.push(TValue::Integer(code as i64));
    3
}

// ============================================================================
// io.open 实现 (对应 C 的 io_open)
// ============================================================================

/// io.open(filename, [mode]) — 打开文件
///
/// 对应 C 的 io_open:
/// - 校验文件名 (字符串) 和模式 (可选, 默认 "r")
/// - 用 libc::fopen 打开文件
/// - 成功: 返回 userdata (带 FILE* 元表)
/// - 失败: 返回 nil, error_msg, errno
fn call_io_open(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'open' (string expected, got no value)".to_string(),
        ));
    }
    let filename_val = get_arg(state, a, 0);
    let filename = match &filename_val {
        TValue::Str(s) => s.as_str().to_string(),
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'open' (string expected, got {})",
                crate::tm::obj_type_name(&filename_val)
            )));
        }
    };
    let mode = if nargs >= 2 {
        let m = get_arg(state, a, 1);
        match &m {
            TValue::Str(s) => s.as_str().to_string(),
            TValue::Nil(_) => "r".to_string(),
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #2 to 'open' (string expected, got {})",
                    crate::tm::obj_type_name(&m)
                )));
            }
        }
    } else {
        "r".to_string()
    };

    if !check_mode(&mode) {
        return Err(VmError::RuntimeError(format!(
            "bad argument #2 to 'open' (invalid mode)"
        )));
    }

    // 设置 errno = 0
    unsafe {
        *libc::__errno_location() = 0;
    }
    let c_filename = std::ffi::CString::new(filename.clone()).unwrap();
    let c_mode = std::ffi::CString::new(mode.clone()).unwrap();
    let f = unsafe { libc::fopen(c_filename.as_ptr(), c_mode.as_ptr()) };

    let mut results = Vec::new();
    if f.is_null() {
        file_result(state, &mut results, false, Some(filename.as_str()));
    } else {
        // 获取 FILE* 元表 (从 dmt[UserData])
        let file_mt = state
            .dmt
            .get(LuaType::UserData)
            .cloned()
            .unwrap_or_else(|| {
                let mut t = crate::table::Table::new();
                t.set(
                    TValue::Str(state.intern_str("__name")),
                    TValue::Str(state.intern_str("FILE*")),
                );
                t
            });
        results.push(new_file_userdata(state, f, &file_mt));
    }
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// io.tmpfile 实现 (对应 C 的 io_tmpfile)
// ============================================================================

fn call_io_tmpfile(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    unsafe {
        *libc::__errno_location() = 0;
    }
    let f = unsafe { libc::tmpfile() };
    let mut results = Vec::new();
    if f.is_null() {
        file_result(state, &mut results, false, None);
    } else {
        let file_mt = state
            .dmt
            .get(LuaType::UserData)
            .cloned()
            .unwrap_or_else(|| {
                let mut t = crate::table::Table::new();
                t.set(
                    TValue::Str(state.intern_str("__name")),
                    TValue::Str(state.intern_str("FILE*")),
                );
                t
            });
        results.push(new_file_userdata(state, f, &file_mt));
    }
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// io.popen 实现 (对应 C 的 io_popen)
// ============================================================================

/// io.popen(prog, [mode]) — 打开进程
///
/// 对应 C 的 io_popen:
/// - 校验 prog (字符串) 和 mode (可选, 默认 "r", 只接受 "r"/"w")
/// - 用 libc::popen 打开进程
/// - 成功: 返回 userdata (带 FILE* 元表), 标记为 popen 文件
/// - 失败: 返回 nil, error_msg, errno
fn call_io_popen(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'popen' (string expected, got no value)".to_string(),
        ));
    }
    let prog_val = get_arg(state, a, 0);
    let prog = match &prog_val {
        TValue::Str(s) => s.as_str().to_string(),
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #1 to 'popen' (string expected, got {})",
                crate::tm::obj_type_name(&prog_val)
            )));
        }
    };
    let mode = if nargs >= 2 {
        let m = get_arg(state, a, 1);
        match &m {
            TValue::Str(s) => s.as_str().to_string(),
            TValue::Nil(_) => "r".to_string(),
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #2 to 'popen' (string expected, got {})",
                    crate::tm::obj_type_name(&m)
                )));
            }
        }
    } else {
        "r".to_string()
    };

    if !check_modep(&mode) {
        return Err(VmError::RuntimeError(
            "bad argument #2 to 'popen' (invalid mode)".to_string(),
        ));
    }

    let c_prog = std::ffi::CString::new(prog.clone()).unwrap();
    let c_mode = std::ffi::CString::new(mode).unwrap();
    // 对应 C 的 l_popen: fflush(NULL) 后 popen
    unsafe {
        libc::fflush(std::ptr::null_mut());
    }
    unsafe {
        *libc::__errno_location() = 0;
    }
    let f = unsafe { libc::popen(c_prog.as_ptr(), c_mode.as_ptr()) };

    let mut results = Vec::new();
    if f.is_null() {
        file_result(state, &mut results, false, Some(prog.as_str()));
    } else {
        let file_mt = state
            .dmt
            .get(LuaType::UserData)
            .cloned()
            .unwrap_or_else(|| {
                let mut t = crate::table::Table::new();
                t.set(
                    TValue::Str(state.intern_str("__name")),
                    TValue::Str(state.intern_str("FILE*")),
                );
                t
            });
        let ud = new_file_userdata(state, f, &file_mt);
        // 标记为 popen 文件，关闭时用 pclose
        if let TValue::UserData(ref u) = ud {
            state.popen_handles.insert(u.gc_header.ptr_id);
        }
        results.push(ud);
    }
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// io.write 实现 (对应 C 的 io_write / g_write)
// ============================================================================

/// 通用 write 实现 — 对应 C 的 g_write
///
/// 将多个参数写入 FILE*，返回 (true) 或 (nil, err, errno, count)
fn g_write(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    f: *mut libc::FILE,
    first_arg: usize,
) -> Result<Vec<TValue>, VmError> {
    let mut total_bytes: u64 = 0;
    unsafe {
        *libc::__errno_location() = 0;
    }
    for i in 0..nargs {
        let arg_idx = first_arg + i;
        let val = if arg_idx < state.stack.len() {
            state.stack[arg_idx].clone()
        } else {
            TValue::Nil(NilKind::Strict)
        };
        let bytes: Vec<u8> = match &val {
            TValue::Str(s) => s.as_str().as_bytes().to_vec(),
            TValue::Integer(n) => n.to_string().into_bytes(),
            TValue::Float(fl) => crate::stdlib::base_lib::lua_value_to_string(&val).into_bytes(),
            _ => {
                // 对应 C 的 luaL_checklstring 抛出错误
                return Err(VmError::RuntimeError(format!(
                    "bad argument #{} to 'write' (string or number expected, got {})",
                    i + 1,
                    crate::tm::obj_type_name(&val)
                )));
            }
        };
        let written =
            unsafe { libc::fwrite(bytes.as_ptr() as *const std::ffi::c_void, 1, bytes.len(), f) };
        total_bytes += written as u64;
        if written < bytes.len() {
            // 写入错误
            let en = unsafe { *libc::__errno_location() };
            let msg = if en != 0 {
                unsafe { std::ffi::CStr::from_ptr(libc::strerror(en)) }
                    .to_string_lossy()
                    .into_owned()
            } else {
                "(no extra info)".to_string()
            };
            return Ok(vec![
                TValue::Nil(NilKind::Strict),
                TValue::Str(state.intern_str(&msg)),
                TValue::Integer(en as i64),
                TValue::Integer(total_bytes as i64),
            ]);
        }
    }
    Ok(vec![TValue::Boolean(true)])
}

/// io.write(...) — 写入到默认输出流
fn call_io_write(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 获取默认输出流
    let f = get_default_output(state)?;
    if nargs == 0 {
        // io.write() 无参数 — 返回默认输出流
        let out = get_current_output_userdata(state);
        state.adjust_results(a, nresults, vec![out]);
        return Ok(());
    }
    let mut results = g_write(state, a, nargs, f, a + 1)?;
    // 成功时返回默认输出流的 userdata (对应 C: g_write 返回栈顶的文件句柄)
    if !results.is_empty() && matches!(results[0], TValue::Boolean(true)) {
        results[0] = get_current_output_userdata(state);
    }
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// io.output 实现 (对应 C 的 io_output / g_iofile)
// ============================================================================

/// io.output([file]) — 设置或获取默认输出流
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
                TValue::UserData(u) => {
                    // 校验是 FILE* userdata
                    let is_file = u.metatable.as_ref().map_or(false, |mt| {
                        let name_key = TValue::Str(state.intern_str("__name"));
                        mt.get(&name_key) == Some(TValue::Str(state.intern_str("FILE*")))
                    });
                    if !is_file {
                        return Err(VmError::RuntimeError(format!(
                            "bad argument #1 to 'output' (FILE* expected, got userdata)"
                        )));
                    }
                    state.io_output_handle = Some(u.gc_header.ptr_id);
                    state.io_output = None; // 清除 Box<dyn Write>
                                            // 保存到 io 表的 _current_output 字段
                    let io_key = TValue::Str(state.intern_str("io"));
                    if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
                        io_table.set(
                            TValue::Str(state.intern_str("_current_output")),
                            arg.clone(),
                        );
                    }
                }
                TValue::Str(s) => {
                    let filename = s.as_str().to_string();
                    // 用 fopen 打开文件，模式 "w"
                    unsafe {
                        *libc::__errno_location() = 0;
                    }
                    let c_filename = std::ffi::CString::new(filename.clone()).unwrap();
                    let c_mode = std::ffi::CString::new("w").unwrap();
                    let f = unsafe { libc::fopen(c_filename.as_ptr(), c_mode.as_ptr()) };
                    if f.is_null() {
                        let en = unsafe { *libc::__errno_location() };
                        let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(en)) }
                            .to_string_lossy()
                            .into_owned();
                        return Err(VmError::RuntimeError(format!("{}: {}", filename, msg)));
                    }
                    let file_mt = state
                        .dmt
                        .get(LuaType::UserData)
                        .cloned()
                        .unwrap_or_else(|| {
                            let mut t = crate::table::Table::new();
                            t.set(
                                TValue::Str(state.intern_str("__name")),
                                TValue::Str(state.intern_str("FILE*")),
                            );
                            t
                        });
                    let udata = new_file_userdata(state, f, &file_mt);
                    if let TValue::UserData(ref u) = udata {
                        state.io_output_handle = Some(u.gc_header.ptr_id);
                    }
                    state.io_output = None;
                    // 保存到 io 表的 _current_output 字段
                    let io_key = TValue::Str(state.intern_str("io"));
                    if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
                        io_table.set(
                            TValue::Str(state.intern_str("_current_output")),
                            udata.clone(),
                        );
                    }
                }
                _ => {
                    let typearg = crate::tm::obj_type_name(&arg);
                    return Err(VmError::RuntimeError(format!(
                        "bad argument #1 to 'output' (FILE* expected, got {})",
                        typearg
                    )));
                }
            }
        }
    }
    // 返回当前输出流
    let result = get_current_output_userdata(state);
    state.adjust_results(a, nresults, vec![result]);
    Ok(())
}

// ============================================================================
// io.input 实现 (对应 C 的 io_input / g_iofile)
// ============================================================================

fn call_io_input(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs > 0 {
        let arg = get_arg(state, a, 0);
        if !arg.is_nil() {
            match &arg {
                TValue::UserData(u) => {
                    let is_file = u.metatable.as_ref().map_or(false, |mt| {
                        let name_key = TValue::Str(state.intern_str("__name"));
                        mt.get(&name_key) == Some(TValue::Str(state.intern_str("FILE*")))
                    });
                    if !is_file {
                        return Err(VmError::RuntimeError(format!(
                            "bad argument #1 to 'input' (FILE* expected, got userdata)"
                        )));
                    }
                    state.io_input_handle = Some(u.gc_header.ptr_id);
                    // 保存到 io 表的 _current_input 字段
                    let io_key = TValue::Str(state.intern_str("io"));
                    if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
                        io_table.set(TValue::Str(state.intern_str("_current_input")), arg.clone());
                    }
                }
                TValue::Str(s) => {
                    let filename = s.as_str().to_string();
                    unsafe {
                        *libc::__errno_location() = 0;
                    }
                    let c_filename = std::ffi::CString::new(filename.clone()).unwrap();
                    let c_mode = std::ffi::CString::new("r").unwrap();
                    let f = unsafe { libc::fopen(c_filename.as_ptr(), c_mode.as_ptr()) };
                    if f.is_null() {
                        let en = unsafe { *libc::__errno_location() };
                        let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(en)) }
                            .to_string_lossy()
                            .into_owned();
                        return Err(VmError::RuntimeError(format!("{}: {}", filename, msg)));
                    }
                    let file_mt = state
                        .dmt
                        .get(LuaType::UserData)
                        .cloned()
                        .unwrap_or_else(|| {
                            let mut t = crate::table::Table::new();
                            t.set(
                                TValue::Str(state.intern_str("__name")),
                                TValue::Str(state.intern_str("FILE*")),
                            );
                            t
                        });
                    let udata = new_file_userdata(state, f, &file_mt);
                    if let TValue::UserData(ref u) = udata {
                        state.io_input_handle = Some(u.gc_header.ptr_id);
                    }
                    // 保存到 io 表的 _current_input 字段
                    let io_key = TValue::Str(state.intern_str("io"));
                    if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
                        io_table.set(
                            TValue::Str(state.intern_str("_current_input")),
                            udata.clone(),
                        );
                    }
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
    // 返回当前输入流
    let result = get_current_input_userdata(state);
    state.adjust_results(a, nresults, vec![result]);
    Ok(())
}

// ============================================================================
// io.close / file:close 实现 (对应 C 的 io_close / f_close)
// ============================================================================

fn call_io_close(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs == 0 {
        // 关闭默认输出流 (对应 C: io_close 无参数时取 IO_OUTPUT 再调用 f_close)
        let ptr_id = state.io_output_handle;
        if let Some(pid) = ptr_id {
            // 检查是否是标准文件 (stdin/stdout/stderr) — 对应 C 的 io_noclose
            let io_key = TValue::Str(state.intern_str("io"));
            let is_standard = if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
                ["stdin", "stdout", "stderr"].iter().any(|name| {
                    let key = TValue::Str(state.intern_str(name));
                    if let Some(TValue::UserData(u2)) = io_table.get(&key) {
                        u2.gc_header.ptr_id == pid
                    } else {
                        false
                    }
                })
            } else {
                false
            };
            if is_standard {
                // 不能关闭标准文件 (对应 C 的 io_noclose: 返回 nil, "cannot close standard file")
                state.adjust_results(
                    a,
                    nresults,
                    vec![
                        TValue::Nil(NilKind::Strict),
                        TValue::Str(state.intern_str("cannot close standard file")),
                    ],
                );
                return Ok(());
            }
            if let Some(f) = state.file_handles.get(&pid).copied() {
                let is_popen = state.popen_handles.remove(&pid);
                unsafe {
                    *libc::__errno_location() = 0;
                }
                let mut results = Vec::new();
                if is_popen {
                    let stat = unsafe { libc::pclose(f) };
                    exec_result(state, &mut results, stat);
                } else {
                    let res = unsafe { libc::fclose(f) };
                    file_result(state, &mut results, res == 0, None);
                }
                state.file_handles.remove(&pid);
                // 保留 io_output_handle 指向已关闭的文件（对应 C 的 registry[IO_OUTPUT] 保留已关闭 userdata）
                // 这样后续 io.write 会检测到文件已关闭并报错 "default output file is closed"
                state.adjust_results(a, nresults, results);
                return Ok(());
            }
            // io_output_handle 有值但文件已关闭 — 无操作
        }
        // 兼容旧 io_output: Box<dyn Write>
        if let Some(mut out) = state.io_output.take() {
            let _ = out.flush();
        }
        state.adjust_results(a, nresults, vec![TValue::Boolean(true)]);
        return Ok(());
    }
    close_file_handle(state, a, nargs, nresults)
}

fn call_file_close_method(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    close_file_handle(state, a, nargs, nresults)
}

fn close_file_handle(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let ptr_id = check_file_arg(state, a, nargs, "close")?;

    // 检查是否是标准文件 (stdin/stdout/stderr)
    let io_key = TValue::Str(state.intern_str("io"));
    let is_standard = if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
        ["stdin", "stdout", "stderr"].iter().any(|name| {
            let key = TValue::Str(state.intern_str(name));
            if let Some(TValue::UserData(u)) = io_table.get(&key) {
                u.gc_header.ptr_id == ptr_id
            } else {
                false
            }
        })
    } else {
        false
    };

    if is_standard {
        state.adjust_results(
            a,
            nresults,
            vec![
                TValue::Nil(NilKind::Strict),
                TValue::Str(state.intern_str("cannot close standard file")),
            ],
        );
        return Ok(());
    }

    // 已关闭文件: 报错 "attempt to use a closed file" (对应 C 的 tofile -> luaL_error)
    if is_closed(state, ptr_id) {
        return Err(VmError::RuntimeError(
            "attempt to use a closed file".to_string(),
        ));
    }

    // 关闭文件
    let f = state.file_handles.remove(&ptr_id).unwrap();
    let is_popen = state.popen_handles.remove(&ptr_id);
    unsafe {
        *libc::__errno_location() = 0;
    }
    let mut results = Vec::new();
    if is_popen {
        // popen 文件: 用 pclose 关闭, 返回 exec_result (true/nil, "exit"/"signal", code)
        // 对应 C 的 io_pclose -> luaL_execresult(L, l_pclose(L, p->f))
        let stat = unsafe { libc::pclose(f) };
        exec_result(state, &mut results, stat);
    } else {
        let res = unsafe { libc::fclose(f) };
        file_result(state, &mut results, res == 0, None);
    }
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// io.type 实现 (对应 C 的 io_type)
// ============================================================================

fn call_io_type(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    if nargs < 1 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'type' (value expected)".to_string(),
        ));
    }
    let arg = get_arg(state, a, 0);
    let result = match &arg {
        TValue::UserData(u) => {
            let is_file = u.metatable.as_ref().map_or(false, |mt| {
                let name_key = TValue::Str(state.intern_str("__name"));
                mt.get(&name_key) == Some(TValue::Str(state.intern_str("FILE*")))
            });
            if is_file {
                if is_closed(state, u.gc_header.ptr_id) {
                    TValue::Str(state.intern_str("closed file"))
                } else {
                    TValue::Str(state.intern_str("file"))
                }
            } else {
                TValue::Nil(NilKind::Strict)
            }
        }
        _ => TValue::Nil(NilKind::Strict),
    };
    state.adjust_results(a, nresults, vec![result]);
    Ok(())
}

// ============================================================================
// file:read / io.read 实现 (对应 C 的 f_read / io_read / g_read)
// ============================================================================

/// 读取数字 — 对应 C 的 read_number
fn read_number(f: *mut libc::FILE) -> Option<TValue> {
    // 对应 C 的 L_MAXLENNUM: 缓冲区最大长度，超过则解析失败
    const L_MAXLENNUM: usize = 200;
    let mut buf: Vec<u8> = Vec::with_capacity(L_MAXLENNUM + 1);
    let mut overflow = false; // 缓冲区溢出标志（对应 C 的 buff[0]='\0'）
    let mut c = unsafe { libc::fgetc(f) };
    // nextc: 保存当前字符到 buf 并读取下一个，溢出时返回 false（对应 C 的 nextc）
    macro_rules! nextc {
        () => {{
            if buf.len() >= L_MAXLENNUM {
                overflow = true;
                false
            } else {
                buf.push(c as u8);
                c = unsafe { libc::fgetc(f) };
                true
            }
        }};
    }
    // 跳过空白
    while c >= 0 && (c as u8 as char).is_ascii_whitespace() {
        c = unsafe { libc::fgetc(f) };
    }
    // 可选符号
    if c == b'-' as c_int || c == b'+' as c_int {
        nextc!();
    }
    let mut hex = false;
    let mut count = 0;
    if c == b'0' as c_int {
        if nextc!() {
            // 保存 '0'
            if c == b'x' as c_int || c == b'X' as c_int {
                if nextc!() {
                    // 保存 'x'/'X'
                    hex = true;
                }
            } else {
                count = 1;
            }
        }
    }
    // 整数部分
    while c >= 0 {
        let ch = c as u8 as char;
        if (hex && ch.is_ascii_hexdigit()) || (!hex && ch.is_ascii_digit()) {
            if !nextc!() {
                break;
            }
            count += 1;
        } else {
            break;
        }
    }
    // 小数点
    if c == b'.' as c_int {
        if nextc!() {
            // 保存 '.'
            while c >= 0 {
                let ch = c as u8 as char;
                if (hex && ch.is_ascii_hexdigit()) || (!hex && ch.is_ascii_digit()) {
                    if !nextc!() {
                        break;
                    }
                    count += 1;
                } else {
                    break;
                }
            }
        }
    }
    // 指数
    if count > 0 {
        if (hex && (c == b'p' as c_int || c == b'P' as c_int))
            || (!hex && (c == b'e' as c_int || c == b'E' as c_int))
        {
            if nextc!() {
                // 保存 'p'/'e'
                if c == b'-' as c_int || c == b'+' as c_int {
                    nextc!(); // 保存符号
                }
                while c >= 0 && (c as u8 as char).is_ascii_digit() {
                    if !nextc!() {
                        break;
                    }
                }
            }
        }
    }
    // 回退一个字符（对应 C 的 ungetc(rn.c, rn.f)）
    if c >= 0 {
        unsafe {
            libc::ungetc(c, f);
        }
    }
    if count == 0 || overflow {
        return None;
    }
    let s = String::from_utf8_lossy(&buf).into_owned();
    // 先尝试解析为整数
    if let Ok(n) = s.parse::<i64>() {
        return Some(TValue::Integer(n));
    }
    if let Ok(n) = s.parse::<f64>() {
        return Some(TValue::Float(n));
    }
    // 尝试 hex 解析
    if hex {
        let s_lower = s.to_lowercase();
        // 处理可选的正负号
        let (neg, rest) = if let Some(r) = s_lower.strip_prefix('-') {
            (true, r)
        } else if let Some(r) = s_lower.strip_prefix('+') {
            (false, r)
        } else {
            (false, s_lower.as_str())
        };
        if let Some(hex_part) = rest.strip_prefix("0x") {
            if let Some(dot_pos) = hex_part.find('.') {
                // 有小数点: 必为浮点数
                let int_part = &hex_part[..dot_pos];
                let frac_part = &hex_part[dot_pos + 1..];
                let (mantissa_str, exp) = if let Some(p_pos) = frac_part.find('p') {
                    let mantissa = &frac_part[..p_pos];
                    let exp_str = &frac_part[p_pos + 1..];
                    let e = exp_str.parse::<i32>().unwrap_or(0);
                    (format!("{}{}", int_part, mantissa), e)
                } else {
                    (format!("{}{}", int_part, frac_part), 0)
                };
                if let Ok(int_val) = u64::from_str_radix(&mantissa_str, 16) {
                    let frac_len = if let Some(p_pos) = hex_part.find('p') {
                        hex_part[dot_pos + 1..p_pos].len()
                    } else {
                        hex_part[dot_pos + 1..].len()
                    };
                    let val = int_val as f64 / (16f64).powi(frac_len as i32) * (2f64).powi(exp);
                    return Some(TValue::Float(if neg { -val } else { val }));
                }
            } else if let Some(p_pos) = hex_part.find('p') {
                // 无小数点但有 p 指数: C 的 l_str2int 不处理 p，由 l_str2d 解析为 Float
                let mantissa = &hex_part[..p_pos];
                let exp_str = &hex_part[p_pos + 1..];
                let e = exp_str.parse::<i32>().unwrap_or(0);
                if let Ok(int_val) = u64::from_str_radix(mantissa, 16) {
                    let val = (int_val as f64) * (2f64).powi(e);
                    return Some(TValue::Float(if neg { -val } else { val }));
                }
            } else {
                // 纯十六进制整数: 直接 u64 as i64 (对应 C 的 l_str2int + l_castU2S)
                if let Ok(int_val) = u64::from_str_radix(hex_part, 16) {
                    // C: *result = l_castU2S(neg ? 0u - a : a)
                    let i = if neg {
                        (0u64).wrapping_sub(int_val) as i64
                    } else {
                        int_val as i64
                    };
                    return Some(TValue::Integer(i));
                }
            }
        }
    }
    None
}

/// 读取一行 — 对应 C 的 read_line
///
/// chop=true: 不包含换行符 (对应 "l" 格式)
/// chop=false: 保留换行符 (对应 "L" 格式)
fn read_line(f: *mut libc::FILE, chop: bool) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut c = unsafe { libc::fgetc(f) };
    let mut got_any = false;
    while c >= 0 && c != b'\n' as c_int {
        buf.push(c as u8);
        got_any = true;
        c = unsafe { libc::fgetc(f) };
    }
    if c == b'\n' as c_int {
        if !chop {
            buf.push(b'\n');
        }
        return Some(buf);
    }
    // EOF
    if got_any {
        Some(buf)
    } else {
        None
    }
}

/// 读取所有内容 — 对应 C 的 read_all
fn read_all(f: *mut libc::FILE) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = unsafe {
            libc::fread(
                chunk.as_mut_ptr() as *mut std::ffi::c_void,
                1,
                chunk.len(),
                f,
            )
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if n < chunk.len() {
            break;
        }
    }
    buf
}

/// 读取 n 个字符 — 对应 C 的 read_chars
fn read_chars(f: *mut libc::FILE, n: usize) -> Option<Vec<u8>> {
    if n == 0 {
        // 测试 EOF: 读一个字符再放回
        let c = unsafe { libc::fgetc(f) };
        if c >= 0 {
            unsafe {
                libc::ungetc(c, f);
            }
            return Some(Vec::new());
        }
        return None;
    }
    let mut buf = vec![0u8; n];
    let nr = unsafe { libc::fread(buf.as_mut_ptr() as *mut std::ffi::c_void, 1, n, f) };
    if nr == 0 {
        return None;
    }
    buf.truncate(nr);
    Some(buf)
}

/// 通用 read 实现 — 对应 C 的 g_read
///
/// first_arg: 第一个读取格式参数在栈上的索引 (io.read: 1, f:read: 2)
fn g_read(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    f: *mut libc::FILE,
    first_arg: usize,
) -> Result<Vec<TValue>, VmError> {
    unsafe {
        libc::clearerr(f);
    }
    unsafe {
        *libc::__errno_location() = 0;
    }

    let mut results: Vec<TValue> = Vec::new();
    let mut success = true;

    if nargs == 0 {
        // 默认读一行
        match read_line(f, true) {
            Some(buf) => results.push(TValue::Str(crate::strings::new_long_bytes(buf))),
            None => results.push(TValue::Nil(NilKind::Strict)),
        }
        success = !results[0].is_nil();
    } else {
        for i in 0..nargs {
            let arg_idx = first_arg + i;
            let val = if arg_idx < state.stack.len() {
                state.stack[arg_idx].clone()
            } else {
                TValue::Nil(NilKind::Strict)
            };
            if !success {
                break;
            }
            match &val {
                TValue::Integer(n) => {
                    let n = *n;
                    if n < 0 {
                        success = false;
                        results.push(TValue::Nil(NilKind::Strict));
                    } else if n == 0 {
                        // 测试 EOF
                        let c = unsafe { libc::fgetc(f) };
                        if c >= 0 {
                            unsafe {
                                libc::ungetc(c, f);
                            }
                            results.push(TValue::Str(state.intern_str("")));
                        } else {
                            success = false;
                            results.push(TValue::Nil(NilKind::Strict));
                        }
                    } else {
                        match read_chars(f, n as usize) {
                            Some(buf) => {
                                results.push(TValue::Str(crate::strings::new_long_bytes(buf)));
                            }
                            None => {
                                success = false;
                                results.push(TValue::Nil(NilKind::Strict));
                            }
                        }
                    }
                }
                TValue::Float(fl) => {
                    // 浮点数也能作为数字参数
                    let n = *fl;
                    if n < 0.0 || n.fract() != 0.0 {
                        success = false;
                        results.push(TValue::Nil(NilKind::Strict));
                    } else if n == 0.0 {
                        let c = unsafe { libc::fgetc(f) };
                        if c >= 0 {
                            unsafe {
                                libc::ungetc(c, f);
                            }
                            results.push(TValue::Str(state.intern_str("")));
                        } else {
                            success = false;
                            results.push(TValue::Nil(NilKind::Strict));
                        }
                    } else {
                        match read_chars(f, n as usize) {
                            Some(buf) => {
                                results.push(TValue::Str(crate::strings::new_long_bytes(buf)));
                            }
                            None => {
                                success = false;
                                results.push(TValue::Nil(NilKind::Strict));
                            }
                        }
                    }
                }
                TValue::Str(s) => {
                    let p = s.as_str();
                    let p = if p.starts_with('*') { &p[1..] } else { p };
                    if p.is_empty() {
                        // 无效格式
                        return Err(VmError::RuntimeError(format!(
                            "bad argument #{} to 'read' (invalid format)",
                            i + 1
                        )));
                    }
                    match p.as_bytes()[0] {
                        b'n' => match read_number(f) {
                            Some(v) => results.push(v),
                            None => {
                                success = false;
                                results.push(TValue::Nil(NilKind::Strict));
                            }
                        },
                        b'l' => match read_line(f, true) {
                            Some(buf) => {
                                results.push(TValue::Str(crate::strings::new_long_bytes(buf)))
                            }
                            None => {
                                success = false;
                                results.push(TValue::Nil(NilKind::Strict));
                            }
                        },
                        b'L' => match read_line(f, false) {
                            Some(buf) => {
                                results.push(TValue::Str(crate::strings::new_long_bytes(buf)))
                            }
                            None => {
                                success = false;
                                results.push(TValue::Nil(NilKind::Strict));
                            }
                        },
                        b'a' => {
                            let buf = read_all(f);
                            results.push(TValue::Str(crate::strings::new_long_bytes(buf)));
                        }
                        _ => {
                            return Err(VmError::RuntimeError(format!(
                                "bad argument #{} to 'read' (invalid format)",
                                i + 1
                            )));
                        }
                    }
                }
                _ => {
                    return Err(VmError::RuntimeError(format!(
                        "bad argument #{} to 'read' (invalid format)",
                        i + 1
                    )));
                }
            }
        }
    }

    // 检查 ferror
    if unsafe { libc::ferror(f) } != 0 {
        let en = unsafe { *libc::__errno_location() };
        let msg = if en != 0 {
            unsafe { std::ffi::CStr::from_ptr(libc::strerror(en)) }
                .to_string_lossy()
                .into_owned()
        } else {
            "(no extra info)".to_string()
        };
        return Ok(vec![
            TValue::Nil(NilKind::Strict),
            TValue::Str(state.intern_str(&msg)),
            TValue::Integer(en as i64),
        ]);
    }

    if !success {
        // 把最后一个结果改成 nil
        if let Some(last) = results.last_mut() {
            *last = TValue::Nil(NilKind::Strict);
        }
    }
    Ok(results)
}

/// io.read(...) — 从默认输入流读取
fn call_io_read(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let f = get_default_input(state)?;
    let results = g_read(state, a, nargs, f, a + 1)?;
    state.adjust_results(a, nresults, results);
    Ok(())
}

/// file:read(...) — 从指定文件读取
fn call_file_read(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let ptr_id = check_file_arg(state, a, nargs, "read")?;
    let f = match get_file_ptr(state, ptr_id) {
        Some(f) => f,
        None => {
            return Err(VmError::RuntimeError(
                "attempt to use a closed file".to_string(),
            ));
        }
    };
    // nargs 包含 self，first_arg=a+2 已跳过 self，故传 nargs-1
    let n_fmts = nargs.saturating_sub(1);
    let results = g_read(state, a, n_fmts, f, a + 2)?;
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// file:write 实现 (对应 C 的 f_write / g_write)
// ============================================================================

fn call_file_write(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let ptr_id = check_file_arg(state, a, nargs, "write")?;
    let f = match get_file_ptr(state, ptr_id) {
        Some(f) => f,
        None => {
            return Err(VmError::RuntimeError(
                "attempt to use a closed file".to_string(),
            ));
        }
    };
    // nargs 包含 self，first_arg=a+2 已跳过 self，故传 nargs-1
    let n_args = nargs.saturating_sub(1);
    let mut results = g_write(state, a, n_args, f, a + 2)?;
    // file:write 在成功时返回文件句柄本身
    if !results.is_empty() && matches!(results[0], TValue::Boolean(true)) {
        // 替换为文件句柄
        let arg = get_arg(state, a, 0);
        results[0] = arg;
    }
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// file:seek 实现 (对应 C 的 f_seek)
// ============================================================================

fn call_file_seek(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let ptr_id = check_file_arg(state, a, nargs, "seek")?;
    let f = match get_file_ptr(state, ptr_id) {
        Some(f) => f,
        None => {
            return Err(VmError::RuntimeError(
                "attempt to use a closed file".to_string(),
            ));
        }
    };

    // whence: 默认 "cur"
    let whence = if nargs >= 2 {
        let v = get_arg(state, a, 1);
        match &v {
            TValue::Str(s) => s.as_str().to_string(),
            TValue::Nil(_) => "cur".to_string(),
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #2 to 'seek' (string expected, got {})",
                    crate::tm::obj_type_name(&v)
                )));
            }
        }
    } else {
        "cur".to_string()
    };

    let mode = match whence.as_str() {
        "set" => libc::SEEK_SET,
        "cur" => libc::SEEK_CUR,
        "end" => libc::SEEK_END,
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #2 to 'seek' (invalid option '{}')",
                whence
            )));
        }
    };

    // offset: 默认 0
    let offset = if nargs >= 3 {
        let v = get_arg(state, a, 2);
        match &v {
            TValue::Integer(n) => *n,
            TValue::Float(fl) => {
                if fl.fract() != 0.0 {
                    return Err(VmError::RuntimeError(
                        "bad argument #3 to 'seek' (not an integer in proper range)".to_string(),
                    ));
                }
                *fl as i64
            }
            TValue::Nil(_) => 0,
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #3 to 'seek' (integer expected, got {})",
                    crate::tm::obj_type_name(&v)
                )));
            }
        }
    } else {
        0
    };

    unsafe {
        *libc::__errno_location() = 0;
    }
    let res = unsafe { libc::fseek(f, offset as libc::c_long, mode) };
    if res != 0 {
        let en = unsafe { *libc::__errno_location() };
        let msg = if en != 0 {
            unsafe { std::ffi::CStr::from_ptr(libc::strerror(en)) }
                .to_string_lossy()
                .into_owned()
        } else {
            "(no extra info)".to_string()
        };
        state.adjust_results(
            a,
            nresults,
            vec![
                TValue::Nil(NilKind::Strict),
                TValue::Str(state.intern_str(&msg)),
                TValue::Integer(en as i64),
            ],
        );
        return Ok(());
    }
    let pos = unsafe { libc::ftell(f) };
    state.adjust_results(a, nresults, vec![TValue::Integer(pos as i64)]);
    Ok(())
}

// ============================================================================
// file:flush 实现 (对应 C 的 f_flush / aux_flush)
// ============================================================================

fn call_file_flush(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let ptr_id = check_file_arg(state, a, nargs, "flush")?;
    let f = match get_file_ptr(state, ptr_id) {
        Some(f) => f,
        None => {
            return Err(VmError::RuntimeError(
                "attempt to use a closed file".to_string(),
            ));
        }
    };
    unsafe {
        *libc::__errno_location() = 0;
    }
    let res = unsafe { libc::fflush(f) };
    let mut results = Vec::new();
    file_result(state, &mut results, res == 0, None);
    state.adjust_results(a, nresults, results);
    Ok(())
}

/// io.flush() — 刷新默认输出流
fn call_io_flush(
    state: &mut LuaState,
    a: usize,
    _nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let f = get_default_output(state)?;
    unsafe {
        *libc::__errno_location() = 0;
    }
    let res = unsafe { libc::fflush(f) };
    let mut results = Vec::new();
    file_result(state, &mut results, res == 0, None);
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// file:setvbuf 实现 (对应 C 的 f_setvbuf)
// ============================================================================

fn call_file_setvbuf(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let ptr_id = check_file_arg(state, a, nargs, "setvbuf")?;
    let f = match get_file_ptr(state, ptr_id) {
        Some(f) => f,
        None => {
            return Err(VmError::RuntimeError(
                "attempt to use a closed file".to_string(),
            ));
        }
    };

    let mode_str = if nargs >= 2 {
        let v = get_arg(state, a, 1);
        match &v {
            TValue::Str(s) => s.as_str().to_string(),
            _ => {
                return Err(VmError::RuntimeError(format!(
                    "bad argument #2 to 'setvbuf' (string expected, got {})",
                    crate::tm::obj_type_name(&v)
                )));
            }
        }
    } else {
        return Err(VmError::RuntimeError(
            "bad argument #2 to 'setvbuf' (string expected, got no value)".to_string(),
        ));
    };

    let mode = match mode_str.as_str() {
        "no" => libc::_IONBF,
        "full" => libc::_IOFBF,
        "line" => libc::_IOLBF,
        _ => {
            return Err(VmError::RuntimeError(format!(
                "bad argument #2 to 'setvbuf' (invalid option '{}')",
                mode_str
            )));
        }
    };

    let size = if nargs >= 3 {
        let v = get_arg(state, a, 2);
        match &v {
            TValue::Integer(n) => *n as usize,
            TValue::Float(fl) => {
                if fl.fract() != 0.0 || *fl < 0.0 {
                    8192
                } else {
                    *fl as usize
                }
            }
            TValue::Nil(_) => 8192,
            _ => 8192,
        }
    } else {
        8192
    };

    unsafe {
        *libc::__errno_location() = 0;
    }
    let res = unsafe { libc::setvbuf(f, std::ptr::null_mut(), mode, size) };
    let mut results = Vec::new();
    file_result(state, &mut results, res == 0, None);
    state.adjust_results(a, nresults, results);
    Ok(())
}

// ============================================================================
// io.lines / file:lines 实现
// ============================================================================

/// lines 的迭代器状态 — 存储在 LightUserData tag 中
/// 我们用一个简单的方案: io.lines/file:lines 返回一个 LightUserData (tag)
/// 每次调用迭代器时, 从 state 中的迭代器表取出状态
#[derive(Clone, Debug)]
pub struct LinesState {
    pub file_ptr_id: u32,
    pub formats: Vec<TValue>, // 读取格式
    pub to_close: bool,       // 完成后是否关闭文件
    pub finished: bool,       // 是否已完成
}

/// 全局 lines 状态存储 — 使用 thread_local 避免修改 state.rs
/// key 是 LightUserData 的 tag 值 (递增计数器)
thread_local! {
    static LINES_STATES: std::cell::RefCell<std::collections::HashMap<usize, Box<LinesState>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    static LINES_COUNTER: std::cell::Cell<usize> = const { std::cell::Cell::new(0x1000_0000_0000_0000) };
}

/// lines 迭代器 tag 的起始范围 (高位, 避免与普通 tag 冲突)
pub const LINES_TAG_BASE: usize = 0x1000_0000_0000_0000;

/// io.lines([filename, [fmt1, ...]]) — 创建行迭代器
fn call_io_lines(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 检查参数数量（对应 C 的 luaL_argcheck n <= MAXARGLINE）
    // nargs 含文件名，格式参数数量 = nargs - 1
    if nargs > 0 {
        let n_fmts = nargs - 1;
        if n_fmts > MAXARGLINE {
            return Err(VmError::RuntimeError(format!(
                "bad argument #{} to 'lines' (too many arguments)",
                MAXARGLINE + 2
            )));
        }
    }
    let mut results: Vec<TValue> = Vec::new();

    if nargs == 0 {
        // 无参数: 使用默认输入流，不关闭
        let ptr_id = state.io_input_handle.unwrap_or_else(|| {
            // 默认是 stdin 的 ptr_id
            get_stdin_ptr_id(state)
        });
        let mut formats = Vec::new();
        // io.lines() 默认格式 "l"
        formats.push(TValue::Str(state.intern_str("l")));
        let ls = LinesState {
            file_ptr_id: ptr_id,
            formats,
            to_close: false,
            finished: false,
        };
        let tag = alloc_lines_tag(state, ls);
        results.push(TValue::LightUserData(tag as *mut std::ffi::c_void));
        state.adjust_results(a, nresults, results);
        return Ok(());
    }

    let first = get_arg(state, a, 0);
    if first.is_nil() {
        // nil 参数: 使用默认输入流，读取后续格式参数
        let ptr_id = state
            .io_input_handle
            .unwrap_or_else(|| get_stdin_ptr_id(state));
        let mut formats = Vec::new();
        if nargs >= 2 {
            for i in 1..nargs {
                formats.push(get_arg(state, a, i));
            }
        } else {
            formats.push(TValue::Str(state.intern_str("l")));
        }
        let ls = LinesState {
            file_ptr_id: ptr_id,
            formats,
            to_close: false,
            finished: false,
        };
        let tag = alloc_lines_tag(state, ls);
        results.push(TValue::LightUserData(tag as *mut std::ffi::c_void));
        state.adjust_results(a, nresults, results);
        return Ok(());
    }

    // 第一个参数是字符串: 打开文件
    match &first {
        TValue::Str(s) => {
            let filename = s.as_str().to_string();
            unsafe {
                *libc::__errno_location() = 0;
            }
            let c_filename = std::ffi::CString::new(filename.clone()).unwrap();
            let c_mode = std::ffi::CString::new("r").unwrap();
            let f = unsafe { libc::fopen(c_filename.as_ptr(), c_mode.as_ptr()) };
            if f.is_null() {
                let en = unsafe { *libc::__errno_location() };
                let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(en)) }
                    .to_string_lossy()
                    .into_owned();
                return Err(VmError::RuntimeError(format!("{}: {}", filename, msg)));
            }
            let file_mt = state
                .dmt
                .get(LuaType::UserData)
                .cloned()
                .unwrap_or_else(|| {
                    let mut t = crate::table::Table::new();
                    t.set(
                        TValue::Str(state.intern_str("__name")),
                        TValue::Str(state.intern_str("FILE*")),
                    );
                    t
                });
            let udata = new_file_userdata(state, f, &file_mt);
            let ptr_id = if let TValue::UserData(ref u) = udata {
                u.gc_header.ptr_id
            } else {
                unreachable!()
            };
            // 默认格式 "l"
            let mut formats = Vec::new();
            if nargs >= 2 {
                for i in 1..nargs {
                    formats.push(get_arg(state, a, i));
                }
            } else {
                formats.push(TValue::Str(state.intern_str("l")));
            }
            let ls = LinesState {
                file_ptr_id: ptr_id,
                formats,
                to_close: true,
                finished: false,
            };
            let tag = alloc_lines_tag(state, ls);
            // toclose=1: 返回 4 个值 (迭代器, nil state, nil control, file to-be-closed)
            // 对应 C 的 io_lines: lua_pushnil(state); lua_pushnil(control);
            // lua_pushvalue(file); return 4;
            // generic for 的第 4 个值是 to-be-closed 变量，循环结束时自动关闭
            results.push(TValue::LightUserData(tag as *mut std::ffi::c_void));
            results.push(TValue::Nil(crate::objects::NilKind::Strict)); // state
            results.push(TValue::Nil(crate::objects::NilKind::Strict)); // control
            results.push(udata); // file (to-be-closed)
            state.adjust_results(a, nresults, results);
            Ok(())
        }
        _ => Err(VmError::RuntimeError(format!(
            "bad argument #1 to 'lines' (string expected, got {})",
            crate::tm::obj_type_name(&first)
        ))),
    }
}

/// file:lines([fmt1, ...]) — 创建行迭代器
fn call_file_lines(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 检查参数数量（对应 C 的 luaL_argcheck n <= MAXARGLINE）
    // nargs 含 self，格式参数数量 = nargs - 1
    let n_fmts = nargs.saturating_sub(1);
    if n_fmts > MAXARGLINE {
        return Err(VmError::RuntimeError(format!(
            "bad argument #{} to 'lines' (too many arguments)",
            MAXARGLINE + 2
        )));
    }
    let ptr_id = check_file_arg(state, a, nargs, "lines")?;
    let mut formats = Vec::new();
    if nargs >= 2 {
        for i in 1..nargs {
            formats.push(get_arg(state, a, i));
        }
    } else {
        formats.push(TValue::Str(state.intern_str("l")));
    }
    let ls = LinesState {
        file_ptr_id: ptr_id,
        formats,
        to_close: false,
        finished: false,
    };
    let tag = alloc_lines_tag(state, ls);
    let result = TValue::LightUserData(tag as *mut std::ffi::c_void);
    state.adjust_results(a, nresults, vec![result]);
    Ok(())
}

/// 分配 lines 迭代器 tag 并存储状态
fn alloc_lines_tag(_state: &mut LuaState, ls: LinesState) -> usize {
    let tag = LINES_COUNTER.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    LINES_STATES.with(|s| {
        s.borrow_mut().insert(tag, Box::new(ls));
    });
    tag
}

/// 调用 lines 迭代器 — 对应 C 的 io_readline
///
/// tag 是 LightUserData 的 tag 值, 用于从 LINES_STATES 中查找状态
pub fn call_lines_iterator(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let _ = nargs; // lines 迭代器无参数
                   // 从 LINES_STATES 取出状态
    let (file_ptr_id, formats, to_close, finished) = LINES_STATES.with(|s| {
        let mut map = s.borrow_mut();
        if let Some(ls) = map.get_mut(&tag) {
            (ls.file_ptr_id, ls.formats.clone(), ls.to_close, ls.finished)
        } else {
            (0, Vec::new(), false, true)
        }
    });

    if file_ptr_id == 0 {
        return Err(VmError::RuntimeError(
            "lines iterator state not found".to_string(),
        ));
    }

    if finished {
        return Err(VmError::RuntimeError("file is already closed".to_string()));
    }

    // 检查文件是否已关闭
    let f = match state.file_handles.get(&file_ptr_id).copied() {
        Some(f) => f,
        None => {
            // 文件已被关闭
            LINES_STATES.with(|s| {
                let mut map = s.borrow_mut();
                if let Some(ls) = map.get_mut(&tag) {
                    ls.finished = true;
                }
            });
            return Err(VmError::RuntimeError("file is already closed".to_string()));
        }
    };

    // 把 formats 推入临时栈, 调用 g_read
    let saved_stack_len = state.stack.len();
    state.stack.truncate(a + 1);
    for fmt in &formats {
        state.stack.push(fmt.clone());
    }
    let n_formats = formats.len();
    let results = g_read(state, a, n_formats, f, a + 1)?;
    state.stack.truncate(saved_stack_len);

    if results.is_empty() || results[0].is_nil() {
        // EOF 或错误
        if results.len() > 1 {
            // 错误信息
            let err_msg = if let TValue::Str(s) = &results[1] {
                s.as_str().to_string()
            } else {
                String::new()
            };
            // 关闭文件（如果需要）
            if to_close {
                if let Some(f) = state.file_handles.remove(&file_ptr_id) {
                    unsafe {
                        libc::fclose(f);
                    }
                }
            }
            LINES_STATES.with(|s| {
                let mut map = s.borrow_mut();
                if let Some(ls) = map.get_mut(&tag) {
                    ls.finished = true;
                }
            });
            return Err(VmError::RuntimeError(err_msg));
        }
        // EOF: 关闭文件
        if to_close {
            if let Some(f) = state.file_handles.remove(&file_ptr_id) {
                unsafe {
                    libc::fclose(f);
                }
            }
        }
        LINES_STATES.with(|s| {
            let mut map = s.borrow_mut();
            if let Some(ls) = map.get_mut(&tag) {
                ls.finished = true;
            }
        });
        // 返回无结果
        state.adjust_results(a, nresults, vec![]);
        Ok(())
    } else {
        // 成功,返回读取的结果
        state.adjust_results(a, nresults, results);
        Ok(())
    }
}

/// 判断 tag 是否是 lines 迭代器 tag
pub fn is_lines_iterator_tag(tag: usize) -> bool {
    tag >= LINES_TAG_BASE
}

// ============================================================================
// 默认输入/输出流辅助函数
// ============================================================================

/// 获取默认输出流的 FILE* — 对应 C 的 getiofile(L, IO_OUTPUT)
fn get_default_output(state: &mut LuaState) -> Result<*mut libc::FILE, VmError> {
    // 优先检查 io_output_handle
    if let Some(pid) = state.io_output_handle {
        if let Some(f) = state.file_handles.get(&pid).copied() {
            return Ok(f);
        }
        return Err(VmError::RuntimeError(
            "default output file is closed".to_string(),
        ));
    }
    // 检查 io_output: Box<dyn Write> (向后兼容)
    if state.io_output.is_some() {
        // 这种情况下我们无法获取 FILE*, 需要特殊处理
        // 实际上,io_output 是 Box<dyn Write>,不是 FILE*
        // 我们需要把它转换为 FILE* — 但不可能
        // 解决方案: 当 io_output 设置时,使用 stdout
        // 但是这会导致写入错误
        // 临时方案: 返回 stdout
        return Ok(c_stdout());
    }
    // 默认使用 stdout
    Ok(c_stdout())
}

/// 获取默认输入流的 FILE* — 对应 C 的 getiofile(L, IO_INPUT)
fn get_default_input(state: &mut LuaState) -> Result<*mut libc::FILE, VmError> {
    if let Some(pid) = state.io_input_handle {
        if let Some(f) = state.file_handles.get(&pid).copied() {
            return Ok(f);
        }
        return Err(VmError::RuntimeError(" input file is closed".to_string()));
    }
    // 默认使用 stdin
    Ok(c_stdin())
}

/// 获取当前输出流的 UserData (用于 io.output() 返回值)
fn get_current_output_userdata(state: &mut LuaState) -> TValue {
    let io_key = TValue::Str(state.intern_str("io"));
    if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
        if let Some(pid) = state.io_output_handle {
            // 先检查 _current_output 字段
            let cur_key = TValue::Str(state.intern_str("_current_output"));
            if let Some(v) = io_table.get(&cur_key) {
                if let TValue::UserData(u) = &v {
                    if u.gc_header.ptr_id == pid {
                        return v;
                    }
                }
            }
            // 再检查 stdout
            let stdout_key = TValue::Str(state.intern_str("stdout"));
            if let Some(stdout_val) = io_table.get(&stdout_key) {
                if let TValue::UserData(u) = &stdout_val {
                    if u.gc_header.ptr_id == pid {
                        return stdout_val;
                    }
                }
            }
            return TValue::Nil(NilKind::Strict);
        }
        // 默认返回 io.stdout
        let stdout_key = TValue::Str(state.intern_str("stdout"));
        if let Some(stdout_val) = io_table.get(&stdout_key) {
            return stdout_val;
        }
    }
    TValue::Nil(NilKind::Strict)
}

/// 获取当前输入流的 UserData (用于 io.input() 返回值)
fn get_current_input_userdata(state: &mut LuaState) -> TValue {
    let io_key = TValue::Str(state.intern_str("io"));
    if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
        if let Some(pid) = state.io_input_handle {
            // 先检查 _current_input 字段
            let cur_key = TValue::Str(state.intern_str("_current_input"));
            if let Some(TValue::UserData(u)) = io_table.get(&cur_key) {
                if u.gc_header.ptr_id == pid {
                    return TValue::UserData(u);
                }
            }
            // 再检查 stdin
            let stdin_key = TValue::Str(state.intern_str("stdin"));
            if let Some(stdin_val) = io_table.get(&stdin_key) {
                if let TValue::UserData(u) = &stdin_val {
                    if u.gc_header.ptr_id == pid {
                        return stdin_val;
                    }
                }
            }
            return TValue::Nil(NilKind::Strict);
        }
        // 默认返回 io.stdin
        let stdin_key = TValue::Str(state.intern_str("stdin"));
        if let Some(stdin_val) = io_table.get(&stdin_key) {
            return stdin_val;
        }
    }
    TValue::Nil(NilKind::Strict)
}

/// 获取 io.stdin 的 ptr_id
fn get_stdin_ptr_id(state: &LuaState) -> u32 {
    let io_key = TValue::Str(state.intern_str("io"));
    if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
        let stdin_key = TValue::Str(state.intern_str("stdin"));
        if let Some(TValue::UserData(u)) = io_table.get(&stdin_key) {
            return u.gc_header.ptr_id;
        }
    }
    0
}

// ============================================================================
// FILE* 元方法实现 (对应 C 的 metameth: __gc, __close, __tostring)
// ============================================================================

fn call_file_gc(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let ptr_id = check_file_arg(state, a, nargs, "__gc")?;
    if let Some(f) = state.file_handles.get(&ptr_id).copied() {
        // 检查是否是标准文件
        let io_key = TValue::Str(state.intern_str("io"));
        let is_standard = if let Some(TValue::Table(io_table)) = state.globals.get(&io_key) {
            ["stdin", "stdout", "stderr"].iter().any(|name| {
                let key = TValue::Str(state.intern_str(name));
                if let Some(TValue::UserData(u2)) = io_table.get(&key) {
                    u2.gc_header.ptr_id == ptr_id
                } else {
                    false
                }
            })
        } else {
            false
        };
        if !is_standard {
            let is_popen = state.popen_handles.remove(&ptr_id);
            if is_popen {
                unsafe {
                    libc::pclose(f);
                }
            } else {
                unsafe {
                    libc::fclose(f);
                }
            }
            state.file_handles.remove(&ptr_id);
        }
    }
    state.adjust_results(a, nresults, vec![]);
    Ok(())
}

fn call_file_close(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // __close 行为同 __gc
    call_file_gc(state, a, nargs, nresults)
}

fn call_file_tostring(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let ptr_id = check_file_arg(state, a, nargs, "__tostring")?;
    let result = if is_closed(state, ptr_id) {
        TValue::Str(state.intern_str("file (closed)"))
    } else {
        // 简化: 返回 "file (0x0)"
        TValue::Str(state.intern_str("file (0x0)"))
    };
    state.adjust_results(a, nresults, vec![result]);
    Ok(())
}

// ============================================================================
// 派发函数
// ============================================================================

pub fn call_io_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    let prev_c_func = state.last_c_function.take();
    state.last_c_function = io_function_name(tag).map(|s| s.to_string());

    let result = match tag {
        IO_WRITE => call_io_write(state, a, nargs, nresults),
        IO_OUTPUT => call_io_output(state, a, nargs, nresults),
        IO_CLOSE => call_io_close(state, a, nargs, nresults),
        IO_INPUT => call_io_input(state, a, nargs, nresults),
        IO_FILE_GC => call_file_gc(state, a, nargs, nresults),
        IO_FILE_CLOSE => call_file_close(state, a, nargs, nresults),
        IO_FILE_TOSTRING => call_file_tostring(state, a, nargs, nresults),
        IO_FILE_CLOSE_METHOD => call_file_close_method(state, a, nargs, nresults),
        IO_TYPE => call_io_type(state, a, nargs, nresults),
        IO_OPEN => call_io_open(state, a, nargs, nresults),
        IO_FILE_READ => call_file_read(state, a, nargs, nresults),
        IO_FILE_WRITE => call_file_write(state, a, nargs, nresults),
        IO_FILE_SEEK => call_file_seek(state, a, nargs, nresults),
        IO_FILE_LINES => call_file_lines(state, a, nargs, nresults),
        IO_FILE_FLUSH => call_file_flush(state, a, nargs, nresults),
        IO_FILE_SETVBUF => call_file_setvbuf(state, a, nargs, nresults),
        IO_READ => call_io_read(state, a, nargs, nresults),
        IO_LINES => call_io_lines(state, a, nargs, nresults),
        IO_FLUSH => call_io_flush(state, a, nargs, nresults),
        IO_TMPFILE => call_io_tmpfile(state, a, nargs, nresults),
        IO_POPEN => call_io_popen(state, a, nargs, nresults),
        _ => Ok(()),
    };

    state.last_c_function = prev_c_func;
    result
}

// ============================================================================
// 打开 I/O 库 — 对应 C 的 luaopen_io
// ============================================================================

pub fn open_io_lib(state: &mut LuaState) {
    let mut lib = crate::table::Table::new();

    // 创建 FILE* 元表 (对应 C 的 LUA_FILEHANDLE)
    let mut file_mt = crate::table::Table::new();
    let name_key = TValue::Str(state.intern_str("__name"));
    file_mt.set(name_key, TValue::Str(state.intern_str("FILE*")));
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

    // 创建 FILE* 方法表
    let mut file_methods = crate::table::Table::new();
    file_methods.set(
        TValue::Str(state.intern_str("close")),
        TValue::LightUserData(IO_FILE_CLOSE_METHOD as *mut std::ffi::c_void),
    );
    file_methods.set(
        TValue::Str(state.intern_str("read")),
        TValue::LightUserData(IO_FILE_READ as *mut std::ffi::c_void),
    );
    file_methods.set(
        TValue::Str(state.intern_str("write")),
        TValue::LightUserData(IO_FILE_WRITE as *mut std::ffi::c_void),
    );
    file_methods.set(
        TValue::Str(state.intern_str("seek")),
        TValue::LightUserData(IO_FILE_SEEK as *mut std::ffi::c_void),
    );
    file_methods.set(
        TValue::Str(state.intern_str("lines")),
        TValue::LightUserData(IO_FILE_LINES as *mut std::ffi::c_void),
    );
    file_methods.set(
        TValue::Str(state.intern_str("flush")),
        TValue::LightUserData(IO_FILE_FLUSH as *mut std::ffi::c_void),
    );
    file_methods.set(
        TValue::Str(state.intern_str("setvbuf")),
        TValue::LightUserData(IO_FILE_SETVBUF as *mut std::ffi::c_void),
    );
    file_mt.set(
        TValue::Str(state.intern_str("__index")),
        TValue::Table(file_methods),
    );

    // 注册为 UserData 的默认元表
    let mt = crate::tm::Metatable::new(file_mt.clone());
    state.dmt.set(LuaType::UserData, mt);

    // 注册标准流作为 FullUserData
    let make_stream = |state: &mut LuaState, file: *mut libc::FILE| -> TValue {
        let udata = crate::objects::Udata {
            gc_header: crate::gc::GCObjectHeader::new(),
            nuvalue: 0,
            len: 0,
            metatable: Some(Box::new(file_mt.clone())),
            user_values: vec![],
            data: vec![],
        };
        let ptr_id = udata.gc_header.ptr_id;
        state.file_handles.insert(ptr_id, file);
        TValue::UserData(Rc::new(udata))
    };

    let stdin_val = make_stream(state, c_stdin());
    let stdout_val = make_stream(state, c_stdout());
    let stderr_val = make_stream(state, c_stderr());

    let stdin_key = TValue::Str(state.intern_str("stdin"));
    lib.set(stdin_key, stdin_val);

    let stdout_key = TValue::Str(state.intern_str("stdout"));
    lib.set(stdout_key, stdout_val);

    let stderr_key = TValue::Str(state.intern_str("stderr"));
    lib.set(stderr_key, stderr_val);

    // 注册 io 库函数
    let register = |lib: &mut crate::table::Table, state: &mut LuaState, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        lib.set(key, TValue::LightUserData(tag as *mut std::ffi::c_void));
    };
    register(&mut lib, state, "write", IO_WRITE);
    register(&mut lib, state, "output", IO_OUTPUT);
    register(&mut lib, state, "close", IO_CLOSE);
    register(&mut lib, state, "input", IO_INPUT);
    register(&mut lib, state, "type", IO_TYPE);
    register(&mut lib, state, "open", IO_OPEN);
    register(&mut lib, state, "read", IO_READ);
    register(&mut lib, state, "lines", IO_LINES);
    register(&mut lib, state, "flush", IO_FLUSH);
    register(&mut lib, state, "tmpfile", IO_TMPFILE);
    register(&mut lib, state, "popen", IO_POPEN);

    let key = TValue::Str(state.intern_str("io"));
    state.globals.set(key, TValue::Table(lib));
}
