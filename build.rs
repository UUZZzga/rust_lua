use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let lua_src_dir = manifest_dir.join("src");
    let rs_src_dir = manifest_dir.join("src_rs");

    // 声明 size_optimized cfg, 避免 unexpected_cfgs 警告.
    println!("cargo::rustc-check-cfg=cfg(size_optimized)");
    println!("cargo::rustc-check-cfg=cfg(lua_use_longjmp)");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", lua_src_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        rs_src_dir.join("capi_variadic.c").display()
    );
    println!("cargo:rerun-if-changed=Cargo.toml");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let is_windows = target_os == "windows";

    // 体积优先 profile 自动启用 size_optimized cfg。
    // 检测 OPT_LEVEL 环境变量: "s" (GCC -Os) 或 "z" (GCC -Oz) 表示体积优化。
    // 这样 `cargo build --profile size-opt` 无需手动加 --features。
    let opt_level = env::var("OPT_LEVEL").unwrap_or_default();
    let size_optimized = opt_level == "s" || opt_level == "z";
    if size_optimized {
        println!("cargo:rustc-cfg=size_optimized");
        // 以下 linker 选项仅适用于 GNU ld (Linux), MSVC linker 不支持。
        if !is_windows {
            // 移除 .eh_frame_hdr 段: panic=abort 模式下不需要 unwind 表头
            println!("cargo:rustc-link-arg=-Wl,--no-eh-frame-hdr");
            // 合并相同函数 (Identical Code Folding): 减少重复代码
            println!("cargo:rustc-link-arg=-Wl,--icf=all");
            // 移除 RELRO 填充页 (~2KB): 体积优先场景不需要 RELRO 安全特性
            println!("cargo:rustc-link-arg=-Wl,-z,norelro");
        }
    }

    // lua_use_longjmp: 启用 setjmp/longjmp 实现 C 函数错误处理 (对应 C 的 LUA_USE_LONGJMP)。
    // 启用条件 (任一即可):
    //   1. 显式启用 lua_longjmp feature: cargo build --features lua_longjmp
    //   2. size_optimized 模式 (panic=abort 下 catch_unwind 不工作, 自动启用)
    let lua_longjmp_feature = env::var("CARGO_FEATURE_LUA_LONGJMP").is_ok();
    let lua_use_longjmp = lua_longjmp_feature || size_optimized;
    if lua_use_longjmp {
        println!("cargo:rustc-cfg=lua_use_longjmp");
    }

    if !is_windows {
        // 默认构建（非 cmp_c）也需导出 C API 符号到动态符号表，
        // 让 dlopen 加载的 C 模块（.so）能解析 lua_xxx/luaL_xxx 符号。
        println!("cargo:rustc-link-arg=-Wl,--export-dynamic");

        // 链接 dl（capi.rs 的 sys_load/sys_sym 用到 dlopen/dlsym）
        // 注意: glibc 2.34+ 已将 dlopen/dlsym 合入 libc, 但旧版本仍需 -ldl
        println!("cargo:rustc-link-lib=dl");
    }
    // Windows: #[no_mangle] extern "C" + __declspec(dllexport) 已自动导出符号,
    // 动态库加载用 LoadLibraryA/GetProcAddress (kernel32, 自动链接), 无需额外 flag。

    // 仅在启用 cmp_c feature 时编译 C Lua 源码。
    // 默认情况下 Rust 实现自给自足，capi.rs 导出 #[no_mangle] 符号；
    // 若同时链接 C 库会导致符号重复定义。
    let cmp_c_enabled = env::var("CARGO_FEATURE_CMP_C").is_ok();

    if !cmp_c_enabled {
        // 非 cmp_c: stable Rust 不支持 c_variadic，lua_pushfstring/lua_pushvfstring
        // 由 capi_variadic.c 提供。Rust 代码不引用它们，但 dlopen 加载的 .so 需要，
        // 用 --undefined 强制保留。
        let mut variadic_build = cc::Build::new();
        variadic_build.file(rs_src_dir.join("capi_variadic.c"));

        let compiler = variadic_build.get_compiler();
        if compiler.is_like_msvc() {
            // MSVC: cc-rs 默认已加 /W4, 不支持 -Wall/-Wextra/-Wno-unused-parameter
        } else {
            // GCC/Clang
            variadic_build.flag("-Wall").flag("-Wextra");
        }

        // 体积优先: 用 -fno-exceptions 禁用 C 异常处理表 (.eh_frame),
        // panic=abort 模式下不需要 C 异常 unwind.
        if opt_level == "s" || opt_level == "z" {
            if !compiler.is_like_msvc() {
                variadic_build.flag("-fno-exceptions");
                variadic_build.flag("-fno-unwind-tables");
            }
        } else if !compiler.is_like_msvc() {
            variadic_build.flag("-fexceptions");
        }
        // 启用 lua_use_longjmp 时, 向 C 代码注入 LUA_USE_LONGJMP 宏,
        // 编译 setjmp/longjmp 包装函数 (lua_rs_pcall_c / lua_rs_longjmp)。
        if lua_use_longjmp {
            variadic_build.define("LUA_USE_LONGJMP", None);
        }
        variadic_build.compile("lua_rs_variadic");

        if !is_windows {
            // stable Rust 不支持 c_variadic, lua_pushfstring/lua_pushvfstring/luaL_error
            // 由 capi_variadic.c 提供。Rust 代码不引用它们, 但 dlopen 加载的 .so 需要,
            // 用 --undefined 强制保留。
            println!("cargo:rustc-link-arg=-Wl,--undefined=lua_pushfstring");
            println!("cargo:rustc-link-arg=-Wl,--undefined=lua_pushvfstring");
            println!("cargo:rustc-link-arg=-Wl,--undefined=luaL_error");
            // lua_rs_pcall_c / lua_rs_longjmp 仅在 lua_use_longjmp 模式下编译,
            // 对应的 --undefined 也仅在该模式下添加。
            if lua_use_longjmp {
                println!("cargo:rustc-link-arg=-Wl,--undefined=lua_rs_pcall_c");
                println!("cargo:rustc-link-arg=-Wl,--undefined=lua_rs_longjmp");
            }

            // skynet feature: 导出 skynet 修改版 Lua 扩展 API (luaL_alloc / lua_clonetable /
            // lua_sharefunction / lua_sharestring / luaL_loadfilex_), 供 dlopen 加载的
            // luaclib/skynet.so 解析. Rust 代码不引用这些符号, 必须用 --undefined 强制保留.
            // 见 src_rs/capi.rs 末尾 "skynet 扩展 API" 章节.
            let skynet_feature = env::var("CARGO_FEATURE_SKYNET").is_ok();
            if skynet_feature {
                println!("cargo:rustc-link-arg=-Wl,--undefined=luaL_alloc");
                println!("cargo:rustc-link-arg=-Wl,--undefined=luaL_loadfilex_");
                println!("cargo:rustc-link-arg=-Wl,--undefined=lua_clonetable");
                println!("cargo:rustc-link-arg=-Wl,--undefined=lua_sharefunction");
                println!("cargo:rustc-link-arg=-Wl,--undefined=lua_sharestring");
            }
        }
        return;
    }

    // cmp_c 模式下仍需编译 capi_variadic.c 提供 lua_rs_clocks_per_sec 和
    // lua_rs_pcall_c / lua_rs_longjmp (C Lua 源码不提供这些)。
    // CMP_C_MODE 排除 lua_pushfstring / lua_pushvfstring / luaL_error
    // (由 C Lua 的 lauxlib.c / lapi.c 提供，避免符号重复定义)。
    let mut variadic_build = cc::Build::new();
    variadic_build
        .file(rs_src_dir.join("capi_variadic.c"))
        .define("CMP_C_MODE", None)
        .define("LUA_USE_LONGJMP", None);
    let variadic_compiler = variadic_build.get_compiler();
    if !variadic_compiler.is_like_msvc() {
        variadic_build.flag("-Wall").flag("-Wextra");
    }
    variadic_build.compile("lua_rs_variadic");

    let mut build = cc::Build::new();

    build
        .cpp(false)
        .include(&lua_src_dir)
        .define("LUA_USE_LONGJMP", None)
        .define("LUA_COMPAT_5_3", None);

    let compiler = build.get_compiler();
    if compiler.is_like_msvc() {
        // MSVC: 不支持 -std=gnu99 / -Wall / -Wextra / -Wno-unused-parameter
        // cc-rs 默认已加 /W4
        if is_windows {
            build.define("LUA_USE_WINDOWS", None);
        }
    } else {
        // GCC/Clang
        build
            .std("gnu99") // 匹配原版 Lua 5.5.0 Makefile 的 -std=gnu99
            .flag("-Wall")
            .flag("-Wextra")
            .flag("-Wno-unused-parameter");
        if is_windows {
            // MinGW on Windows
            build.define("LUA_USE_WINDOWS", None);
        } else {
            build.define("LUA_USE_LINUX", None);
        }
    }

    let is_debug = env::var("PROFILE").unwrap() == "debug";
    if is_debug {
        build.define("LUA_USE_APICHECK", None);
    }

    let c_files: Vec<&str> = vec![
        "lapi.c",
        "lauxlib.c",
        "lbaselib.c",
        "lcode.c",
        "lcorolib.c",
        "lctype.c",
        "ldblib.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "linit.c",
        "liolib.c",
        "llex.c",
        "lmathlib.c",
        "lmem.c",
        "loadlib.c",
        "lobject.c",
        "lopcodes.c",
        "loslib.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "lstrlib.c",
        "ltable.c",
        "ltablib.c",
        "ltm.c",
        "lundump.c",
        "lutf8lib.c",
        "lvm.c",
        "lzio.c",
    ];

    for f in &c_files {
        let path = lua_src_dir.join(f);
        if path.exists() {
            build.file(&path);
        } else {
            panic!("Missing source file: {}", path.display());
        }
    }

    build.compile("lua");

    // Linux 需要链接 libm; Windows 的数学函数在 libc 中, 无需单独链接。
    if !is_windows {
        println!("cargo:rustc-link-lib=m");
    }
}
