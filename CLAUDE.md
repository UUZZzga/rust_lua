# 项目规则

## 项目目录结构

src_rs/ 目录下为 Lua Rust 实现的目录。
tests_lua/ 目录下为 Lua C 官方的测试用例目录，内部还有几个新增的用于测试 Rust 实现的测试用例。
tests_rs/ 目录下为 Lua Rust 实现的测试目录。
build/ 目录下为 Lua C 实现的构建目录，如果需要重新构建，也只能使用该目录。
target/ 目录下为 Lua Rust 实现的构建目录，如果需要重新构建，也只能使用该目录。
target_perf/ 目录下为 Lua Rust 实现的性能测试目录，如果需要重新构建，也只能使用该目录。

## 内存限制规则

测试时 `systemd-run --user --wait --collect --pipe --property=LimitAS=204800`（200MB），构建时用 `systemd-run --user --wait --collect --pipe --property=LimitAS=infinity` 绕过限制。

不遵守规则会被hook拦截，导致测试失败。

禁止使用 `ulimit -v 204800` 等命令限制内存，否则会导致测试失败,除非编写shell脚本时才能使用。

## 编译器改动校验

修改 `src_rs/` 下的核心数据文件或 `src_rs/compiler/` 目录时，需执行编译器比对测试（已固化到 hook），确保 Rust 输出与 C 实现一致。测试输出重定向到 `test.log` 查看。

