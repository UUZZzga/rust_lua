fn main() {
    // Set MALLOC_ARENA_MAX=1 to reduce glibc arena overhead
    // Without this, glibc creates up to 8*cores arenas, each reserving large virtual memory
    // This reduces VmSize by ~60MB without affecting performance
    unsafe { libc::mallopt(libc::M_ARENA_MAX, 1); }
    
    // 32MB stack - enough for deep recursion in gc.lua and other tests
    // 64MB was wasteful (takes 64MB VmSize), 8MB caused stack overflow
    let child = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(lua_rs::cli::main)
        .unwrap();
    child.join().unwrap();
}
