cmake_minimum_required(VERSION 3.16)
project(lua VERSION 5.5.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# Build options
option(LUA_USE_READLINE "Enable readline support" OFF)
option(LUA_COMPAT_MATHLIB "Enable deprecated math functions" OFF)

# Compile options
include(CheckCCompilerFlag)
include(CheckCXXCompilerFlag)

# 检查 C++ 编译器是否支持 -g3
check_cxx_compiler_flag("-g3" CXX_COMPILER_SUPPORTS_G3)
if(CXX_COMPILER_SUPPORTS_G3)
    set(CMAKE_CXX_FLAGS_DEBUG "${CMAKE_CXX_FLAGS_DEBUG} -g3")
endif()

set(LUA_COMPILE_OPTIONS -Wall -Wextra)

if(CMAKE_BUILD_TYPE STREQUAL "Debug")
    list(APPEND LUA_COMPILE_OPTIONS -O0)
else()
    list(APPEND LUA_COMPILE_OPTIONS -O2)
endif()

# C++ Core sources
set(CORE_SOURCES
    src/lapi.cpp
    src/lcode.cpp
    src/lctype.cpp
    src/ldebug.cpp
    src/ldo.cpp
    src/ldump.cpp
    src/lfunc.cpp
    src/lgc.cpp
    src/llex.cpp
    src/lmem.cpp
    src/lobject.cpp
    src/lopcodes.cpp
    src/lparser.cpp
    src/lstate.cpp
    src/lstring.cpp
    src/ltable.cpp
    src/ltm.cpp
    src/lundump.cpp
    src/lvm.cpp
    src/lzio.cpp
)

# C++ Library sources
set(LIB_SOURCES
    src/lauxlib.cpp
    src/lbaselib.cpp
    src/lcorolib.cpp
    src/ldblib.cpp
    src/liolib.cpp
    src/lmathlib.cpp
    src/loadlib.cpp
    src/loslib.cpp
    src/lstrlib.cpp
    src/ltablib.cpp
    src/lutf8lib.cpp
    src/linit.cpp
)

# Build C++ library
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
    src/llex.cpp src/lparser.cpp src/lcode.cpp
    PROPERTIES COMPILE_FLAGS "-Os"
)

if(LUA_COMPAT_MATHLIB)
    target_compile_definitions(lualib PRIVATE LUA_COMPAT_MATHLIB)
endif()

# Lua interpreter
add_executable(lua src/lua.cpp)
target_link_libraries(lua lualib)
target_compile_options(lua PRIVATE ${LUA_COMPILE_OPTIONS})
set_target_properties(lua PROPERTIES ENABLE_EXPORTS TRUE)

# Lua compiler
add_executable(luac src/luac.cpp)
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
message(STATUS "C++ standard: ${CMAKE_CXX_STANDARD}")
message(STATUS "Readline: ${LUA_USE_READLINE}")
message(STATUS "Compat mathlib: ${LUA_COMPAT_MATHLIB}")
message(STATUS "===========================================")
message(STATUS "")