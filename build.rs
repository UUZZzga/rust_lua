use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let lua_src_dir = manifest_dir.join("src");
    let rs_src_dir = manifest_dir.join("src_rs");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", lua_src_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        rs_src_dir.join("capi_variadic.c").display()
    );
    println!("cargo:rerun-if-changed=Cargo.toml");

    // 默认构建（非 ffi）也需导出 C API 符号到动态符号表，
    // 让 dlopen 加载的 C 模块（.so）能解析 lua_xxx/luaL_xxx 符号。
    println!("cargo:rustc-link-arg=-Wl,--export-dynamic");

    // 链接 dl（capi.rs 的 sys_load/sys_sym 用到 dlopen/dlsym）
    println!("cargo:rustc-link-lib=dl");

    // 仅在启用 ffi feature 时编译 C Lua 源码。
    // 默认情况下 Rust 实现自给自足，capi.rs 导出 #[no_mangle] 符号；
    // 若同时链接 C 库会导致符号重复定义。
    let ffi_enabled = env::var("CARGO_FEATURE_FFI").is_ok();

    if !ffi_enabled {
        // 非 ffi: stable Rust 不支持 c_variadic，lua_pushfstring/lua_pushvfstring
        // 由 capi_variadic.c 提供。Rust 代码不引用它们，但 dlopen 加载的 .so 需要，
        // 用 --undefined 强制保留。
        let mut variadic_build = cc::Build::new();
        variadic_build
            .file(rs_src_dir.join("capi_variadic.c"))
            .flag("-Wall")
            .flag("-Wextra")
            .flag("-fexceptions");
        variadic_build.compile("lua_rs_variadic");

        println!("cargo:rustc-link-arg=-Wl,--undefined=lua_pushfstring");
        println!("cargo:rustc-link-arg=-Wl,--undefined=lua_pushvfstring");
        println!("cargo:rustc-link-arg=-Wl,--undefined=luaL_error");
        return;
    }

    let mut build = cc::Build::new();

    build
        .cpp(true)
        .std("c++11")
        .include(&lua_src_dir)
        .flag("-Wall")
        .flag("-Wextra")
        .flag("-Wno-unused-parameter")
        .define("LUA_USE_LINUX", None)
        .define("LUA_USE_LONGJMP", None)
        .define("LUA_COMPAT_5_3", None);

    let is_debug = env::var("PROFILE").unwrap() == "debug";
    if is_debug {
        build.define("LUA_USE_APICHECK", None);
    }

    let cpp_files: Vec<&str> = vec![
        "lapi.cpp",
        "lauxlib.cpp",
        "lbaselib.cpp",
        "lcode.cpp",
        "lcorolib.cpp",
        "lctype.cpp",
        "ldblib.cpp",
        "ldebug.cpp",
        "ldo.cpp",
        "ldump.cpp",
        "lfunc.cpp",
        "lgc.cpp",
        "linit.cpp",
        "liolib.cpp",
        "llex.cpp",
        "lmathlib.cpp",
        "lmem.cpp",
        "loadlib.cpp",
        "lobject.cpp",
        "lopcodes.cpp",
        "loslib.cpp",
        "lparser.cpp",
        "lstate.cpp",
        "lstring.cpp",
        "lstrlib.cpp",
        "ltable.cpp",
        "ltablib.cpp",
        "ltm.cpp",
        "lundump.cpp",
        "lutf8lib.cpp",
        "lvm.cpp",
        "lzio.cpp",
    ];

    for f in &cpp_files {
        let path = lua_src_dir.join(f);
        if path.exists() {
            build.file(&path);
        } else {
            panic!("Missing source file: {}", path.display());
        }
    }

    build.compile("lua");

    println!("cargo:rustc-link-lib=m");
}
