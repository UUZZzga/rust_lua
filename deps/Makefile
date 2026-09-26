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
#   make skynet       Build only skynet (linked against liblua_rs.a)
#   make clean        Remove build artifacts

SHELL := /bin/bash

ifndef PLATFORM
  UNAME_S := $(shell uname -s 2>/dev/null)

  ifneq (,$(findstring MINGW,$(UNAME_S)))
    PLATFORM := MINGW
  else ifneq (,$(findstring MSYS,$(UNAME_S)))
    PLATFORM := MSYS
  else ifneq (,$(findstring CYGWIN,$(UNAME_S)))
    PLATFORM := CYGWIN
  else ifeq ($(UNAME_S),Linux)
    PLATFORM := LINUX
  else ifeq ($(UNAME_S),Darwin)
    PLATFORM := MACOS
  else ifeq ($(OS),Windows_NT)
    PLATFORM := WINDOWS
  else
    PLATFORM := UNKNOWN
  endif
endif

PLATFORM_LC := $(shell echo $(PLATFORM) | sed 's/.*/\L&/')

# --- Paths ----------------------------------------------------------------
PROJECT_ROOT := $(realpath $(CURDIR)/..)
LUA_INCLUDE  := $(PROJECT_ROOT)/src
DEPS_SRC     := $(CURDIR)/src
DEPS_BUILD   := $(CURDIR)/build
DEPS_LIB     := $(CURDIR)/lib/lua/5.5

CC       := gcc
CFLAGS   := -O2 -fPIC -fexceptions -Wall -Wextra -I$(LUA_INCLUDE)
LDFLAGS  := -shared
LDLIBS   := -lm

ifeq ($(PLATFORM),MINGW)
	CFLAGS := $(CFLAGS) -DLUA_BUILD_AS_DLL -DLUA_LIB
	LDLIBS := $(LDLIBS) -lws2_32 -lNtosKrnl -lntdll -luserenv
else ifeq ($(PLATFORM),MSYS)
	CFLAGS := $(CFLAGS) -DLUA_BUILD_AS_DLL -DLUA_LIB
	LDLIBS := $(LDLIBS) -lws2_32 -lNtosKrnl -lntdll -luserenv
else ifeq ($(PLATFORM),LINUX)
	CFLAGS := $(CFLAGS) -DLUA_BUILD_AS_DLL -DLUA_LIB
	LDLIBS := $(LDLIBS) -ldl -lpthread
endif


# --- Targets --------------------------------------------------------------
CJSON_TARGET     := $(DEPS_LIB)/cjson.so
SOCKET_CORE_TARGET := $(DEPS_LIB)/socket/core.so
MIME_TARGET       := $(DEPS_LIB)/mime/core.so
LSQLITE3_TARGET   := $(DEPS_LIB)/lsqlite3.so

ifeq ($(PLATFORM),MINGW)
	CJSON_TARGET     := $(DEPS_LIB)/cjson.dll
	SOCKET_CORE_TARGET := $(DEPS_LIB)/socket/core.dll
	MIME_TARGET       := $(DEPS_LIB)/mime/core.dll
	LSQLITE3_TARGET   := $(DEPS_LIB)/lsqlite3.dll
else ifeq ($(PLATFORM),MSYS)
	CJSON_TARGET     := $(DEPS_LIB)/cjson.dll
	SOCKET_CORE_TARGET := $(DEPS_LIB)/socket/core.dll
	MIME_TARGET       := $(DEPS_LIB)/mime/core.dll
	LSQLITE3_TARGET   := $(DEPS_LIB)/lsqlite3.dll
endif

ALL_TARGETS := $(CJSON_TARGET) $(SOCKET_CORE_TARGET) $(MIME_TARGET)

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

$(CJSON_TARGET): $(CJSON_OBJS)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ $(LUA_RS_LIB) $(LDLIBS)

cjson: $(CJSON_TARGET)

# --- luasocket (core + mime) ----------------------------------------------
LUASOCKET_SRC := $(DEPS_SRC)/luasocket/src
SOCK_OBJ:=usocket.o
ifeq ($(PLATFORM),MINGW)
	SOCK_OBJ := wsocket.o
else ifeq ($(PLATFORM),MSYS)
	SOCK_OBJ := wsocket.o
endif
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
	$(DEPS_BUILD)/$(SOCK_OBJ) \
	$(DEPS_BUILD)/compat.o

SOCKET_CFLAGS := $(CFLAGS) -DLUASOCKET_API=extern

$(DEPS_BUILD)/%.o: $(LUASOCKET_SRC)/%.c
	@mkdir -p $(@D)
	$(CC) $(SOCKET_CFLAGS) -c $< -o $@

$(SOCKET_CORE_TARGET): $(SOCKET_OBJS)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ $(LUA_RS_LIB) $(LDLIBS)

$(DEPS_BUILD)/mime.o: $(LUASOCKET_SRC)/mime.c
	@mkdir -p $(@D)
	$(CC) $(SOCKET_CFLAGS) -c $< -o $@

$(MIME_TARGET): $(DEPS_BUILD)/mime.o
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ $(LUA_RS_LIB) $(LDLIBS)

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

$(LSQLITE3_TARGET): $(DEPS_BUILD)/lsqlite3.o $(DEPS_BUILD)/sqlite3.o
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ $(LUA_RS_LIB) $(LDLIBS)

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
	$(CXX) $(CXXFLAGS) $< -o $@ $(LUA_RS_LIB) $(LDLIBS)

sol2: $(SOL2_SMOKETEST)
	@echo "=== sol2 smoke test built: $< ==="

# --- skynet (链接 lua-rs 静态库，禁用 jemalloc) ----------------------------
# skynet 自带修改版 Lua 5.5.1，本项目通过 LUA_LIB/LUA_INC 覆盖为 lua-rs 的
# liblua_rs.a 与 src/ 头文件。setup.sh 已应用补丁去除 luaL_makeseed 硬依赖。
# 禁用 jemalloc (MALLOC_STATICLIB="") 避免构建 jemalloc 子模块；
# NOUSE_JEMALLOC 让 skynet 使用系统 malloc。
#
# capi_variadic.c（luaL_error / lua_pushfstring / lua_pushvfstring 等 C variadic
# 函数，stable Rust 不支持 c_variadic）已由 build.rs 编译进 liblua_rs.a，无需
# 单独提供 .o。Rust 端 os_lib 引用 lua_rs_clocks_per_sec，确保 capi_variadic.o
# 从归档中被拉取；-Wl,-E 导出符号供 dlopen 加载的 .so 使用。
# --undefined 作为 fail-fast 安全网：若符号未被拉取，链接阶段即报错而非运行时。
SKYNET_SRC := $(DEPS_SRC)/skynet
SKYNET_BIN := $(SKYNET_SRC)/skynet

# 强制保留 capi_variadic.c 的 variadic 符号（供 .so 模块运行时解析）
SKYNET_EXPORT := -Wl,-E \
	-Wl,--undefined=lua_pushfstring \
	-Wl,--undefined=lua_pushvfstring \
	-Wl,--undefined=luaL_error \
	-Wl,--undefined=lua_rs_clocks_per_sec

SKYNET_MINGW_CC_ARGS  :=
SKYNET_MINGW_EXTRA_CFLAGS :=

ifeq ($(PLATFORM),MINGW)
  SKYNET_MINGW_CC_ARGS      := CC=gcc AR=ar RANLIB=ranlib
  SKYNET_MINGW_EXTRA_CFLAGS := -DIN_WINPTHREAD -DCLOCK_REALTIME=0 \
                        -DCLOCK_MONOTONIC=1 -DCLOCK_THREAD_CPUTIME_ID=3
else ifeq ($(PLATFORM),MSYS)
  SKYNET_MINGW_CC_ARGS      := CC=gcc AR=ar RANLIB=ranlib
  SKYNET_MINGW_EXTRA_CFLAGS := -DIN_WINPTHREAD -DCLOCK_REALTIME=0 \
                        -DCLOCK_MONOTONIC=1 -DCLOCK_THREAD_CPUTIME_ID=3
endif

skynet: $(LUA_RS_LIB)
	@echo "=== Building skynet with lua-rs ==="
	# LUA_RUST_COMPAT: 跳过 coroutine.resume/wrap 替换 (lua-rs 的 lua_resume 与 skynet timing_resume 不兼容)
	cd $(SKYNET_SRC) && $(MAKE) $(PLATFORM_LC) \
		$(SKYNET_MINGW_CC_ARGS) \
		LUA_LIB=$(LUA_RS_LIB) \
		LUA_INC=$(LUA_INCLUDE) \
		MALLOC_STATICLIB="" \
		MYCFLAGS="-DLUA_RUST_COMPAT $(SKYNET_MINGW_EXTRA_CFLAGS)" \
		EXPORT="$(SKYNET_EXPORT)" \
		SKYNET_DEFINES="-DNOUSE_JEMALLOC"
	@echo "=== skynet built: $(SKYNET_BIN) ==="

# --- Phony targets --------------------------------------------------------
.PHONY: all clean cjson luasocket lsqlite3 sol2 skynet

# 依赖 phony targets 而非 .so 文件目标，确保 luasocket 的 cp（安装 Lua 文件）被执行
all: cjson luasocket lsqlite3 sol2 skynet
	@echo "=== Build complete ==="
	@ls -la $(DEPS_LIB)/

clean:
	rm -rf $(DEPS_BUILD)
	rm -f $(CJSON_TARGET) $(SOCKET_CORE_TARGET) $(MIME_TARGET) $(LSQLITE3_TARGET)
	rm -f $(SOL2_SMOKETEST)
	cd $(SKYNET_SRC) && $(MAKE) clean 2>/dev/null || true
	rmdir $(DEPS_LIB) 2>/dev/null; true
