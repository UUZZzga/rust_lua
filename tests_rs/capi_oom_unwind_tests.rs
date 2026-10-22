//! capi 入口抛内存错误时，展开必须能穿到保护点，而不是让进程 abort。
//!
//! 默认构建（非 `lua_longjmp`）里 `raise_mem_error` 是 **panic**：panic 从
//! Rust `extern "C"`（nounwind）函数向外展开会命中编译器插入的
//! `panic_cannot_unwind` 落地垫 → `abort`（实测 `STATUS_STACK_BUFFER_OVERRUN`）。
//! `lua_longjmp` 构建里同一处换成 `longjmp`：POSIX 上只是静默跳过（泄漏），
//! Windows/MSVC 上经 `RtlUnwind` 展开，同样撞上 nounwind 帧 → abort
//! （见 `tests_rs/longjmp_tests.rs::capi_setter_oom_raise_does_not_abort`）。
//!
//! 因此 **凡是可能抛错（或被抛错穿过）的 capi 入口一律声明 `C-unwind`**
//! （`capi.rs` 的导出入口已统一；C ABI 不变，只多一个「允许展开穿过」的语义）。
//! 本文件不依赖 `lua_longjmp`，所以在默认构建下也能检出 —— 两个配置各修一次
//! 同一个根因，两个测试各守一扇门。
//!
//! **注入方式必须是项目自己的分配器**（`LuaState::new_main_with_allocator` +
//! `Allocator` trait，同 `alloc_failure_tests.rs`），而不是全局分配器按大小过滤：
//! 生产路径的分配一律经漏斗（失败返回 `AllocFailed`），而 panic 运行时、
//! `state.stack` 这类普通 `Vec`、错误路径里尚未迁移的不可失败分配
//! （如 `LuaState::pcall` 错误路径的 `stack.split_off`）都走**全局**分配器 ——
//! 用全局注入把它们一起打掉就会命中 `handle_alloc_error`（打印
//! `memory allocation of N bytes failed` 后挂死/abort），实测在 Drone Linux 上把
//! 整条流水线拖到 60 分钟超时（build #228/#229）。

// capi 模块在 cmp_c feature 下不存在（该配置链接 C 实现, 见 CLAUDE.md）
#![cfg(not(feature = "cmp_c"))]
#![allow(non_snake_case)]

use lua_rs::capi::{lua_State, lua_pushcfunction, lua_pushinteger, lua_setglobal, lua_settable};
use lua_rs::mem::{Allocator, DefaultAllocator};
use lua_rs::state::LuaState;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

// ============================================================================
// 可编程失败的**项目**分配器（只拦项目自己的分配, 全部经漏斗）
// ============================================================================

/// 武装开关与命中计数（进程级静态量: 替身 C 函数需要在自己体内武装/解除，
/// 而 `extern "C"` 函数无法捕获闭包）。释放（`new_size == 0`）永不失败 ——
/// 与 C 的 `frealloc` 约定一致，也是紧急回收能腾出内存的前提。
static FAIL_ARMED: AtomicBool = AtomicBool::new(false);
static FAIL_HITS: AtomicIsize = AtomicIsize::new(0);

/// 只打掉**大额**分配：小额分配即便落在尚未迁移的不可失败路径上（典型是
/// 字符串创建 —— `strings.rs` 的 `insert_unique` 没有错误通道）也会成功，
/// 从而避免 `handle_alloc_error` 把测试进程打挂。表扩容（数组/哈希区）
/// 在 400 次整数键插入里必然超过这个阈值。
const FAIL_MIN: usize = 4096;

struct FlakyAlloc {
    inner: DefaultAllocator,
}

impl Allocator for FlakyAlloc {
    fn alloc(&self, ptr: *mut u8, old_size: usize, new_size: usize, align: usize) -> *mut u8 {
        if new_size >= FAIL_MIN && FAIL_ARMED.load(Ordering::Relaxed) {
            FAIL_HITS.fetch_add(1, Ordering::Relaxed);
            return std::ptr::null_mut();
        }
        self.inner.alloc(ptr, old_size, new_size, align)
    }
}

/// 武装开关与命中计数是**进程级静态量**（替身 C 函数要在自己体内武装/解除，
/// 而 `extern "C"` 函数无法捕获闭包）：各测试必须串行。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 把本文件的 C-unwind 替身转成 `lua_CFunction`（`extern "C"`，ABI 相同）。
fn as_c_function(
    f: unsafe extern "C-unwind" fn(*mut c_void) -> c_int,
) -> lua_rs::capi::lua_CFunction {
    unsafe { std::mem::transmute(f) }
}

fn new_state() -> LuaState<'static> {
    let mut state = LuaState::new_main_with_allocator(
        lua_rs::mock::io_mock::lua_io(),
        Box::new(FlakyAlloc {
            inner: DefaultAllocator,
        }),
    )
    .expect("创建 state 失败");
    state.open_selected_libs(0, 0).expect("打开标准库失败");
    state
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
// 替身 C 函数：在 capi 入口内部制造内存错误
// ============================================================================

/// `lua_settable` 的表扩容分配失败 → `raise_mem_error`（展开要穿过它自己的帧）。
///
/// 表的**创建**在武装之前（不注入），随后武装并反复插入整数键 —— 表扩容经
/// `Table::try_reserve_for` 走漏斗，失败即 `raise_mem_error`。
unsafe extern "C-unwind" fn err_settable_oom(L: *mut c_void) -> c_int {
    let L = L as *mut lua_State;
    lua_rs::capi::lua_createtable(L, 0, 0);
    FAIL_ARMED.store(true, Ordering::Relaxed);
    let mut k: i64 = 0;
    while k < 400 {
        lua_pushinteger(L, k as _);
        lua_pushinteger(L, k as _);
        lua_settable(L, -3);
        k += 1;
    }
    // 抛错时不会执行到这里: 由测试主体在 pcall 返回后解除注入
    FAIL_ARMED.store(false, Ordering::Relaxed);
    0
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

    // 观察本身必须零分配：武装期间 Lua 侧任何字符串构造（`..` / `tostring`）
    // 都会落在尚未迁移的不可失败分配上 → `handle_alloc_error`。因此错误对象的
    // 比对放在 Rust 侧做，chunk 只做常量比较与 `return err`。
    let probe = "local ok, err = pcall(err_settable_oom)\n\
         assert(ok == false)\n\
         return err\n";
    assert_eq!(st.load_buffer(probe, "=oom_probe"), 0, "编译探针失败");

    let hits_before = FAIL_HITS.load(Ordering::Relaxed);
    let status = st.pcall(0, 1, 0);
    FAIL_ARMED.store(false, Ordering::Relaxed); // 抛错路径不会自行解除
    let hits = FAIL_HITS.load(Ordering::Relaxed) - hits_before;

    assert_eq!(
        status,
        0,
        "内存错误应被 Lua 层 pcall 捕获（status={status}, 错误={}）",
        error_object(&st)
    );
    assert!(
        hits >= 1,
        "必须真的命中过项目分配失败（hits={hits}），否则本测试退化为空转"
    );
    // 错误对象必须是预注册的 MEMERRMSG（Rust 侧比对，零分配）
    let err = st.obj_at(-1).expect("缺少 pcall 的返回值").clone();
    assert_eq!(
        lua_rs::strings::lua_string_as_str(&err),
        "not enough memory",
        "错误对象应为预注册 MEMERRMSG"
    );

    assert_eq!(
        st.load_buffer("assert(1 + 1 == 2); assert(#'abc' == 3)", "=oom_after"),
        0,
        "编译后置检查失败"
    );
    assert_eq!(st.pcall(0, 0, 0), 0, "OOM 之后解释器应可继续工作");
}
