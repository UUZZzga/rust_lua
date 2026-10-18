#![cfg(feature = "lua_longjmp")]
// C 函数替身沿用 capi 的参数命名约定 (L: *mut lua_State)
#![allow(non_snake_case)]
//! longjmp 模式的错误路径安全性测试（`--features lua_longjmp`，对应 bench / panic=abort 构建）。
//!
//! `lua_use_longjmp` 下 C 侧用 `setjmp` / `longjmp` 替代 `catch_unwind`
//! （`capi_variadic.c` 的 `lua_rs_pcall_c` / `lua_rs_longjmp`）。longjmp **不会**
//! 执行被跳过帧的析构，因此有两类必须成立的性质：
//!
//! 1. **Drop 安全**：每次 raise 都必须在「不持有需要 Drop 的值」的帧里抛出 ——
//!    否则那些 String / CString / Vec / Rc 会永久泄漏。
//! 2. **保护点完整**：内存错误（`raise_memory_error`，来自没有错误通道的分配点，
//!    如 VM 内的栈扩容）必须被 Lua 层 `pcall` 捕获（对应 C 的 `luaD_pcall` 保护），
//!    而不是穿透到外层 C 边界；更不是无保护点时直接 `exit(1)`。
//!
//! 探针：计数**全局分配器**（量化 Rust 堆存活字节）+ 按大小失败的分配注入。
//! 两者都是进程级的，因此两个测试用同一把锁串行执行。
//!
//! 平台语义差异（决定了第 1 条只在 POSIX 上可被检出）：
//! - POSIX（glibc `longjmp` / `__builtin_longjmp`）不执行 Rust 析构 → raise 点持有
//!   的值会**泄漏**，这正是本文件要防的问题；
//! - Windows/MSVC 的 `longjmp` 经 `RtlUnwind` 实现，会真正执行被跳过帧的清理
//!   （实测 Drop 会运行）→ 同一份代码在 Windows 上不泄漏，但**代价是**：
//!   展开经过任何 Rust `extern "C"`（nounwind）帧都会把进程 abort，因此被跨过的
//!   帧必须是 `C-unwind`（本文件的替身 C 函数一律如此声明后 transmute）。

use lua_rs::capi::{
    luaL_argerror, luaL_checklstring, luaL_checkstack, luaL_typeerror, lua_State, lua_createtable,
    lua_getglobal, lua_pushcclosure, lua_pushcfunction, lua_pushinteger, lua_setglobal,
    lua_settable,
};
use lua_rs::state::LuaState;
use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicIsize, Ordering};

// ============================================================================
// 计数全局分配器 —— 量化 Rust 堆存活字节
// ============================================================================

struct CountingAlloc;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

/// 大额分配失败注入开关 / 命中计数（0 = 关闭）。
///
/// 栈扩容（`Vec::try_reserve`）走的是**全局分配器**（`state.stack` 是普通
/// `Vec<TValue>`），因此要制造「VM 内栈扩容失败」只能从这里注入。注入的都是
/// fallible 路径（`try_reserve`），不会触发 std 的 abort-on-OOM。
static FAIL_LARGE_ARMED: AtomicIsize = AtomicIsize::new(0);
static FAIL_LARGE_HITS: AtomicIsize = AtomicIsize::new(0);
const FAIL_LARGE_MIN: usize = 4096;

/// 全局分配器状态是进程级的：两个测试必须串行（测量窗口不得与失败注入重叠）。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= FAIL_LARGE_MIN && FAIL_LARGE_ARMED.load(Ordering::Relaxed) != 0 {
            FAIL_LARGE_HITS.fetch_add(1, Ordering::Relaxed);
            return std::ptr::null_mut();
        }
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size >= FAIL_LARGE_MIN && FAIL_LARGE_ARMED.load(Ordering::Relaxed) != 0 {
            FAIL_LARGE_HITS.fetch_add(1, Ordering::Relaxed);
            return std::ptr::null_mut();
        }
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE_BYTES.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        p
    }
}

/// 只在本测试二进制内生效（每个集成测试目标独立编译），不影响其它测试。
#[global_allocator]
static COUNTING: CountingAlloc = CountingAlloc;

fn live_bytes() -> isize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// 取出栈顶错误对象（仅用于失败信息）。
fn error_object(st: &LuaState) -> String {
    match st.obj_at(-1) {
        Some(lua_rs::objects::TValue::ShortStr(_)) | Some(lua_rs::objects::TValue::LongStr(_)) => {
            lua_rs::strings::lua_string_as_str(st.obj_at(-1).unwrap()).to_string()
        }
        Some(other) => format!("{other:?}"),
        None => "<空栈>".to_string(),
    }
}

// ============================================================================
// 各走一条 raise 路径的 C 函数
// ============================================================================

/// do_lua_error 路径（luaL_argerror → lua_error → longjmp）。
unsafe extern "C-unwind" fn err_lual_argerror(L: *mut c_void) -> c_int {
    let L = L as *mut lua_State;
    luaL_argerror(L, 1, c"bad value".as_ptr())
}

/// luaL_typeerror 路径：会构造 String/CString 后抛错。
unsafe extern "C-unwind" fn err_lual_typeerror(L: *mut c_void) -> c_int {
    let L = L as *mut lua_State;
    luaL_typeerror(L, 1, c"str".as_ptr())
}

/// luaL_checklstring 类型错误路径：会构造 CString 后抛错。
unsafe extern "C-unwind" fn err_lual_checklstring(L: *mut c_void) -> c_int {
    let L = L as *mut lua_State;
    luaL_checklstring(L, 1, std::ptr::null_mut());
    0
}

/// luaL_checkstack 栈溢出路径：会构造 String 后抛错。
unsafe extern "C-unwind" fn err_lual_checkstack(L: *mut c_void) -> c_int {
    let L = L as *mut lua_State;
    // space < 0 → lua_checkstack 返回 0 → 构造 "stack overflow (custom)" 并抛错
    luaL_checkstack(L, -1, c"custom".as_ptr());
    0
}

/// 把本文件的 C-unwind 函数转成 `lua_CFunction`（`extern "C"`）。
///
/// 真 C 模块的帧对 MSVC 的 `RtlUnwind`（`longjmp` 的实现）透明，而 Rust 的
/// `extern "C"` 帧会把展开 abort 掉；因此测试里的替身一律声明为 `C-unwind`
/// 再 transmute（ABI 一致，仅差「允许展开穿过」的语义）。
fn as_c_function(
    f: unsafe extern "C-unwind" fn(*mut c_void) -> c_int,
) -> lua_rs::capi::lua_CFunction {
    unsafe { std::mem::transmute(f) }
}

fn register(st: &mut LuaState) {
    let L = st as *mut LuaState as *mut lua_State;
    for (name, f) in [
        (c"err_argerror", as_c_function(err_lual_argerror)),
        (c"err_typeerror", as_c_function(err_lual_typeerror)),
        (c"err_checklstring", as_c_function(err_lual_checklstring)),
        (c"err_checkstack", as_c_function(err_lual_checkstack)),
    ] {
        lua_pushcfunction(L, f);
        lua_setglobal(L, name.as_ptr());
    }
}

fn new_state() -> LuaState<'static> {
    // lua_io(): ZST 的 'static io（library 提供，非泄漏）
    let mut st = LuaState::new_main(lua_rs::mock::io_mock::lua_io()).expect("创建 state 失败");
    st.open_selected_libs(0, 0).expect("打开标准库失败");
    register(&mut st);
    st
}

// ============================================================================
// 测试 1：capi 错误路径的 Drop 安全（计数分配器）
// ============================================================================

/// 反复从 C 函数抛错（longjmp 跳过 capi 帧）后，Rust 堆存活字节必须回到基线。
///
/// 每条错误路径都在同一轮里跑 N 次：任何在 raise 点持有 String/CString/Rc 的帧
/// 都会线性泄漏（每次数十字节 × N 次），在完整的 GC 之后仍然可由本断言观测到。
#[test]
fn capi_error_paths_do_not_leak() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut st = new_state();

    let run = |st: &mut LuaState, src: &str| {
        assert_eq!(st.load_buffer(src, "=longjmp"), 0, "编译失败: {src}");
        assert_eq!(st.pcall(0, 0, 0), 0, "执行失败: {src}");
    };

    // 每条路径都必须真的报错（否则本轮测试退化为空转）
    let check_errors =
        "local fns = {err_argerror, err_typeerror, err_checklstring, err_checkstack}\n\
         for _, f in ipairs(fns) do\n\
           local ok, err = pcall(f, true)\n\
           assert(ok == false, 'C 函数应当抛错')\n\
           assert(type(err) == 'string' and #err > 0, '错误对象应为字符串')\n\
         end\n";

    // 预热：让内部化字符串 / 各缓冲区的首次增长先发生
    run(&mut st, check_errors);
    run(
        &mut st,
        "collectgarbage('collect'); collectgarbage('collect')",
    );
    let before = live_bytes();

    let loop_src = "local fns = {err_argerror, err_typeerror, err_checklstring, err_checkstack}\n\
         for i = 1, 400 do\n\
           for _, f in ipairs(fns) do\n\
             assert(pcall(f, true) == false)\n\
           end\n\
         end\n";
    run(&mut st, loop_src);
    run(
        &mut st,
        "collectgarbage('collect'); collectgarbage('collect')",
    );
    let after = live_bytes();

    let growth = after - before;
    assert!(
        growth <= 4096,
        "longjmp 错误路径泄漏了 {growth} 字节 (before={before}, after={after})"
    );
}

// ============================================================================
// 测试 2：内存错误（VM 内栈扩容失败）必须被 Lua 层 pcall 捕获
// ============================================================================

/// 生成一个带 `n` 个局部变量的 Lua 函数头（帧寄存器数 ≈ n）。
fn locals_fn(name: &str, n: usize) -> String {
    let mut s = format!("function {name}()");
    for i in 0..n {
        s.push_str(&format!(" local {name}_v{i} = {i}"));
    }
    s
}

/// VM 内的栈扩容失败（`raise_memory_error` → longjmp）必须被 Lua 层 `pcall` 捕获，
/// 以预注册的 MEMERRMSG 上报，且事后解释器仍完全可用。
///
/// 若 Lua 层 pcall 不是保护点（longjmp 模式下曾如此），这里没有 `jmp_buf` 可用 ——
/// 实现只能打印错误并 `exit(1)`，整个测试进程直接死亡（= 本测试失败）。
#[test]
fn deep_stack_growth_failure_is_caught_by_lua_pcall() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut st = new_state();

    // 定义 6 层嵌套、每层 190 个局部的函数（层间调用不是尾调用，帧不复用）。
    // 深层帧需求（≈ 6×192 + 190 + 20 槽）远超初始栈容量（320 槽，实测 libs
    // 初始化后 640 槽）→ 第 4 层起必然触发 VM 内栈扩容。
    const LEVELS: usize = 6;
    const LOCALS: usize = 190;
    let mut defs = String::new();
    for lvl in 0..LEVELS {
        defs.push_str(&locals_fn(&format!("f{lvl}"), LOCALS));
        if lvl + 1 < LEVELS {
            defs.push_str(&format!(" local r = f{}(); return r end\n", lvl + 1));
        } else {
            defs.push_str(" return 'deep' end\n");
        }
    }
    assert_eq!(st.load_buffer(&defs, "=longjmp_defs"), 0, "定义函数失败");
    st.top = st.stack.len(); // Rust 侧压栈不推进 top, 而 GC 只把 [..top] 当根
    assert_eq!(st.pcall(0, 0, 0), 0, "定义函数执行失败");

    let probe = "local ok, err = pcall(f0)\n\
         assert(ok == false, '内存错误必须被 Lua 层 pcall 捕获')\n\
         assert(err == 'not enough memory', '错误对象应为预注册 MEMERRMSG, 实为 ' .. tostring(err))\n\
         return 'caught'\n";
    assert_eq!(st.load_buffer(probe, "=longjmp_probe"), 0, "编译探针失败");
    st.top = st.stack.len(); // 同上: 武装注入触发的紧急 GC 必须能看到探针函数

    // 反复走同一条 raise 路径并量化存活字节：每次 longjmp 都跳过 VM 派发帧
    // （`fast_resize_stack_nil` 的调用帧、op_call* 取出的 `proto` / `upvals` 引用等），
    // 那里若还持有 Rc 克隆，POSIX 上每轮泄漏一份。
    //
    // 每轮**重新编译**探针 chunk 而非复用同一份：Rust 侧 `load_buffer` 压栈不推进
    // `top`，而注入失败触发的紧急 GC 只把 `[..top]` 当根；复用预加载 chunk 会让
    // 紧急回收期间的对象生命周期语义变得微妙（另见 CLAUDE.md "已知异常"）。
    let run_once = |st: &mut LuaState<'static>| {
        assert_eq!(
            st.load_buffer(probe, "=longjmp_probe"),
            0,
            "每轮编译探针失败"
        );
        st.top = st.stack.len();
        FAIL_LARGE_ARMED.store(1, Ordering::Relaxed);
        let status = st.pcall(0, 1, 0);
        FAIL_LARGE_ARMED.store(0, Ordering::Relaxed);
        assert_eq!(
            status,
            0,
            "每轮内存错误都应被捕获（status={status}, 错误对象={}）",
            error_object(st)
        );
    };
    let full_gc = |st: &mut LuaState<'static>| {
        assert_eq!(
            st.load_buffer(
                "collectgarbage('collect'); collectgarbage('collect')",
                "=longjmp_gc"
            ),
            0,
            "编译 GC chunk 失败"
        );
        st.top = st.stack.len();
        assert_eq!(st.pcall(0, 0, 0), 0, "GC chunk 执行失败");
    };
    // 预热一轮：栈容量的增长只发生一次，不该算进泄漏
    run_once(&mut st);
    full_gc(&mut st);
    let before = live_bytes();

    for _ in 0..50 {
        run_once(&mut st);
    }
    full_gc(&mut st);
    let after = live_bytes();

    let growth = after - before;
    assert!(
        growth <= 4096,
        "深帧内存错误路径泄漏了 {growth} 字节 (before={before}, after={after})"
    );
}

// ============================================================================
// 测试 3：plain `extern "C"` 的 capi 入口抛内存错误不得 abort (Windows 展开代价)
// ============================================================================

/// 从一个 `extern "C"`（nounwind）capi 入口**内部**抛内存错误。
///
/// `lua_settable` 的表增长分配失败 → `raise_mem_error` → longjmp 跳到最近的保护点，
/// 展开必须穿过 `lua_settable` 自己的 Rust 帧。
/// - POSIX：longjmp 不展开，最多泄漏（本测试在 Linux 上恒过）；
/// - Windows/MSVC：longjmp 经 `RtlUnwind`，穿过 nounwind（`extern "C"`）帧会 abort
///   （`panic_cannot_unwind`）—— 因此凡是**可能抛错**的 capi 入口都必须声明
///   `C-unwind`（本测试即为此而设；修复前本测试会让测试进程直接死掉）。
///
/// 触发点选 `lua_settable`：表插入走 `try_reserve_for` → 漏斗 → 失败即
/// `raise_mem_error`。400 次整数键插入必然出现一次 ≥ `FAIL_LARGE_MIN` 的哈希区扩容。
unsafe extern "C-unwind" fn err_settable_oom(L: *mut c_void) -> c_int {
    let L = L as *mut lua_State;
    lua_createtable(L, 0, 0);
    FAIL_LARGE_ARMED.store(1, Ordering::Relaxed);
    let mut k: i64 = 0;
    while k < 400 {
        lua_pushinteger(L, k as _);
        lua_pushinteger(L, k as _);
        lua_settable(L, -3);
        k += 1;
    }
    // 抛错时到不了这里: 由测试主体在 pcall 返回后解除注入
    FAIL_LARGE_ARMED.store(0, Ordering::Relaxed);
    0
}

#[test]
fn capi_setter_oom_raise_does_not_abort() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut st = new_state();

    let L = &mut st as *mut LuaState as *mut lua_State;
    lua_pushcfunction(L, as_c_function(err_settable_oom));
    lua_setglobal(L, c"err_settable_oom".as_ptr());

    let hits_before = FAIL_LARGE_HITS.load(Ordering::Relaxed);
    let probe = "local ok, err = pcall(err_settable_oom)\n\
         assert(ok == false, 'capi setter 应当在分配失败时抛错')\n\
         assert(err == 'not enough memory', '错误对象应为预注册 MEMERRMSG, 实为 ' .. tostring(err))\n\
         return 'caught'\n";
    assert_eq!(st.load_buffer(probe, "=oom_setter"), 0, "编译探针失败");
    let status = st.pcall(0, 1, 0);
    FAIL_LARGE_ARMED.store(0, Ordering::Relaxed);
    let hits = FAIL_LARGE_HITS.load(Ordering::Relaxed) - hits_before;

    assert_eq!(
        status,
        0,
        "capi OOM 应被 Lua 层 pcall 捕获（status={status}, 错误={}）",
        error_object(&st)
    );
    assert!(
        hits >= 1,
        "必须真的命中过大额分配失败（hits={hits}）, 否则本测试退化为空转"
    );

    assert_eq!(
        st.load_buffer("assert(1 + 1 == 2); assert(#'abc' == 3)", "=oom_after"),
        0,
        "编译后置检查失败"
    );
    assert_eq!(st.pcall(0, 0, 0), 0, "OOM 之后解释器应可继续工作");
}

// ============================================================================
// 测试 4：C 闭包抛错不得泄漏 VM 派发帧持有的函数值
// ============================================================================

/// 抛错替身（注册为带 upvalue 的 C 闭包）。
unsafe extern "C-unwind" fn err_cclosure(L: *mut c_void) -> c_int {
    let L = L as *mut lua_State;
    luaL_argerror(L, 1, c"closure boom".as_ptr())
}

/// C 闭包（`TValue::CClosure`，持有 Rc）反复抛错：raise 会跳过 VM 派发帧
/// （`op_call_generic` / `op_tailcall` / `op_tforcall`），那里若还持有函数值的
/// 克隆，POSIX 上每次泄漏一整个闭包对象（连带其 upvalue）。
///
/// upvalue 故意用**Lua 闭包**：它持有 `Rc<Proto>` / `Rc<Vec<..>>`（全局堆分配），
/// 泄漏时能被计数分配器看到（纯 arena 对象的内存不会归还全局分配器，观测不到）。
/// 两条路径各测一遍：普通 CALL 与 TAILCALL（`return f()`）。
#[test]
fn c_closure_error_paths_do_not_leak() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut st = new_state();

    assert_eq!(
        st.load_buffer(
            "inner = function() return 'payload' end\n\
             function wrap_tail() return err_cclosure() end\n",
            "=cclosure_defs"
        ),
        0,
        "定义 upvalue 失败"
    );
    assert_eq!(st.pcall(0, 0, 0), 0, "定义执行失败");

    let L = &mut st as *mut LuaState as *mut lua_State;
    lua_getglobal(L, c"inner".as_ptr());
    lua_pushcclosure(L, as_c_function(err_cclosure), 1);
    lua_setglobal(L, c"err_cclosure".as_ptr());

    let run = |st: &mut LuaState, src: &str| {
        assert_eq!(st.load_buffer(src, "=longjmp"), 0, "编译失败: {src}");
        assert_eq!(st.pcall(0, 0, 0), 0, "执行失败: {src}");
    };

    // 先确认两条路径真的会抛错（否则下面的测量可能只是空转）
    run(
        &mut st,
        "local ok, err = pcall(err_cclosure)\n\
         assert(ok == false and type(err) == 'string')\n\
         local ok2 = pcall(wrap_tail)\n\
         assert(ok2 == false)\n",
    );
    run(
        &mut st,
        "collectgarbage('collect'); collectgarbage('collect')",
    );
    let before = live_bytes();

    let loop_src = "for i = 1, 400 do\n\
           assert(pcall(err_cclosure) == false)\n\
           assert(pcall(wrap_tail) == false)\n\
         end\n";
    run(&mut st, loop_src);
    run(
        &mut st,
        "collectgarbage('collect'); collectgarbage('collect')",
    );
    let after = live_bytes();

    let growth = after - before;
    assert!(
        growth <= 4096,
        "C 闭包抛错路径泄漏了 {growth} 字节 (before={before}, after={after})"
    );
}

// ============================================================================
