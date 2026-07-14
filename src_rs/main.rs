use tikv_jemallocator::Jemalloc;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

fn main() {
    // 32MB stack - enough for deep recursion in gc.lua and other tests
    // 64MB was wasteful (takes 64MB VmSize), 8MB caused stack overflow
    let child = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(lua_rs::cli::main)
        .unwrap();
    child.join().unwrap();
}
