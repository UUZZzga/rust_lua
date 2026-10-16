//! 内存分配失败处理集成测试 —— 对应 C 的 `lmem.c`（`luaM_realloc_` / `tryagain` /
//! `luaM_error`）与 `ldo.c`（`LUA_ERRMEM` → 复用 `G(L)->memerrmsg`）。
//!
//! 覆盖两条核心语义（用可编程分配器精确制造失败，对应 C 测试库 ltests 的
//! `T.alloccount` / `EMERGENCYGCTESTS`，不依赖真实内存耗尽、不 flaky）：
//!
//! 1. **先 GC 再重试**：首次分配失败 → 紧急全量回收（`luaC_fullgc(L, 1)` 语义，
//!    不运行 finalizer）→ 重试成功，调用方完全无感；
//! 2. **不可恢复时抛可捕获的内存错误**：重试仍失败 → `VmError::MemoryError`
//!    （消息 "not enough memory"），`pcall` 可捕获，解释器随后仍可正常工作
//!    （错误对象取自状态创建时预注册的 `G(L)->memerrmsg`，传播过程零分配）。
//!
//! 真实 200MB 地址空间下的端到端验证见 `tests_lua/alloc_fail.lua`
//! （由 `tools/verify_alloc_fail.sh` / `tools/verify.sh` 用 `LimitAS=204800` 运行）。

use lua_rs::mem::{Allocator, DefaultAllocator};
use lua_rs::mock::io_mock::BufferIo;
use lua_rs::objects::{NilKind, TValue};
use lua_rs::state::LuaState;
use std::cell::Cell;
use std::rc::Rc;

/// 可编程失败的分配器。
///
/// - `fail_next`：强制接下来的 N 次**分配**（`new_size > 0`）失败（每次失败递减）；
/// - `attempts`：分配尝试计数（对照「重试确实发生了」）；
/// - `failures`：被强制失败的次数。
///
/// 释放（`new_size == 0`）永不失败 —— 与 C 的 `frealloc` 约定一致，
/// 也是紧急回收能腾出内存的前提。
struct FlakyAlloc {
    inner: DefaultAllocator,
    fail_next: Rc<Cell<usize>>,
    attempts: Rc<Cell<usize>>,
    failures: Rc<Cell<usize>>,
}

impl Allocator for FlakyAlloc {
    fn alloc(&self, ptr: *mut u8, old_size: usize, new_size: usize, align: usize) -> *mut u8 {
        if new_size > 0 {
            self.attempts.set(self.attempts.get() + 1);
            let left = self.fail_next.get();
            if left > 0 {
                self.fail_next.set(left - 1);
                self.failures.set(self.failures.get() + 1);
                return std::ptr::null_mut();
            }
        }
        self.inner.alloc(ptr, old_size, new_size, align)
    }
}

struct Harness<'a> {
    state: LuaState<'a>,
    fail_next: Rc<Cell<usize>>,
    attempts: Rc<Cell<usize>>,
    failures: Rc<Cell<usize>>,
}

fn harness<'a>(io: &'a mut BufferIo) -> Harness<'a> {
    let fail_next = Rc::new(Cell::new(0usize));
    let attempts = Rc::new(Cell::new(0usize));
    let failures = Rc::new(Cell::new(0usize));
    let mut state = LuaState::new_main_with_allocator(
        io,
        Box::new(FlakyAlloc {
            inner: DefaultAllocator,
            fail_next: fail_next.clone(),
            attempts: attempts.clone(),
            failures: failures.clone(),
        }),
    )
    .expect("创建 state 失败");
    state.open_selected_libs(0, 0).expect("打开标准库失败");
    Harness {
        state,
        fail_next,
        attempts,
        failures,
    }
}

/// 栈顶值转字符串（只用于错误对象 / 字符串结果，不触发 Lua 侧分配）。
fn tv_to_string(tv: &TValue) -> String {
    match tv {
        TValue::ShortStr(_) | TValue::LongStr(_) => {
            lua_rs::strings::lua_string_as_str(tv).to_string()
        }
        TValue::Boolean(b) => b.to_string(),
        TValue::Nil(_) => "nil".to_string(),
        other => format!("{other}"),
    }
}

impl<'a> Harness<'a> {
    /// 编译并执行一段 chunk（调用方自行保证此时未武装失败）——返回 `pcall` 状态码。
    fn run_ok(&mut self, src: &str) -> i32 {
        assert_eq!(
            self.state.load_buffer(src, "=alloc_failure"),
            0,
            "编译失败: {src}"
        );
        self.state.pcall(0, 0, 0)
    }

    /// 只编译（不执行）：供后续在「武装失败」状态下执行。
    fn load(&mut self, src: &str) {
        assert_eq!(
            self.state.load_buffer(src, "=alloc_failure"),
            0,
            "编译失败: {src}"
        );
    }

    /// 执行栈顶函数并在武装状态下取回**两个**返回值（`pcall` 的 ok/err）。
    ///
    /// 用栈上的返回值观察结果（而不是 Lua 侧拼接字符串）——
    /// 分配全失败时 Lua 侧任何字符串构造都会失败，观察本身必须零分配。
    fn call_capture_two(&mut self) -> (String, String) {
        let status = self.state.pcall(0, 2, 0);
        assert_eq!(
            status, 0,
            "pcall 自身应正常返回（错误被 Lua 捕获，不是崩溃）"
        );
        let ok = self
            .state
            .obj_at(-2)
            .expect("缺少 pcall 的第一个返回值")
            .clone();
        let err = self
            .state
            .obj_at(-1)
            .expect("缺少 pcall 的第二个返回值")
            .clone();
        (tv_to_string(&ok), tv_to_string(&err))
    }

    /// 读取 `collectgarbage("count")`（KB）。
    fn gc_count(&mut self) -> f64 {
        assert_eq!(self.run_ok("__probe = collectgarbage('count')"), 0);
        let key = self.state.intern_str("__probe");
        let tv = self.state.globals.get(&key).expect("__probe 应存在");
        match tv {
            TValue::Float(f) => f,
            TValue::Integer(i) => i as f64,
            other => panic!("collectgarbage('count') 应返回数字，实为 {other:?}"),
        }
    }
}

/// 「先紧急 GC，再重试」：一次性强制失败必须被重试抹平，且紧急回收确实回收了垃圾。
///
/// 与 C 语义对照：`luaM_realloc_` 失败 → `tryagain` → `luaC_fullgc(L, 1)` → 重试。
#[test]
fn transient_alloc_failure_triggers_emergency_gc_and_retry() {
    let mut buff = BufferIo::new("");
    let mut h = harness(&mut buff);

    // 1) 准备垃圾：停止 GC（对应 `collectgarbage("stop")`），制造 20k 个不可达表
    assert_eq!(h.run_ok("collectgarbage('stop')"), 0, "gc_stop 应成功");
    assert_eq!(
        h.run_ok("hold = {} for i = 1, 20000 do hold[i] = { i } end hold = nil"),
        0
    );
    let before = h.gc_count();

    // 2) 载入「增长表」chunk（此时未武装失败，编译本身需要的分配不受影响）
    h.load("return pcall(fill)");
    assert_eq!(h.run_ok("grow = {}"), 0);
    assert_eq!(
        h.run_ok("function fill() for i = 1, 200000 do grow[i] = i end end"),
        0
    );

    // 3) 武装：下一次分配必定失败（其后恢复正常）→ 必须走「紧急 GC + 重试」
    let attempts_before = h.attempts.get();
    h.fail_next.set(1);
    let (ok, err) = h.call_capture_two();
    assert_eq!(ok, "true", "一次性失败应被重试抹平（err={err}）");
    assert_eq!(h.failures.get(), 1, "应恰好发生一次强制失败");
    assert!(
        h.attempts.get() > attempts_before + 1,
        "失败后必须重新尝试分配（对应 C 的 tryagain）"
    );

    // 4) 紧急回收确实回收了步骤 1 的垃圾（GC 处于 stop 状态也照收 —— 与 C 的
    //    `tryagain` → `luaC_fullgc(L, 1)` 一致，只受 gcstopem 限制）
    let after = h.gc_count();
    assert!(
        after < before,
        "紧急回收应释放不可达垃圾（before={before}KB after={after}KB）"
    );
    assert_eq!(h.run_ok("assert(#grow == 200000)"), 0, "表增长结果应完整");
}

/// 不可恢复的分配失败：`pcall` 必须收到预注册的 "not enough memory"，
/// 且解释器随后仍完全可用（进程不中止）。
#[test]
fn unrecoverable_alloc_failure_is_catchable_memory_error() {
    let mut buff = BufferIo::new("");
    let mut h = harness(&mut buff);

    assert_eq!(h.run_ok("marker = 'alive'"), 0);
    // 「创建表 + 数组增长」两件事都放进被保护函数体内，覆盖对象块与数组扩容两条分配路径。
    // 函数本身在未武装时创建 —— 否则闭包分配会先失败，错误就落在 pcall 之外。
    h.load("return pcall(boom)");
    assert_eq!(
        h.run_ok("function boom() local t = {} for i = 1, 500 do t[i] = i end return t end"),
        0
    );

    // 武装：之后所有分配都失败 → 重试（含紧急 GC）仍然失败 → 内存错误
    h.fail_next.set(usize::MAX);
    let (ok, err) = h.call_capture_two();
    assert_eq!(ok, "false", "分配失败必须以错误返回");
    assert_eq!(err, "not enough memory", "错误消息必须是预注册的 MEMERRMSG");

    // 解除武装：解释器（栈 / 全局表 / GC）必须仍然可用
    h.fail_next.set(0);
    assert_eq!(
        h.run_ok("assert(marker == 'alive'); assert(type({}) == 'table'); assert(#'abc' == 3)"),
        0,
        "内存错误之后解释器应可继续工作"
    );
}

/// 内存错误的对象必须是**预注册**的（`G(L)->memerrmsg`）：构造/传播全程不能再分配。
///
/// 若错误对象是现场 intern 的字符串，则分配器已耗尽时传播错误本身会再次失败
/// （C 的 `luaD_seterrorobj` 对 LUA_ERRMEM 专门复用 `memerrmsg` 正是此意）。
#[test]
fn memory_error_object_is_preregistered() {
    let mut buff = BufferIo::new("");
    let mut h = harness(&mut buff);

    h.load("return pcall(mk)");
    assert_eq!(h.run_ok("function mk() return {} end"), 0);

    h.fail_next.set(usize::MAX);
    let (ok, err) = h.call_capture_two();
    assert_eq!(ok, "false");
    assert_eq!(err, "not enough memory");

    h.fail_next.set(0);
    assert_eq!(
        h.run_ok("local t = {} t.x = 1 assert(t.x == 1)"),
        0,
        "内存错误后的表操作应正常"
    );
}

/// 表**扩容**失败的专项验证：表已存在（对象块分配已完成），
/// 失败点落在 `luaH_*` 的数组/哈希扩容上（对应 C 的 `luaH_growarrays` / `rehash`）。
#[test]
fn table_growth_failure_is_catchable() {
    let mut buff = BufferIo::new("");
    let mut h = harness(&mut buff);

    h.load("return pcall(fill)");
    assert_eq!(h.run_ok("grow = {}"), 0);
    assert_eq!(
        h.run_ok("function fill() for i = 1, 100000 do grow[i] = i end end"),
        0
    );

    h.fail_next.set(usize::MAX);
    let (ok, err) = h.call_capture_two();
    assert_eq!(ok, "false");
    assert_eq!(err, "not enough memory");

    // 表仍然自洽（扩容失败不得留下半写状态：先扩容、后写入的纪律）
    h.fail_next.set(0);
    assert_eq!(
        h.run_ok("local n = 0 for i = 1, #grow do n = n + 1 end assert(n == #grow)"),
        0,
        "扩容失败后表内容应保持自洽"
    );
    // 解除武装后原先失败的写入必须能成功（证明失败时未破坏表状态）
    assert_eq!(
        h.run_ok("for i = 1, 100000 do grow[i] = i end assert(#grow == 100000)"),
        0
    );
}

/// 内存错误之后 GC 仍必须正常工作（紧急 GC 不得破坏颜色/对象表状态）。
#[test]
fn gc_still_works_after_memory_error() {
    let mut buff = BufferIo::new("");
    let mut h = harness(&mut buff);

    h.load("return pcall(mk)");
    assert_eq!(h.run_ok("function mk() return {} end"), 0);
    h.fail_next.set(usize::MAX);
    let (ok, _err) = h.call_capture_two();
    assert_eq!(ok, "false");

    h.fail_next.set(0);
    // 弱表必须在随后的常规 GC 中被正确清理（依赖颜色/对象表状态完好）
    assert_eq!(
        h.run_ok(
            "local wt = setmetatable({}, {__mode = 'v'}); \
             wt[1] = {}; collectgarbage('collect'); assert(wt[1] == nil, 'weak table not cleared')"
        ),
        0,
        "内存错误后的弱表回收应正常"
    );
    assert_eq!(
        h.run_ok("local t = {} t.self = t t = nil collectgarbage('collect')"),
        0
    );
    assert_ne!(h.state.obj_at(-1), Some(&TValue::Nil(NilKind::Strict)));
}

/// 紧急回收**不得运行 finalizer**（C 的 `gcemergency` 语义），
/// 但待 finalize 的对象不得被扫掉：`__gc` 必须在下一次常规 GC 照常执行。
///
/// 若紧急回收既跳过 finalizer 又让对象被 sweep 释放，finalizer 就永久丢失
/// （tests_lua/gc.lua 的 `__gc` 断言会失败）。
#[test]
fn emergency_gc_defers_finalizers_without_losing_them() {
    let mut buff = BufferIo::new("");
    let mut h = harness(&mut buff);

    assert_eq!(
        h.run_ok(
            "gc_ran = 0; \
             local t = setmetatable({}, {__gc = function() gc_ran = gc_ran + 1 end}); \
             t = nil"
        ),
        0
    );

    // 武装一次失败 → 迫使紧急回收发生（finalizer 必须被推迟）
    h.load("return pcall(mk)");
    assert_eq!(h.run_ok("function mk() return {} end"), 0);
    h.fail_next.set(1);
    let (ok, err) = h.call_capture_two();
    assert_eq!(ok, "true", "一次性失败应被重试抹平（err={err}）");

    // 紧急回收后：finalizer 尚未运行（推迟）
    assert_eq!(
        h.run_ok("assert(gc_ran == 0, 'emergency GC must not run finalizers')"),
        0,
        "紧急回收不得运行 finalizer"
    );

    // 常规回收必须照常运行它（对象没有被丢失）
    assert_eq!(
        h.run_ok("collectgarbage('collect'); assert(gc_ran == 1, 'finalizer lost: ' .. gc_ran)"),
        0,
        "finalizer 不得因紧急回收而丢失"
    );
}
