//! Lua 5.5 Rust FFI 绑定层
//!
//! 通过 CMake 将 Rust 源码编译为静态库，再用 C++ 调用。
//! 逐步替代 Lua 的 C 实现。

// 原始 FFI 声明（extern "C"）
pub mod bindings;

// 核心配置（luaconf.h）— 类型定义、常量、路径、数值运算
pub mod config;

// 安全包装层 —— 操作码（lopcodes.h / lopcodes.cpp）
pub mod opcodes;

// 安全包装层 —— 操作码名称（lopnames.h）
pub mod opnames;

// 安全包装层 —— 解析器（lparser.h / lparser.cpp）
pub mod parser;

// Lua 对象和值的表示
pub mod objects;

// 虚拟机核心实现
pub mod vm;

// 虚拟机核心操作 (lvm.h / lvm.cpp — 转换、比较、算术、表访问等)
pub mod lvm;

// 虚拟机主解释器循环 (luaV_execute)
pub mod execute;

// 字符串处理
pub mod strings;

// 表实现
pub mod table;

// 垃圾回收
pub mod gc;

// 内存管理器（lmem.h / lmem.cpp）— 分配/释放/GC 集成
pub mod mem;

// FFI 接口，用于与 C/C++ 代码交互
#[cfg(feature = "ffi")]
pub mod ffi;