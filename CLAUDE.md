# 项目规则

## 项目概述

本项目是 **Lua 5.5.0 的 Rust 实现**（`lua-rs`），目标是逐步替代 `src/` 下的 C/C++ 官方实现。Rust 实现自给自足，默认通过 `capi.rs` 以 `#[no_mangle] extern "C"` 形式导出 C ABI，使第三方 Lua C 模块（`.so`）能够直接链接调用。启用 `cmp_c` feature 时改为链接 C 实现的 `liblua`，用于编译器比对测试等场景。

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
| `deps/` | 第三方依赖库（lua-cjson / luasocket / lsqlite3 / luarocks / sol2 / skynet），入库内容：`Makefile` / `fetch.sh` / `setup.sh` / `test.sh` / `run_skynet_e2e.sh` / `sol2_smoke.cpp` / `lua_dll/`（Windows 用 lua_rs.dll 的 cdylib 壳） |
| `doc/` | Lua 官方文档 |
| `bench/` | CI 性能基准目录（`harness.lua` 公共驱动 + `bench_*.lua` 按测试项拆分，每项一个文件） |
|`tools/`|工具脚本（`verify.sh` / `check_memory.sh` / `gc_bench_run.sh` / `ci_bench.sh` / `bench_fast_unsafe.sh` / `build_pgo.sh` / `build_release_opt.sh` / `ci_bench_win.ps1`）|
| `logs/` | 测试与构建日志输出目录（由 `verify.sh` 等自动创建，已 gitignore） |

## 构建系统

- **Rust**：Cargo，`Cargo.toml` 中 `crate-type = ["rlib", "staticlib"]`，产物 `target/release/lua`（解释器二进制）与 `target/release/liblua_rs.a`（静态库）。
- **C/C++**：CMake（`CMakeLists.txt`），产物 `build/lua` 与 `build/luac`，作为 `verify.sh` / `gc_bench_run.sh` 的比对基准。
- **build.rs**：默认构建链接 `dl`，并导出 `--export-dynamic` 让 dlopen 加载的 C 模块能解析 `lua_xxx` / `luaL_xxx` 符号；同时编译 `src_rs/capi_variadic.c`（stable Rust 不支持 C variadic，由该 C 文件提供 `lua_pushfstring` / `lua_pushvfstring` / `luaL_error`，并通过 `--undefined` 强制保留）。

## Feature Flags

- `fast_unsafe`（**默认启用**）：VM 热路径 unsafe 优化总开关。启用时使用：指令/常量流裸指针缓存（`execute.rs` 主循环的 `code_ptr`/`constants_ptr`，免每指令 `Rc→Vec` 双 deref 与边界检查）、栈读写 `as_mut_ptr` + `get_unchecked`（跳过边界检查）与 `ptr::write`/`set_len`（跳过 trivially-droppable 旧值的 drop_glue）、upvalue 访问 `RefCell::as_ptr`（跳过 borrow 计数）、表读路径 `Table::data_ro`（跳过 `RefCell::borrow`）、`string` 模式匹配 `get_unchecked`、`math` 单参纯函数 `get_unchecked`。关闭时（`cargo build --release --no-default-features`）以上全部改为安全等价实现（Rc 句柄 + 边界检查 + 常规 drop + borrow 守卫 + clone-owned），语义逐位不变，用于量化性能代价 / 安全审查。
  - 量化脚本：`bash tools/bench_fast_unsafe.sh [quick|full] [轮数]` — 构建 on/off 两个二进制后交错采样，输出每指标 on/off 最小耗时比值与几何平均（结果写 `logs/fast_unsafe_ab/`）。
  - 不在开关范围内（这些 unsafe 属架构/FFI 必需，不是性能技巧）：FFI/C ABI（`capi.rs` / `io_lib` / `os_lib` / `base_lib` dlopen）、自定义分配器（`alloc.rs` / `mem.rs`）、GC 裸指针与 `RawTValue`（`gc.rs`）、`TString` 指针表示与外部字符串（`strings.rs`）、`MaybeUninit` 捕获数组（`string_lib.rs`）、枚举 `transmute`（`opcodes.rs`，已由范围检查保证）、`lua_State` 裸指针协程（`coroutine_lib.rs`）、`from_utf8_unchecked`（Lua 字节串语义，换成校验会改变行为）。
- `cmp_c`（默认关闭）：启用时链接 C 实现的 `liblua`，`bindings.rs` / `parser.rs` / `lua_ffi.rs` 模块生效，`capi.rs` 模块禁用以避免符号冲突。用于编译器比对测试（`cargo test --features cmp_c -- compiler::cmp_tests::compiler_compare_tests`）。
- 默认（非 `cmp_c`）：Rust 实现自给自足，`capi.rs` 导出 C API 符号。

## 数值类型配置（对应 luaconf.h 的 LUA_INT_TYPE / LUA_FLOAT_TYPE）

`src_rs/config.rs` 的 `LuaInteger` / `LuaUnsigned` / `LuaNumber` 是全局唯一的数值类型来源，默认等于 C 的默认配置（`long long` + `double`），可通过 cargo feature 切换到嵌入式32位配置：

| feature | `LuaInteger` / `LuaUnsigned` / `LuaNumber` | C 对应 |
|---|---|---|
| （无，默认） | `i64` / `u64` / `f64` | `LUA_INT_LONGLONG` + `LUA_FLOAT_DOUBLE` |
| `num-i32` | `i32` / `u32` / `f64` | `LUA_INT_TYPE = LUA_INT_INT` |
| `num-f32` | `i64` / `u64` / `f32` | `LUA_FLOAT_TYPE = LUA_FLOAT_FLOAT` |
| `embedded` | `i32` / `u32` / `f32` | `LUA_32BITS` |

用法：`cargo build --release --features embedded`（可与默认 feature 叠加）。不支持 long double（Rust stable 无 f80/f128）。

**约束（与 C 的 luaconf.h 一致）**：任何与 Lua 连接的外部 C 代码必须使用同一配置 —— `capi.rs` / `lua_ffi.rs` 的 `lua_Integer` / `lua_Number` / `lua_Unsigned` 直接绑定上述别名（对应 `typedef LUA_INTEGER lua_Integer;`），变更配置即变更这些 C ABI 类型的宽度。

唯一来源的配套常量/函数（`config.rs`，**禁止在别处硬编码宽度**）：`MAX_INTEGER` / `MIN_INTEGER` / `MAX_UNSIGNED` / `INTEGER_BITS` / `INTEGER_FMT` / `NUMBER_FMT` / `NUMBER_FMT_N` / `number_to_integer`（C `lua_numbertointeger`）/ `int_fits_float`（C `l_intfitsf`）。

编码约定：
- **值位置用别名**：流入/流出 `TValue::Integer`/`Float`、C API 数值函数、或构成 Lua 可见结果的量一律用 `LuaInteger`/`LuaNumber`/`LuaUnsigned`；纯内部偏移/下标/长度（`usize`、pc、文件偏移、`c_long`）保持原类型。
- **宽度假设必须参数化**：`i64::MAX`/`i64::MIN`/`u64::MAX` → `MAX_INTEGER`/`MIN_INTEGER`/`MAX_UNSIGNED`；`1u64 << n` → `LuaUnsigned`；移位位数 `63`/`64` → `INTEGER_BITS`；`%lld` → `INTEGER_FMT`；`%.15g`/`%.17g` → `NUMBER_FMT`/`NUMBER_FMT_N`。
- **位运算/移位/取模/整除一律经 `LuaUnsigned`**（C 的 `luaV_shiftl` / `luaV_mod` / `luaV_idiv`）；`MAXSIZE` 类上限按 C 定义 `size_of::<usize>() < size_of::<LuaInteger>() ? usize::MAX : MAX_INTEGER as usize`。
- 浮点库函数用 `LuaNumber` 的固有方法（`floor`/`ceil`/`fmod`/`powf`…），不要写 `f64::` 限定路径（C 的 `l_mathop` 对应「按类型选函数」）。
- **假设 64 位/双精度的测试**必须用 `#[cfg(not(feature = "num-i32"))]` / `#[cfg(not(feature = "num-f32"))]` 门控，不得为编译而删除测试。
- 验证入口：`bash tools/verify_num_types.sh`（4 种配置各自 release 构建 → 跑 `tests_lua/num_types_smoke.lua`（断言与位宽无关）→ 测试目标编译检查；`--full` 追加 `cargo test`）。

## 内存限制规则

测试时 `systemd-run --user --wait --collect --pipe --property=LimitAS=204800`（200MB），构建时用 `systemd-run --user --wait --collect --pipe --property=LimitAS=infinity` 绕过限制。

不遵守规则会被 hook 拦截，导致测试失败。

禁止使用 `ulimit -v 204800` 等命令限制内存，否则会导致测试失败，除非编写 shell 脚本时才能使用。

例外：`tests_lua/gc_linkedlist_*.lua` 与 `tests_lua/constructs.lua` 因虚拟内存峰值较高，`verify.sh` 内部将其限制提升至 512MB；`deps/test.sh` 因 C 模块测试需要也将限制提升至 512MB。

## 命令执行规则

禁止使用 `sleep N | exe` 形式的管道命令。原因：shell 会等待管道中所有命令结束，`exe` 退出后 `sleep` 仍会继续运行至计时结束，造成大量无效等待（如 `lua` 1 秒结束，整体却要等 300 秒）。`tools/check_sleep_pipe.sh` hook 会拦截此类命令。

禁止使用 `exe < /dev/null` 重定向 stdin。原因：`all.lua` 等测试用例会从 stdin 读取输入，stdin 立即 EOF 会导致测试用例报错失败。

替代方案（按用途选择）：

- 仅需 stdin 不可 seek（如 `tests_lua/files.lua:88` 的 `io.stdin:seek` 场景）：使用 `printf '' | exe`，空管道立即 EOF 且 stdin 不可 seek。
- 需少量数据输入：使用 `echo "..." | exe` 或 `printf '%s\n' "..." | exe`。
- 需从文件读取：使用 `exe < file`。
- 不需要 stdin：使用 `printf '' | exe`（空管道）或直接放任 stdin 继承终端。
- 需超时控制：使用 `timeout N exe`。

## 测试体系

### 一键测试入口
- `tools/verify.sh`：编译器比对 → `cargo test` → `cargo build --release` → 运行 `tests_lua/` 全套 Lua 测试（含 `all.lua`）。日志输出到 `logs/` 目录。`cargo test` 跑两遍：默认 features 与 `--features lua_longjmp,threaded`（deps/setup.sh 构建 skynet 版 lua 的配置，覆盖 `StringTable` 的 parking_lot 锁 + `SimpleArc` 分支与并发 intern 的字符串身份/去重计数回归，日志 `logs/cargo_test_threaded.log`）。
- `deps/test.sh`：第三方依赖库（lua-cjson / luasocket / lsqlite3 / luarocks / sol2 / skynet abort）测试；`skynet/e2e` 仅 POSIX（Windows 跳过，服务端能力由 abort 覆盖）。注意 **CI 只在 Linux 跑 deps**（Drone Windows 流水线只有 build+bench，无 MinGW 工具链），Windows 侧的 `deps/Makefile` 导入库规则 / `lua_rs.dll` / mingw.mk 补丁只能靠本地 `bash deps/setup.sh` + `bash deps/test.sh` 验证（实测 10 通过 0 失败 + skynet/e2e 跳过）。
- `tools/gc_bench_run.sh`：C 与 Rust 实现的 GC 性能对比测试。
- `tools/ci_bench.sh`：C 与 Rust 实现的通用性能基准（12 项解释器热点，bench/ 按项拆分），Drone 两条流水线 + GitHub Actions Linux job 均执行，日志 `logs/ci_bench_*`。
- `tools/miri.sh`：Miri UB 检测（本地工具，需 nightly 工具链）。检测 `tests_rs/` 与 `src_rs/` 单元测试中的未定义行为（越界访问、未初始化内存、Tree Borrows 违规等）。日志输出到 `logs/miri_test.log`。
- `tools/verify_num_types.sh`：数值类型配置验证（默认 / `num-i32` / `num-f32` / `embedded` 各构建 release 并跑 `tests_lua/num_types_smoke.lua` + 测试目标编译检查，`--full` 追加 `cargo test`）。详见"数值类型配置"章节。
- `tools/verify_alloc_fail.sh`：内存分配失败处理测试（`tests_lua/alloc_fail.lua`），**主动**给进程加 200MB 地址空间上限（`systemd-run --property=LimitAS=204800`，退化 `ulimit -v 204800`；两者都不可用时跳过而不是无限运行）。`tools/verify.sh` 已调用它，本地单独运行：`bash tools/verify_alloc_fail.sh`。日志 `logs/alloc_fail*.log`。详见"内存分配失败处理"章节。

### 测试分类
1. **编译器比对测试**：`src_rs/compiler/cmp_tests.rs`，确保 Rust 编译器输出与 C 实现一致。
2. **Rust 单元/集成测试**：`tests_rs/` 下 7 个测试文件，由 `cargo test` 统一执行。
3. **Lua 官方测试套件**：`tests_lua/` 下 30+ 个 `.lua` 测试文件，由 `verify.sh` 调用 `target/release/lua` 执行。
4. **依赖库测试**：`deps/test.sh`，验证 Rust lua 的 C ABI 兼容性。
5. **GC 性能对比**：`tools/gc_bench_run.sh`，对比 C 与 Rust 的 GC 性能。
6. **CI 性能基准**：`tools/ci_bench.sh`，对比 C 与 Rust 实现的 12 项解释器热点性能（bench/ 按项拆分），quick/full 两种规模。
7. **Miri UB 检测**：`tools/miri.sh`，用 Miri 解释执行 Rust 测试检测 UB。覆盖范围与限制见下方"Miri 检测规则"章节。

## 编译器改动校验

修改 `src_rs/` 下的核心数据文件或 `src_rs/compiler/` 目录时，需执行编译器比对测试（已固化到 hook），确保 Rust 输出与 C 实现一致。测试输出重定向到 `test.log` 查看。

## Miri 检测规则

### 使用方式
```bash
bash tools/miri.sh                # 运行全部可检测测试（首次会自动安装 nightly + miri）
bash tools/miri.sh --setup        # 仅安装 nightly + miri 组件
bash tools/miri.sh --filter math  # 仅运行名称含 "math" 的测试
bash tools/miri.sh --lib          # 仅运行库单元测试 (src_rs/)
bash tools/miri.sh --no-log       # 不写日志，直接输出到终端
```

### 检测范围
- **覆盖**：`tests_rs/` 7 个集成测试 + `src_rs/` 22 个含 `#[cfg(test)]` 的模块单元测试
- **跳过**：
  - `cmp_c` feature（链接 C liblua，Miri 无法解释 C 代码）
  - `src_rs/compiler/cmp_tests.rs`（依赖 `cmp_c` feature）
  - `tests_rs/integration_tests::test_stdin_execution`（`Command::spawn` 启动子进程，Miri 不支持）
  - 任何调用 `lua_pushfstring` / `luaL_error`（来自 `capi_variadic.c`）的测试路径

### 检测模型
- **Tree Borrows**（`-Zmiri-tree-borrows`）：比 Stacked Borrows 宽松，适合本项目 `Rc` / `Arc` 重度共享场景，减少误报
- **禁用环境隔离**（`-Zmiri-disable-isolation`）：允许测试访问文件系统与环境变量（`env!("CARGO_MANIFEST_DIR")` 等）

### 编码约定
- 新增测试若涉及 `Command::spawn`、`dlopen`、`extern "C"` 调用 C 代码、文件 IO、网络等 Miri 不支持的操作，必须用 `#[cfg_attr(miri, ignore)]` 标注跳过，并附注释说明原因
- 新增 `unsafe` 块后建议运行 `bash tools/miri.sh --filter <相关测试名>` 验证无 UB
- Miri 是本地工具，不集成 CI；开发者按需运行，日志输出到 `logs/miri_test.log`

### 已知 Miri 兼容性处理
- **io 库初始化跳过**：`src_rs/state.rs` 的 `open_selected_libs` 中，`open_io_lib` 用 `#[cfg(not(miri))]` 包围。原因：Miri 不支持 `extern static stdin/stdout/stderr`（`src_rs/stdlib/io_lib.rs`）与 `fdopen`。跳过后 `io` 全局表不存在，但 `print` 用 `state.stdout`（Rust `Box<dyn Write>`）不受影响。
- **stdin_execution 测试跳过**：`tests_rs/integration_tests::test_stdin_execution` 用 `#[cfg_attr(miri, ignore)]` 跳过。原因：`Command::spawn` 启动子进程，Miri 不支持。
- **信号处理跳过**：`src_rs/cli.rs` 的 `setup_signal_handler` / `reset_signal_handler` 用 `#[cfg(not(miri))]` 包围。原因：Miri 不支持 `sigemptyset` / `sigaction` 等信号操作。
- **strcoll 替代**：`src_rs/vm.rs` 的 `strcoll_compare` 在 Miri 下用字节比较（`a.cmp(b)`）代替 `libc::strcoll`。原因：Miri 不支持 `strcoll`。默认 "C" locale 下两者行为一致。
- **内存泄漏忽略**：MIRIFLAGS 中加 `-Zmiri-ignore-leaks`。原因：LuaState 中 Rc 循环引用（如 metatable/registry）导致测试结束时内存未释放，这是已知设计特性，不是 UB。
- **函数指针比较跳过**：`test_call_ipairs` / `test_call_pairs` 用 `#[cfg_attr(miri, ignore)]` 跳过。原因：Miri 下函数指针地址比较不可靠（`bf.func as usize != fn as usize`）。
- **浮点精度测试跳过/放宽**：`test_math_log_base_10` 跳过（`log10(100)` 输出 `1.9999999999999998` 而非 `2.0`）；`test_math_log_base` 容差在 Miri 下放宽至 `1e-10`。原因：Miri 浮点运算精度略低。

### 已修复的 UB
- **编译器 aliasing 违规**（已修复）：`src_rs/compiler/compile.rs` 中 `FuncState` 通过 `ls: *mut LexState` 裸指针持有 `LexState` 引用。在 `compile_chunk` 和 `parse_func_body` 中，`FuncState::new(ls)` 后仍直接使用 `ls`（如 `ls.next()`），这是 foreign write，导致 `FuncState` 持有的 Tree Borrows 标签变为 Disabled。修复方案：把 `FuncState::new` 后所有直接使用 `ls` 的地方改为通过 `fs.ls()` / `fs.ls_mut()` 访问，用临时变量分离借用范围避免与 `fs.proto` 冲突。
- **内存分配对齐不匹配**（已修复）：早期 `src_rs/mem.rs` 的类型化分配辅助（`new_vec<T>` / `grow_vec<T>` / `shrink_vec<T>` / `new_box<T>`，已作为死代码删除）用 `Layout::from_size_align(new_size, 1)` 硬编码 1 字节对齐分配，而返回的 `Vec<T>` / `Box<T>` 在 drop 时按 `T` 的对齐（如 `i32` 为 4 字节）释放，导致对齐不匹配 UB。修复方案：类型化容器（`AllocBox` / `AllocVec`）统一经 `Allocator` trait 的 `alloc(ptr, old, new, align)` 路由（`DefaultAllocator` / `SlotAlloc`），`align` 由容器自身的 `Layout` 提供，分配与释放对齐天然一致。例外：`CapiAllocator` 走宿主 `lua_Alloc` 的 1 字节对齐 realloc 语义，只管理 C 侧内存，不得交给 `Box<T>` / `Vec<T>` 的 std drop 路径。

## Hook 机制

`.claude/settings.json` 配置了以下 hook：

- **PreToolUse / Bash**：
  - `tools/check_memory.sh`：扫描命令（包括脚本内容）是否符合内存限制规则。构建命令必须用 `systemd-run --property=LimitAS=infinity`；测试命令必须用 `systemd-run --property=LimitAS=204800` 并加 `timeout`。
  - `tools/check_sleep_pipe.sh`：拦截 `sleep N | exe` 形式的命令。详见"命令执行规则"章节。
- **Stop**：`tools/verify.sh`，每次 Agent 停止时自动运行编译器比对、cargo test、构建与 Lua 测试。失败时查看 `logs/compiler_test.log` / `logs/cargo_test.log` / `logs/cargo_test_threaded.log` / `logs/build.log` / `logs/*_run.log`。

## CI 流程

`.github/workflows/ci.yml` 仅手动触发（workflow_dispatch），依次执行：构建 Rust lua → 构建 C lua → `deps/setup.sh` → `deps/test.sh` → `tools/verify.sh` → `tools/gc_bench_run.sh --diff` → `tools/ci_bench.sh --skip-build full`。失败时上传 `logs/` 与 bench 输出作为 artifact。

`.drone.yml` 定义 Drone CI 双流水线：`rust-linux`（docker，镜像 `lua-ci:latest`，见 `ci/Dockerfile`）执行 `cargo build/test`（含 `--features lua_longjmp,threaded` 全套，覆盖 threaded 模式的 `StringTable` 锁实现与并发 intern 回归）→ CMP 编译器比对测试（`cargo test --features cmp_c -- compiler::cmp_tests::compiler_compare_tests`）→ deps 依赖库测试（构建 C lua 后运行 `deps/setup.sh` + `deps/test.sh`，含 skynet e2e）→ bench 性能基准（`bash tools/build_release_opt.sh` 构建 bench 二进制后 `bash tools/ci_bench.sh --skip-build full`）→ `fast_unsafe` on/off 性能差距（`bash tools/bench_fast_unsafe.sh quick 3`，普通 release 构建两变体交错采样，不干扰上一步的 PGO 产物）；`rust-windows`（exec）执行 `cargo build/test` → bench 性能基准（经 `tools/ci_bench_win.ps1` 包装：初始化 VS DevShell 提供 cmake，剥离 VS 环境变量后调 Git Bash 运行 `tools/ci_bench.sh full`，脚本内自建 C lua 与 Rust 二进制）。bench 构建策略全部内聚在 `tools/build_release_opt.sh`：默认 PGO（`build_pgo.sh` 三步：instrumented → tests_lua+bench quick 通用负载训练 → profile-use 重建）+ `-Cpanic=abort` + `lua_longjmp` feature（C 模块错误边界用 setjmp/longjmp 替代 catch_unwind）；`BENCH_PGO=0` / `BENCH_ABORT=0` 显式退档；任一步失败自动回退普通构建（`PGO_FALLBACK` 日志标记）。drone 命令行必须保持单条无内联逻辑（Windows exec runner 对全部 pipeline commands 做启动期 bash 解析，内联 `VAR=x` 前缀会泄漏成 cargo 参数）。ci_bench.sh 采用 C/Rust **交错采样**（每轮先 C 后 Rust，指标级取全部轮次最小值；linux full 5 轮、windows/quick 3 轮），比值噪声 ~±1-2%——低于 ~1.5% 的改动不可判定，判定性能改动必须看 CI 比值多轮均值而非本地 plain build（本地收益在 PGO 下常归零甚至反号）。`save-cache` 步骤 `failure: ignore`（宿主缓存盘写满不连累流水线）。镜像需在 Drone 宿主机预构建（`docker build -t lua-ci:latest -f ci/Dockerfile .`），修改 `ci/Dockerfile` 后需重建镜像。

## 内存分配失败处理（alloc_error）

对应 C 的 `lmem.c`（`luaM_realloc_` / `tryagain` / `luaM_error`）与 `ldo.c` 的 `LUA_ERRMEM` 语义：**分配失败不中止进程**，而是「先紧急 GC、再重试分配」，重试仍失败才抛**可捕获**的内存错误。

### 机制

| 概念 | Rust 实现 | C 对应 |
|---|---|---|
| 分配失败信号 | `mem::AllocFailed`（内部控制流，不直接对 Lua 可见） | `luaM_realloc_` 返回 NULL |
| 重试漏斗 | `LuaState::retry_alloc` / `retry_alloc_mut`：尝试 → 紧急全量 GC → 重试 → `VmError::MemoryError` | `luaM_realloc_` + `tryagain` |
| 紧急回收 | `LuaState::emergency_gc`（置 `GCState.gcemergency` 后跑完整回收） | `luaC_fullgc(L, 1)` |
| 紧急模式语义 | 不运行 finalizer（不可达的待 finalize 对象**标活并保留**到下次常规回收）、不缩栈、不回调 Lua | `gcemergency` 下不执行 finalizer / 不 `luaD_shrinkstack` |
| 可否重试 | `LuaState::can_retry_alloc` = `mem.complete_state && !gc_running` | `cantryagain(g)` |
| 内存错误 | `VmError::MemoryError`（无负载，`Display` = `strings::MEMERRMSG`） | `LUA_ERRMEM` |
| 错误对象 | `GlobalState.memerrmsg`（状态创建时预注册的字符串） | `G(L)->memerrmsg` + `luaD_seterrorobj` 的 LUA_ERRMEM 分支 |
| 按状态码返回的入口 | `load_buffer` / `load_bytes` 返回 `ERR_MEM`（= `LUA_ERRMEM` = 4），错误对象留在栈上 | `lua_load` 返回 `LUA_ERRMEM` |

内存错误的传播路径**全程零分配**：错误对象直接复用 `memerrmsg`（不重新 intern），不加 `source:line` 前缀（`build_traceback` 对内存错误短路，连 traceback 字符串都不构造），`last_error_msg` 用 `try_reserve` 写静态文本（容量不足则留空，消费者回退到错误对象）。

### 编码约定（新增分配点必须遵守）

- 生产路径一律走漏斗入口，禁止再用会 abort 的构造：`MyRc/SimpleRc/SimpleArc::try_new_in`（失败时**原样交还 payload**）→ `LuaState::new_object`；`Table::try_new_in` / `try_with_capacity_in` → `LuaState::new_table` / `new_table_with_capacity`；userdata 一律经 `LuaState::new_udata`（构造 + 对象块分配 + GC 登记 + id 写回一体，不得在调用点手写 `new_object` + `register_object` + `set_id` 三连）；Lua 闭包一律经 `LuaState::new_closure`（同四步一体；`OP_CLOSURE` / `load_buffer` / undump / `load` 四条路径统一走它 —— 其中 `load` 原先漏了 GC 登记，闭包不计入 `gc_estimate`）；表写入 `LuaState::table_set` / `table_set_int`；栈扩容 `LuaState::reserve_stack`。`#[cfg(test)]` 下的 `new_in` / `Table::new_in` / `Table::set` 等只是**测试专用**便捷包装（失败即 panic），生产代码不得使用。
- **先扩容再写入**：失败必须不留半写状态（对应 C 的 `luaM_saferealloc_` 只在成功后更新 `*psize`）。表写入即按此拆成 `Table::try_reserve_for`（可失败，只做容量预留）+ `Table::set_unchecked` / `set_int_unchecked`（无分配写入）；`try_reserve_for` 失败时表内容不变，因此漏斗可以安全地「紧急 GC 后整体重试」。`TableData::idx_insert` 同样要求调用点先用 `try_reserve_index` 备好容量。
- 漏斗闭包必须**可重复执行**：失败时不得留下部分写入，且要把数据交还出来供重试继续（`MyRc::try_new_in` 的 `Err(payload)` 形态、`Some(value).take()` 形态）。
- C API（`capi.rs`）没有返回错误的通道：失败必须**抛错**（`raise_mem_error`，复用与 `lua_error` 相同的 panic/longjmp 机制），对应 C 的 `luaM_error` → `luaD_throw`。
- `lua_newstate` 语义：状态创建期分配失败返回 NULL（此时还无法做紧急回收，对应 C 的 `cantryagain` 为假）。

### 测试

- `tests_rs/alloc_failure_tests.rs`：可编程分配器（`FlakyAlloc`，对应 C 测试库 ltests 的 `T.alloccount`）**精确**制造失败（不依赖真实内存耗尽、不 flaky），覆盖「一次性失败被紧急 GC + 重试抹平且垃圾确实被回收」「不可恢复失败抛出可捕获的 `not enough memory`」「错误后解释器 / GC / 弱表仍正常」。
- `tests_lua/alloc_fail.lua` + `tools/verify_alloc_fail.sh`：在 200MB 地址空间上限下**真实耗尽**内存（对应 C 的 `memerr.lua`，但不需要 ltests），验证真耗尽时不崩溃、错误可捕获、错误后仍可用；并额外验证**顶层**（未被 `pcall` 捕获）内存错误以消息 + 非零退出码结束，而不是被信号杀死。

### 尚未纳入漏斗的分配路径（后续迁移，当前仍会 abort）

**实测结论**（200MB 地址空间真实耗尽，`tools/verify_alloc_fail.sh`）：耗尽时仍会 abort，
最后失败的是 **align=8 / 16384 字节的 realloc**（= `Vec<TValue>` 一类 16 字节元素容器的扩容）。
定位手段（本机已用、勿重复造）：零分配的全局分配器探针（`WriteFile` 直写 stderr +
`RtlCaptureStackBackTrace` 取调用栈，**不能**用 `eprintln!`/`format!` —— 它们在分配器内递归，
表现为 "has overflowed its stack"），再用 `llvm-symbolizer --obj=target/perf/lua.exe
--inlines 0x140000000+<RVA>` 离线符号化（MSVC 用 PDB，偏移要加 preferred base `0x140000000`）。
探针栈显示失败点在 `LuaState::pcall` 的错误路径（`truncate_live` / 错误值入栈一段）内。

已知未迁移的类别：

- **错误路径自身的分配**：`pcall` 错误路径把栈值搬进 `c_safety_keepalive`（`split_off` 建新
  Vec + `extend` 增长，已改为**可失败预留**，失败退化为 `truncate_live`）；`call_info.clone()`
  快照、错误消息 `.to_string()`/`format!` 仍会分配。对应 C 在此不需要分配：它的栈是数组、
  值原地留存（`L->top = ci->func` 不释放），错误对象复用 `G(L)->memerrmsg`。
- **帧入口栈扩容**：`execute.rs::fast_resize_stack_nil` 与 `state.rs::pad_frame_scratch_slow`、
  `pcall` 帧预置、`capi` 的 `lua_settop`/`lua_checkstack` 已接漏斗（失败→紧急 GC→重试），
  **但失败后的上报走 `LuaState::raise_memory_error`（panic / longjmp）**：panic 载荷与展开
  本身要分配，在真耗尽时可能再次失败并 abort —— 因此这条路径只能算「尽力」，根治需要把它们
  全部改成**返回 `Result` 的错误通道**（约 50 处调用点的签名扩散），或改用真正无分配的
  setjmp/longjmp 抛出（`lua_longjmp` 构建已有该机制，默认构建没有）。
- **字符串创建与内部化**：`str_to_ls` / `ArcRc::<ShortString>::new_sstr` / `StringTable` 的
  hashbrown `HashTable` 增长（`insert_unique` 不可失败）。`intern_bytes(&self)` 无 `&mut`/错误
  通道，需先改签名。
- **编译器与解析器的 `Vec`**（编译期内存不足仍中止）。

### 测试与 CI 现状（勿误读）

- `tests_rs/alloc_failure_tests.rs`（6 个用例，可编程分配器精确制造失败）**已由 CI 的
  `cargo test` 覆盖**，全绿：一次性失败被「紧急 GC + 重试」抹平且垃圾确实被回收、不可恢复
  失败抛出可捕获的 `not enough memory`、错误后解释器/GC/弱表仍正常、紧急 GC 延迟 finalizer。
- `tests_lua/alloc_fail.lua` + `tools/verify_alloc_fail.sh`（**主动** 200MB 上限）**未接入
  `tools/verify.sh`/CI**：真实耗尽路径仍会 abort（见上），接入会让流水线常红。补齐错误通道后
  应接回 `verify.sh`。

## longjmp 模式的 Drop 安全契约（`lua_use_longjmp`）

`lua_use_longjmp`（bench 构建默认：`tools/build_release_opt.sh` 的 PGO + `panic=abort` + `lua_longjmp`；C 模块路径：`deps/setup.sh` 的 `--features lua_longjmp,threaded,skynet`）下，C 侧用
`setjmp` / `longjmp`（`capi_variadic.c` 的 `lua_rs_pcall_c` / `lua_rs_longjmp`）替代 `catch_unwind`。
**longjmp 不执行被跳过帧的析构**，因此实现必须满足以下四条（改动 capi / 状态机时逐条对照）：

1. **raise 点不得持有需要 Drop 的值**。`lua_error` / `luaL_argerror` / `luaL_typeerror` /
   `luaL_checklstring` / `luaL_checkstack` / `capi::raise_mem_error` 的抛出点必须只持有裸指针
   与已 move 进 `pending_error` 的值 —— 局部 `Rc`/`Vec`/`String`/`CString`/`RefCell` 守卫一律在
   抛错前显式 `drop(..)` 或用块作用域收掉（`raise_mem_error` 自己也要先 `drop(e)`）。
   日常错误路径的消息先压栈、再在不持有 Rust 局部的帧里抛（`luaL_typeerror` 即按 C 的
   `luaPushfString` 形态：把消息压成 Lua 字符串后传栈上指针）。
2. **每个 Lua 层 pcall 都是保护点**（对应 C 的 `luaD_pcall` / `luaD_rawrunprotected`）。
   `LuaState::pcall` 在 longjmp 模式下用 `ProtectedExec` + `protected_exec_trampoline`
   （`state.rs`）建立 setjmp 边界；没有它，`raise_memory_error` 会 longjmp 到外层 C 边界，
   无边界时直接 `exit(1)`，并且会跳过 pcall 帧保存的 VM 寄存器（`code`/`constants`/`base`/`pc`/
   `closure_upvals`/`tbc_list`/`call_stack`）导致随后按错误函数的字节码继续执行。
3. **帧入口的容量预留必须先做、且走漏斗**。`LuaState::pcall`、`pcall_c_function`、
   `execute.rs::call_c_function` 的栈预留都在任何 state 变更**之前**，失败按 `ERR_MEM` /
   `VmError::MemoryError` 返回（对应 `luaD_precall` 先 ensurestack；C 的 `luaD_pcall` 也保护
   帧入口的 `luaD_growstack`）。VM 内**没有错误通道**的扩容点（`fast_resize_stack_nil`、
   `pad_frame_scratch_slow`、`adjust_results_on_stack`）仍是 raise，但必须落在保护边界之内。
4. **被 longjmp / panic 跨过的 Rust 帧必须是 `C-unwind`**。POSIX 的 `longjmp` 不跑析构（值会泄漏）；
   Windows/MSVC 的 `longjmp` 经 `RtlUnwind` 实现，会真正执行被跨过帧的清理，但展开经过任何
   Rust `extern "C"`（nounwind）帧都会把进程 abort（编译器插入的 `panic_cannot_unwind` 落地垫，
   实测 `exit code 0xc0000409`）—— `protected_exec_trampoline` 因此声明为 `extern "C-unwind"`
   并 transmute 成 C 函数指针。**同一根因也存在于默认（非 longjmp）构建**：那里 `raise_mem_error`
   是 `panic!`，panic 从 nounwind 的 capi 入口向外展开同样 abort（与平台无关）。
   因此规约是：**`capi.rs` 的 `#[no_mangle] pub` 导出入口一律 `pub [unsafe] extern "C-unwind" fn`**
   （149 个入口里的 148 个；C ABI 逐位不变，只多「允许展开穿过」的语义）。唯一例外：`luaL_alloc`
   —— 它作为 `lua_Alloc` 值传给 `lua_newstate`，必须保持 `extern "C"` 才能与 `lua_Alloc`
   类型匹配；它自身永不抛错（分配原语失败只是返回 NULL），不会被 raise 穿过。
   被跨过帧还包含 C 模块自己的帧（`deps/Makefile` 加 `-fexceptions` / `-EHsc` 保证可展开）。
   平台差异意味着：**同一份代码在 Windows 上不泄漏、在 POSIX 上泄漏**，所以第 1 条按 POSIX 标准写。

**未覆盖的残留（诚实记录，勿误读为"已完全安全"）**：

- **VM 派发帧持有的函数值**（POSIX raise 泄漏）：已按所有权纪律修掉 —— `op_call_generic` /
  `op_tailcall` 在调用前显式 `drop(func_val)`（`match func_val` 的 `_` 臂不移动它，
  `RustClosure` 的 Rc 会活到函数末尾），`op_call_generic` / `op_tailcall` / `op_tforcall`
  的 `CClosure` 臂在 `call_c_function` 前取出 `cc.f` 并释放闭包引用（`op_tforcall` 的
  scrutinee 是 `&func_val`，须 `mem::replace` 取走），`op_vararg` 的 PF_VATAB 分支把
  vararg 表的克隆推迟到 `ensure_scratch_to` **之后**（扩容点前只借用栈槽读标量 `n`）。
  被调函数经栈槽读自己的 upvalues，不依赖这些克隆；回归 `longjmp_tests::c_closure_error_paths_do_not_leak`。
  仍然残留：协程恢复/结束路径的 `co.pad_frame_scratch()`（栈被 `mem::take` / 换成克隆快照后
  再补齐，失败时 `co` 自己没有 jmp_buf → 打印后 `exit(1)`）。
  另一处是 `--no-default-features`（安全版）+ `lua_longjmp` 的组合：安全版的 `execute_loop` 用
  **循环级** Rc 句柄 `code_rc` / `constants_rc` 代替裸指针缓存（安全版不能像 fast 版那样靠裸指针
  绕借用），longjmp 跨过主循环帧时会各泄漏一次克隆；默认 panic 构建下 Rust 展开会正常析构，
  故只有该组合受影响，而该组合当前无人使用（bench / C 模块构建都开 `fast_unsafe`）。
  `write_stack` 的 `val` **不在**泄漏之列（赋值即 move，赋值前没有 raise 点）。
  Linux 侧回归证据（CI #230）：`--features lua_longjmp --test longjmp_tests` 4 个用例全过且
  **零泄漏字节输出**（POSIX 是唯一能检出泄漏的平台；Windows 因清理会运行而恒为 0）。
- 真 OOM（200MB 地址空间耗尽）下错误路径自身的分配仍可能 abort，见上面"内存分配失败处理"一节的
  未迁移清单（字符串内部化 / `push_*` 增长 / 解析器 Vec 等没有错误通道的分配点，仍走
  `raise_memory_error` 之外的原生 abort 路径）；`lua_use_longjmp` 只替换了抛出/捕获机制，不改变这条。
- **既有异常（本轮诊断中发现，未修）**：深帧内存错误被捕获后**复用同一份预先加载的 chunk**
  再执行，会得到 `attempt to index a nil value (upvalue '_ENV')` —— 该 chunk 闭包的上值本身完好
  （`LClosure.upvals` 仍是 `{Table(globals)}`），但执行期读到的 `_ENV` 为 nil；同一时刻
  `LuaState::pcall` 错误返回后 `state.top` 可能停在大于 `stack.len()` 的旧值（实测 top=12 /
  len=4）。触发条件：Rust 侧用 `load_buffer` / `push_value` 压栈（**不推进 `top`**）之后，注入
  大额分配失败 → 紧急 GC 与错误恢复路径交错。**每轮重新编译 chunk 则不触发**（`longjmp_tests`
  的深帧泄漏测量即按此写法）。已在 `790a226`（本次修复之前的 `execute.rs`）上复现 → 与本次
  改动无关，属错误恢复路径的既有问题，待单独定位。待查线索（假设，未证实）：`op_closure` 在
  `parent_idx >= state.closure_upvals.len()` 时补的是 Nil 闭包（`execute.rs` 7470 附近），
  若某帧执行期 `state.closure_upvals` 恰好是 `pcall` 装上的临时空表（`state.rs` 1313），新建
  闭包的 `_ENV` 就是 Nil。
- 热路径扩容点尚未全部改成 `Result` 通道（约 50 处签名扩散），所以 `pad_frame_scratch` /
  `fast_resize_stack_nil` / `adjust_results_on_stack` 在 VM 内仍是 raise；当前靠「帧入口预预留
  + 上限有界」保证这些点通常不真正扩容（帧 floor = `base + MAX_FSTACK + 1` 在入口已被预留）。

**回归测试**（`cargo test --features lua_longjmp --test longjmp_tests`，同时建议带上
`--test alloc_failure_tests`；enumerated 入口的展开安全性另有 `tests_rs/capi_oom_unwind_tests.rs`，
它**不依赖 lua_longjmp**、在默认构建下就能检出）：

- 计数全局分配器量化 Rust 堆存活字节：反复触发 capi 各条错误路径、C 闭包抛错路径后必须回到
  基线（POSIX 上才有检出能力，Windows 因清理会运行而恒为 0）；
- 大额分配（≥4 KiB）失败注入：把 6 层 × 190 局部的嵌套函数推过初始栈容量 → VM 内栈扩容失败
  （`raise_memory_error`）必须被 Lua 层 `pcall` 捕获为预注册的 `not enough memory`，而不是
  穿透外层或 `exit(1)`（去掉第 2 条边界后本测试会打印 `lua not enough memory` 并退出进程）；
- `lua_settable` 内部注入分配失败 → 展开穿它自己的帧 + 替身 C 帧，必须被捕获为 `not enough
  memory` 而不是 abort（`capi_setter_oom_raise_does_not_abort` 守 longjmp 模式、
  `capi_oom_unwind_tests::settable_oom_raise_is_catchable_not_abort` 守默认 panic 模式；
  两者都已实测「修复前 abort / 修复后通过」）。
- **失败注入方式有硬约束（CI #228/#229 实证）**：capi 抛错用例必须用**项目自己的分配器**
  （`LuaState::new_main_with_allocator` + `Allocator` trait，见 `tests_rs/capi_oom_unwind_tests.rs`；
  只走这条路的分配才「一律经漏斗」），并加 ≥4 KiB 阈值过滤；**不要**用全局分配器按大小过滤
  （`longjmp_tests` 里给栈扩容用的那种 `FAIL_LARGE_ARMED`）。原因：panic 运行时、`state.stack`
  这类普通 `Vec`、以及错误路径里尚未迁移的**不可失败**分配（`pcall` 错误路径的
  `stack.split_off`（6144 B = 384×16）、字符串创建（120 B））都走全局分配器或项目侧不可失败路径，
  一起打掉就命中 `handle_alloc_error`（打印 `memory allocation of N bytes failed` 后挂死/abort）
  —— 实测把 Drone Linux 整条流水线拖到 60 分钟超时（build #228/#229 的 step 3 被 kill，
  cmp-tests / deps-tests / bench 全部 skipped；Windows 因清理会运行而不受影响）。
  同理：**武装期间 Lua 侧不得做任何字符串构造**（`..` / `tostring` 会分配）——错误对象比对
  一律放 Rust 侧（`obj_at(-1)` + `lua_string_as_str`），chunk 只用常量比较 + `return err`。

## 关键编码约定

- **fast_unsafe 开关完整性**：新增的 VM 热路径 unsafe 优化（跳过边界检查 / 跳过 drop_glue / 绕过 RefCell borrow / 裸指针缓存）必须用 `#[cfg(feature = "fast_unsafe")]` 包住，并同时给出 `#[cfg(not(feature = "fast_unsafe"))]` 的安全等价实现（语义逐位一致：Rc 句柄 + 边界检查 + 常规 drop + borrow 守卫）。架构/FFI 必需的 unsafe（见 Feature Flags 一节的不在开关范围清单）不需要包。
- **Rc 共享避免深拷贝**：`Proto.code` / `constants` / `upvalues` 字段使用 `Rc<Vec<...>>`，避免 `op_call` / `op_tailcall` 中的 O(n) 深拷贝。
- **NUL 终止字符串**：`ShortString` / `LongString` 从字节数据构造时必须使用 `new_short_bytes()` / `new_long_bytes()`，确保正确处理 NUL 终止。
- **TValue 布局**：16 字节（discriminant 1B + payload 8B + padding；`objects.rs` 有 `assert_eq!(size_of::<TValue>(), 16)` 守卫，GC 估算与 C ABI 都依赖这个宽度）。`BuiltinFn` 是 8 字节纯函数指针（pure 标志内嵌 bit0，函数名走全局 `builtin_names` 表），`RustClosure` 是 `MyRc<RustClosure>`（函数指针 + upvalues）变体，用于有状态内置函数（如 `coroutine.wrap`）。`is_callable()` 统一处理 `LightUserData` 内置 tag。
- **TValue 变体顺序不变式（write_stack 单比较 drop 判定的前提）**：无引用计数的变体（`Nil` / `Boolean` / `LightUserData` / `Integer` / `Float` / `LCFn` / `BuiltinFn`）必须全部排在持有引用计数的 8 个变体（`ShortStr` / `LongStr` / `Table` / `LClosure` / `CClosure` / `RustClosure` / `UserData` / `Thread`，tag 7..=14）之前。tag 按声明顺序分配，故 `write_stack` 的旧值判定落在连续 tag 区间上 → 实测单条 `cmpb $0x7,(slot); jae <冷块>`（判定/释放见 `execute.rs::old_slot_holds_ref` / `push_drop`，后者 `#[cold]` 出线让热路径保持 fall-through）；顺序被打乱时 LLVM 退化为「范围保护 `ja` + 位掩码 `bt`/`jae`」两分支（每寄存器写多 ~4 uops，只是性能问题、无正确性风险）。`impl Hash for TValue` 用的是手工数值 tag（与该顺序无关），故重排不改变哈希值、表迭代顺序或任何可观察行为。**第二个消费者**：`execute.rs::copy_stack_slot`（`OP_MOVE`，对应 C 的 `setobjs2s`）把「源槽持引用 / 旧槽持引用」两次判定都落成单条 `cmpb $6,(slot); ja <冷块>`（`tag <= 6` 一次性覆盖全部 7 个不持引用的变体），两个判定的冷块出口相同 → 热路径只剩 16B `movupd` 覆盖写 + `cmov` 更新 `len`/`top`，与 C 的零分支形态一致；快路径成立的前提是**源与旧槽都不持引用**（任一持引用即整段走 `clone` + `write_stack`），漏一侧就是泄漏/二次释放（回归：`tests_lua/rc_release_test.lua`）。CI 实测（两个改码后 build 的四个样本全低于基线）：浮点算术 Linux 1.419→1.363/1.387、Windows 1.052→1.010/1.023，即 −2~−4%；其余 11 项在噪声带内。**注意 CI 跨 build 噪声按指标而异**：同码重跑（#218 vs #219，二进制近乎相同）在 Linux 整数算术 / Windows 字符串拼接上各偏 +14% / +15%，调用/协程 +5% —— 单 build 的 <5% 差异不可判定，判定小幅改动必须多 build 复采并看同平台同指标的方向一致性（浮点算术这类 spread ~1.5% 的指标才能分辨）。
- **cmp_c feature 隔离**：`parser.rs` / `bindings.rs` 引用 C 符号（`luaY_checklimit` / `luaY_nvarstack`），必须在 `#[cfg(feature = "cmp_c")]` 下；`capi` 模块在 `cmp_c` feature 下不存在，任何引用 `crate::capi::` 的代码必须用 `#[cfg(not(feature = "cmp_c"))]` 包围或改用本地辅助函数。
- **`G.main` 重绑（规约 R1 的 capi 落实）**：`G(L)->main` 是主线程执行态的裸指针（对应 C 的 `mainthread`），`LuaState` 移动后即失效，只能靠进入点 `bind_exec()` 重绑。`gc.rs::collect_gc_inner` 对此已做防御：**主线程自己触发 GC 时不再解引用 `G.main`**（判据 `LuaState::is_main_thread()`，自身根已由 `collect_roots_into(self)` 覆盖），只有协程才按 `G.main` 扫主线程栈。因此：任何**能触发 GC 或放行协程**的 capi 入口都必须先 `bind_exec()`（`lua_gc`、`lua_resume(from)` 已按此处理）。宿主从 Rust 侧 `let mut st = LuaState::default();` 后顶层直接 `lua_gc(L, LUA_GCCOLLECT, 0)` 是合法用法 —— 修复前该处对陈旧 `G.main`（构造点地址）解引用，实测 0xC0000005（`main=0x...fe2d0` vs `self=0x...ff0d0`；回归：`capi::tests::test_lua_gc_collect_toplevel_from_rust`）。
- **LuaState 初始化完整性**：测试代码中初始化 `LuaState` 时必须包含所有字段（如 `func.rs` 中的 `make_vm_state`）。新增字段时同步更新所有测试初始化代码。
- **pending_return_adjust 机制**：当 return hook 启用（`hook_mask & 2 != 0`）且 `allowhook` 且 `in_op_call` 时，`adjust_results` 不立即截断栈，而是设置 pending 由调用方的 `op_call` / `op_tailcall` / `op_tforcall` 在 return hook 完成后通过 `finish_pending_adjust` 执行实际截断。任何调用 BuiltinFn 的指令都必须保证最终会执行 `finish_pending_adjust`。
- **op_tailcall 与 op_call 一致性**：`op_tailcall` 的 BuiltinFn / RustClosure 分支必须与 `op_call` 的 BuiltinFn 分支保持结构一致，包括 call hook（"tail call" 事件）、return hook（LUA_MASKRET，含 `saved_pending = take()` / `pending = saved_pending` 保存恢复）、`finish_pending_adjust()`。
- **GC 与可达性**：无 `gc_header` 的对象（如 `RustClosure` / `Thread` / `CClosure`）使用 `Rc::as_ptr` 地址判断可达性；`is_marked` / `mark_tvalue` 需覆盖所有可达对象类型。
- **collectgarbage 语义**：`collectgarbage("collect")` 在 finalizer 内调用返回 `Integer(0)`（在 Lua 中为真值），需注意 `assert(not res)` 类断言。
- **open_upvals 槽位回收**：`LuaState.open_upvals` 是带空闲表的 arena（`Vec<Option<UpValRef>>` + `free_upvals`）。`func::close_upval` 关闭 upvalue 后必须把槽位置 `None` 并把索引归还 `free_upvals` —— 否则已关闭 upvalue 的内存及其 Closed 值被 arena 永久强引用（无界泄漏，且 `open_upvals` 不是 GC 根，`collectgarbage("count")` 观测不到，只能看 RSS）。未释放槽位的索引在 upvalue 存活期间保持稳定；协程 yield/resume 重新打开 upvalue 时必须经 `func::register_upval_slot` 重新登记（槽位可能已被回收）。仅 `func::close_upval` 永久关闭并回收；`close_yield_upvals` / `close_upval_by_ref` 等可重开路径只 unlink，不回收。
- **c_safety_keepalive 仅限 C 发起 pcall**：只有 `lua_pcallk` 会置 `LuaState.c_pcall_stash`，错误路径才把被截断的栈值移入 `c_safety_keepalive`（模拟 C Lua 中 C 代码仍持有 userdata 指针的场景）；Lua 层 `pcall`/`xpcall` 直接 `truncate`，否则该区随每次失败无界增长。该区在 `collect_roots_into` 中登记为 GC 根（对应 C Lua 中这些值仍在栈数组里）。
- **collect_gc 不可重入**：`collect_gc` 入口发现 `gc_running` 即返回 —— finalizer 运行期间的分配会经 `maybe_collect_gc` / `concat_gc_check` / `step_gc` 重入，重入的 `enter_cycle` 会翻转 white 并清空标记队列，使外层周期已完成的标记作废（其后的 `clear_weak_tables` / `sweep_unreachable` 会基于被破坏的颜色状态误清活对象）。
- **协程执行态不得反向持有强引用**：`ThreadContext.exec`（协程自己的 `lua_State`）由 `ThreadContext` 拥有，因此 `LuaState.current_thread` 必须是 `Weak<RefCell<ThreadContext>>`（读访问一律走 `LuaState::current_thread_ctx()`）。反向持 `Rc` 即构成 `ThreadContext --exec--> LuaState --current_thread--> ThreadContext` 自环，引用计数永不归零：任何 resume 过的协程（正常结束/错误/挂起都算）都会永久泄漏整个执行态及其栈上对象（ASan 实测每协程约 1KB；仓库 all.lua 全量曾因此累计 22.9MB）。执行期间该 Weak 必定 upgrade 成功（线程对象由调用方栈 / wrap RustClosure 的 upvalue / C 侧句柄强持有）。
- **协程对象必须登记到 GC 线程表**：`coroutine.create` / `coroutine.wrap` 创建处调用 `GCState::register_thread`。线程没有 `gc_header`，可达性只用 `aux_marked`（键 = `Rc::as_ptr(&thread.context)`，见 `mark_tvalue` 的 Thread 分支），因此进不了 `register_object` / `sweep_unreachable` 通道；`collect_gc_inner` 末尾的 `clear_dead_threads` 依 `aux_marked` 判活，把不可达线程的 `context` 换回默认值以 drop 其执行态 —— 这是打断「Thread → context.exec → 栈/upvalue → Thread」（`coroutine.resume(co, co)`、协程内闭包反向引用线程等）自引用环的唯一途径；漏登记则环永久存活，且 `collectgarbage("count")` 观测不到（协程执行态内部 Vec 不计入 GC 估算）。主线程与一次性临时线程不登记。
- **close_state 末尾必须做一次完整回收，且断环必须在这之前**：`func::close` 关上值、finalizer 运行都可能新造出不可达环（闭包↔上值、协程↔执行态、表自引用），只有 GC 的 sweep 能打断它们，之后 Rc 才能归零。不回收则这些环留到进程退出（LSan 报泄漏）——对应 C 的 `lua_close` → `luaC_freeallobjects`「关闭即释放全部」语义。**顺序关键**：`GlobalState::break_close_refs`（清空 globals / registry / `package.loaded`，后者必须与 `base_lib::get_package_table` 一致走 `registry["_PACKAGE"]`——Lua 脚本可能删掉全局 `package`，all.lua 结尾就会擦除所有全局变量）必须在这次 `collect_gc` **之前**调用：globals/registry 是 GC 根，若先回收后断环，环内对象（如 `M.rrotate` 上值捕获模块表 M、metatable ↔ `__gc` 闭包、`registry._LOADED` 里的模块）会被判为可达而逃过清扫，而 drop 阶段释放外部引用后再无第二次回收 → 纯 Rc 环永久存活（实测泄漏 561 块 / 57.9KB → 断环前置后 0 泄漏）。
- **外部字符串（lua_pushexternalstring）按 C 的 TString 语义实现**：`LongString` 仿 `lobject.h` 的 `TString`，固定头 = `hash/len/extra/kind/contents/falloc/ud`（`kind` 为 C `shrlen` 的负值编码 `LSTRREG`/`LSTRFIX`/`LSTRMEM`）。**内容一律经 `contents` 指针访问（`strings::long_content`），不得直接切 `data`**：常规长字符串（`LSTRREG`）`contents` 指向对象尾部内联内容且 DST 元数据 = `len`；外部字符串（`LSTRFIX`/`LSTRMEM`）`contents` 指向调用方缓冲区、DST 元数据 = 0（无内联内容，写 `len` 会让引用覆盖分配外地址 → UB）。`lua_pushexternalstring` **不拷贝内容**，返回值与 `lua_tolstring` 均为调用方缓冲区指针（C 的 `getstr(ts)`）；`falloc != NULL` 时缓冲区所有权交给 Lua，字符串不可达（引用计数归零）时由 `LongString::drop_payload` 调 `falloc(ud, contents, len+1, 0)` 释放一次 —— 对应 C `luaC_freeobj` 的 LSTRMEM 分支（`GlobalState::ext_string_buffers` 与关闭期集中释放已删除）。`falloc == NULL`（C 的 LSTRFIX）永不释放。注意 tests_lua 的 Lua 层外部字符串测试（strings.lua:530 / attrib.lua:334 块）在 Rust 侧因 `T == nil` 被跳过，回归由 `capi.rs::tests::test_lua_pushexternalstring_matches_c_semantics`（镜像 lib22.c:newstr + attrib.lua 断言，含 falloc 恰释放一次）与 `strings.rs` 的 `test_external_string_frees_buffer_on_last_ref` 覆盖。
- **字符串跨变体相等按内容**（对应 C `luaS_eqstr` / `luaV_equalobj` 混合变体分支）：`lua_string_eq` 同时处理短 vs 长（常规长串 > `LUAI_MAXSHORTLEN` 必与短串等长,实际只有外部字符串可能命中）；`execute.rs::op_eq` 的 `mem::discriminant` 快路径与 `tm.rs::equal_obj` 也必须把「两个字符串变体」当同一等价类放行，否则外部短字符串与驻留短字符串 `==` 会假阴性（tests_lua/attrib.lua 的 lib2-v2 外部字符串块实测）。表键不驻留化（C 的 `luaS_normstr` 只在 `luaH_newkey` 上做）：哈希与相等都按内容，Lua 层语义等价，差异仅为外部字符串作为键时缓冲区寿命随该键。
- **长字符串开销**：`LongString` 固定头 40 字节（常规长串 = 40 + 内容 + 1），比 C 的 `offsetof(TString, falloc)=32` 多 8 字节（Rust 侧无法对 `LSTRREG` 省略 `falloc`/`ud`）。外部字符串只计固定头（内容在 Lua 管理内存之外）。
- **Userdata 是变长对象（单块分配，不再是 `Vec`）**：`Udata`（`src_rs/objects.rs`）对应 C 的 `Udata`（`lobject.h`）—— 布局 = 固定头（`UdataFixed`：`gc_header`/`nuvalue`/`len`/`metatable`）+ 变长尾（前 `nuvalue * (size_of::<TValue>()/8)` 个 u64 是用户值，随后 `ceil(len/8)` 个 u64 是数据区），单块 `MyRc<Udata, SlotAlloc>`（8 字节薄指针；`TValue` 仍 16 字节，有 `const _` 断言 TValue 16B/align 8）。`Udata::data_ptr()` = C 的 `getudatamem`（`lua_touserdata` 返回值：块内、8 字节对齐、跨栈增长稳定），`len` = `lua_rawlen`，数据区初始为 0、用户值初始为 nil、元表在块分配成功后写入。**写路径必须从 payload 薄指针（`MyRc::as_raw_ptr()`）重新派生**——`Udata::from_payload` / `data_ptr_of` / `metatable_of` / `set_user_value`（capi 的 `lua_setiuservalue`/`lua_setmetatable`、`debug.setuservalue`、io 库写 FILE* 均走这条）；经 `&self` 派生指针写是 Tree Borrows UB。读路径用 `user_values()` / `data_ptr()` / `ud_stream()`。DST 胖指针重建用手写 `transmute((ptr, tail_words))`（与 `strings.rs` 的 `LongString::as_ref` 同手法，`ptr::from_raw_parts` 仍是 unstable）。`RcPayload::drop_payload` 必须**显式**析构元表与各用户值（`[u64]` 尾无 drop glue）。io 库的 FILE* 存在数据区首个 8 字节单元（`len = 8`；C 的 `luaL_Stream` 为 16 字节）。`lua_getiuservalue` 越界仍返回 nil 的类型码（C 返回 `LUA_TNONE`，属既有偏差，未改）。
- **GC 根扫描范围与 C 帧**：C API 压栈只增长 `stack.len()`，不推进 `state.top`（C 函数内 `top` 停在参数末尾），故 C 代码压入的活值位于 `[top, len)`。外部 C 帧活跃时（`LuaState.c_frame_depth > 0`，在 `call_c_function` / `pcall_c_function` 内增减）`collect_roots_into` 必须扫到 `stack.len()`，否则 C 调用期间触发的 GC（`lua_createtable` / `lua_pushcclosure` / 表增长内的 `maybe_collect_gc`）会把 C 刚建的对象当垃圾清扫（sweep 对死表 `clear_for_drop`）—— lua-cjson 的 `cjson.safe` 上值变 nil、skynet e2e 失败即由此而来。其余场合保持 `stack[..top]`：VM 刻意把可回收旧值留在 `top` 之上（`execute.rs::smart_clear_stack`，仅保证 trivial 值），全栈扫描会让应回收对象继续存活（`gc.lua` 弱表断言失败）。
- **close_yield_upvals**：必须关闭所有非 TBC 开放上值（通过 `state.open_upval` 链），不仅限于 yield 值可达的；TBC 上值必须跳过（`sync_yield_upvals_back` 会重置 `tbc=false`，否则 `func::close` 会跳过 `__close` 元方法）。
- **step_gc 生成模式**：生成模式下 `step_gc` 必须直接做完整回收（未实现 minor collection），否则 `collectgarbage("step")` 无法清理弱表。
- **call_require 缓存语义**：`package.loaded[name]` 为 `false` 时必须视为"未加载"（使用 `lua_toboolean` 语义，非 nil 检查），触发重新加载；`load_lua_module` 返回值检查使用 `!lua_isnil`（非 `lua_toboolean`），`false` 是合法返回值会被缓存。
- **package.config**：必须为 `"/\n;\n?\n!\n-\n"`（DIRSEP / PATH_SEP / PATH_MARK / EXEC_DIR / IGMARK 以换行分隔）。
- **searchpath / findfile**：不能跳过空模板；`findfile` 应返回列出所有尝试路径的完整错误消息（`no file 'X'\n\tno file 'Y'`）。
- **concat_gc_interval**：保持 4096，设置为 100 会因 GC 触发过于频繁导致 `constructs.lua` 性能下降。
- **窄 superblock 融合前提**：`fuse_pure_call`（MOVE+CALL / MODK+MMBINK+CALL 并入 GETTABUP/GETFIELD 处理器）与 GETFIELD 融合共享 gate：`hook_mask == 0`（line/count hook 要求逐指令观察，#147）、函数 `is_pure`、CALL 固定 B=2/C=2 非 k；出错路径先 `state.pc = CALL 索引` 再传播（traceback 行号 = 原 CALL 行）。前向 peek 类融合只在命中率 ≥50% 的形态划算（arith_chain 低命中被证伪；call 后尾部算术 form-C 被 PGO 吃掉，Linux +0.5% 已 revert）。
- **栈 scratch 不变量（write_stack 无分支写的前提）**：`LuaState.stack`（`Vec<TValue>`）的 `len` 恒为**精确的语义 live top 上界**——C 侧 `lua_gettop` 等所有位置读取者一律读 `len`，不得改变其含义；帧寄存器区另用 `LuaState.scratch_end` 跟踪：`[len, scratch_end)` 是**已写入 Nil 的已初始化内存**，供 `write_stack` 无分支写复用（对应 C 的 `L2->top = ci->top` 免边界检查写）。维护规则：
  - `pad_frame_scratch()` 在每个 Lua 帧入口/恢复点调用；`frame_floor()` 取**保守上界** `anchor + MAX_FSTACK + 1`（anchor = `base` 有效时取 `base`，否则取 `stack.len()`）——**不用 `proto.max_stack_size`**：变参前导/展开与 C 侧构造的帧实测会写到 proto 声明的寄存器区之外，少填一次就是越界写 UB。它只填 Nil，不动 `len`/`top`；动态长度写入（`op_vararg` 展开）另有 `ensure_scratch_to(end)`。
  - 语义截断一律经 `truncate_live` / `truncate_live_vec`（= 就地释放 `[n, len)` 的非 trivial 值 + `set_len`），**禁止直接 `Vec::truncate`**——它会把槽位留成未初始化内存，之后 `write_stack` 的原地写 + Vec drop 会造成二次释放。
  - **任何换掉 `stack` 向量或把尾部移出的操作**（协程 save/restore、`mem::take(&mut state.stack)`、`split_off`、`drain`、C API 的 `lua_rotate`/`lua_xmove`/`lua_yield`）必须走 `reset_scratch()`（= 清零 `scratch_end` + 立即 `pad_frame_scratch()`），**不得只写 `scratch_end = 0`**：被移出/丢弃的槽位不再是「已初始化」，否则写路径的旧值 drop 检查会读到 moved-out 值而二次减引用（实测崩溃：`op_concat` 的 `split_off` 路径先前会让 `nextvar.lua`/`tpack.lua` 静默崩溃）。
  - 诊断与验收：`--features probe_wstack` 输出 `WSOOB`（越过 `max(len, scratch_end)`）/`WSCAP`（越过 capacity）计数与越界读时的 `STACK UNDERFLOW` 现场，全套 tests_lua 零输出即不变量成立；`debug_assert` 在 debug 构建（`cargo test`）下直接暴露破例点。**release 下 `write_stack` 与 `read_stack` 均无边界判定、无扩容调用** —— `write_stack` 只剩「旧值是否需 drop」（Rc 释放, 必须保留, 预测命中率≈100%）, `read_stack` 直接 `get_unchecked`；`len`/`top` 用 `max` 更新（cmov）。安全构建（`--no-default-features`）保留按需补齐分支作为等价实现。同类消费者：`copy_stack_slot`（`OP_MOVE`）的无分支快路径同样按此不变量越界写 —— 它只读 tag 判持有引用、再 16B 覆盖写，`top` 仍是 `max(top, idx+1)`。
  - 补齐点（不变量维护的完整清单）：帧入口（`execute_loop` / `op_call*` / `op_varargprep`）、被移动或截断之后的恢复点（`truncate_live` 尾部、各 `reset_scratch()`）、以及 `tm::obj_len` 这类「按寄存器号写调用者帧、且可能不经 VM 入口」的辅助函数（直接调用路径的兜底）。
- **旧槽值延迟释放队列（write_stack 的冷路径出线）**：`LuaState.drop_queue`（`Vec<TValue>`，容量 = 上界 `state::DROP_QUEUE_LIMIT` = 64，构造时一次性预留）是 `fast_unsafe` 的 `write_stack` 覆盖「持有引用的旧值」时的落点（`execute.rs::push_drop`，`#[cold] #[inline(never)]`；安全构建不走队列，`.stack[idx] = val` 就地释放）。队列项是**已被覆盖的死值**，延迟的只是引用计数递减、不是可达性判定（值仍由队列强持有，GC 扫不到它们也不影响正确性）。两条不变量必须同时守住：
  - **有界 + 热路径零分配**：`push_drop` 是唯一写入点，先判满（`len >= DROP_QUEUE_LIMIT` → `drain_drop_queue()`）再入队 → 长度恒 ≤ 上界；容量一次性预留，故 `push` 永不触发 `Vec` 增长 —— 保持 `write_stack` 在内存分配失败漏斗之外（真 OOM 下这里不新增 abort 点）。去掉判满，一段没有 CALL/RETURN 的紧凑赋值循环会让死值无界累积（实测 100001 项）。
  - **GC 入口无条件排空**：`gc::collect_gc` 在 `gc_running.set(true)` 之后**不做任何长度判定**地 `drain_drop_queue()`，且必须早于标记阶段 —— 否则队列里的死值（引用计数未减）会先被 sweep 清空内容、再由队列 drop；并且 `lua_gc(COLLECT)` 之后外部字符串缓冲区的 `falloc` 尚未调用（实测 `capi::tests::test_lua_pushexternalstring_matches_c_semantics` 的计数从 12 变 6）。
  - 回归：`tests_rs/drop_queue_tests.rs`（两个用例分别钉住两条不变量，去掉对应机制即失败；`cargo test --test drop_queue_tests`）。
- **TableData.probe_cache 禁令**：`find_str_ref`/`data_ro` 读路径上不得引入 Cell 内部状态写回（Linux OOO 隐藏探测延迟无收益，Windows MSVC+PGO 实测 +35% 回归，#118 证伪）。
- **Windows C 模块的宿主符号导出**：MSVC 不会自动导出 `#[no_mangle] extern "C"` 符号（GNU 目标用 `--export-all-symbols`），而 `deps/` 下的 C 模块要在运行时解析 `lua_xxx`。机制：`build.rs` 扫描 `src_rs/*.rs` 的 `#[no_mangle]` 项（按 feature 判定 cfg，无法判定的 cfg 保守跳过）→ 写 `target/<profile>/lua_exe_import.def` 与 `lua_rs_dll_import.def`（后者 `LIBRARY lua_rs.dll`）→ `deps/Makefile` 用 `dlltool` 转成 MinGW 导入库（`liblua_exe.a` / `liblua_rs_dll.a`）。两个宿主分工：`lua.exe`（lua-rs 解释器自身加载 cjson/luasocket/lsqlite3 —— 模块导入表写 `lua.exe`，因为该 EXE 就是主模块，加载器直接解析自身导出）+ `lua_rs.dll`（`deps/lua_dll` 的 cdylib，供 skynet 这类外部宿主 + `sol2_smoke`；**不能拿 `lua.exe` 顶替**：`LoadLibrary` 一个 EXE 会把 `mainCRTStartup` 当 DllMain 调用，Rust 运行期不初始化，实测 `lua_newstate` 即崩）。`.def` 里多写一个不存在的符号会让 MSVC 以 LNK2001 硬失败，故只列真实实现的符号；`dlltool` 读不了 MSVC 链接出的 PE 导出表（只能读 COFF 符号表），所以清单必须由 `build.rs` 落盘而不是从 `lua.exe` 反读。deps 侧用 `dlltool` 时需要 `TMP/TEMP` 为 Windows 形式路径（POSIX `/d/...` 会让它回退到 `C:\Windows` 失败），`deps/Makefile` 用 `cygpath -m` 导出。

## .gitignore 关键排除项

`librust_out.rlib`、`luac.out`、`perf.data`、`perf.data.*`、`tests_lua/time.txt`、`.omp/` 目录、`deps/src/` / `deps/build/` / `deps/lib/` / `deps/cache/` / `deps/luarocks-root/` / `deps/*.tar.gz` / `deps/*.zip` / `deps/.test_*.log`、`logs/`、`Cargo.lock`、`build` / `build_c` / `cbuild` / `target` / `target_perf`。

## 会话接续规则

- **接续后第一件事**：检查 `logs/` 目录下是否有报错日志（`compiler_test.log`、`cargo_test.log`、`build.log`），或运行 `tools/verify.sh` 验证上次修改是否引入编译错误。
- **hook 自动报错处理**：修改 `src_rs/` 后 hook 会自动运行 `verify.sh`，如果 Terminal 中出现 "Compiler compare tests failed" 或类似报错，必须立即查看 `logs/compiler_test.log` 并修复，不要等待用户提醒。
- **cmp_c feature 编译检查**：`verify.sh` 用 `cargo test --features cmp_c` 编译，启用 cmp_c feature 时 `capi` 模块不存在。任何引用 `crate::capi::` 的代码必须用 `#[cfg(not(feature = "cmp_c"))]` 包围，或改用本地辅助函数。
- **LuaState 初始化完整性**：在测试代码中初始化 `LuaState` 时，必须包含所有字段。新增字段时同步更新 `func.rs` 等文件中的测试用 `make_vm_state` 函数。
