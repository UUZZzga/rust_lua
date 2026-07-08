/*
 * capi_variadic.c — 提供 lua_pushfstring / lua_pushvfstring 的 C 实现
 *
 * stable Rust 不支持 c_variadic，因此这两个需要可变参数的 C API
 * 函数由本 C 文件实现，链接到 Rust 二进制。
 *
 * 依赖 Rust 端导出的 lua_pushlstring 符号。
 */

#include <stdio.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

/* 可见性宏：导出符号供 .so 链接 */
#define LUA_RS_API __attribute__((visibility("default")))

/* Rust 端 capi.rs 导出的 lua_pushlstring 符号 */
extern const char *lua_pushlstring(void *L, const char *s, size_t len);

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
