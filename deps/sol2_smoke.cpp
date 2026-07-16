// sol2_smoke.cpp - sol2 与 Rust lua 兼容性冒烟测试
//
// 验证 sol2 (header-only C++ Lua binding) 能正常链接 Rust lua 静态库
// (target/release/liblua_rs.a) 并执行基本的 Lua 交互操作。

#include <sol/compatibility/compat-5.4.h>
#include <sol/sol.hpp>
#include <cassert>
#include <cstdio>
#include <string>

int main() {
    sol::state lua;
    lua.open_libraries(sol::lib::base, sol::lib::string, sol::lib::math);

    // 1. 执行 Lua 代码并读取全局变量
    lua.script("x = 42");
    int x = lua["x"];
    assert(x == 42);
    std::printf("x = %d\n", x);
    std::fflush(stdout);

    // 2. 字符串
    lua.script("s = 'hello'");
    std::string s = lua["s"];
    assert(s == "hello");
    std::printf("s = %s\n", s.c_str());
    std::fflush(stdout);

    // 3. 从 C++ 调用 Lua 函数
    lua.script("function add(a, b) return a + b end");
    {
        sol::safe_function_result fr = lua.safe_script("return add(10, 32)", sol::script_pass_on_error);
        if (!fr.valid()) {
            std::printf("add call failed\n");
            return 1;
        }
        int sum = fr.get<int>(0);
        std::printf("add(10,32) = %d (expected 42)\n", sum);
        assert(sum == 42);
    }
    std::fflush(stdout);

    // 4. 表操作
    lua.script("t = {1, 2, 3}");
    lua.script("assert(t[1] == 1, 't[1] failed')");
    lua.script("assert(t[2] == 2, 't[2] failed')");
    lua.script("assert(t[3] == 3, 't[3] failed')");
    std::printf("table test passed (via Lua assert)\n");
    std::fflush(stdout);

    // 5. 从 C++ 注册 lambda 到 Lua 并调用
    lua["multiply"] = [](int a, int b) { return a * b; };
    lua.script("assert(multiply(6, 7) == 42, 'multiply failed')");
    std::printf("multiply(6,7) = 42 (C++ lambda)\n");
    std::fflush(stdout);

    std::printf("sol2 smoke test OK\n");
    return 0;
}
