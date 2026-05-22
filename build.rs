use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let lua_src_dir = manifest_dir.join("src");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", lua_src_dir.display());

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
    println!("cargo:rustc-link-lib=dl");
    println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
}