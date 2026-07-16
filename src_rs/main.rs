fn main() {
    let child = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(lua_rs::cli::main)
        .unwrap();
    child.join().unwrap();
}
