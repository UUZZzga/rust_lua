# 项目规则

## 项目概述

本项目是 **Lua 5.5.0 的 Rust 实现**（`lua-rs`），目标是逐步替代 `src/` 下的 C/C++ 官方实现。Rust 实现自给自足，默认通过 `capi.rs` 以 `#[no_mangle] extern "C"` 形式导出 C ABI，使第三方 Lua C 模块（`.so`）能够直接链接调用。启用 `ffi` feature 时改为链接 C 实现的 `liblua`，用于编译器比对测试等场景。

## 项目目录结构

| 目录 | 用途 |
| --- | --- |
| `src/` | Lua C/C++ 官方实现源码（参考与比对基准，使用 C++17 编译） |
| `src_rs/` | Lua Rust 实现源码 |
| `src_rs/compiler/` | 纯 Rust 实现的词法分析器、解析器、代码生成器、字节码 dump 与比对测试 |
| `src_rs/stdlib/` | 标准库 Rust 实现（base / coroutine / debug / io / math / os / string / table / utf8） |
| `tests_lua/` | Lua C 官方测试用例目录，内部还包含几个新增的用于测试 Rust 实现的测试用例 |
| `tests_rs/` | Lua Rust 实现的测试目录（integration / upvalue / metamethod / arithmetic / string / base_lib / math_lib） |
| `build/` | Lua C 实现的构建目录，如果需要重新构建，也只能使用该目录 |
| `target/` | Lua Rust 实现的构建目录，如果需要重新构建，也只能使用该目录 |
| `target_perf/` | Lua Rust 实现的性能测试目录，如果需要重新构建，也只能使用该目录 |
| `deps/` | 第三方依赖库（lua-cjson / luasocket / lsqlite3 / luarocks / sol2），仅 `Makefile` / `fetch.sh` / `setup.sh` / `test.sh` / `sol2_smoke.cpp` 入库 |
| `tools/` | 工具脚本（`verify.sh` / `check_memory.sh` / `gc_bench_run.sh`） |
| `doc/` | Lua 官方文档 |
| `logs/` | 测试与构建日志输出目录（由 `verify.sh` 等自动创建，已 gitignore） |

## 构建系统

- **Rust**：Cargo，`Cargo.toml` 中 `crate-type = ["rlib", "staticlib"]`，产物 `target/release/lua`（解释器二进制）与 `target/release/liblua_rs.a`（静态库）。
- **C/C++**：CMake（`CMakeLists.txt`），产物 `build/lua` 与 `build/luac`，作为 `verify.sh` / `gc_bench_run.sh` 的比对基准。
- **build.rs**：默认构建链接 `dl`，并导出 `--export-dynamic` 让 dlopen 加载的 C 模块能解析 `lua_xxx` / `luaL_xxx` 符号；同时编译 `src_rs/capi_variadic.c`（stable Rust 不支持 C variadic，由该 C 文件提供 `lua_pushfstring` / `lua_pushvfstring` / `luaL_error`，并通过 `--undefined` 强制保留）。

## Feature Flags

- `ffi`（默认关闭）：启用时链接 C 实现的 `liblua`，`bindings.rs` / `parser.rs` / `lua_ffi.rs` 模块生效，`capi.rs` 模块禁用以避免符号冲突。用于编译器比对测试（`cargo test --features ffi -- compiler::cmp_tests::compiler_compare_tests`）。
- 默认（非 `ffi`）：Rust 实现自给自足，`capi.rs` 导出 C API 符号。

## 内存限制规则

测试时 `systemd-run --user --wait --collect --pipe --property=LimitAS=204800`（200MB），构建时用 `systemd-run --user --wait --collect --pipe --property=LimitAS=infinity` 绕过限制。

不遵守规则会被 hook 拦截，导致测试失败。

禁止使用 `ulimit -v 204800` 等命令限制内存，否则会导致测试失败，除非编写 shell 脚本时才能使用。

例外：`tests_lua/gc_linkedlist_*.lua` 与 `tests_lua/constructs.lua` 因虚拟内存峰值较高，`verify.sh` 内部将其限制提升至 512MB；`deps/test.sh` 因 C 模块测试需要也将限制提升至 512MB。

## 测试体系

### 一键测试入口
- `tools/verify.sh`：编译器比对 → `cargo test` → `cargo build --release` → 运行 `tests_lua/` 全套 Lua 测试（含 `all.lua`）。日志输出到 `logs/` 目录。
- `deps/test.sh`：第三方依赖库（lua-cjson / luasocket / lsqlite3 / luarocks / sol2）测试。
- `tools/gc_bench_run.sh`：C 与 Rust 实现的 GC 性能对比测试。

### 测试分类
1. **编译器比对测试**：`src_rs/compiler/cmp_tests.rs`，确保 Rust 编译器输出与 C 实现一致。
2. **Rust 单元/集成测试**：`tests_rs/` 下 7 个测试文件，由 `cargo test` 统一执行。
3. **Lua 官方测试套件**：`tests_lua/` 下 30+ 个 `.lua` 测试文件，由 `verify.sh` 调用 `target/release/lua` 执行。
4. **依赖库测试**：`deps/test.sh`，验证 Rust lua 的 C ABI 兼容性。
5. **GC 性能对比**：`tools/gc_bench_run.sh`，对比 C 与 Rust 的 GC 性能。

## 编译器改动校验

修改 `src_rs/` 下的核心数据文件或 `src_rs/compiler/` 目录时，需执行编译器比对测试（已固化到 hook），确保 Rust 输出与 C 实现一致。测试输出重定向到 `test.log` 查看。

## Hook 机制

`.claude/settings.json` 配置了两个 hook：

- **PreToolUse / Bash**：`tools/check_memory.sh`，扫描命令（包括脚本内容）是否符合内存限制规则。构建命令必须用 `systemd-run --property=LimitAS=infinity`；测试命令必须用 `systemd-run --property=LimitAS=204800` 并加 `timeout`。
- **Stop**：`tools/verify.sh`，每次 Agent 停止时自动运行编译器比对、cargo test、构建与 Lua 测试。失败时查看 `logs/compiler_test.log` / `logs/cargo_test.log` / `logs/build.log` / `logs/*_run.log`。

## CI 流程

`.github/workflows/ci.yml` 在 push/PR 到 `main` 时触发，依次执行：构建 Rust lua → 构建 C lua → `deps/setup.sh` → `deps/test.sh` → `tools/verify.sh` → `tools/gc_bench_run.sh --diff`。失败时上传 `logs/` 与 GC bench 输出作为 artifact。

## 关键编码约定

- **Rc 共享避免深拷贝**：`Proto.code` / `constants` / `upvalues` 字段使用 `Rc<Vec<...>>`，避免 `op_call` / `op_tailcall` 中的 O(n) 深拷贝。
- **NUL 终止字符串**：`ShortString` / `LongString` 从字节数据构造时必须使用 `new_short_bytes()` / `new_long_bytes()`，确保正确处理 NUL 终止。
- **TValue 布局**：`TValue` 包含 `BuiltinFn`（函数指针 + NUL 终止字符串，24 字节）与 `RustClosure`（`Rc<RustClosure>`，函数指针 + upvalues）变体，用于有状态内置函数（如 `coroutine.wrap`）。`is_callable()` 统一处理 `LightUserData` 内置 tag。
- **ffi feature 隔离**：`parser.rs` / `bindings.rs` 引用 C 符号（`luaY_checklimit` / `luaY_nvarstack`），必须在 `#[cfg(feature = "ffi")]` 下；`capi` 模块在 `ffi` feature 下不存在，任何引用 `crate::capi::` 的代码必须用 `#[cfg(not(feature = "ffi"))]` 包围或改用本地辅助函数。
- **LuaState 初始化完整性**：测试代码中初始化 `LuaState` 时必须包含所有字段（如 `func.rs` 中的 `make_vm_state`）。新增字段时同步更新所有测试初始化代码。
- **pending_return_adjust 机制**：当 return hook 启用（`hook_mask & 2 != 0`）且 `allowhook` 且 `in_op_call` 时，`adjust_results` 不立即截断栈，而是设置 pending 由调用方的 `op_call` / `op_tailcall` / `op_tforcall` 在 return hook 完成后通过 `finish_pending_adjust` 执行实际截断。任何调用 BuiltinFn 的指令都必须保证最终会执行 `finish_pending_adjust`。
- **op_tailcall 与 op_call 一致性**：`op_tailcall` 的 BuiltinFn / RustClosure 分支必须与 `op_call` 的 BuiltinFn 分支保持结构一致，包括 call hook（"tail call" 事件）、return hook（LUA_MASKRET，含 `saved_pending = take()` / `pending = saved_pending` 保存恢复）、`finish_pending_adjust()`。
- **GC 与可达性**：无 `gc_header` 的对象（如 `RustClosure` / `Thread` / `CClosure`）使用 `Rc::as_ptr` 地址判断可达性；`is_marked` / `mark_tvalue` 需覆盖所有可达对象类型。
- **collectgarbage 语义**：`collectgarbage("collect")` 在 finalizer 内调用返回 `Integer(0)`（在 Lua 中为真值），需注意 `assert(not res)` 类断言。
- **close_yield_upvals**：必须关闭所有非 TBC 开放上值（通过 `state.open_upval` 链），不仅限于 yield 值可达的；TBC 上值必须跳过（`sync_yield_upvals_back` 会重置 `tbc=false`，否则 `func::close` 会跳过 `__close` 元方法）。
- **step_gc 生成模式**：生成模式下 `step_gc` 必须直接做完整回收（未实现 minor collection），否则 `collectgarbage("step")` 无法清理弱表。
- **call_require 缓存语义**：`package.loaded[name]` 为 `false` 时必须视为"未加载"（使用 `lua_toboolean` 语义，非 nil 检查），触发重新加载；`load_lua_module` 返回值检查使用 `!lua_isnil`（非 `lua_toboolean`），`false` 是合法返回值会被缓存。
- **package.config**：必须为 `"/\n;\n?\n!\n-\n"`（DIRSEP / PATH_SEP / PATH_MARK / EXEC_DIR / IGMARK 以换行分隔）。
- **searchpath / findfile**：不能跳过空模板；`findfile` 应返回列出所有尝试路径的完整错误消息（`no file 'X'\n\tno file 'Y'`）。
- **concat_gc_interval**：保持 4096，设置为 100 会因 GC 触发过于频繁导致 `constructs.lua` 性能下降。

## .gitignore 关键排除项

`librust_out.rlib`、`luac.out`、`perf.data`、`perf.data.*`、`tests_lua/time.txt`、`.omp/` 目录、`deps/src/` / `deps/build/` / `deps/lib/` / `deps/cache/` / `deps/luarocks-root/` / `deps/*.tar.gz` / `deps/*.zip` / `deps/.test_*.log`、`logs/`、`Cargo.lock`、`build` / `build_c` / `cbuild` / `target` / `target_perf`。

## 会话接续规则

- **接续后第一件事**：检查 `logs/` 目录下是否有报错日志（`compiler_test.log`、`cargo_test.log`、`build.log`），或运行 `tools/verify.sh` 验证上次修改是否引入编译错误。
- **hook 自动报错处理**：修改 `src_rs/` 后 hook 会自动运行 `verify.sh`，如果 Terminal 中出现 "Compiler compare tests failed" 或类似报错，必须立即查看 `logs/compiler_test.log` 并修复，不要等待用户提醒。
- **ffi feature 编译检查**：`verify.sh` 用 `cargo test --features ffi` 编译，启用 ffi feature 时 `capi` 模块不存在。任何引用 `crate::capi::` 的代码必须用 `#[cfg(not(feature = "ffi"))]` 包围，或改用本地辅助函数。
- **LuaState 初始化完整性**：在测试代码中初始化 `LuaState` 时，必须包含所有字段。新增字段时同步更新 `func.rs` 等文件中的测试用 `make_vm_state` 函数。
