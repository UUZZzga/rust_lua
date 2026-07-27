#!/bin/bash
# setup.sh - 一键构建所有依赖库（下载 + 编译）
#
# 流程:
#   1. 构建 Rust lua (target/release/lua + liblua_rs.a)
#   2. 下载依赖库源码 (deps/fetch.sh)
#   3. 编译所有 C 模块 (deps/Makefile)
#
# 用法: ./setup.sh
#
# 遵守 CLAUDE.md 内存规则:
#   - 构建用 systemd-run --property=LimitAS=infinity 绕过内存限制
#   - 不使用 ulimit -v 限制内存（除 shell 脚本内部）
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log()  { echo -e "${BLUE}[setup]${NC} $*"; }
ok()   { echo -e "${GREEN}[ok]${NC} $*"; }
warn() { echo -e "${YELLOW}[warn]${NC} $*"; }
die()  { echo -e "${RED}[error]${NC} $*" >&2; exit 1; }

# 检查命令是否存在
need() {
    command -v "$1" >/dev/null 2>&1 || die "缺少依赖命令: $1"
}

need cargo
need gcc
need g++
need curl
need tar
need unzip
need make

# systemd-run 包装（绕过内存限制，遵守 CLAUDE.md 规则）
# 在无 user systemd 会话的环境（如 GitHub Actions CI）中自动降级为直接运行
run_unlimited() {
    if command -v systemd-run >/dev/null 2>&1 && systemctl --user status >/dev/null 2>&1; then
        systemd-run --user --wait --collect --pipe \
            --property=LimitAS=infinity \
            --working-directory="$PROJECT_ROOT" \
            "$@"
    else
        "$@"
    fi
}

# ============================================================================
# 1. 构建 Rust lua
# ============================================================================
LUA_BIN="$PROJECT_ROOT/target/release/lua"
LUA_LIB="$PROJECT_ROOT/target/release/liblua_rs.a"

if [[ -f "$LUA_BIN" && -f "$LUA_LIB" && "${1:-}" != "--rebuild-lua" ]]; then
    ok "Rust lua 已构建: $LUA_BIN"
else
    log "构建 Rust lua (cargo build --release)..."
    run_unlimited cargo build --release
    [[ -f "$LUA_BIN" ]] || die "lua 二进制未生成: $LUA_BIN"
    [[ -f "$LUA_LIB" ]] || die "liblua_rs.a 静态库未生成: $LUA_LIB"
    ok "Rust lua 构建完成"
fi

# ============================================================================
# 2. 下载依赖库源码
# ============================================================================
log "下载依赖库源码..."
bash "$SCRIPT_DIR/fetch.sh"
ok "依赖库源码就绪"

# ============================================================================
# 3. 应用 sol2 Lua 5.5 兼容补丁（用 sed 直接修改，避免 patch 行尾问题）
# ============================================================================
# sol2 v3.3.0 不支持 Lua 5.5，需修复三处：
#   a) compat-5.3.h: 版本检查 #error 阻止 Lua 5.5（LUA_VERSION_NUM > 504）
#   b) compat-5.4.h: LUA_ERRGCMM 定义仅对 5.4 生效（== 504），需扩展到 >= 504
#   c) state.hpp:    lua_newstate 在 5.5 中增加 unsigned seed 参数
SOL2_SRC="$SCRIPT_DIR/src/sol2"
apply_sol2_patch() {
    local compat53="$SOL2_SRC/include/sol/compatibility/compat-5.3.h"
    local compat54="$SOL2_SRC/include/sol/compatibility/compat-5.4.h"
    local state_hpp="$SOL2_SRC/include/sol/state.hpp"

    if [[ ! -f "$compat53" ]]; then
        return 0
    fi

    # 检查是否已打补丁（state.hpp 中是否已有 lua_newstate 3 参数版本）
    if grep -q "lua_newstate(alfunc, alpointer, 0)" "$state_hpp" 2>/dev/null; then
        ok "sol2 补丁已存在（跳过）"
        return 0
    fi

    log "应用 sol2 Lua 5.5 兼容补丁..."

    # a) compat-5.3.h: 允许 Lua 5.5（CRLF 行尾，sed 需指定 \r）
    #    将 "LUA_VERSION_NUM > 504" 改为 "LUA_VERSION_NUM > 505"
    sed -i 's/LUA_VERSION_NUM > 504/LUA_VERSION_NUM > 505/' "$compat53" \
        || die "compat-5.3.h 修改失败"

    # b) compat-5.4.h: 对 Lua >= 5.4 定义 LUA_ERRGCMM（CRLF 行尾）
    #    将 "LUA_VERSION_NUM == 504" 改为 "LUA_VERSION_NUM >= 504"
    sed -i 's/LUA_VERSION_NUM == 504/LUA_VERSION_NUM >= 504/' "$compat54" \
        || die "compat-5.4.h 修改失败"

    # c) state.hpp: lua_newstate 增加 seed 参数（0）
    sed -i 's|lua_newstate(alfunc, alpointer))|lua_newstate(alfunc, alpointer, 0))|' "$state_hpp" \
        || die "state.hpp 修改失败"

    # 验证修改
    grep -q "LUA_VERSION_NUM > 505" "$compat53" \
        || die "compat-5.3.h 修改未生效"
    grep -q "LUA_VERSION_NUM >= 504" "$compat54" \
        || die "compat-5.4.h 修改未生效"
    grep -q "lua_newstate(alfunc, alpointer, 0)" "$state_hpp" \
        || die "state.hpp 修改未生效"

    ok "sol2 补丁已应用"
}
apply_sol2_patch

# ============================================================================
# 4. 生成 lsqlite3 Lua 5.5 兼容头文件
# ============================================================================
# lsqlite3 v0.9.7 使用了 luaL_reg/luaL_typerror/luaL_openlib/luaL_register 等
# Lua 5.1/5.2 旧 API，在 Lua 5.5 中已移除。lua_compat.h 提供这些宏的兼容实现，
# 由 Makefile 的 -include 强制注入。该文件不在 lsqlite3_v097.zip 中，需手动生成。
LSQLITE3_SRC_DIR="$SCRIPT_DIR/src/lsqlite3"
ensure_lsqlite3_compat() {
    local compat_h="$LSQLITE3_SRC_DIR/lua_compat.h"
    if [[ -f "$compat_h" ]]; then
        ok "lsqlite3 lua_compat.h 已存在（跳过）"
        return 0
    fi
    log "生成 lsqlite3 lua_compat.h ..."
    cat > "$compat_h" <<'EOF'
/* Lua 5.1/5.2 compatibility defines for lsqlite3 when built against Lua 5.5 */
#ifndef LUA_COMPAT_H
#define LUA_COMPAT_H

/* luaL_reg was renamed to luaL_Reg in Lua 5.2 */
#ifndef luaL_reg
#define luaL_reg luaL_Reg
#endif

/* luaL_typerror was removed in Lua 5.3; use luaL_typeerror (same signature) */
#define luaL_typerror(L, n, t) luaL_typeerror(L, n, t)

/* luaL_openlib with NULL name: just sets functions on the table at stack top */
#define luaL_openlib(L, name, reg, nup) \
    do { \
        if (name) { \
            luaL_newlib(L, reg); \
        } else { \
            luaL_setfuncs(L, reg, nup); \
        } \
    } while (0)

/* luaL_register was removed in Lua 5.2.
 * Original behavior: create/open module table, set functions, push it,
 * set package.loaded[name] = table, set _G[name] = table.
 * Our compat: create new table, set functions, leave on stack,
 * set global name to it so require() can find it. */
#define luaL_register(L, name, reg) \
    do { \
        luaL_newlib(L, reg); \
        lua_pushvalue(L, -1); \
        lua_setglobal(L, name); \
    } while (0)
#endif /* LUA_COMPAT_H */
EOF
    ok "lsqlite3 lua_compat.h 已生成: $compat_h"
}
ensure_lsqlite3_compat

# ============================================================================
# 4b. 给 lunit.lua 打 Lua 5.2+ 兼容补丁
# ============================================================================
# LuaDist/lsqlite3 自带的 lunit.lua 是 Lua 5.1 版本，直接用 setfenv/getfenv。
# Lua 5.2+ 移除了这两个函数，需在 lunit.lua 开头插入兼容 shim。
patch_lunit_compat() {
    local lunit_lua="$LSQLITE3_SRC_DIR/lunit.lua"
    if [[ ! -f "$lunit_lua" ]]; then
        return 0
    fi
    if grep -q "Lua 5.2+ compatibility for setfenv" "$lunit_lua" 2>/dev/null; then
        ok "lunit.lua 兼容补丁已存在（跳过）"
        return 0
    fi
    log "应用 lunit.lua Lua 5.2+ 兼容补丁..."
    local tmp_shim
    tmp_shim=$(mktemp)
    cat > "$tmp_shim" <<'LUAEOF'
-- Lua 5.2+ compatibility for setfenv/getfenv (removed in Lua 5.2)
if not setfenv then
    local _debug = debug
    local _type = type
    function setfenv(f, t)
        if _type(f) == 'number' then
            f = _debug.getinfo(f + 1, 'f').func
        end
        _debug.setupvalue(f, 1, t)
        return f
    end
end
if not getfenv then
    local _debug = debug
    local _type = type
    function getfenv(f)
        if _type(f) == 'number' then
            f = _debug.getinfo(f + 1, 'f').func
        end
        local name, value = _debug.getupvalue(f, 1)
        if name == '_ENV' then return value end
        return _G
    end
end
-- Lua 5.2+ compatibility for table.getn (removed in Lua 5.2, use # operator)
if not table.getn then
    table.getn = function(t) return #t end
end
LUAEOF
    # 在 license 头部 --]] 之后插入 shim（sed 'r' 读取临时文件内容追加到匹配行后）
    sed -i '/^--\]\]/r '"$tmp_shim" "$lunit_lua"
    rm -f "$tmp_shim"
    grep -q "Lua 5.2+ compatibility for setfenv" "$lunit_lua" \
        || die "lunit.lua 补丁未生效"
    ok "lunit.lua 兼容补丁已应用"
}
patch_lunit_compat

# ============================================================================
# 4c. 应用 skynet Lua-rs 兼容补丁
# ============================================================================
# skynet 自带修改版 Lua 5.5.1 (ejoy/lua skynet55 分支)，包含 skynet 扩展 API。
# 用 lua-rs (官方 Lua 5.5.0 ABI) 替换 skynet 自带 Lua 时，需处理三处不兼容：
#   a) service_snlua.c: luaL_makeseed → time(NULL)（硬依赖，无兜底）
#   b) service_snlua.c: coroutine.resume/wrap 替换跳过（LUA_RUST_COMPAT 宏，
#      lua-rs 的 lua_resume 不支持恢复挂起协程）
#   c) lua-skynet.c: callback 机制改用 luaL_ref 存 registry（lua-rs 的
#      lua_newthread 返回主 L，无法提供独立栈）
#   d) lua-sharetable.c: #include "lgc.h" 移到 #ifdef makeshared 内
# a/b/c 通过一个 patch 文件应用，d 通过 sed 应用。
SKYNET_SRC_DIR="$SCRIPT_DIR/src/skynet"
apply_skynet_patch() {
    return 0  # 已在 lua-rs 中实现 luaL_makeseed/lua_newthread/lua_resume，无需修改 skynet 源码
    # 以下为历史补丁逻辑，保留备查但不执行
    local snlua_c="$SKYNET_SRC_DIR/service-src/service_snlua.c"
    local skynet_c="$SKYNET_SRC_DIR/lualib-src/lua-skynet.c"
    local sharetable_c="$SKYNET_SRC_DIR/lualib-src/lua-sharetable.c"

    if [[ ! -f "$snlua_c" ]]; then
        return 0
    fi

    # 检查是否已打补丁（lua-skynet.c 中是否有 traceback_ref）
    if grep -q "traceback_ref" "$skynet_c" 2>/dev/null; then
        ok "skynet 补丁已存在（跳过）"
        return 0
    fi

    log "应用 skynet Lua-rs 兼容补丁..."

    # a/b/c) 应用 lua-skynet.c + service_snlua.c 补丁
    local patch_file
    patch_file=$(mktemp /tmp/skynet-lua-rs-XXXXXX.patch)
    cat > "$patch_file" <<'PATCH_EOF'
diff --git a/lualib-src/lua-skynet.c b/lualib-src/lua-skynet.c
--- a/lualib-src/lua-skynet.c
+++ b/lualib-src/lua-skynet.c
@@ -10,6 +10,7 @@
 #include <lauxlib.h>
 #include <stdlib.h>
 #include <string.h>
+#include <stdio.h>
 #include <assert.h>
 #include <inttypes.h>

@@ -48,6 +49,8 @@ traceback (lua_State *L) {

 struct callback_context {
 	lua_State *L;
+	int traceback_ref;
+	int dispatch_ref;
 };

 static int
@@ -56,7 +59,12 @@ _cb(struct skynet_context * context, void * ud, int type, int session, uint32_t
 	lua_State *L = cb_ctx->L;
 	int trace = 1;
 	int r;
-	lua_pushvalue(L,2);
+	lua_rawgeti(L, LUA_REGISTRYINDEX, cb_ctx->traceback_ref);
+	lua_rawgeti(L, LUA_REGISTRYINDEX, cb_ctx->dispatch_ref);

 	lua_pushinteger(L, type);
 	lua_pushlightuserdata(L, (void *)msg);
@@ -66,6 +74,9 @@ _cb(struct skynet_context * context, void * ud, int type, int session, uint32_t

 	r = lua_pcall(L, 5, 0 , trace);

+	lua_settop(L, 0);
+
 	if (r == LUA_OK) {
 		return 0;
 	}
@@ -95,28 +106,40 @@ forward_cb(struct skynet_context * context, void * ud, int type, int session, ui
 }

 static void
-clear_last_context(lua_State *L) {
+release_last_context(lua_State *L) {
 	if (lua_getfield(L, LUA_REGISTRYINDEX, "callback_context") == LUA_TUSERDATA) {
-		lua_pushnil(L);
-		lua_setiuservalue(L, -2, 2);
+		struct callback_context *old = (struct callback_context *)lua_touserdata(L, -1);
+		if (old && old->traceback_ref != LUA_NOREF) {
+			luaL_unref(L, LUA_REGISTRYINDEX, old->traceback_ref);
+			luaL_unref(L, LUA_REGISTRYINDEX, old->dispatch_ref);
+			old->traceback_ref = LUA_NOREF;
+			old->dispatch_ref = LUA_NOREF;
+		}
 	}
 	lua_pop(L, 1);
 }

 static int
 _cb_pre(struct skynet_context * context, void * ud, int type, int session, uint32_t source, const void * msg, size_t sz) {
-	struct callback_context *cb_ctx = (struct callback_context *)ud;
-	clear_last_context(cb_ctx->L);
+	release_last_context(((struct callback_context *)ud)->L);
 	skynet_callback(context, ud, _cb);
-	return _cb(context, cb_ctx, type, session, source, msg, sz);
+	return _cb(context, ud, type, session, source, msg, sz);
 }

 static int
 _forward_pre(struct skynet_context *context, void *ud, int type, int session, uint32_t source, const void *msg, size_t sz) {
-	struct callback_context *cb_ctx = (struct callback_context *)ud;
-	clear_last_context(cb_ctx->L);
+	release_last_context(((struct callback_context *)ud)->L);
 	skynet_callback(context, ud, forward_cb);
-	return forward_cb(context, cb_ctx, type, session, source, msg, sz);
+	return forward_cb(context, ud, type, session, source, msg, sz);
 }

 static int
@@ -124,15 +147,31 @@ lcallback(lua_State *L) {
 	struct skynet_context * context = lua_touserdata(L, lua_upvalueindex(1));
 	int forward = lua_toboolean(L, 2);
 	luaL_checktype(L,1,LUA_TFUNCTION);
-	lua_settop(L,1);
-	struct callback_context * cb_ctx = (struct callback_context *)lua_newuserdatauv(L, sizeof(*cb_ctx), 2);
-	cb_ctx->L = lua_newthread(L);
-	lua_pushcfunction(cb_ctx->L, traceback);
-	lua_setiuservalue(L, -2, 1);
-	lua_getfield(L, LUA_REGISTRYINDEX, "callback_context");
-	lua_setiuservalue(L, -2, 2);
+	lua_settop(L,1);
+	release_last_context(L);
+	struct callback_context * cb_ctx = (struct callback_context *)lua_newuserdatauv(L, sizeof(*cb_ctx), 0);
+	cb_ctx->L = L;
+	cb_ctx->traceback_ref = LUA_NOREF;
+	cb_ctx->dispatch_ref = LUA_NOREF;
+	lua_pushcfunction(L, traceback);
+	cb_ctx->traceback_ref = luaL_ref(L, LUA_REGISTRYINDEX);
+	lua_pushvalue(L, 1);
+	cb_ctx->dispatch_ref = luaL_ref(L, LUA_REGISTRYINDEX);
 	lua_setfield(L, LUA_REGISTRYINDEX, "callback_context");
-	lua_xmove(L, cb_ctx->L, 1);

 	skynet_callback(context, cb_ctx, (forward)?(_forward_pre):(_cb_pre));
 	return 0;
diff --git a/service-src/service_snlua.c b/service-src/service_snlua.c
--- a/service-src/service_snlua.c
+++ b/service-src/service_snlua.c
@@ -391,14 +391,22 @@ init_cb(struct snlua *l, struct skynet_context *ctx, const char * args, size_t s
 	luaL_requiref(L, "skynet.profile", init_profile, 0);

 	int profile_lib = lua_gettop(L);
-	// replace coroutine.resume / coroutine.wrap
+	// lua-rs compat: skip coroutine.resume/wrap replacement
+#ifndef LUA_RUST_COMPAT
 	lua_getglobal(L, "coroutine");
 	lua_getfield(L, profile_lib, "resume");
 	lua_setfield(L, -2, "resume");
 	lua_getfield(L, profile_lib, "wrap");
 	lua_setfield(L, -2, "wrap");
-
 	lua_settop(L, profile_lib-1);
+#else
+	lua_settop(L, profile_lib-1);
+#endif

 	lua_pushlightuserdata(L, ctx);
 	lua_setfield(L, LUA_REGISTRYINDEX, "skynet_context");
@@ -504,7 +512,7 @@ global_seed() {
 	static ATOM_INT seed = 0;
 	unsigned ret = ATOM_LOAD(&seed);
 	while (ret == 0) {
-		unsigned t = luaL_makeseed(NULL);
+		unsigned t = (unsigned)time(NULL);
 		if (t == 0)
 			t = 1;
 		ATOM_CAS(&seed, 0, t);
PATCH_EOF

    # 应用补丁（在 skynet 源码目录内执行 patch -p1）
    ( cd "$SKYNET_SRC_DIR" && patch -p1 < "$patch_file" ) || {
        rm -f "$patch_file"
        die "skynet 补丁应用失败"
    }
    rm -f "$patch_file"

    # d) lua-sharetable.c: 把 #include "lgc.h" 移到 #ifdef makeshared 内
    if grep -q '^#include "lgc.h"$' "$sharetable_c" 2>/dev/null; then
        sed -i '/^#include "lgc.h"$/d' "$sharetable_c"
        sed -i '/^#ifdef makeshared$/a #include "lgc.h"' "$sharetable_c"
    fi

    # 验证
    grep -q "traceback_ref" "$skynet_c" || die "lua-skynet.c 补丁未生效"
    grep -q "LUA_RUST_COMPAT" "$snlua_c" || die "service_snlua.c LUA_RUST_COMPAT 补丁未生效"
    grep -q "time(NULL)" "$snlua_c" || die "service_snlua.c time(NULL) 替换未生效"

    ok "skynet 补丁已应用"
}
apply_skynet_patch

# ============================================================================
# 5. 编译 C 模块 (cjson, luasocket, lsqlite3, sol2 smoke test, skynet)
# ============================================================================
log "编译 C/C++ 模块 (make all)..."
# make 不需要绕过内存限制（编译单个 .so 内存占用小）
make -C "$SCRIPT_DIR" all
ok "C/C++ 模块编译完成"

# ============================================================================
# 6. 配置 luarocks（生成配置文件，便于 test.sh 使用）
# ============================================================================
LUAROCKS_SRC="$SCRIPT_DIR/src/luarocks-3.13.0"
LUAROCKS_CONFIG="$SCRIPT_DIR/luarocks-config.lua"
if [[ -d "$LUAROCKS_SRC" && ! -f "$LUAROCKS_CONFIG" ]]; then
    log "生成 luarocks 配置..."
    cat > "$LUAROCKS_CONFIG" <<EOF
-- luarocks 配置：指向本仓库 Rust lua 与 deps/lib
local DEPS = "$SCRIPT_DIR"
local PROJECT = "$PROJECT_ROOT"

lua_version = "5.5"
lua_interpreter = "$LUA_BIN"

-- 让 luarocks 找到 deps/lib/lua/5.5 下的已安装模块
package.path  = DEPS .. "/lib/lua/5.5/?.lua;" .. DEPS .. "/lib/lua/5.5/?/init.lua;;"
package.cpath = DEPS .. "/lib/lua/5.5/?.so;" .. DEPS .. "/lib/lua/5.5/?/core.so;;"

-- 不去系统目录安装
rocks_trees = {
    { root = DEPS .. "/luarocks-root", lib_dir = "lib/lua/5.5", bin_dir = "bin" },
}

-- 临时构建目录
local_variables = {
    LUA_DIR = PROJECT,
    LUA_INCDIR = PROJECT .. "/src",
    LUA_LIBDIR = PROJECT .. "/target/release",
}
EOF
    ok "luarocks 配置已生成: $LUAROCKS_CONFIG"
fi

echo
ok "=== setup 完成 ==="
echo "  Rust lua:    $LUA_BIN"
echo "  Rust lua 静态库: $LUA_LIB"
echo "  C 模块目录:   $SCRIPT_DIR/lib/lua/5.5/"
echo "  sol2 测试:    $SCRIPT_DIR/build/sol2_smoke"
echo "  skynet 二进制: $SCRIPT_DIR/src/skynet/skynet"
echo
echo "运行测试: bash $SCRIPT_DIR/test.sh"
