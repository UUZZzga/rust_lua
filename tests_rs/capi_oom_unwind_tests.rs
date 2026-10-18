//! capi 入口抛内存错误时，展开必须能穿到保护点，而不是让进程 abort。
//!
//! 默认构建（非 `lua_longjmp`）里 `raise_mem_error` 是 **panic**：panic 从
//! Rust `extern "C"`（nounwind）函数向外展开会命中编译器插入的
//! `panic_cannot_unwind` 落地垫 → `abort`（`STATUS_STACK_BUFFER_OVERRUN`）。
//! `lua_longjmp` 构建里同一处换成 `longjmp`：POSIX 上只是静默跳过（泄漏），
//! Windows/MSVC 上经 `RtlUnwind` 展开，同样撞上 nounwind 帧 → abort
//! （见 `tests_rs/longjmp_tests.rs::capi_setter_oom_raise_does_not_abort`）。
//!
//! 因此 **凡是可能抛错（或被抛错穿过）的 capi 入口一律声明 `C-unwind`**
//! （`capi.rs` 的导出入口已统一；C ABI 不变，只多一个「允许展开穿过」的语义）。
//! 本文件不依赖 `lua_longjmp`，所以在默认构建下也能检出 —— 两个配置各修一次
//! 同一个根因，两个测试各守一扇门。
//!
//! 探针：按大小注入分配失败（≥ 4 KiB），保证错误路径自身的小额分配（panic 载荷、
//! 预注册的 memerrmsg）仍能成功 —— 真实耗尽（全量失败）是另一回事，见
//! `tests_rs/alloc_failure_tests.rs`。

// capi 模块在 cmp_c feature 下不存在（该配置链接 C 实现, 见 CLAUDE.md）
#![cfg(not(feature = "cmp_c"))]
#![allow(non_snake_case)]

use lua_rs::capi::{lua_State, lua_pushcfunction, lua_pushinteger, lua_setglobal, lua_settable};
use lua_rs::state::LuaState;
use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicIsize, Ordering};

// ============================================================================
// 失败注入（按大小过滤）
// ============================================================================

static FAIL_LARGE_ARMED: AtomicIsize = AtomicIsize::new(0);
static FAIL_LARGE_HITS: AtomicIsize = AtomicIsize::new(0);
const FAIL_LARGE_MIN: usize = 4096;

struct FlakyAlloc;

unsafe impl GlobalAlloc for FlakyAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= FAIL_LARGE_MIN && FAIL_LARGE_ARMED.load(Ordering::Relaxed) != 0 {
            FAIL_LARGE_HITS.fetch_add(1, Ordering::Relaxed);
            return std::ptr::null_mut();
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static FLAKY: FlakyAlloc = FlakyAlloc;

/// 全局分配器是进程级的 → 各测试串行。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 把本文件的 C-unwind 替身转成 `lua_CFunction`（`extern "C"`，ABI 相同）。
fn as_c_function(
    f: unsafe extern "C-unwind" fn(*mut c_void) -> c_int,
) -> lua_rs::capi::lua_CFunction {
    unsafe { std::mem::transmute(f) }
}

fn new_state() -> LuaState<'static> {
    let mut st = LuaState::new_main(lua_rs::mock::io_mock::lua_io()).expect("创建 state 失败");
    st.open_selected_libs(0, 0).expect("打开标准库失败");
    st
}

fn register(st: &mut LuaState, name: &std::ffi::CStr, f: lua_rs::capi::lua_CFunction) {
    let L = st as *mut LuaState as *mut lua_State;
    lua_pushcfunction(L, f);
    lua_setglobal(L, name.as_ptr());
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
// 替身 C 函数：各自在「plain extern "C" 的 capi 入口内部」制造内存错误
// ============================================================================

/// `lua_settable` 的表扩容分配失败 → `raise_mem_error`（展开要穿过它自己的帧）。
unsafe extern "C-unwind" fn err_settable_oom(L: *mut c_void) -> c_int {
    let L = L as *mut lua_State;
    lua_rs::capi::lua_createtable(L, 0, 0);
    FAIL_LARGE_ARMED.store(1, Ordering::Relaxed);
    let mut k: i64 = 0;
    while k < 400 {
        lua_pushinteger(L, k as _);
        lua_pushinteger(L, k as _);
        lua_settable(L, -3);
        k += 1;
    }
    // 抛错时不会执行到这里: 由测试主体在 pcall 返回后解除注入
    FAIL_LARGE_ARMED.store(0, Ordering::Relaxed);
    0
}

/// 通用跑法：期望 C 函数抛出的内存错误被 **Lua 层 pcall** 捕获为
/// 预注册的 MEMERRMSG，且解释器事后仍可用。
fn expect_catchable_oom(st: &mut LuaState, fname: &std::ffi::CStr, label: &str) {
    let probe = format!(
        "local ok, err = pcall({name})\n\
         assert(ok == false, '{label}: C 函数应当在分配失败时抛错')\n\
         assert(err == 'not enough memory', '{label}: 错误对象应为预注册 MEMERRMSG, 实为 ' .. tostring(err))\n\
         return 'caught'\n",
        name = fname.to_str().unwrap(),
        label = label
    );
    assert_eq!(st.load_buffer(&probe, "=oom_probe"), 0, "编译探针失败");

    let hits_before = FAIL_LARGE_HITS.load(Ordering::Relaxed);
    let status = st.pcall(0, 1, 0);
    FAIL_LARGE_ARMED.store(0, Ordering::Relaxed); // 抛错路径不会自行解除
    let hits = FAIL_LARGE_HITS.load(Ordering::Relaxed) - hits_before;

    assert_eq!(
        status,
        0,
        "{label}: 内存错误应被 Lua 层 pcall 捕获（status={status}, 错误={}）",
        error_object(st)
    );
    assert!(
        hits >= 1,
        "{label}: 必须真的命中过分配失败（hits={hits}），否则本测试退化为空转"
    );
    assert_eq!(
        st.load_buffer("assert(1 + 1 == 2); assert(#'abc' == 3)", "=oom_after"),
        0,
        "{label}: 编译后置检查失败"
    );
    assert_eq!(st.pcall(0, 0, 0), 0, "{label}: OOM 之后解释器应可继续工作");
}

// ============================================================================
// 测试
// ============================================================================

/// `lua_settable` 内部抛内存错误：展开穿 `lua_settable` 自身帧 + 本文件的 C 替身帧
/// → 被 VM 的 C 调用保护点（`call_c_function` 的 catch_unwind）接住 → Lua pcall 可见。
#[test]
fn settable_oom_raise_is_catchable_not_abort() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut st = new_state();
    register(
        &mut st,
        c"err_settable_oom",
        as_c_function(err_settable_oom),
    );
    expect_catchable_oom(&mut st, c"err_settable_oom", "lua_settable");
}
