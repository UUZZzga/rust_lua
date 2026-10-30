fn main() {
    // 体积优先: 自定义 panic hook, 避免 std 默认 hook 引入 backtrace 符号化代码
    // (~93KB gimli/addr2line/miniz_oxide)。panic=abort 模式下直接 abort 即可。
    // 注意: 不打印 PanicInfo 的 Display (会调用 backtrace), 只打印简单信息 + 直接 abort.
    #[cfg(all(size_optimized, not(target_os = "windows")))]
    std::panic::set_hook(Box::new(|_info| {
        unsafe {
            let msg = b"lua-rs: panic occurred, aborting\n";
            libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
        }
        std::process::abort();
    }));
    #[cfg(all(size_optimized, target_os = "windows"))]
    std::panic::set_hook(Box::new(|_info| {
        let _ = std::io::Write::write_all(
            &mut std::io::stderr(),
            b"lua-rs: panic occurred, aborting\n",
        );
        std::process::abort();
    }));

    // 体积优先: 不使用 std::thread::Builder (会引入线程 panic 处理 + backtrace 代码).
    // 直接调用 cli::main, 用 RUST_MIN_STACK 环境变量控制栈大小 (默认 2MB, 需 8MB).
    #[cfg(size_optimized)]
    {
        // RUST_MIN_STACK 仅影响新生成的线程, 主线程栈大小由 OS 决定 (通常 8MB).
        lua_rs::cli::main();
    }
    #[cfg(not(size_optimized))]
    {
        // 16MB: debug 构建下 execute_loop 的单帧很大 (~43KB), 而 LUAI_MAXCCALLS(200)
        // 允许 200 层 C→Lua 嵌套 (calls.lua 的 "C-stack overflow while handling
        // C-stack overflow" 用例会一直递归到该上限)。8MB 只够约 190 层, 会在达到
        // 上限前原生栈溢出; 16MB 留出约 1.8 倍余量。release 构建帧更小, 余量更大。
        let child = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(lua_rs::cli::main)
            .unwrap();
        child.join().unwrap();
    }
}
