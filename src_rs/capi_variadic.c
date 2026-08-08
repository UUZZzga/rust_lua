/*
 * capi_variadic.c — 提供 lua_pushfstring / lua_pushvfstring / luaL_error 的 C 实现
 *
 * stable Rust 不支持 c_variadic，因此需要可变参数的 C API 函数由本 C 文件实现，
 * 链接到 Rust 二进制。
 *
 * 依赖 Rust 端导出的 lua_pushlstring / lua_pushstring / lua_error /
 * lua_concat / luaL_where 符号。
 */

#include <stdio.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#ifdef LUA_USE_LONGJMP
#include <setjmp.h>
#endif

/* 可见性宏：导出符号供 .so/.dll 链接 */
#ifdef _WIN32
#define LUA_RS_API __declspec(dllexport)
#define LUA_RS_NORETURN __declspec(noreturn)
#else
#define LUA_RS_API __attribute__((visibility("default")))
#define LUA_RS_NORETURN __attribute__((noreturn))
#endif

/* Rust 端 capi.rs 导出的符号 */
extern const char *lua_pushlstring(void *L, const char *s, size_t len);
extern const char *lua_pushstring(void *L, const char *s);
extern int lua_error(void *L);
extern void lua_concat(void *L, int n);
extern void luaL_where(void *L, int level);

/*
 * CMP_C_MODE: 启用 cmp_c feature 时，C Lua 源码 (lauxlib.c / lapi.c) 已提供
 * lua_pushvfstring / lua_pushfstring / luaL_error，此处跳过以避免符号重复定义。
 * lua_rs_clocks_per_sec 和 longjmp 包装函数不受影响 (C Lua 不提供)。
 */
#ifndef CMP_C_MODE

LUA_RS_API const char *lua_pushvfstring(void *L, const char *fmt, va_list argp) {
    char buffer[4096];
    int n = vsnprintf(buffer, sizeof(buffer), fmt, argp);
    if (n < 0) {
        return lua_pushlstring(L, "", 0);
    }
    if ((size_t)n < sizeof(buffer)) {
        return lua_pushlstring(L, buffer, (size_t)n);
    }
    /* 缓冲区不够，动态分配 */
    char *big = (char *)malloc((size_t)n + 1);
    if (!big) {
        return lua_pushlstring(L, "", 0);
    }
    vsnprintf(big, (size_t)n + 1, fmt, argp);
    const char *ret = lua_pushlstring(L, big, (size_t)n);
    free(big);
    return ret;
}

LUA_RS_API const char *lua_pushfstring(void *L, const char *fmt, ...) {
    va_list argp;
    va_start(argp, fmt);
    const char *ret = lua_pushvfstring(L, fmt, argp);
    va_end(argp);
    return ret;
}

/* luaL_error: 格式化错误消息并抛出（对应 C lauxlib.cpp::luaL_error） */
LUA_RS_API int luaL_error(void *L, const char *fmt, ...) {
    va_list argp;
    va_start(argp, fmt);
    luaL_where(L, 1);
    lua_pushvfstring(L, fmt, argp);
    va_end(argp);
    lua_concat(L, 2);
    return lua_error(L);
}

#endif /* !CMP_C_MODE */

/*
 * setjmp/longjmp 包装函数 — 用于 panic=abort 模式下替代 catch_unwind。
 *
 * Rust 实现用 panic!("lua_error") + catch_unwind 模拟 C 的 longjmp/setjmp，
 * 但 panic=abort 模式下 catch_unwind 不工作，改用 setjmp/longjmp。
 *
 * 启用方式 (对应 C 的 LUA_USE_LONGJMP 宏):
 *   - cargo build --features lua_longjmp
 *   - size_optimized 模式 (panic=abort) 自动启用
 * build.rs 检测后向本文件传入 -DLUA_USE_LONGJMP, 编译以下两个函数。
 *
 * 调用流程:
 *   Rust pcall_c_function → lua_rs_pcall_c(f, L, buf)
 *     → setjmp(buf) == 0 → f(L) [正常调用]
 *     → setjmp(buf) != 0 → return -1 [lua_error/longjmp 返回]
 *   C 模块 → lua_error → lua_rs_longjmp(buf) → longjmp 回 lua_rs_pcall_c
 *
 * buf 指向 Rust 栈上分配的 512 字节缓冲区 (>= sizeof(jmp_buf) on all platforms)。
 */

/*
 * lua_rs_clocks_per_sec — 返回 C 库的 CLOCKS_PER_SEC 值
 *
 * CLOCKS_PER_SEC 是 <time.h> 中的宏, Rust 无法直接读取。
 * 不同平台的值不同: Windows (UCRT/MSVC/MinGW) = 1000, Linux (glibc) = 1000000。
 * os.clock() 用 clock() / CLOCKS_PER_SEC 计算 CPU 时间, 必须使用匹配的值。
 */
LUA_RS_API double lua_rs_clocks_per_sec(void) {
    return (double)CLOCKS_PER_SEC;
}

#ifdef LUA_USE_LONGJMP

LUA_RS_API int lua_rs_pcall_c(int (*f)(void *), void *L, void *buf) {
    if (setjmp(*(jmp_buf *)buf) != 0) {
        return -1;
    }
    return f(L);
}

LUA_RS_API LUA_RS_NORETURN void lua_rs_longjmp(void *buf) {
    longjmp(*(jmp_buf *)buf, 1);
}

#endif /* LUA_USE_LONGJMP */

