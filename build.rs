use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        build_test_c_libs(&manifest_dir, &lua_src_dir, is_windows);
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

/// 编译 `tests_lua/libs` 下的 C 动态库测试模块 (跨平台替代 tests_lua/libs/makefile)。
///
/// 产物 (被 `tests_lua/attrib.lua` 的 `package.loadlib(DC"lib1", ...)` 及 `require` 使用):
/// - Unix:    lib1.so / lib11.so / lib2.so / lib21.so / lib2-v2.so
/// - Windows: lib1.dll / lib11.dll / lib2.dll / lib21.dll / lib2-v2.dll
///
/// Unix 上 `lua` 可执行文件由 build.rs 加 `-Wl,--export-dynamic` 导出 C API,
/// 故动态库可保留未定义的 `lua_*` 符号, 由 dlopen 时解析。
/// Windows (PE) 链接期不允许未定义符号, 因此需额外生成 import library:
/// 先编译出 .obj 读取其符号表, 据此写 .def, 再由 dlltool (GNU) / lib.exe (MSVC)
/// 生成 import library 供动态库链接; 同时让 `lua.exe` 导出同名符号, 运行时由
/// 加载器按模块名 `lua.exe` 解析。
/// 测试模块之间也可以互相引用 (lib11 -> lib1_export, lib21 -> luaopen_lib2),
/// 这类符号改为从兄弟模块的 import library 导入, 并按依赖顺序构建。
///
/// 任何步骤失败都只发 warning 并跳过, 不中断构建 (attrib.lua 在 loadlib
/// 失败时会自行跳过对应的 C 模块测试)。
fn build_test_c_libs(manifest_dir: &Path, lua_src_dir: &Path, is_windows: bool) {
    // (源文件, 输出文件名去掉扩展名的部分) —— 对应 tests_lua/libs/makefile 的 5 个目标
    const LIBS: [(&str, &str); 5] = [
        ("lib1.c", "lib1"),
        ("lib11.c", "lib11"),
        ("lib2.c", "lib2"),
        ("lib21.c", "lib21"),
        ("lib22.c", "lib2-v2"), // lib2-v2 由 lib22.c 构建, 同 makefile
    ];

    let libs_dir = manifest_dir.join("tests_lua").join("libs");
    if !libs_dir.is_dir() {
        return;
    }
    // 只监视源文件: 产物 (.so/.dll) 也写在该目录, 监视整个目录会让 build.rs 每次都重跑。
    for (src, _) in LIBS {
        println!("cargo:rerun-if-changed={}", libs_dir.join(src).display());
    }

    let out_dir = match env::var("OUT_DIR") {
        Ok(v) => PathBuf::from(v),
        Err(_) => return,
    };

    let mut probe = cc::Build::new();
    probe.include(lua_src_dir);
    let tool = probe.get_compiler();

    let result = if is_windows {
        // 只有宿主确实实现的符号才允许写进 lua.exe 的导出表, 否则 MSVC 链接
        // 会以 LNK2001 硬失败 (见 host_api_symbols)。
        let host_symbols = host_api_symbols(manifest_dir);
        build_test_c_libs_windows(
            &tool,
            lua_src_dir,
            &libs_dir,
            &out_dir,
            &LIBS,
            "dll",
            &host_symbols,
        )
    } else {
        build_test_c_libs_unix(&tool, &libs_dir, &LIBS, "so")
    };

    if let Err(e) = result {
        println!("cargo:warning=tests_lua/libs 测试模块编译失败, 已跳过: {e}");
    }
}

/// 汇集宿主 `lua` 可执行文件能够导出的 C API 符号名。
///
/// 名字全部取自实际实现文件 (非 cmp_c 模式: capi.rs + capi_variadic.c;
/// cmp_c 模式: `src/*.c`), 避免向 MSVC 的 `/DEF` 写入不存在的符号 —— 那会让
/// lua.exe 链接直接失败, 而不是优雅跳过。
fn host_api_symbols(manifest_dir: &Path) -> BTreeSet<String> {
    let files = vec![
        manifest_dir.join("src_rs").join("capi.rs"),
        manifest_dir.join("src_rs").join("capi_variadic.c"),
    ];

    let mut set = BTreeSet::new();
    for file in files {
        if let Ok(text) = fs::read_to_string(&file) {
            collect_lua_identifiers(&text, &mut set);
        }
    }
    set
}

/// 把文本中所有以 `lua` 开头、长度大于 3 的标识符收集进 `set`。
fn collect_lua_identifiers(text: &str, set: &mut BTreeSet<String>) {
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i + 2 < bytes.len() {
        if &bytes[i..i + 3] == b"lua" {
            let start = i;
            let mut end = i + 3;
            while end < bytes.len() && is_word(bytes[end]) {
                end += 1;
            }
            let clean_start = start == 0 || !is_word(bytes[start - 1]);
            if clean_start && end > start + 3 {
                set.insert(text[start..end].to_string());
            }
            i = end;
        } else {
            i += 1;
        }
    }
}

/// Unix: 直接 `cc -shared -fPIC` 生成 .so, 未定义符号交由主程序在 dlopen 时解析。
fn build_test_c_libs_unix(
    tool: &cc::Tool,
    libs_dir: &Path,
    libs: &[(&str, &str)],
    ext: &str,
) -> Result<(), String> {
    for (src, name) in libs {
        let out = libs_dir.join(format!("{name}.{ext}"));
        let mut cmd = tool.to_command();
        cmd.arg("-shared")
            .arg("-fPIC")
            .arg("-o")
            .arg(&out)
            .arg(libs_dir.join(src));
        run_cmd(&mut cmd, &format!("编译 {src}"))?;
    }
    Ok(())
}

/// Windows: 为每个测试模块生成 import library 后链接为 .dll, 并让 lua.exe 导出宿主 API。
fn build_test_c_libs_windows(
    tool: &cc::Tool,
    lua_src_dir: &Path,
    libs_dir: &Path,
    out_dir: &Path,
    libs: &[(&str, &str)],
    ext: &str,
    host_symbols: &BTreeSet<String>,
) -> Result<(), String> {
    let is_msvc = tool.is_like_msvc();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    // 32 位目标的 C 符号带前导下划线, 而 .def 与 import library 都用未修饰名。
    let strip_underscore = is_msvc && arch != "x86_64" && arch != "aarch64";

    // 1. 编译为 .obj 并读取符号表。
    let mut objs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut syms: BTreeMap<String, ObjSymbols> = BTreeMap::new();
    for (src, name) in libs {
        let obj = out_dir.join(format!("{name}.{}", if is_msvc { "obj" } else { "o" }));
        compile_obj(tool, is_msvc, lua_src_dir, &libs_dir.join(src), &obj)?;
        let parsed = parse_symbols(tool, is_msvc, &obj, strip_underscore)?;
        syms.insert((*name).to_string(), parsed);
        objs.insert((*name).to_string(), obj);
    }

    // 2. "符号 -> 定义它的测试模块"。用于把兄弟模块之间的引用与宿主 API 区分开
    //    (注意 luaopen_lib2 由 lib2/lib22 定义, 不是宿主 API)。
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for (_, name) in libs {
        for sym in &syms[*name].defined {
            owner
                .entry(sym.clone())
                .or_insert_with(|| (*name).to_string());
        }
    }

    // 3. 分类各模块的未定义符号: 兄弟模块提供的走该模块的 import library,
    //    其余由宿主提供的 lua* 符号汇总成 lua.exe 的导出表。
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut host_used: BTreeSet<String> = BTreeSet::new();
    for (_, name) in libs {
        let mut dep = BTreeSet::new();
        for sym in &syms[*name].undefined {
            match owner.get(sym) {
                Some(o) if o.as_str() != *name => {
                    dep.insert(o.clone());
                }
                Some(_) => {}
                None if host_symbols.contains(sym) => {
                    host_used.insert(sym.clone());
                }
                None => {}
            }
        }
        deps.insert((*name).to_string(), dep);
    }

    // 4. 先让 lua.exe 导出这些符号 —— 与后面 .dll 的链接解耦, 保证一定生效。
    //    exe.def 只能有 EXPORTS (MSVC 下带 LIBRARY 会把输出变成 DLL);
    //    imp.def 带 LIBRARY lua.exe, 让 .dll 的导入表指向宿主进程。
    let host_exe_def = out_dir.join("lua_test_host_exe.def");
    let host_imp_def = out_dir.join("lua_test_host_imp.def");
    let host_syms: Vec<String> = host_used.into_iter().collect();
    write_def(&host_exe_def, None, &host_syms)?;
    write_def(&host_imp_def, Some("lua.exe"), &host_syms)?;
    let host_lib = if host_syms.is_empty() {
        None
    } else {
        let lib = out_dir.join(if is_msvc {
            "lua_test_host.lib"
        } else {
            "lua_test_host_imp.a"
        });
        make_import_lib(tool, is_msvc, &host_imp_def, &lib)?;
        if is_msvc {
            println!("cargo:rustc-link-arg-bins=/DEF:{}", host_exe_def.display());
        } else {
            // MinGW 的 ld 不支持 --export-dynamic, PE 目标需用 --export-all-symbols。
            println!("cargo:rustc-link-arg-bins=-Wl,--export-all-symbols");
        }
        Some(lib)
    };

    // 5. 按依赖顺序构建 (lib11 需要 lib1_export, lib21 需要 luaopen_lib2)。
    let mut pending: Vec<String> = libs.iter().map(|(_, n)| (*n).to_string()).collect();
    let mut order: Vec<String> = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    while !pending.is_empty() {
        let mut progressed = false;
        pending.retain(|name| {
            if deps[name].iter().all(|d| done.contains(d)) {
                order.push(name.clone());
                done.insert(name.clone());
                progressed = true;
                false
            } else {
                true
            }
        });
        if !progressed {
            return Err(format!("测试模块之间存在循环依赖: {pending:?}"));
        }
    }

    // 6. 链接各测试模块为 .dll。中间产物 (obj/lib/exp) 都留在 OUT_DIR,
    //    只把最终的 .dll 拷贝到源码树。
    let mut imp_libs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for name in &order {
        let src = libs
            .iter()
            .find(|(_, n)| n == name)
            .map(|(s, _)| *s)
            .ok_or_else(|| format!("未知的测试模块 {name}"))?;
        let staged = out_dir.join(format!("{name}.{ext}"));
        let imp_lib = out_dir.join(if is_msvc {
            format!("{name}.lib")
        } else {
            format!("{name}_imp.a")
        });
        let exp_def = out_dir.join(format!("{name}_exp.def"));
        let defined: Vec<String> = syms[name].defined.iter().cloned().collect();
        // LUAMOD_API 在未定义 LUA_BUILD_AS_DLL 时没有 dllexport, 所以 MSVC 用 /DEF
        // 导出 .dll 自身的符号 (luaopen_* / onefunction / lib1_export ...);
        // GNU 侧用 --export-all-symbols 达到同样效果。
        let library = if is_msvc {
            None
        } else {
            Some(format!("{name}.{ext}"))
        };
        write_def(&exp_def, library.as_deref(), &defined)?;
        if !is_msvc {
            make_import_lib(tool, is_msvc, &exp_def, &imp_lib)?;
        }

        let mut cmd = tool.to_command();
        if is_msvc {
            cmd.arg("/nologo")
                .arg("/LD")
                .arg(&objs[name])
                .arg(format!("/Fe:{}", staged.display()))
                .arg("/link")
                .arg(format!("/DEF:{}", exp_def.display()))
                .arg(format!("/IMPLIB:{}", imp_lib.display()));
        } else {
            cmd.arg("-shared").arg("-o").arg(&staged).arg(&objs[name]);
        }
        if let Some(host_lib) = &host_lib {
            cmd.arg(host_lib);
        }
        for dep in &deps[name] {
            cmd.arg(&imp_libs[dep]);
        }
        if !is_msvc {
            cmd.arg("-Wl,--export-all-symbols");
        }
        run_cmd(&mut cmd, &format!("链接 {src}"))?;

        imp_libs.insert(name.clone(), imp_lib);
        let out = libs_dir.join(format!("{name}.{ext}"));
        fs::copy(&staged, &out).map_err(|e| format!("拷贝到 {} 失败: {e}", out.display()))?;
    }

    Ok(())
}

/// 一个目标文件的外部符号表: 已定义符号 (可导出) 与未定义符号 (需导入)。
struct ObjSymbols {
    defined: BTreeSet<String>,
    undefined: BTreeSet<String>,
}

/// 读取 .obj 的符号表。GNU 用 `nm -g`, MSVC 用 `dumpbin /symbols`
/// (均取编译器同目录下的工具)。
fn parse_symbols(
    tool: &cc::Tool,
    is_msvc: bool,
    obj: &Path,
    strip_underscore: bool,
) -> Result<ObjSymbols, String> {
    let tool_path = if is_msvc {
        sibling_tool(tool, "dumpbin.exe")
    } else {
        sibling_tool(tool, "nm.exe")
    };
    let mut cmd = Command::new(&tool_path);
    if is_msvc {
        cmd.arg("/nologo").arg("/symbols");
    } else {
        cmd.arg("-g");
    }
    let out = cmd
        .arg(obj)
        .output()
        .map_err(|e| format!("无法执行 {}: {e}", tool_path.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} 解析 {} 失败: {}",
            tool_path.display(),
            obj.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let mut defined = BTreeSet::new();
    let mut undefined = BTreeSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if is_msvc {
            // "010 00000000 UNDEF  notype ()    External     | lua_gettop"
            // "008 00000000 SECT1  notype ()    External     | onefunction"
            let Some(bar) = line.rfind('|') else { continue };
            if !line.contains("External") {
                continue;
            }
            let raw = line[bar + 1..].trim();
            // 跳过编译器内部符号 (@feat.00)、段符号 (.text/.rdata) 与导入桩 (__imp_*)。
            if raw.is_empty()
                || raw.starts_with('.')
                || raw.starts_with('@')
                || raw.starts_with("__")
            {
                continue;
            }
            let name = if strip_underscore {
                raw.trim_start_matches('_')
            } else {
                raw
            };
            if line.contains("UNDEF") {
                undefined.insert(name.to_string());
            } else {
                defined.insert(name.to_string());
            }
        } else {
            // "0000000000000000 T onefunction" / "                 U lua_gettop"
            let mut it = line.split_whitespace();
            match (it.next(), it.next(), it.next()) {
                (Some("U"), Some(name), _) => {
                    undefined.insert(name.to_string());
                }
                (Some(_addr), Some(kind), Some(name))
                    if kind.len() == 1
                        && kind.as_bytes()[0].is_ascii_uppercase()
                        && kind != "U"
                        && !name.starts_with('.')
                        && !name.starts_with('_') =>
                {
                    defined.insert(name.to_string());
                }
                _ => {}
            }
        }
    }
    Ok(ObjSymbols { defined, undefined })
}

/// 用编译器把源文件编译成 .obj (不链接), 显式带上 Lua 头文件目录。
fn compile_obj(
    tool: &cc::Tool,
    is_msvc: bool,
    lua_src_dir: &Path,
    src: &Path,
    obj: &Path,
) -> Result<(), String> {
    let mut cmd = tool.to_command();
    if is_msvc {
        cmd.arg("/c")
            .arg(format!("/I{}", lua_src_dir.display()))
            .arg(format!("/Fo{}", obj.display()))
            .arg(src);
    } else {
        cmd.arg("-c")
            .arg(format!("-I{}", lua_src_dir.display()))
            .arg("-o")
            .arg(obj)
            .arg(src);
    }
    run_cmd(&mut cmd, &format!("编译 {}", src.display()))
}

/// 生成 import library。MSVC 用 lib.exe, GNU 用 dlltool; 两者都依据 .def
/// 里的 LIBRARY 行决定 .dll 的模块名。
fn make_import_lib(
    tool: &cc::Tool,
    is_msvc: bool,
    def_file: &Path,
    import_lib: &Path,
) -> Result<(), String> {
    let mut cmd = if is_msvc {
        let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let machine = match arch.as_str() {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            other => other,
        };
        let mut cmd = Command::new(sibling_tool(tool, "lib.exe"));
        cmd.arg("/nologo")
            .arg(format!("/def:{}", def_file.display()))
            .arg(format!("/out:{}", import_lib.display()))
            .arg(format!("/machine:{machine}"));
        cmd
    } else {
        let mut cmd = Command::new(sibling_tool(tool, "dlltool.exe"));
        cmd.arg("-d").arg(def_file).arg("-l").arg(import_lib);
        cmd
    };
    run_cmd(&mut cmd, "生成 import library")
}

/// 写 .def 文件。`library` 为 Some 时写入 LIBRARY 行 (import library 用),
/// 为 None 时只写 EXPORTS (MSVC 下用于给 .exe 导出符号)。
fn write_def(path: &Path, library: Option<&str>, symbols: &[String]) -> Result<(), String> {
    let mut content = String::new();
    if let Some(name) = library {
        content.push_str(&format!("LIBRARY {name}\n"));
    }
    content.push_str("EXPORTS\n");
    for sym in symbols {
        content.push_str("  ");
        content.push_str(sym);
        content.push('\n');
    }
    fs::write(path, content).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

/// 定位与编译器同目录的工具 (lib.exe / dumpbin.exe / nm / dlltool),
/// 找不到时回退为直接按名字调用 (依赖 PATH, MSYS2 环境下可用)。
fn sibling_tool(tool: &cc::Tool, name: &str) -> PathBuf {
    if let Some(dir) = tool.path().parent() {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

/// 执行外部命令, 失败时把 stdout/stderr 一并作为错误返回。
fn run_cmd(cmd: &mut Command, what: &str) -> Result<(), String> {
    match cmd.output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "{what} 失败 ({}):\n{}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )),
        Err(e) => Err(format!("{what} 无法执行: {e}")),
    }
}
