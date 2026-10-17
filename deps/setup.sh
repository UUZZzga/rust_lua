#!/bin/bash
# setup.sh - 一键构建所有依赖库（下载 + 编译）
#
# 流程:
#   1. 构建 Rust lua (target/release/lua + liblua_rs.a)
#   2. 下载依赖库源码 (deps/fetch.sh)
#   3. 编译所有 C 模块 (deps/Makefile); Windows 下额外构建 lua_rs.dll
#      (deps/lua_dll 的 cdylib, skynet 的宿主 Lua) 并给 skynet 的 mingw.mk 打补丁
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

USE_DEBUG=0
if [[ "${2:-}" == "--debug" ]]; then
    USE_DEBUG=1
    ok "使用调试模式"
fi

# Windows (MinGW/MSYS) 与 POSIX 的分叉: lua_rs.dll 与 skynet 的 mingw.mk 补丁只在
# Windows 需要 (Linux 的 skynet 直接链 liblua_rs.a)
IS_WINDOWS=0
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) IS_WINDOWS=1 ;;
esac

# ============================================================================
# 1. 构建 Rust lua (启用 threaded + skynet feature)
# ============================================================================
# threaded feature: 让 StringTable/ArcRc/LongString 走线程安全路径
#   - StringTable 内部用 parking_lot::RwLock 保护 (替代 RefCell)
#   - ArcRc 切换为 Arc (替代 Rc), 引用计数原子化
#   - LongString 的 hash/cached 字段用 AtomicU64/AtomicU8
# 这让 StringTable 自身是 Sync, 即使 skynet 多 worker 线程并发访问同一个
# string_table (理论上 actor 模型保证不会, 但作为防御性措施更安全).
#
# skynet feature: 导出 skynet 修改版 Lua 扩展 API (luaL_alloc/lua_clonetable/
#   lua_sharefunction/lua_sharestring/luaL_loadfilex_), 让 luaclib/skynet.so
#   能正确解析符号. 见 src_rs/capi.rs 末尾 "skynet 扩展 API" 章节.
#
# 注: LuaState 整体仍 !Sync (含 Rc<GCState>/Rc<RefCell<...>> 等共享字段),
#     跨线程共享同一个 LuaState 仍不允许. skynet 每个 service 用 luaL_newstate
#     创建独立 LuaState, 不跨线程共享.
LUA_BIN="$PROJECT_ROOT/target/release/lua"
LUA_LIB="$PROJECT_ROOT/target/release/liblua_rs.a"
if [[ $USE_DEBUG == 1 ]]; then
    LUA_BIN="$PROJECT_ROOT/target/debug/lua"
    LUA_LIB="$PROJECT_ROOT/target/debug/liblua_rs.a"
fi
# Windows: 可执行文件带 .exe, Rust 的 staticlib 叫 lua_rs.lib (MSVC 命名);
# MinGW 侧 skynet 实际链接的是 deps/Makefile 用 dlltool 从导出清单生成的导入库
# (liblua_exe.a / liblua_rs_dll.a), 不直接用 staticlib。
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        LUA_BIN="${LUA_BIN}.exe"
        LUA_LIB="${LUA_LIB%/*}/lua_rs.lib"
        ;;
esac

if [[ -f "$LUA_BIN" && -f "$LUA_LIB" && "${1:-}" != "--rebuild-lua" ]]; then
    ok "Rust lua 已构建: $LUA_BIN"
else
    log "构建 Rust lua (cargo build --release --features lua_longjmp,threaded,skynet)..."
    run_unlimited cargo build --release --features lua_longjmp,threaded,skynet
    [[ -f "$LUA_BIN" ]] || die "lua 二进制未生成: $LUA_BIN"
    [[ -f "$LUA_LIB" ]] || die "静态库未生成: $LUA_LIB"
    ok "Rust lua 构建完成"
fi

# C 模块 (含 skynet) 的宿主导入库清单, 由 build.rs 生成 —— 缺了就重新构建 lua-rs
# (仅 Windows 用; Linux 由 skynet 直接链 liblua_rs.a)
if [[ $IS_WINDOWS == 1 ]]; then
    for def in lua_exe_import.def lua_rs_dll_import.def; do
        diff_def="$PROJECT_ROOT/target/release/$def"
        [[ -f "$diff_def" ]] || die "缺少导出清单 $diff_def (build.rs 未生成; 用 setup.sh --rebuild-lua 重建)"
    done
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
# 4c. skynet: 宿主 Lua 一律用 lua-rs
# ============================================================================
# skynet 自带修改版 Lua 5.5.1 (ejoy/lua skynet55 分支, 3rd/lua: 扩了
# lua_sharefunction/lua_sharestring/lua_clonetable/luaL_loadfilex_/luaL_makeseed,
# lgc.h 里加了 makeshared/isshared, 另有 onelua.c 单文件构建), 本项目不用它:
#   - 头文件: src/ (官方 Lua 5.5.0, 即 lua-rs 遵循的 C API)
#   - Linux: skynet 直接链接 liblua_rs.a, 用 -Wl,-E 把符号导出给 dlopen 的 .so
#   - MinGW: 用 lua_rs.dll 作宿主 Lua (deps/lua_dll 的 cdylib, 见下面 cargo 步骤);
#     deps/Makefile 用 dlltool 把 build.rs 生成的导出清单转成导入库
# 因此 skynet 源码无需任何补丁:
#   - luaL_makeseed 是官方 API (src/lauxlib.h 声明, src/lauxlib.c 实现)
#   - lua_newthread / lua_resume 已由 lua-rs 实现 (无 .so 需要 registry 版 callback)
#   - lua-sharetable.c 的主体在 #ifdef makeshared 内, 用官方头文件时整体编译掉
#     (src/lgc.h 无 makeshared 宏), 不依赖 skynet 的 Lua 内部结构
# 唯一要改的是 skynet 的 MinGW 构建脚本 mingw.mk: 它硬编码了 3rd/lua 的头文件
# 目录、lua54.dll 与 vendored Lua 的构建规则 (见下面的补丁)。
SKYNET_SRC_DIR="$SCRIPT_DIR/src/skynet"
SKYNET_MINGW_MK="$SKYNET_SRC_DIR/mingw.mk"

# lua_rs.dll: skynet 的宿主 Lua 动态库 (仅 Windows 需要)。
# 不能拿 lua-rs 自己的 lua.exe 顶替 —— LoadLibrary 一个 EXE 会以 mainCRTStartup
# 当 DllMain 调用, Rust 运行期不初始化, 实测 lua_newstate 即崩。
if [[ $IS_WINDOWS == 1 ]]; then
    log "构建 lua_rs.dll (skynet 宿主 Lua, deps/lua_dll)..."
    run_unlimited cargo build --release --manifest-path "$SCRIPT_DIR/lua_dll/Cargo.toml" \
        --target-dir "$PROJECT_ROOT/target"
    [[ -f "$PROJECT_ROOT/target/release/lua_rs.dll" ]] || die "lua_rs.dll 未生成"
    ok "lua_rs.dll 构建完成"
fi

# mingw.mk: 去掉 3rd/lua (vendored 修改版 Lua) 的头文件/库/构建规则, 换成 lua-rs
patch_skynet_mingw_mk() {
    [[ -f "$SKYNET_MINGW_MK" ]] || die "skynet mingw.mk 不存在: $SKYNET_MINGW_MK (先运行 fetch.sh)"
    if grep -q "LUA_RS_DLL" "$SKYNET_MINGW_MK"; then
        ok "skynet mingw.mk lua-rs 补丁已存在（跳过）"
        return 0
    fi
    log "应用 skynet mingw.mk lua-rs 补丁..."
    local patch_file
    patch_file=$(mktemp /tmp/skynet-mingw-lua-rs-XXXXXX.patch)
    cat > "$patch_file" <<'PATCH_EOF'
diff --git a/mingw.mk b/mingw.mk
--- a/mingw.mk
+++ b/mingw.mk
@@ -8,15 +8,23 @@ CSERVICE_PATH ?= cservice
 SKYNET_BUILD_PATH ?= .
 COMPAT_MINGW_DIR = 3rd/compat-mingw
 
-LUA_DIR = 3rd/lua
-LUA_CFLAGS = -g -O2 -Wall -I. -std=gnu99 -I../../skynet-src
-LUA_STATICLIB := 3rd/lua/liblua.a
-LUA_DLL ?= 3rd/lua/lua54.dll
+# 宿主 Lua 使用 lua-rs（不再构建/使用 skynet 自带的修改版 Lua: 3rd/lua）:
+#   LUA_RS_SRC         官方 Lua 5.5 头文件目录（lua-rs 遵循的 C API，src/）
+#   LUA_RS_DLL         lua-rs 的 C ABI 动态库 lua_rs.dll（deps/lua_dll 构建；
+#                      构建后被拷到本目录，运行时提供 lua_* 符号）
+#   LUA_RS_IMPORT_LIB  lua_rs.dll 的 MinGW 导入库（deps/Makefile 用 dlltool 生成）
+LUA_RS_ROOT ?= ../../..
+LUA_RS_SRC ?= $(LUA_RS_ROOT)/src
+LUA_RS_DLL ?= $(LUA_RS_ROOT)/target/release/lua_rs.dll
+LUA_RS_IMPORT_LIB ?= $(LUA_RS_ROOT)/target/release/liblua_rs_dll.a
+
+LUA_STATICLIB := $(LUA_RS_IMPORT_LIB)
+LUA_DLL ?= $(LUA_RS_DLL)
 SKYNET_DLL ?= $(SKYNET_BUILD_PATH)/skynet.dll
-LUA_INC ?= $(LUA_DIR)
+LUA_INC ?= $(LUA_RS_SRC)
 
 # Lua linking options
-LUA_LIBS = -L$(LUA_DIR) -llua54
+LUA_LIBS = $(LUA_RS_IMPORT_LIB)
 LUA_STATIC_LIBS = $(LUA_STATICLIB)
 
 # Skynet linking and include options
@@ -26,7 +34,7 @@ SHARED_BUILD = $(CC) $(CFLAGS) $(SHARED)
 
 # Compiler flags
 CFLAGS = -g -O2 -Wall -std=gnu99 -I$(LUA_INC) $(MYCFLAGS)
-CFLAGS += -I3rd/lua -Iskynet-src -I$(COMPAT_MINGW_DIR)
+CFLAGS += -Iskynet-src -I$(COMPAT_MINGW_DIR)
 CFLAGS += -include $(COMPAT_MINGW_DIR)/compat.h
 CFLAGS += -Wno-int-conversion -Wno-incompatible-pointer-types -Wno-pointer-sign -Wno-unused-function
 
@@ -64,27 +72,6 @@ SKYNET_SRC = skynet_main.c skynet_handle.c skynet_module.c skynet_mq.c \
   skynet_harbor.c skynet_env.c skynet_monitor.c skynet_socket.c socket_server.c \
   mem_info.c malloc_hook.c skynet_daemon.c skynet_log.c
 
-$(LUA_STATICLIB): 
-	@echo "Building Lua static library..."
-	cd $(LUA_DIR) && $(CC) $(LUA_CFLAGS) -DMAKE_LIB -c onelua.c -o onelua.o
-	cd $(LUA_DIR) && $(AR) rcs liblua.a onelua.o
-
-$(LUA_DLL) : $(LUA_STATICLIB)
-	@echo "Building Lua DLL..."
-	cd $(LUA_DIR) && $(CC) -shared -o lua54.dll onelua.o -Wl,--export-all-symbols,--out-implib,liblua54.a $(SKYNET_LIBS)
-	@echo "Lua DLL and import library created successfully"
-
-$(LUA_DIR)/lua.exe : $(LUA_DLL) $(LUA_STATICLIB)
-	@echo "Building Lua interpreter..."
-	cd $(LUA_DIR) && $(CC) $(LUA_CFLAGS) -c lua.c -o lua.o
-	cd $(LUA_DIR) && $(CC) $(LUA_CFLAGS) -o lua.exe lua.o -L. -llua54 $(SKYNET_LIBS)
-	cp $(LUA_DLL) $(SKYNET_BUILD_PATH)/
-
-$(LUA_DIR)/luac.exe : $(LUA_DLL) $(LUA_STATICLIB)
-	@echo "Building Lua compiler..."
-	cd $(LUA_DIR) && $(CC) $(LUA_CFLAGS) -DMAKE_LUAC -c onelua.c -o luac.o
-	cd $(LUA_DIR) && $(CC) $(LUA_CFLAGS) -o luac.exe luac.o liblua.a $(SKYNET_LIBS)
-
 $(COMPAT_LIB): $(COMPAT_MINGW_DIR)/compat.c
 	@echo "Building compatibility library..."
 	$(CC) $(CFLAGS) -c $(COMPAT_MINGW_DIR)/compat.c -o $(COMPAT_MINGW_DIR)/compat.o
@@ -100,7 +87,7 @@ core_libs: $(LUA_STATICLIB) $(LUA_DLL) $(SKYNET_DLL)
 
 modules: core_libs $(foreach v, $(CSERVICE), $(CSERVICE_PATH)/$(v).so) $(foreach v, $(LUA_CLIB), $(LUA_CLIB_PATH)/$(v).so)
 
-tools: core_libs $(LUA_DIR)/lua.exe $(LUA_DIR)/luac.exe $(SKYNET_BUILD_PATH)/skynet.exe
+tools: core_libs $(SKYNET_BUILD_PATH)/skynet.exe
 
 
 SKYNET_LIB_SRC = $(filter-out skynet_main.c, $(SKYNET_SRC))
@@ -154,8 +141,5 @@ clean :
 	rm -f $(SKYNET_BUILD_PATH)/*.exe $(SKYNET_BUILD_PATH)/*.dll $(SKYNET_BUILD_PATH)/*.a
 	rm -f $(COMPAT_LIB) $(COMPAT_MINGW_DIR)/*.o
 	rm -f $(CSERVICE_PATH)/*.so $(LUA_CLIB_PATH)/*.so
-	rm -f $(LUA_DIR)/*.a
 
 cleanall: clean
-	rm -f $(LUA_STATICLIB)
-	rm -f $(LUA_DIR)/*.exe $(LUA_DIR)/*.dll $(LUA_DIR)/*.a $(LUA_DIR)/*.o
PATCH_EOF

    # git checkout 在 core.autocrlf=true 下写出 CRLF, patch 的上下文就不匹配了
    sed -i 's/\r$//' "$SKYNET_MINGW_MK"
    if ! ( cd "$SKYNET_SRC_DIR" && patch -p1 --no-backup-if-mismatch < "$patch_file" ); then
        rm -f "$patch_file"
        die "skynet mingw.mk 补丁应用失败"
    fi
    rm -f "$patch_file"

    grep -q "LUA_RS_DLL" "$SKYNET_MINGW_MK" || die "mingw.mk 补丁未生效"
    # 注意匹配到 3rd/lua 这个目录本身 (3rd/lua-md5 是 lpeg/md5 的源码目录, 无需改)
    if grep -qE -- '(^|[[:space:]])-I3rd/lua([[:space:]]|$)' "$SKYNET_MINGW_MK"; then
        die "mingw.mk 仍包含 -I3rd/lua (vendored 头文件目录)"
    fi
    ok "skynet mingw.mk 已切换到 lua-rs"
}
# mingw.mk 只有 Windows 构建会用到
if [[ $IS_WINDOWS == 1 ]]; then
    need patch
    patch_skynet_mingw_mk
fi

# ============================================================================
# 5. 编译 C 模块 (cjson, luasocket, lsqlite3, sol2 smoke test, skynet)
# ============================================================================
log "编译 C/C++ 模块 (make all)..."
# make 不需要绕过内存限制（编译单个 .so 内存占用小）
USE_DEBUG=$USE_DEBUG make -C "$SCRIPT_DIR" all
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
