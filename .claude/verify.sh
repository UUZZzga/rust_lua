#!/bin/bash
echo "Running verification tests..."

# 创建 logs 目录
mkdir -p logs

# 1. 执行编译器比对测试
echo "Running compiler compare tests..."
cargo test --features ffi -- compiler::cmp_tests::compiler_compare_tests > logs/compiler_test.log 2>&1
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
cargo test > logs/cargo_test.log 2>&1
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
cargo build > logs/build.log 2>&1
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

# 测试时限制内存为 512MB
ulimit -v 524288

for test_file in tests_lua/db.lua tests_lua/calls.lua tests_lua/coroutine.lua tests_lua/goto.lua tests_lua/literals.lua tests_lua/locals.lua tests_lua/math.lua tests_lua/nextvar.lua tests_lua/sort.lua tests_lua/strings.lua tests_lua/utf8.lua; do
    test_name=$(basename "$test_file")
    log_name="logs/${test_name%.lua}_run.log"
    echo "Running $test_name ..."
    timeout 10 ./target/debug/lua "$test_file" > "$log_name" 2>&1
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

echo "Project run passed."

echo "All tests passed!"
exit 0
