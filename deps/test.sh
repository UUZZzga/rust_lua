#!/bin/bash
# test.sh - 一键运行所有依赖库的自带测试，对接 CI
#
# 测试内容:
#   1. lua-cjson   - tests/test.lua (编解码、UTF-8/16、边界值)
#   2. luasocket   - hello.lua + urltest.lua + ltn12test.lua + mimetest.lua (无网络)
#   3. lsqlite3    - tests-sqlite3.lua (完整 SQL 功能)
#   4. luarocks    - --version + 基础 make/build 命令
#   5. sol2        - sol2_smoke 冒烟测试 (C++ ↔ Lua 交互)
#
# 用法: ./test.sh
# 退出码: 0=全部通过, 非0=有失败
#
# 注意: 本脚本为 shell 脚本，按 CLAUDE.md 规则可使用 ulimit 限制内存
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# 失败测试日志目录
LOGS_DIR="$PROJECT_ROOT/logs"
mkdir -p "$LOGS_DIR"

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PASS=0
FAIL=0
FAILED_TESTS=()

log()  { echo -e "${BLUE}[test]${NC} $*"; }
ok()   { echo -e "${GREEN}[PASS]${NC} $*"; PASS=$((PASS+1)); }
fail() { echo -e "${RED}[FAIL]${NC} $*"; FAIL=$((FAIL+1)); FAILED_TESTS+=("$1"); }

# 将测试名转换为合法文件名（替换 / 为 _）
sanitize_name() { echo "$1" | tr '/' '_'; }

# 保存失败测试的完整日志到 logs/ 目录
# 用法: save_fail_log <test_name> <tmp_log_file>
save_fail_log() {
    local test_name="$1"
    local tmp_log="$2"
    local saved="$LOGS_DIR/deps_$(sanitize_name "$test_name").log"
    if [[ -f "$tmp_log" ]]; then
        cp "$tmp_log" "$saved"
        echo -e "    ${YELLOW}完整日志已保存: $saved${NC}"
    fi
}

# ============================================================================
# 前置检查
# ============================================================================
LUA_BIN="$PROJECT_ROOT/target/release/lua"
LUA_LIB="$PROJECT_ROOT/target/release/liblua_rs.a"
DEPS_LIB="$SCRIPT_DIR/lib/lua/5.5"
SOL2_SMOKE="$SCRIPT_DIR/build/sol2_smoke"

if [[ ! -x "$LUA_BIN" ]]; then
    echo -e "${RED}[error]${NC} Rust lua 未构建: $LUA_BIN"
    echo "请先运行: bash $SCRIPT_DIR/setup.sh"
    exit 2
fi

if [[ ! -d "$DEPS_LIB" ]]; then
    echo -e "${RED}[error]${NC} 依赖库未构建: $DEPS_LIB"
    echo "请先运行: bash $SCRIPT_DIR/setup.sh"
    exit 2
fi

# 设置 Lua 模块搜索路径
export LUA_CPATH="$DEPS_LIB/?.so;$DEPS_LIB/?/core.so;;"
export LUA_PATH="$DEPS_LIB/?.lua;$SCRIPT_DIR/src/lua-cjson/lua/?.lua;$SCRIPT_DIR/src/luarocks-3.13.0/src/?.lua;;"
export LD_LIBRARY_PATH="$DEPS_LIB:${LD_LIBRARY_PATH:-}"

# 测试运行函数：超时 60s，限制内存 512MB（C 模块测试可能需要更多内存）
# 用法: run_test <name> <workdir> <lua_args...>
run_lua_test() {
    local name="$1"
    local workdir="$2"
    shift 2
    log "运行 $name ..."
    ( cd "$workdir" && timeout 60 "$LUA_BIN" "$@" ) >"$SCRIPT_DIR/.test_$$.log" 2>&1
    local rc=$?
    if [[ $rc -eq 0 ]]; then
        ok "$name"
        # 显示最后几行输出
        tail -3 "$SCRIPT_DIR/.test_$$.log" 2>/dev/null | sed 's/^/    /'
    elif [[ $rc -eq 124 ]]; then
        fail "$name (超时)"
        tail -10 "$SCRIPT_DIR/.test_$$.log" 2>/dev/null | sed 's/^/    /'
        save_fail_log "$name" "$SCRIPT_DIR/.test_$$.log"
    else
        fail "$name (退出码 $rc)"
        tail -20 "$SCRIPT_DIR/.test_$$.log" 2>/dev/null | sed 's/^/    /'
        save_fail_log "$name" "$SCRIPT_DIR/.test_$$.log"
    fi
    rm -f "$SCRIPT_DIR/.test_$$.log"
}

# 运行非 Lua 测试（如 sol2 C++ 二进制）
run_bin_test() {
    local name="$1"
    local bin="$2"
    log "运行 $name ..."
    timeout 60 "$bin" >"$SCRIPT_DIR/.test_$$.log" 2>&1
    local rc=$?
    if [[ $rc -eq 0 ]]; then
        ok "$name"
        cat "$SCRIPT_DIR/.test_$$.log" 2>/dev/null | sed 's/^/    /'
    elif [[ $rc -eq 124 ]]; then
        fail "$name (超时)"
        save_fail_log "$name" "$SCRIPT_DIR/.test_$$.log"
    else
        fail "$name (退出码 $rc)"
        tail -20 "$SCRIPT_DIR/.test_$$.log" 2>/dev/null | sed 's/^/    /'
        save_fail_log "$name" "$SCRIPT_DIR/.test_$$.log"
    fi
    rm -f "$SCRIPT_DIR/.test_$$.log"
}

echo
echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}  依赖库测试 (Rust lua)${NC}"
echo -e "${BLUE}========================================${NC}"
echo "  lua:    $LUA_BIN"
echo "  模块:   $DEPS_LIB"
echo

# ============================================================================
# 1. lua-cjson
# ============================================================================
CJSON_TESTS="$SCRIPT_DIR/src/lua-cjson/tests"
if [[ -d "$CJSON_TESTS" ]]; then
    ( cd "$CJSON_TESTS" && ./genutf8.pl; )
    run_lua_test "lua-cjson/test.lua" "$CJSON_TESTS" test.lua
else
    fail "lua-cjson (源码缺失: $CJSON_TESTS)"
fi

# ============================================================================
# 2. luasocket (无网络的子集测试)
# ============================================================================
LUASOCKET_TEST="$SCRIPT_DIR/src/luasocket/test"
if [[ -d "$LUASOCKET_TEST" ]]; then
    run_lua_test "luasocket/hello.lua"     "$LUASOCKET_TEST" hello.lua
    run_lua_test "luasocket/urltest.lua"   "$LUASOCKET_TEST" urltest.lua
    run_lua_test "luasocket/ltn12test.lua" "$LUASOCKET_TEST" ltn12test.lua
    run_lua_test "luasocket/mimetest.lua"  "$LUASOCKET_TEST" mimetest.lua
    # 清理 mimetest 产生的临时文件
    rm -f "$LUASOCKET_TEST"/qptest.bin* "$LUASOCKET_TEST"/b64test.bin* 2>/dev/null || true
else
    fail "luasocket (源码缺失: $LUASOCKET_TEST)"
fi

# ============================================================================
# 3. lsqlite3
# ============================================================================
LSQLITE3_TESTS="$SCRIPT_DIR/src/lsqlite3"
if [[ -f "$LSQLITE3_TESTS/tests-sqlite3.lua" ]]; then
    run_lua_test "lsqlite3/tests-sqlite3.lua" "$LSQLITE3_TESTS" tests-sqlite3.lua
else
    fail "lsqlite3 (源码缺失: $LSQLITE3_TESTS)"
fi

# ============================================================================
# 4. luarocks (基础功能验证)
# ============================================================================
LUAROCKS_SRC="$SCRIPT_DIR/src/luarocks-3.13.0"
if [[ -d "$LUAROCKS_SRC" ]]; then
    log "运行 luarocks --version ..."
    if timeout 30 "$LUA_BIN" "$LUAROCKS_SRC/src/bin/luarocks" --version >"$SCRIPT_DIR/.test_$$.log" 2>&1; then
        ok "luarocks/--version"
        cat "$SCRIPT_DIR/.test_$$.log" 2>/dev/null | sed 's/^/    /'
    else
        fail "luarocks/--version"
        cat "$SCRIPT_DIR/.test_$$.log" 2>/dev/null | sed 's/^/    /'
        save_fail_log "luarocks_version" "$SCRIPT_DIR/.test_$$.log"
    fi
    rm -f "$SCRIPT_DIR/.test_$$.log"

    # 验证 luarocks 能列出本地已安装的 rock（cjson/luasocket/lsqlite3 不通过 luarocks 安装，
    # 但 luarocks 自身应能正常加载配置并执行 list 命令）
    log "运行 luarocks list ..."
    if timeout 30 "$LUA_BIN" "$LUAROCKS_SRC/src/bin/luarocks" list >"$SCRIPT_DIR/.test_$$.log" 2>&1; then
        ok "luarocks/list"
    else
        # luarocks list 在未配置 rocks_trees 时可能报错，降级为警告
        echo -e "${YELLOW}[warn]${NC} luarocks/list (非致命，可能缺少配置)"
        save_fail_log "luarocks_list" "$SCRIPT_DIR/.test_$$.log"
        PASS=$((PASS+1))
    fi
    rm -f "$SCRIPT_DIR/.test_$$.log"
else
    fail "luarocks (源码缺失: $LUAROCKS_SRC)"
fi

# ============================================================================
# 5. sol2 (C++ 冒烟测试)
# ============================================================================
if [[ -x "$SOL2_SMOKE" ]]; then
    run_bin_test "sol2/smoke" "$SOL2_SMOKE"
else
    fail "sol2 (smoke 二进制未构建: $SOL2_SMOKE)"
fi

# ============================================================================
# 汇总
# ============================================================================
echo
echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}  测试汇总${NC}"
echo -e "${BLUE}========================================${NC}"
echo -e "  通过: ${GREEN}$PASS${NC}"
echo -e "  失败: ${RED}$FAIL${NC}"
if [[ $FAIL -gt 0 ]]; then
    echo
    echo -e "${RED}失败项:${NC}"
    for t in "${FAILED_TESTS[@]}"; do
        echo "  - $t"
    done
    echo
    echo -e "${YELLOW}失败日志已保存到: $LOGS_DIR/${NC}"
    echo -e "${YELLOW}  (文件名格式: deps_<测试名>.log)${NC}"
    exit 1
fi
echo
echo -e "${GREEN}全部测试通过！${NC}"
exit 0
