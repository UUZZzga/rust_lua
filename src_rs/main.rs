fn main() {
    // 使用大栈子线程运行，避免 execute_loop 递归导致的栈溢出
    let child = std::thread::Builder::new()
        .stack_size(128 * 1024 * 1024)  // 128MB
        .spawn(lua_rs::cli::main)
        .unwrap();
    child.join().unwrap();
}
