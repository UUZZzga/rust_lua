fn main() {
    // 使用适度栈大小 — 默认 8MB 在处理递归时溢出；128MB 安全但过冲
    let child = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)  // 64MB
        .spawn(lua_rs::cli::main)
        .unwrap();
    child.join().unwrap();
}
