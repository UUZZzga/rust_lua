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

# 失败的目标 (如 dlltool 中断留下的空导入库) 直接删除, 否则下次 make 会把残缺产物
# 当成最新而跳过重建。
.DELETE_ON_ERROR:

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

PLATFORM_LC := $(shell echo $(PLATFORM) | tr 'A-Z' 'a-z')

# --- Paths ----------------------------------------------------------------
PROJECT_ROOT := $(realpath $(CURDIR)/..)
LUA_INCLUDE  := $(PROJECT_ROOT)/src
DEPS_SRC     := $(CURDIR)/src
DEPS_BUILD   := $(CURDIR)/build
DEPS_LIB     := $(CURDIR)/lib/lua/5.5

CC       := gcc
CFLAGS   := $(BASE_CFLAGS) -fPIC -fexceptions -Wall -Wextra -I$(LUA_INCLUDE)
# C++ 编译器用于 sol2 smoke test
CXX      := g++
CXXFLAGS := $(BASE_CFLAGS) -std=c++17 -I$(LUA_INCLUDE)

LDFLAGS  := -shared
LDLIBS   := -lm

USE_DEBUG ?= 0
BASE_CFLAGS :=
TARGET_PATH :=
EXE_EXT :=

ifeq ($(USE_DEBUG),1)
	BASE_CFLAGS := -g
	TARGET_PATH := $(PROJECT_ROOT)/target/debug
else
	BASE_CFLAGS := -O2
	TARGET_PATH := $(PROJECT_ROOT)/target/release
endif

# Rust lua 静态库（由 cargo build --release 生成）
LUA_RS_LIB := $(TARGET_PATH)/liblua_rs.a
LUA_RS_STATIC_LIB := $(TARGET_PATH)/liblua_rs.a

ifneq (,$(filter MINGW MSYS CYGWIN,$(PLATFORM)))
	EXE_EXT :=.exe
	CFLAGS := $(CFLAGS) -DLUA_BUILD_AS_DLL -DLUA_LIB
	LDLIBS := $(LDLIBS) -lws2_32 -lNtosKrnl -lntdll -luserenv
else ifeq ($(PLATFORM),LINUX)
	LDLIBS := $(LDLIBS) -ldl -lpthread
endif

# Rust lua 可执行文件（MinGW 下同时充当 C 模块的宿主导入库来源）
LUA_EXE := $(TARGET_PATH)/lua$(EXE_EXT)

ifneq (,$(filter MINGW MSYS CYGWIN,$(PLATFORM)))
DLLTOOL  := dlltool
LUA_RS_LIB := $(TARGET_PATH)/liblua_exe.a

# 原生工具 (gcc/dlltool) 的临时目录: 它们不认 POSIX 路径 (/d/...), 会回退到
# C:\Windows 创建临时文件而失败; 这里用 cygpath 转成 Windows 形式 (D:/...) 并导出
# 给所有子进程 (deps/build 现建, 保证存在)。
NATIVE_TMP := $(shell mkdir -p '$(DEPS_BUILD)' 2>/dev/null; cygpath -m '$(DEPS_BUILD)' 2>/dev/null || echo '$(DEPS_BUILD)')
export TMP := $(NATIVE_TMP)
export TEMP := $(NATIVE_TMP)

# lua.exe 的 C API 导出清单（build.rs 生成, 见下）→ 导入库。
# lua-rs 自己的 lua.exe 加载 C 模块时 (deps 的 cjson/luasocket/lsqlite3/sol2),
# 模块从 lua.exe 的导出表解析 lua_* 符号（对应官方 C lua 用 lua54.dll）。
LUA_EXE_DEF := $(TARGET_PATH)/lua_exe_import.def

# skynet 的宿主 Lua: lua_rs.dll (true DLL, 由 deps/lua_dll 的 cdylib 构建)。
# 不能拿 lua.exe 顶替 —— LoadLibrary 一个 EXE 会以 mainCRTStartup 当 DllMain
# 调用, Rust 运行期不初始化, 实测 lua_newstate 即崩。
LUA_RS_DLL := $(TARGET_PATH)/lua_rs.dll
LUA_RS_DLL_DEF := $(TARGET_PATH)/lua_rs_dll_import.def
LUA_RS_DLL_IMPORT_LIB := $(TARGET_PATH)/liblua_rs_dll.a

# lua-cjson 仅 Windows 需要 _MSC_VER 强制 __declspec(dllexport) 导出
# luaopen_* 符号供 dlopen 加载 (Linux ELF -shared 默认全量导出, 加了反而
# 在 GCC 上因 __declspec 语法报错, CI #162 实证)。
CJSON_DEFS := -D_MSC_VER

# lua_rs.dll (deps/lua_dll 的 cdylib) 与两个 .def 都是 lua-rs 的构建产物, 由
# deps/setup.sh 里的 cargo 步骤生成; deps/Makefile 只把它们转成 MinGW 导入库。
$(LUA_RS_DLL) $(LUA_EXE_DEF) $(LUA_RS_DLL_DEF):
	@echo "缺少 $@：请先运行 bash deps/setup.sh (构建 lua-rs 与 lua_rs.dll)"
	@exit 1

# lua.exe 导出表 → MinGW 导入库
$(LUA_RS_LIB): $(LUA_EXE_DEF)
	@mkdir -p $(@D)
	$(DLLTOOL) -d $< -l $@

# lua_rs.dll 导出清单 → MinGW 导入库 (skynet 的宿主 Lua)
$(LUA_RS_DLL_IMPORT_LIB): $(LUA_RS_DLL) $(LUA_RS_DLL_DEF)
	@mkdir -p $(@D)
	$(DLLTOOL) -d $(LUA_RS_DLL_DEF) -l $@
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
	$(CC) $(CFLAGS) -c $< -o $@ $(CJSON_DEFS)

$(DEPS_BUILD)/strbuf.o: $(CJSON_SRC)/strbuf.c
	$(CC) $(CFLAGS) -c $< -o $@

$(DEPS_BUILD)/fpconv.o: $(CJSON_SRC)/fpconv.c
	$(CC) $(CFLAGS) -c $< -o $@

$(CJSON_TARGET): $(CJSON_OBJS) $(LUA_RS_LIB)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ $(LDLIBS)

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
	$(DEPS_BUILD)/options.o \
	$(DEPS_BUILD)/select.o \
	$(DEPS_BUILD)/tcp.o \
	$(DEPS_BUILD)/timeout.o \
	$(DEPS_BUILD)/udp.o \
	$(DEPS_BUILD)/$(SOCK_OBJ) \
	$(DEPS_BUILD)/compat.o

SOCKET_CFLAGS := $(CFLAGS)

ifeq ($(PLATFORM),LINUX)
	SOCKET_CFLAGS := $(CFLAGS) -DLUASOCKET_API=extern
endif


$(DEPS_BUILD)/%.o: $(LUASOCKET_SRC)/%.c
	@mkdir -p $(@D)
	$(CC) $(SOCKET_CFLAGS) -c $< -o $@

$(SOCKET_CORE_TARGET): $(SOCKET_OBJS) $(LUA_RS_LIB)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ $(LDLIBS)

$(DEPS_BUILD)/mime.o: $(LUASOCKET_SRC)/mime.c
	@mkdir -p $(@D)
	$(CC) $(SOCKET_CFLAGS) -c $< -o $@

$(MIME_TARGET): $(DEPS_BUILD)/mime.o $(LUA_RS_LIB)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ $(LDLIBS)

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
	$(CC) $(CFLAGS) -fPIC -c $< -o $@

$(DEPS_BUILD)/lsqlite3.o: $(DEPS_SRC)/lsqlite3/lsqlite3.c
	@mkdir -p $(@D)
	$(CC) $(LSQLITE3_CFLAGS) -c $< -o $@

$(LSQLITE3_TARGET): $(DEPS_BUILD)/lsqlite3.o $(DEPS_BUILD)/sqlite3.o $(LUA_RS_LIB)
	@mkdir -p $(@D)
	$(CC) $(LDFLAGS) -o $@ $^ $(LDLIBS)

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
SOL2_SMOKETEST := $(DEPS_BUILD)/sol2_smoke$(EXE_EXT)
SOL2_CXXFLAGS := $(CXXFLAGS) -I$(SOL2_INCLUDE)

# 源码在 deps 目录下（提交进 git 的最小测试程序）
SOL2_SMOKETEST_SRC := $(CURDIR)/sol2_smoke.cpp

# 链接目标: Linux/其它平台直接用 Rust 静态库 (liblua_rs.a); MinGW 下 Rust 的
# staticlib 是 MSVC 目标文件 (lua_rs.lib), g++ 无法链接, 而 lua.exe 又不能当 DLL
# 用, 故链接 lua_rs.dll (deps/lua_dll) 的导入库, 运行时需同目录有 lua_rs.dll。
SOL2_LINK_LIB := $(LUA_RS_STATIC_LIB)
# 注意用递归展开 (=): 命令里的 $(@D) 是自动变量, 必须在 recipe 执行时才展开。
SOL2_POST_LINK = :
ifneq (,$(filter MINGW MSYS CYGWIN,$(PLATFORM)))
  SOL2_LINK_LIB := $(LUA_RS_DLL_IMPORT_LIB)
  SOL2_POST_LINK = cp -f $(LUA_RS_DLL) $(@D)/
endif

$(SOL2_SMOKETEST): $(SOL2_SMOKETEST_SRC) $(SOL2_LINK_LIB)
	@mkdir -p $(@D)
	$(CXX) $(SOL2_CXXFLAGS) $^ -o $@ $(LDLIBS)
	$(SOL2_POST_LINK)

sol2: $(SOL2_SMOKETEST)
	@echo "=== sol2 smoke test built: $< ==="

# --- skynet (宿主 Lua 一律用 lua-rs，禁用 jemalloc) -------------------------
# skynet 自带修改版 Lua 5.5.1 (ejoy/lua skynet55 分支: 3rd/lua)，本项目完全不用它:
#   - 头文件: src/ (官方 Lua 5.5.0 头文件 = lua-rs 遵循的 C API)
#   - Linux: LUA_LIB=liblua_rs.a + -Wl,-E 导出符号供 dlopen 的 .so 解析
#   - MinGW: 由 mingw.mk 的 lua-rs 变量接入 lua_rs.dll (setup.sh 的补丁);
#     deps 侧用 dlltool 把 build.rs 生成的导出清单转成导入库 liblua_rs_dll.a,
#     运行时由 skynet.exe 同目录的 lua_rs.dll 提供 lua_xxx 符号
#     (对应官方 skynet 用 lua54.dll 的角色)。
# skynet 自带的修改版 Lua 头文件不得进入 include 路径 (mingw.mk 里的 -I3rd/lua
# 已由 setup.sh 的补丁移除)。
# 禁用 jemalloc (MALLOC_STATICLIB="") 避免构建 jemalloc 子模块；
# NOUSE_JEMALLOC 让 skynet 使用系统 malloc。
#
# capi_variadic.c（luaL_error / lua_pushfstring / lua_pushvfstring 等 C variadic
# 函数，stable Rust 不支持 c_variadic）已由 build.rs 编译进 liblua_rs.a，无需
# 单独提供 .o。Rust 端 os_lib 引用 lua_rs_clocks_per_sec，确保 capi_variadic.o
# 从归档中被拉取；-Wl,-E 导出符号供 dlopen 加载的 .so 使用。
# --undefined 作为 fail-fast 安全网：若符号未被拉取，链接阶段即报错而非运行时。
SKYNET_SRC := $(DEPS_SRC)/skynet
SKYNET_BIN := $(SKYNET_SRC)/skynet$(EXE_EXT)

# 强制保留 capi_variadic.c 的 variadic 符号（供 .so 模块运行时解析）
SKYNET_EXPORT := -Wl,-E \
	-Wl,--undefined=lua_pushfstring \
	-Wl,--undefined=lua_pushvfstring \
	-Wl,--undefined=luaL_error \
	-Wl,--undefined=lua_rs_clocks_per_sec

SKYNET_MINGW_CC_ARGS  :=
SKYNET_MINGW_EXTRA_CFLAGS :=
SKYNET_RS_ARGS :=

ifeq ($(PLATFORM),MINGW)
  SKYNET_MINGW_CC_ARGS      := CC=gcc AR=ar RANLIB=ranlib
  SKYNET_MINGW_EXTRA_CFLAGS := -DIN_WINPTHREAD -DCLOCK_REALTIME=0 \
                        -DCLOCK_MONOTONIC=1 -DCLOCK_THREAD_CPUTIME_ID=3
else ifeq ($(PLATFORM),MSYS)
  SKYNET_MINGW_CC_ARGS      := CC=gcc AR=ar RANLIB=ranlib
  SKYNET_MINGW_EXTRA_CFLAGS := -DIN_WINPTHREAD -DCLOCK_REALTIME=0 \
                        -DCLOCK_MONOTONIC=1 -DCLOCK_THREAD_CPUTIME_ID=3
endif

ifneq (,$(filter MINGW MSYS CYGWIN,$(PLATFORM)))
  # mingw.mk 的 lua-rs 接入点 (由 setup.sh 的补丁定义)
  SKYNET_RS_ARGS := \
	LUA_RS_ROOT=$(PROJECT_ROOT) \
	LUA_RS_SRC=$(LUA_INCLUDE) \
	LUA_RS_DLL=$(LUA_RS_DLL) \
	LUA_RS_IMPORT_LIB=$(LUA_RS_DLL_IMPORT_LIB)
  SKYNET_HOST_LUA := $(LUA_RS_DLL_IMPORT_LIB) $(LUA_RS_DLL)
else
  # Linux: 直接链接 liblua_rs.a, 靠 -Wl,-E 把符号导出给 dlopen 的 .so
  SKYNET_HOST_LUA := $(LUA_RS_STATIC_LIB)
endif

# skynet 的平台目标只有 linux/freebsd/openbsd/macosx/mingw, 没有 msys;
# MSYS2 的编译器即 MinGW-w64, 归一到 mingw 目标 (mingw.mk)。
SKYNET_PLAT := $(PLATFORM_LC)
ifeq ($(PLATFORM),MSYS)
  SKYNET_PLAT := mingw
endif

skynet: $(SKYNET_HOST_LUA)
	@echo "=== Building skynet with lua-rs ==="
	cd $(SKYNET_SRC) && $(MAKE) $(SKYNET_PLAT) \
		$(SKYNET_MINGW_CC_ARGS) \
		$(SKYNET_RS_ARGS) \
		LUA_LIB=$(LUA_RS_STATIC_LIB) \
		LUA_INC=$(LUA_INCLUDE) \
		MALLOC_STATICLIB="" \
		MYCFLAGS="$(SKYNET_MINGW_EXTRA_CFLAGS)" \
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
