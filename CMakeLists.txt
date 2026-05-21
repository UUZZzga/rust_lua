cmake_minimum_required(VERSION 3.16)
project(lua VERSION 5.5.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# Build options
option(LUA_USE_READLINE "Enable readline support" OFF)
option(LUA_COMPAT_MATHLIB "Enable deprecated math functions" OFF)

# Compile options
set(LUA_COMPILE_OPTIONS -Wall -Wextra)

if(CMAKE_BUILD_TYPE STREQUAL "Debug")
    list(APPEND LUA_COMPILE_OPTIONS -g -O0)
    set(CARGO_BUILD_PROFILE "dev")
    set(RUST_OUTPUT_DIR "debug")
else()
    list(APPEND LUA_COMPILE_OPTIONS -O2)
    set(CARGO_BUILD_PROFILE "release")
    set(RUST_OUTPUT_DIR "release")
endif()

# Detect Rust
find_program(CARGO_EXECUTABLE cargo)
    
if(NOT CARGO_EXECUTABLE)
    message(WARNING "cargo not found, building without Rust integration")
    set(LUA_BUILD_WITH_RUST OFF)
else()
    message(STATUS "Found cargo: ${CARGO_EXECUTABLE}")

    # Rust build directory
    set(RUST_TARGET_DIR ${CMAKE_BINARY_DIR}/rust)
    set(RUST_LIB_PATH ${RUST_TARGET_DIR}/${RUST_OUTPUT_DIR}/liblua_rs.a)
    
    # FFI feature for debug builds
    set(RUST_FEATURES "")
    if(CMAKE_BUILD_TYPE STREQUAL "Debug")
        set(RUST_FEATURES "--features=ffi")
    endif()
        
    # Build Rust library via custom command
    add_custom_command(
        OUTPUT ${RUST_LIB_PATH}
        COMMAND ${CMAKE_COMMAND} -E env
            CARGO_TARGET_DIR=${RUST_TARGET_DIR}
            ${CARGO_EXECUTABLE} build
                --manifest-path=${CMAKE_SOURCE_DIR}/Cargo.toml
                --profile=${CARGO_BUILD_PROFILE}
                ${RUST_FEATURES}
        DEPENDS
            ${CMAKE_SOURCE_DIR}/Cargo.toml
            ${CMAKE_SOURCE_DIR}/src_rs/lib.rs
            ${CMAKE_SOURCE_DIR}/src_rs/objects.rs
            ${CMAKE_SOURCE_DIR}/src_rs/vm.rs
            ${CMAKE_SOURCE_DIR}/src_rs/strings.rs
            ${CMAKE_SOURCE_DIR}/src_rs/table.rs
            ${CMAKE_SOURCE_DIR}/src_rs/gc.rs
            ${CMAKE_SOURCE_DIR}/src_rs/ffi.rs
        WORKING_DIRECTORY ${CMAKE_SOURCE_DIR}
        COMMENT "Building Rust library (${CARGO_BUILD_PROFILE})"
        VERBATIM
    )
        
    add_custom_target(lua-rs-build ALL DEPENDS ${RUST_LIB_PATH})
        
    # Create an imported library target for Rust
    add_library(lua-rs STATIC IMPORTED GLOBAL)
    set_target_properties(lua-rs PROPERTIES
        IMPORTED_LOCATION ${RUST_LIB_PATH}
    )
    add_dependencies(lua-rs lua-rs-build)
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
add_library(lualib-cpp STATIC ${CORE_SOURCES} ${LIB_SOURCES})
target_compile_options(lualib-cpp PRIVATE ${LUA_COMPILE_OPTIONS})
target_link_libraries(lualib-cpp PUBLIC m)

if(UNIX AND NOT APPLE)
    target_compile_definitions(lualib-cpp PUBLIC LUA_USE_LINUX)
    target_link_libraries(lualib-cpp PUBLIC ${CMAKE_DL_LIBS})
endif()

if(APPLE)
    target_compile_definitions(lualib-cpp PUBLIC LUA_USE_MACOSX)
endif()

set_source_files_properties(
    src/llex.cpp src/lparser.cpp src/lcode.cpp
    PROPERTIES COMPILE_FLAGS "-Os"
)

if(LUA_COMPAT_MATHLIB)
    target_compile_definitions(lualib-cpp PRIVATE LUA_COMPAT_MATHLIB)
endif()

# Unified library combining C++ and Rust
if(LUA_BUILD_WITH_RUST)
    add_library(lualib INTERFACE)
    target_link_libraries(lualib INTERFACE lualib-cpp lua-rs)
    
    add_dependencies(lualib lua-rs)
    
    # Include Rust headers if any
    target_include_directories(lualib INTERFACE ${CMAKE_SOURCE_DIR}/src_rs)
else()
    add_library(lualib ALIAS lualib-cpp)
endif()

# Lua interpreter
add_executable(lua src/lua.cpp)
target_link_libraries(lua lualib)
target_compile_options(lua PRIVATE ${LUA_COMPILE_OPTIONS})
set_target_properties(lua PROPERTIES ENABLE_EXPORTS TRUE)

if(LUA_BUILD_WITH_RUST)
    add_dependencies(lua lua-rs-build)
endif()

# Lua compiler
add_executable(luac src/luac.cpp)
target_link_libraries(luac lualib)
target_compile_options(luac PRIVATE ${LUA_COMPILE_OPTIONS})

if(LUA_BUILD_WITH_RUST)
    add_dependencies(luac lua-rs-build)
endif()

if(LUA_USE_READLINE)
    if(APPLE)
        target_link_libraries(lua PRIVATE edit)
        target_compile_definitions(lua PRIVATE LUA_USE_READLINE)
    else()
        target_link_libraries(lua PRIVATE readline)
        target_compile_definitions(lua PRIVATE LUA_USE_READLINE)
    endif()
endif()

# Test target
if(LUA_BUILD_WITH_RUST)
    add_custom_target(test-rust
        COMMAND ${CMAKE_COMMAND} -E env
            CARGO_TARGET_DIR=${RUST_TARGET_DIR}
            ${CARGO_EXECUTABLE} test
                --manifest-path=${CMAKE_SOURCE_DIR}/Cargo.toml
                --profile=${CARGO_BUILD_PROFILE}
        WORKING_DIRECTORY ${CMAKE_SOURCE_DIR}
        COMMENT "Running Rust tests"
        VERBATIM
    )
    
    add_custom_target(test-all
        DEPENDS test-rust
        COMMAND ${CMAKE_COMMAND} -E echo "All tests passed"
        COMMENT "Running all tests"
    )
endif()

# Installation
install(TARGETS lualib-cpp lua luac
    RUNTIME DESTINATION bin
    LIBRARY DESTINATION lib
    ARCHIVE DESTINATION lib
)

if(LUA_BUILD_WITH_RUST)
    install(FILES ${RUST_LIB_PATH}
        DESTINATION lib
    )
endif()

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
message(STATUS "Rust integration: ${LUA_BUILD_WITH_RUST}")
if(LUA_BUILD_WITH_RUST)
    message(STATUS "Rust target dir: ${RUST_TARGET_DIR}")
    message(STATUS "Cargo: ${CARGO_EXECUTABLE}")
endif()
message(STATUS "Readline: ${LUA_USE_READLINE}")
message(STATUS "Compat mathlib: ${LUA_COMPAT_MATHLIB}")
message(STATUS "===========================================")
message(STATUS "")
