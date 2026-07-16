#!/bin/bash
# fetch.sh - 下载所有依赖库源码到 cache/，支持缓存（已存在则跳过）
#
# 下载内容：
#   - lua-cjson   (openresty fork, master)
#   - luasocket   (lunarmodules, master)
#   - lsqlite3    (v0.9.7)
#   - luarocks    (3.13.0)
#   - sol2        (v3.3.0)
#   - sqlite3     (amalgamation 3450100, 供 lsqlite3 静态链接)
#
# 用法：./fetch.sh
#
# 缓存目录：deps/cache/
# 解压目录：deps/src/
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE_DIR="$SCRIPT_DIR/cache"
SRC_DIR="$SCRIPT_DIR/src"
mkdir -p "$CACHE_DIR" "$SRC_DIR"

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[fetch]${NC} $*"; }
warn() { echo -e "${YELLOW}[warn]${NC} $*"; }
die()  { echo -e "${RED}[error]${NC} $*" >&2; exit 1; }

# 下载文件到 cache（带缓存）
# 用法: download <url> <output_filename>
download() {
    local url="$1"
    local out="$CACHE_DIR/$2"
    if [[ -f "$out" && -s "$out" ]]; then
        log "缓存命中: $2"
        return 0
    fi
    log "下载: $url"
    # 重试 3 次
    local i=0
    for ((i=0; i<3; i++)); do
        if curl -fL --retry 3 --retry-delay 2 -o "$out.tmp" "$url"; then
            mv "$out.tmp" "$out"
            log "完成: $2 ($(du -h "$out" | cut -f1))"
            return 0
        fi
        warn "下载失败，重试 $((i+1))/3..."
        sleep 2
    done
    rm -f "$out.tmp"
    die "下载失败: $url"
}

# 解压并重命名顶层目录
# 用法: extract <archive> <expected_top_dir_name> <final_dir_name>
extract_tarball() {
    local archive="$1"
    local expected="$2"
    local final="$3"
    if [[ -d "$SRC_DIR/$final" ]]; then
        log "已存在: src/$final (跳过解压)"
        return 0
    fi
    log "解压: $(basename "$archive") -> src/$final"
    local tmp_extract="$SRC_DIR/.extract_$$"
    mkdir -p "$tmp_extract"
    case "$archive" in
        *.tar.gz|*.tgz) tar -xzf "$archive" -C "$tmp_extract" ;;
        *.zip)          unzip -q "$archive" -d "$tmp_extract" ;;
        *) die "不支持的压缩格式: $archive" ;;
    esac
    # 找到解压后的顶层目录
    local top
    top=$(ls -A "$tmp_extract" | head -1)
    if [[ -z "$top" ]]; then
        rm -rf "$tmp_extract"
        die "解压后为空: $archive"
    fi
    mv "$tmp_extract/$top" "$SRC_DIR/$final"
    rmdir "$tmp_extract" 2>/dev/null || true
    log "完成: src/$final"
}

# ============================================================================
# 1. lua-cjson (openresty fork)
# ============================================================================
# CJSON_URL="https://github.com/openresty/lua-cjson/archive/refs/heads/master.tar.gz"
CJSON_URL="https://github.com/KritzelKratzel/lua-cjson-lua55/archive/refs/heads/master.tar.gz"
CJSON_ARCHIVE="lua-cjson-master.tar.gz"
download "$CJSON_URL" "$CJSON_ARCHIVE"
extract_tarball "$CACHE_DIR/$CJSON_ARCHIVE" "lua-cjson-master" "lua-cjson"

# ============================================================================
# 2. luasocket (lunarmodules)
# ============================================================================
LUASOCKET_URL="https://github.com/lunarmodules/luasocket/archive/refs/heads/master.tar.gz"
LUASOCKET_ARCHIVE="luasocket-master.tar.gz"
download "$LUASOCKET_URL" "$LUASOCKET_ARCHIVE"
extract_tarball "$CACHE_DIR/$LUASOCKET_ARCHIVE" "luasocket-master" "luasocket"

# ============================================================================
# 3. lsqlite3 v0.9.7
# ============================================================================
LSQLITE3_URL="http://lua.sqlite.org/home/zip/lsqlite3_v097.zip?uuid=v0.9.7"
LSQLITE3_ARCHIVE="lsqlite3_v097.zip"
download "$LSQLITE3_URL" "$LSQLITE3_ARCHIVE"
extract_tarball "$CACHE_DIR/$LSQLITE3_ARCHIVE" "lsqlite3_v097" "lsqlite3"

# ============================================================================
# 4. luarocks 3.13.0
# ============================================================================
LUAROCKS_URL="https://luarocks.org/releases/luarocks-3.13.0.tar.gz"
LUAROCKS_ARCHIVE="luarocks-3.13.0.tar.gz"
download "$LUAROCKS_URL" "$LUAROCKS_ARCHIVE"
extract_tarball "$CACHE_DIR/$LUAROCKS_ARCHIVE" "luarocks-3.13.0" "luarocks-3.13.0"

# ============================================================================
# 5. sol2 v3.3.0
# ============================================================================
SOL2_URL="https://github.com/ThePhD/sol2/archive/refs/tags/v3.3.0.tar.gz"
SOL2_ARCHIVE="sol2-3.3.0.tar.gz"
download "$SOL2_URL" "$SOL2_ARCHIVE"
extract_tarball "$CACHE_DIR/$SOL2_ARCHIVE" "sol2-3.3.0" "sol2"

# ============================================================================
# 6. sqlite3 amalgamation (供 lsqlite3 静态链接)
# ============================================================================
SQLITE_URL="https://www.sqlite.org/2024/sqlite-amalgamation-3450100.zip"
SQLITE_ARCHIVE="sqlite-amalgamation-3450100.zip"
download "$SQLITE_URL" "$SQLITE_ARCHIVE"
# sqlite3 解压后是 sqlite-amalgamation-3450100/，放到 src/sqlite3/ 下
if [[ -d "$SRC_DIR/sqlite3/sqlite-amalgamation-3450100" ]]; then
    log "已存在: src/sqlite3/sqlite-amalgamation-3450100 (跳过解压)"
else
    log "解压: $SQLITE_ARCHIVE -> src/sqlite3/sqlite-amalgamation-3450100"
    mkdir -p "$SRC_DIR/sqlite3"
    local_tmp="$SRC_DIR/sqlite3/.extract_$$"
    mkdir -p "$local_tmp"
    unzip -q "$CACHE_DIR/$SQLITE_ARCHIVE" -d "$local_tmp"
    mv "$local_tmp/sqlite-amalgamation-3450100" "$SRC_DIR/sqlite3/sqlite-amalgamation-3450100"
    rmdir "$local_tmp" 2>/dev/null || true
    # 兼容旧 Makefile 中 src/sqlite3/sqlite3.h 软链
    ln -sf sqlite-amalgamation-3450100/sqlite3.h "$SRC_DIR/sqlite3/sqlite3.h"
    ln -sf sqlite-amalgamation-3450100/sqlite3.c "$SRC_DIR/sqlite3/sqlite3.c"
    log "完成: src/sqlite3/sqlite-amalgamation-3450100"
fi

log "全部依赖下载完成"
