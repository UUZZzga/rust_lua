//! lua-rs 的 C ABI 动态库 (Windows: `lua_rs.dll`)。
//!
//! 只做一件事: re-export `capi` 模块的 `#[no_mangle]` C API。rustc 在构建 cdylib 时
//! 会把这些符号写进 DLL 的导出表, 供 skynet 的 C 代码与 `luaclib/*.so` 在运行时
//! 解析 (deps/Makefile 用 build.rs 生成的 `lua_rs_dll_import.def` + dlltool 生成
//! MinGW 导入库)。Linux 下 deps 不使用本 crate: 那里 skynet 直接链接
//! `liblua_rs.a` 并用 `-Wl,-E` 导出符号给 dlopen 的 .so。
pub use lua_rs::capi::*;
