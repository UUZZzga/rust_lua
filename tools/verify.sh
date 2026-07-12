#!/bin/bash
echo "Running verification tests..."

# 创建 logs 目录
mkdir -p logs

# 构建命令包装：用 systemd-run 绕过内存限制（CLAUDE.md 规则）
run_build() {
    systemd-run --user --wait --collect --pipe \
        --property=LimitAS=infinity \
        --working-directory="$(pwd)" \
        "$@"
}

# 1. 执行编译器比对测试
echo "Running compiler compare tests..."
run_build cargo test --features ffi -- compiler::cmp_tests::compiler_compare_tests > logs/compiler_test.log 2>&1 < /dev/null
CMP_EXIT=$?

if [ $CMP_EXIT -ne 0 ]; then
    echo "ERROR: Compiler compare tests failed (exit code $CMP_EXIT)!"
    echo "Check logs/compiler_test.log for details"
    exit 2
fi

if grep -q "test result: FAILED" logs/compiler_test.log 2>/dev/null; then
    echo "ERROR: Compiler compare tests failed!"
    echo "Check logs/compiler_test.log for details"
    exit 2
fi

echo "Compiler compare tests passed."

# 2. 执行所有 cargo test
echo "Running all cargo tests..."
run_build cargo test > logs/cargo_test.log 2>&1 < /dev/null
TEST_EXIT=$?

if [ $TEST_EXIT -ne 0 ]; then
    echo "ERROR: Cargo tests failed (exit code $TEST_EXIT)!"
    echo "Check logs/cargo_test.log for details"
    exit 2
fi

if grep -q "test result: FAILED" logs/cargo_test.log 2>/dev/null; then
    echo "ERROR: Cargo tests failed!"
    echo "Check logs/cargo_test.log for details"
    exit 2
fi

# 3. 构建项目
echo "Building project..."
run_build cargo build --release > logs/build.log 2>&1
BUILD_EXIT=$?

if [ $BUILD_EXIT -ne 0 ]; then
    echo "ERROR: Project build failed (exit code $BUILD_EXIT)!"
    echo "Check logs/build.log for details"
    exit 2
fi

echo "Project build passed."

echo "Running project..."

# 设置 LUA_PATH,让 require 能找到 tests_lua 目录下的模块 (如 tracegc.lua)
export LUA_PATH="tests_lua/?.lua;./?.lua;./?/init.lua"

# 测试命令包装：用 systemd-run 限制内存为 200MB（CLAUDE.md 规则）
run_test() {
    systemd-run --user --wait --collect --pipe \
        --property=LimitAS=204800 \
        --working-directory="$(pwd)" \
        "$@"
}

# big.lua 含主 chunk 中的 coroutine.yield, 必须用 coroutine.wrap 包装运行
# (与 tests_lua/all.lua 中的调用方式一致)
echo "Running big.lua ..."
timeout 30 run_test ./target/release/lua -e "local f = coroutine.wrap(assert(loadfile('tests_lua/big.lua'))); assert(f() == 'b'); assert(f() == 'a')" > logs/big_run.log 2>&1 < /dev/null
RUN_EXIT=$?
if [ $RUN_EXIT -ne 0 ]; then
    if [ $RUN_EXIT -eq 124 ]; then
        echo "Timeout: big.lua use LUA_VM_TRACE=1 to debug"
    fi
    echo "ERROR: big.lua failed (exit code $RUN_EXIT)!"
    echo "Check logs/big_run.log for details"
    exit 2
fi

for test_file in tests_lua/calls.lua tests_lua/closure.lua tests_lua/code.lua tests_lua/constructs.lua tests_lua/coroutine.lua tests_lua/db.lua tests_lua/errors.lua tests_lua/events.lua tests_lua/files.lua tests_lua/gc.lua tests_lua/gengc.lua tests_lua/goto.lua tests_lua/literals.lua tests_lua/locals.lua tests_lua/math.lua tests_lua/memerr.lua tests_lua/nextvar.lua tests_lua/pm.lua tests_lua/sort.lua tests_lua/strings.lua tests_lua/tpack.lua tests_lua/utf8.lua tests_lua/vararg.lua tests_lua/verybig.lua; do
    test_name=$(basename "$test_file")
    log_name="logs/${test_name%.lua}_run.log"
    echo "Running $test_name ..."
    if [ "$test_name" = "files.lua" ]; then
        # files.lua:88 的 io.stdin:seek 要求 stdin 不可 seek；/dev/null 可 seek 会导致断言失败
        # （C 版本同行为，属环境依赖）。files.lua 在 line 300 用 io.input(file) 重定向输入，
        # 不从 stdin 读取，因此空管道（立即 EOF）安全。
        printf '' | timeout 10 run_test ./target/release/lua "$test_file" > "$log_name" 2>&1
    else
        timeout 10 run_test ./target/release/lua "$test_file" > "$log_name" 2>&1 < /dev/null
    fi
    RUN_EXIT=$?
    if [ $RUN_EXIT -ne 0 ]; then
        if [ $RUN_EXIT -eq 124 ]; then
            echo "Timeout: $test_name use LUA_VM_TRACE=1 to debug"
        fi
        echo "ERROR: $test_name failed (exit code $RUN_EXIT)!"
        echo "Check $log_name for details"
        exit 2
    fi
done

log_name="../logs/all_run.log"
echo "Running all.lua ..."
cd tests_lua
timeout 300 run_test ../target/release/lua all.lua > "$log_name" 2>&1 < /dev/null
cd ..
RUN_EXIT=$?
if [ $RUN_EXIT -ne 0 ]; then
    if [ $RUN_EXIT -eq 124 ]; then
        echo "Timeout: all.lua use LUA_VM_TRACE=1 to debug"
    fi
    echo "ERROR: all.lua failed (exit code $RUN_EXIT)!"
    echo "Check $log_name for details"
    exit 2
fi

echo "Project run passed."

echo "All tests passed!"
exit 0
