# Makefile for building Lua C modules against the Rust Lua 5.5 implementation
#
# Each module is compiled as a shared library (.so) with lauxlib.c statically
# linked, so luaL_* functions (luaL_error, luaL_checklstring, etc.) are
# available even though they aren't exported by the Rust binary.
#
# Usage:
#   make all          Build all modules
#   make cjson        Build only lua-cjson
#   make luasocket    Build only luasocket (core + mime)
#   make lsqlite3     Build only lsqlite3 (requires sqlite3 headers)
#   make clean        Remove build artifacts

SHELL := /bin/bash

# --- Paths ----------------------------------------------------------------
PROJECT_ROOT := $(realpath $(CURDIR)/..)
LUA_INCLUDE  := $(PROJECT_ROOT)/src
DEPS_SRC     := $(CURDIR)/src
DEPS_BUILD   := $(CURDIR)/build
DEPS_LIB     := $(CURDIR)/lib/lua/5.5

CC       := gcc
CFLAGS   := -O2 -fPIC -fexceptions -Wall -Wextra -I$(LUA_INCLUDE)
LDFLAGS  := -shared


# --- Targets --------------------------------------------------------------
CJSON_TARGET     := $(DEPS_LIB)/cjson.so
SOCKET_CORE_TARGET := $(DEPS_LIB)/socket/core.so
MIME_TARGET       := $(DEPS_LIB)/mime/core.so
LSQLITE3_TARGET   := $(DEPS_LIB)/lsqlite3.so

ALL_TARGETS := $(CJSON_TARGET) $(SOCKET_CORE_TARGET) $(MIME_TARGET)

# --- lauxlib object (statically linked into each .so) ---------------------
LAUXLIB_O := $(DEPS_BUILD)/lauxlib.o

$(LAUXLIB_O): $(LUA_INCLUDE)/lauxlib.c $(LUA_INCLUDE)/lauxlib.h $(LUA_INCLUDE)/lua.h
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -c $< -o $@

# --- lua-cjson ------------------------------------------------------------
CJSON_SRC := $(DEPS_SRC)/lua-cjson
CJSON_OBJS := $(DEPS_BUILD)/lua_cjson.o $(DEPS_BUILD)/strbuf.o $(DEPS_BUILD)/fpconv.o

$(DEPS_BUILD)/lua_cjson.o: $(CJSON_SRC)/lua_cjson.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -c $< -o $@

$(DEPS_BUILD)/strbuf.o: $(CJSON_SRC)/strbuf.c
	$(CC) $(CFLAGS) -c $< -o $@

$(DEPS_BUILD)/fpconv.o: $(CJSON_SRC)/fpconv.c
	$(CC) $(CFLAGS) -c $< -o $@

$(CJSON_TARGET): $(CJSON_OBJS) $(LAUXLIB_O)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ -lm

cjson: $(CJSON_TARGET)

# --- luasocket (core + mime) ----------------------------------------------
LUASOCKET_SRC := $(DEPS_SRC)/luasocket/src
SOCKET_OBJS := \
	$(DEPS_BUILD)/luasocket.o \
	$(DEPS_BUILD)/auxiliar.o \
	$(DEPS_BUILD)/buffer.o \
	$(DEPS_BUILD)/except.o \
	$(DEPS_BUILD)/inet.o \
	$(DEPS_BUILD)/io.o \
	$(DEPS_BUILD)/luasocket.o \
	$(DEPS_BUILD)/options.o \
	$(DEPS_BUILD)/select.o \
	$(DEPS_BUILD)/tcp.o \
	$(DEPS_BUILD)/timeout.o \
	$(DEPS_BUILD)/udp.o \
	$(DEPS_BUILD)/usocket.o \
	$(DEPS_BUILD)/compat.o

SOCKET_CFLAGS := $(CFLAGS) -DLUASOCKET_API=extern

$(DEPS_BUILD)/%.o: $(LUASOCKET_SRC)/%.c
	@mkdir -p $(@D)
	$(CC) $(SOCKET_CFLAGS) -c $< -o $@

$(SOCKET_CORE_TARGET): $(SOCKET_OBJS) $(LAUXLIB_O)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ -lm

$(DEPS_BUILD)/mime.o: $(LUASOCKET_SRC)/mime.c
	@mkdir -p $(@D)
	$(CC) $(SOCKET_CFLAGS) -c $< -o $@

$(MIME_TARGET): $(DEPS_BUILD)/mime.o $(LAUXLIB_O)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ -lm

luasocket: $(SOCKET_CORE_TARGET) $(MIME_TARGET)
	@mkdir -p $(DEPS_LIB)
	@echo "=== Install luasocket Lua files ==="
	# 顶层 Lua 文件
	cp $(LUASOCKET_SRC)/socket.lua $(DEPS_LIB)/
	cp $(LUASOCKET_SRC)/mime.lua $(DEPS_LIB)/
	cp $(LUASOCKET_SRC)/ltn12.lua $(DEPS_LIB)/
	# socket 子目录 Lua 文件
	@mkdir -p $(DEPS_LIB)/socket
	cp $(LUASOCKET_SRC)/http.lua $(DEPS_LIB)/socket/
	cp $(LUASOCKET_SRC)/url.lua $(DEPS_LIB)/socket/
	cp $(LUASOCKET_SRC)/tp.lua $(DEPS_LIB)/socket/
	cp $(LUASOCKET_SRC)/ftp.lua $(DEPS_LIB)/socket/
	cp $(LUASOCKET_SRC)/smtp.lua $(DEPS_LIB)/socket/
	cp $(LUASOCKET_SRC)/headers.lua $(DEPS_LIB)/socket/
	cp $(LUASOCKET_SRC)/mbox.lua $(DEPS_LIB)/socket/
	@echo "=== luasocket Lua files installed ==="
# --- lsqlite3 (uses bundled sqlite3 amalgamation) -------------------------
SQLITE3_SRC := $(DEPS_SRC)/sqlite3/sqlite-amalgamation-3450100
SQLITE3_INC := $(SQLITE3_SRC)

# Check if sqlite3 amalgamation is available
ifeq ($(shell test -f $(SQLITE3_SRC)/sqlite3.h && echo 1),1)
LSQLITE3_CFLAGS := $(CFLAGS) -I$(SQLITE3_INC) -DLSQLITE3_AMALGAMATION \
	-DLUA_COMPAT_APIINTCASTS \
	-include $(DEPS_SRC)/lsqlite3/lua_compat.h

$(DEPS_BUILD)/sqlite3.o: $(SQLITE3_SRC)/sqlite3.c $(SQLITE3_SRC)/sqlite3.h
	@mkdir -p $(@D)
	$(CC) -O2 -fPIC -c $< -o $@

$(DEPS_BUILD)/lsqlite3.o: $(DEPS_SRC)/lsqlite3/lsqlite3.c
	@mkdir -p $(@D)
	$(CC) $(LSQLITE3_CFLAGS) -c $< -o $@

$(LSQLITE3_TARGET): $(DEPS_BUILD)/lsqlite3.o $(DEPS_BUILD)/sqlite3.o $(LAUXLIB_O)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ -lm

lsqlite3: $(LSQLITE3_TARGET)
ALL_TARGETS += $(LSQLITE3_TARGET)
else
lsqlite3:
	@echo "WARNING: sqlite3 amalgamation not found at $(SQLITE3_SRC), skipping lsqlite3"
endif
# --- sol2 (header-only C++ binding, 仅构建 smoke 测试) --------------------
# sol2 是 header-only 库，无需单独构建 .so；测试目标编译一个最小 C++ 程序
# 链接 Rust lua 静态库 (target/release/liblua_rs.a) 验证 sol2 与 Rust lua 兼容。
SOL2_INCLUDE := $(DEPS_SRC)/sol2/include
SOL2_SMOKETEST := $(DEPS_BUILD)/sol2_smoke
# Rust lua 静态库（由 cargo build --release 生成）
LUA_RS_LIB := $(PROJECT_ROOT)/target/release/liblua_rs.a

# C++ 编译器用于 sol2 smoke test
CXX      := g++
CXXFLAGS := -O2 -std=c++17 -I$(LUA_INCLUDE) -I$(SOL2_INCLUDE)

# 源码在 deps 目录下（提交进 git 的最小测试程序）
SOL2_SMOKETEST_SRC := $(CURDIR)/sol2_smoke.cpp

$(SOL2_SMOKETEST): $(SOL2_SMOKETEST_SRC) $(LUA_INCLUDE)/lua.hpp $(LUA_INCLUDE)/lua.h
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) $< -o $@ $(LUA_RS_LIB) -lm -ldl -lpthread

sol2: $(SOL2_SMOKETEST)
	@echo "=== sol2 smoke test built: $< ==="

# --- Phony targets --------------------------------------------------------
.PHONY: all clean cjson luasocket lsqlite3 sol2

# 依赖 phony targets 而非 .so 文件目标，确保 luasocket 的 cp（安装 Lua 文件）被执行
all: cjson luasocket lsqlite3 sol2
	@echo "=== Build complete ==="
	@ls -la $(DEPS_LIB)/

clean:
	rm -rf $(DEPS_BUILD)
	rm -f $(CJSON_TARGET) $(SOCKET_CORE_TARGET) $(MIME_TARGET) $(LSQLITE3_TARGET)
	rm -f $(SOL2_SMOKETEST)
	rmdir $(DEPS_LIB) 2>/dev/null; true
