//! I/O 库 (liolib.cpp → Rust)
//!
//! 对应 C 源码: liolib.cpp
//!
//! ## 主要功能
//! - 注册 io 全局表，包含标准 I/O 流 (stdin/stdout/stderr)
//! - 当前实现: 仅提供 stdin/stdout/stderr 作为占位符值
//!
//! ## 标签分配
//! - 标签 700+: I/O 库

use crate::objects::TValue;
use crate::state::LuaState;

// ============================================================================
// 函数标签 (LightUserData 占位符值)
// ============================================================================

/// io.stdin 的标签 — 用于标识标准输入流
pub const IO_STDIN: usize = 700;
/// io.stdout 的标签 — 用于标识标准输出流
pub const IO_STDOUT: usize = 701;
/// io.stderr 的标签 — 用于标识标准错误流
pub const IO_STDERR: usize = 702;

/// I/O 库标签范围: [700, 710)
pub fn is_io_tag(tag: usize) -> bool {
    (700..710).contains(&tag)
}

// ============================================================================
// 打开 I/O 库 — 对应 C 的 luaopen_io
// ============================================================================

/// 打开 I/O 库并注册到全局变量 io
///
/// 当前仅提供 stdin/stdout/stderr 作为 LightUserData 占位符值。
/// 这些值用于让 `rawlen(io.stdin)` 等操作失败（如 events.lua 的测试所要求）。
pub fn open_io_lib(state: &mut LuaState) {
    let mut lib = crate::table::Table::new();

    // 注册标准流作为 LightUserData (对应 C 的 FILE* 指针)
    let stdin_key = TValue::Str(state.intern_str("stdin"));
    lib.set(stdin_key, TValue::LightUserData(IO_STDIN as *mut std::ffi::c_void));

    let stdout_key = TValue::Str(state.intern_str("stdout"));
    lib.set(stdout_key, TValue::LightUserData(IO_STDOUT as *mut std::ffi::c_void));

    let stderr_key = TValue::Str(state.intern_str("stderr"));
    lib.set(stderr_key, TValue::LightUserData(IO_STDERR as *mut std::ffi::c_void));

    let key = TValue::Str(state.intern_str("io"));
    state.globals.set(key, TValue::Table(lib));
}
