//! Lua 5.5 Rust FFI 绑定层
//!
//! 通过 CMake 将 Rust 源码编译为静态库，再用 C++ 调用。
//! 逐步替代 Lua 的 C 实现。

// 原始 FFI 声明（extern "C"）— 仅 ffi feature 时编译（引用 C 符号）
#[cfg(feature = "ffi")]
pub mod bindings;

// 核心配置（luaconf.h）— 类型定义、常量、路径、数值运算
pub mod config;

// 安全包装层 —— 操作码（lopcodes.h / lopcodes.cpp）
pub mod opcodes;

// 安全包装层 —— 操作码名称（lopnames.h）
pub mod opnames;

// 安全包装层 —— 解析器（lparser.h / lparser.cpp）— 仅 ffi feature 时编译
#[cfg(feature = "ffi")]
pub mod parser;

// Lua 对象和值的表示
pub mod objects;

// 解释器状态
pub mod state;
// 编译器 — 纯 Rust 实现的词法分析器 + 解析器 + 代码生成器
pub mod compiler;

// 虚拟机核心 (lvm.h/lvm.cpp — 转换、比较、算术、表访问、解释器循环)
pub mod vm;

// 虚拟机执行器 (lvm.cpp — 指令分发、运算实现)
pub mod execute;

// 函数/闭包管理 (lfunc.h/lfunc.cpp — 原型、闭包、上值)
pub mod func;

// 字符串处理
pub mod strings;

// 表实现
pub mod table;

// 标签方法 / 元方法 (ltm.h / ltm.cpp)
pub mod tm;

// 垃圾回收
pub mod gc;

// 内存管理器（lmem.h / lmem.cpp）— 分配/释放/GC 集成
pub mod mem;

// 调试工具
pub mod debug;

// 标准库 (lstrlib.cpp, lmathlib.cpp 等)
pub mod stdlib;

// FFI 接口，用于与 C/C++ 代码交互
// C Lua API FFI 声明（lua.h / lauxlib.h）
// 仅在 ffi feature 启用时编译（需要链接 C 实现的 liblua）。
// 默认情况下 Rust 实现自给自足，capi.rs 导出 #[no_mangle] 符号供第三方使用。
#[cfg(feature = "ffi")]
pub mod lua_ffi;

// C API 导出层（#[no_mangle] extern "C" fn）
// 将 Rust VM 以 C ABI 形式导出，供第三方 Lua C 模块链接调用。
// 启用 ffi feature 时禁用，避免与 C 库的符号冲突。
#[cfg(not(feature = "ffi"))]
pub mod capi;

// 命令行解释器
pub mod cli;
