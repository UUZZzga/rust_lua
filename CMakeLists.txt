cmake_minimum_required(VERSION 3.16)
project(lua VERSION 5.5.0 LANGUAGES C)

set(CMAKE_C_STANDARD 99)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)  # 启用 GNU 扩展，匹配原版 gnu99

# Build options
option(LUA_USE_READLINE "Enable readline support" OFF)
option(LUA_COMPAT_MATHLIB "Enable deprecated math functions" OFF)

# Compile options
include(CheckCCompilerFlag)

# 检查 C 编译器是否支持 -g3
check_c_compiler_flag("-g3" C_COMPILER_SUPPORTS_G3)
if(C_COMPILER_SUPPORTS_G3)
    set(CMAKE_C_FLAGS_DEBUG "${CMAKE_C_FLAGS_DEBUG} -g3")
endif()

set(LUA_COMPILE_OPTIONS -Wall -Wextra)

if(CMAKE_BUILD_TYPE STREQUAL "Debug")
    list(APPEND LUA_COMPILE_OPTIONS -O0)
else()
    list(APPEND LUA_COMPILE_OPTIONS -O2)
endif()

# C Core sources
set(CORE_SOURCES
    src/lapi.c
    src/lcode.c
    src/lctype.c
    src/ldebug.c
    src/ldo.c
    src/ldump.c
    src/lfunc.c
    src/lgc.c
    src/llex.c
    src/lmem.c
    src/lobject.c
    src/lopcodes.c
    src/lparser.c
    src/lstate.c
    src/lstring.c
    src/ltable.c
    src/ltm.c
    src/lundump.c
    src/lvm.c
    src/lzio.c
)

# C Library sources
set(LIB_SOURCES
    src/lauxlib.c
    src/lbaselib.c
    src/lcorolib.c
    src/ldblib.c
    src/liolib.c
    src/lmathlib.c
    src/loadlib.c
    src/loslib.c
    src/lstrlib.c
    src/ltablib.c
    src/lutf8lib.c
    src/linit.c
)

# Build C library
add_library(lualib STATIC ${CORE_SOURCES} ${LIB_SOURCES})
target_compile_options(lualib PRIVATE ${LUA_COMPILE_OPTIONS})
target_link_libraries(lualib PUBLIC m)

if(UNIX AND NOT APPLE)
    target_compile_definitions(lualib PUBLIC LUA_USE_LINUX)
    target_link_libraries(lualib PUBLIC ${CMAKE_DL_LIBS})
endif()

if(APPLE)
    target_compile_definitions(lualib PUBLIC LUA_USE_MACOSX)
endif()

set_source_files_properties(
    src/llex.c src/lparser.c src/lcode.c
    PROPERTIES COMPILE_FLAGS "-Os"
)

if(LUA_COMPAT_MATHLIB)
    target_compile_definitions(lualib PRIVATE LUA_COMPAT_MATHLIB)
endif()

# Lua interpreter
add_executable(lua src/lua.c)
target_link_libraries(lua lualib)
target_compile_options(lua PRIVATE ${LUA_COMPILE_OPTIONS})
set_target_properties(lua PROPERTIES ENABLE_EXPORTS TRUE)

# Lua compiler
add_executable(luac src/luac.c)
target_link_libraries(luac lualib)
target_compile_options(luac PRIVATE ${LUA_COMPILE_OPTIONS})

if(LUA_USE_READLINE)
    if(APPLE)
        target_link_libraries(lua PRIVATE edit)
        target_compile_definitions(lua PRIVATE LUA_USE_READLINE)
    else()
        target_link_libraries(lua PRIVATE readline)
        target_compile_definitions(lua PRIVATE LUA_USE_READLINE)
    endif()
endif()

# Installation
install(TARGETS lualib lua luac
    RUNTIME DESTINATION bin
    LIBRARY DESTINATION lib
    ARCHIVE DESTINATION lib
)

install(DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}/
    DESTINATION include/lua
    FILES_MATCHING
    PATTERN "src/*.h"
    PATTERN "src/*.hpp"
    PATTERN "CMakeLists.txt" EXCLUDE
)

# Print build summary
message(STATUS "")
message(STATUS "=== Lua ${PROJECT_VERSION} Build Configuration ===")
message(STATUS "Build type: ${CMAKE_BUILD_TYPE}")
message(STATUS "C standard: ${CMAKE_C_STANDARD}")
message(STATUS "Readline: ${LUA_USE_READLINE}")
message(STATUS "Compat mathlib: ${LUA_COMPAT_MATHLIB}")
message(STATUS "===========================================")
message(STATUS "")
