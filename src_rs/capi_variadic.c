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

/* 可见性宏：导出符号供 .so 链接 */
#define LUA_RS_API __attribute__((visibility("default")))

/* Rust 端 capi.rs 导出的符号 */
extern const char *lua_pushlstring(void *L, const char *s, size_t len);
extern const char *lua_pushstring(void *L, const char *s);
extern int lua_error(void *L);
extern void lua_concat(void *L, int n);
extern void luaL_where(void *L, int level);

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

