//! 垃圾回收器 (Lua 5.5 GC → Rust 惯用重写)
//!
//! 对应 C 源码: lgc.h + lgc.cpp + lstate.cpp 中的 GC 相关部分
//!
//! ## 核心算法
//! - **三色标记**: 白色(未标记)、灰色(已标记但未扫描引用)、黑色(已标记且引用已扫描)
//! - **颜色即集合**: 可达性直接编码在 `GCObjectMeta.color` 中，
//!   `sweep_unreachable` 无参读颜色判定生死。有 `GCObjectId` 的对象
//!   （Table/LClosure/UserData）不再需要任何外挂 HashSet。
//! - **辅助集合仅用于无 ID 对象**: CClosure / RustClosure / Thread 无 gc_header，
//!   用 `GCState.aux_marked` 记录（**跨周期复用**，GC 路径零 malloc）。
//! - **写屏障**: 维护"黑色对象不指向白色对象"的不变式
//! - **分代**: 对象按 age 分层，老对象不参与每次回收
//!
//! ## GC 路径零分配
//! - `sweep_unreachable` 无参，直接读颜色
//! - `aux_marked` / `gray` / `gray_again` / `weak` / `all_weak` / `ephemeron`
//!   都跨周期复用（`clear()` 保留容量）
//! - worklist（`Vec<RawTValue>`）在 GC 期间借用复用
//!
//! ## 文件组织
//! 本文件集中**全部** GC 逻辑：`GCState`、三色标记、sweep、弱表清理、finalizer、
//! GC 统计、触发源统计、时间统计，以及 `impl LuaState`/`impl GlobalState` 的
//! GC 入口方法（`collect_gc` / `maybe_collect_gc` / `step_gc` /
//! `clear_weak_tables` / `mark_tvalue` 等）。`state.rs` 只保留数据结构、栈操作、
//! 调用/错误保护、C API 桥接。

use crate::alloc::{MyRc, WeakRc};
use crate::execute::VmError;
use crate::mem::SlotAlloc;
use crate::objects::{
    FxBuildHasher, LuaThread, NilKind, TValue, ThreadContext, UpVal, UpValVec, WeakValue,
};
use crate::state::{CallInfoEntry, GlobalState, LuaState};
use crate::strings::lua_string_as_str;
use crate::table::Table;
use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::num::NonZeroU32;
use std::rc::Rc;

// ============================================================================
// GCObjectId — GC 对象的唯一标识
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct GCObjectId(pub(crate) u32);

// ============================================================================
// GCColor — 对象颜色 (三色标记)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GCColor {
    White0,
    White1,
    Gray,
    Black,
}

impl GCColor {
    pub fn is_white(&self, current_white: u8) -> bool {
        match self {
            GCColor::White0 => current_white & 1 != 0,
            GCColor::White1 => current_white & 2 != 0,
            _ => false,
        }
    }

    pub fn is_gray(&self) -> bool {
        matches!(self, GCColor::Gray)
    }

    pub fn is_black(&self) -> bool {
        matches!(self, GCColor::Black)
    }
}

// ============================================================================
// GCAge — 对象年龄 (分代 GC)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum GCAge {
    New = 0,
    Survival = 1,
    Old1 = 3,
    Old = 4,
    Touched1 = 5,
    Touched2 = 6,
}

impl Default for GCAge {
    fn default() -> Self {
        GCAge::New
    }
}

// ============================================================================
// GCPhase — GC 状态机阶段
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GCPhase {
    Propagate = 0,
    EnterAtomic = 1,
    Atomic = 2,
    SweepAllGC = 3,
    SweepEnd = 6,
    Pause = 8,
}

// ============================================================================
// GCMode — GC 模式
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GCMode {
    Incremental,
    Generational,
}

// ============================================================================
// GCObjectMeta — 每个 GC 对象的元数据
// ============================================================================

#[derive(Debug, Clone)]
pub(crate) struct GCObjectMeta<'a> {
    pub color: GCColor,
    pub age: GCAge,
    pub weak_value: WeakValue<'a>,
    #[allow(dead_code)]
    pub finalized: bool,
    pub size: usize,
    #[allow(dead_code)]
    pub traversed: bool,
}

impl<'a> GCObjectMeta<'a> {
    fn new(size: usize) -> Self {
        GCObjectMeta {
            color: GCColor::White0,
            age: GCAge::New,
            weak_value: WeakValue::Nil(NilKind::Strict),
            finalized: false,
            size,
            traversed: false,
        }
    }
}

// ============================================================================
// GCObjectHeader — 可嵌入的 GC 对象头部
// ============================================================================

/// 全局指针 ID 计数器 — 用于 %p 格式输出稳定的唯一标识符。
/// 对应 C 实现中对象的堆地址。
static PTR_ID_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// 分配一个新的唯一指针 ID（对应 C 中堆对象的地址）
pub fn new_ptr_id() -> u32 {
    PTR_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct GCObjectHeader {
    id: Cell<Option<NonZeroU32>>,
    /// 稳定的唯一标识符，用于 %p 格式输出。
    /// 克隆时保留同一值（表示同一个对象）。
    pub ptr_id: u32,
}

impl GCObjectHeader {
    pub fn new() -> Self {
        GCObjectHeader {
            id: Cell::new(None),
            ptr_id: new_ptr_id(),
        }
    }

    pub fn id(&self) -> Option<GCObjectId> {
        self.id.get().map(|n| GCObjectId(n.get()))
    }

    pub fn set_id(&self, id: GCObjectId) {
        self.id.set(Some(NonZeroU32::new(id.0).unwrap()));
    }
}

impl Default for GCObjectHeader {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// GC 统计 — 由环境变量 LUA_GC_STATS=1 启用
// ============================================================================

#[derive(Default, Clone, Copy)]
pub struct GcTypeStats {
    pub table: u64,
    pub table_with_mt: u64,
    pub lclosure: u64,
    pub lclosure_with_upvals: u64,
    pub cclosure: u64,
    pub rust_closure: u64,
    pub thread: u64,
    pub userdata: u64,
    pub proto: u64,
    pub short_string: u64,
    pub gc_cycles: u64,
    pub reachable_total: u64,
    pub active_total: u64,
}

impl GcTypeStats {
    const ZERO: Self = GcTypeStats {
        table: 0,
        table_with_mt: 0,
        lclosure: 0,
        lclosure_with_upvals: 0,
        cclosure: 0,
        rust_closure: 0,
        thread: 0,
        userdata: 0,
        proto: 0,
        short_string: 0,
        gc_cycles: 0,
        reachable_total: 0,
        active_total: 0,
    };
}

thread_local! {
    static GC_STATS: Cell<GcTypeStats> = const { Cell::new(GcTypeStats::ZERO) };
}

pub fn gc_stats_enabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("LUA_GC_STATS")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false)
    })
}

#[inline]
fn gc_stats_inc<F: FnOnce(&mut GcTypeStats)>(f: F) {
    if !gc_stats_enabled() {
        return;
    }
    GC_STATS.with(|cell| {
        let mut s = cell.get();
        f(&mut s);
        cell.set(s);
    });
}

fn reset_gc_type_stats() {
    GC_STATS.with(|cell| {
        let mut s = cell.get();
        s.table = 0;
        s.table_with_mt = 0;
        s.lclosure = 0;
        s.lclosure_with_upvals = 0;
        s.cclosure = 0;
        s.rust_closure = 0;
        s.thread = 0;
        s.userdata = 0;
        s.proto = 0;
        s.short_string = 0;
        cell.set(s);
    });
}

fn dump_gc_stats(active_before: usize) {
    let reachable_count = {
        // reachable_total 由 collect_gc_inner 在调用前更新
        GC_STATS.with(|c| c.get().reachable_total)
    };
    let active_count = active_before as u64;
    gc_stats_inc(|s| {
        s.gc_cycles += 1;
        s.reachable_total = reachable_count;
        s.active_total = active_count;
    });
    GC_STATS.with(|cell| {
        let s = cell.get();
        let cyclic = s.table + s.lclosure + s.cclosure + s.rust_closure + s.thread + s.userdata;
        let non_cyclic = s.proto + s.short_string;
        let total = cyclic + non_cyclic;
        eprintln!(
            "[GC stats] cycle={} reachable={} active={} freed={} | \
             Table={} (with_mt={}) LClosure={} (with_upvals={}) CClosure={} RustClosure={} \
             Thread={} UserData={} | cyclic={} non_cyclic={} | Proto={} ShortString={}",
            s.gc_cycles,
            s.reachable_total,
            s.active_total,
            s.active_total.saturating_sub(s.reachable_total),
            s.table,
            s.table_with_mt,
            s.lclosure,
            s.lclosure_with_upvals,
            s.cclosure,
            s.rust_closure,
            s.thread,
            s.userdata,
            cyclic,
            non_cyclic,
            s.proto,
            s.short_string,
        );
        let pct = if total > 0 {
            cyclic as f64 * 100.0 / total as f64
        } else {
            0.0
        };
        eprintln!(
            "[GC stats] cyclic_ratio={:.2}% (cyclic={} / total={})",
            pct, cyclic, total
        );
        GC_TRIGGER_STATS.with(|cell| {
            let t = cell.get();
            eprintln!(
                "[GC stats] triggers: maybe={} concat={} step={} explicit={}",
                t.maybe, t.concat, t.step, t.explicit
            );
        });
    });
}

// ============================================================================
// GC 触发源统计
// ============================================================================

#[derive(Default, Clone, Copy)]
struct GcTriggerStats {
    maybe: u64,
    concat: u64,
    step: u64,
    explicit: u64,
}

impl GcTriggerStats {
    const ZERO: Self = GcTriggerStats {
        maybe: 0,
        concat: 0,
        step: 0,
        explicit: 0,
    };
}

#[derive(Clone, Copy)]
pub(crate) enum GcTrigger {
    Maybe,
    Concat,
    Step,
    Explicit,
}

thread_local! {
    static GC_TRIGGER_STATS: Cell<GcTriggerStats> = const { Cell::new(GcTriggerStats::ZERO) };
}

pub(crate) fn gc_trigger_inc(trigger: GcTrigger) {
    if !gc_stats_enabled() {
        return;
    }
    GC_TRIGGER_STATS.with(|cell| {
        let mut s = cell.get();
        match trigger {
            GcTrigger::Maybe => s.maybe += 1,
            GcTrigger::Concat => s.concat += 1,
            GcTrigger::Step => s.step += 1,
            GcTrigger::Explicit => s.explicit += 1,
        }
        cell.set(s);
    });
}

pub(crate) fn gc_trigger_explicit() {
    gc_trigger_inc(GcTrigger::Explicit);
}

// ============================================================================
// GC 时间统计
// ============================================================================

#[derive(Default, Clone, Copy)]
struct GcTimeStats {
    total_us: u64,
    count: u64,
    max_us: u64,
    str_sweep_us: u64,
    str_for_each_us: u64,
}

impl GcTimeStats {
    const ZERO: Self = GcTimeStats {
        total_us: 0,
        count: 0,
        max_us: 0,
        str_sweep_us: 0,
        str_for_each_us: 0,
    };
}

thread_local! {
    static GC_TIME_STATS: Cell<GcTimeStats> = const { Cell::new(GcTimeStats::ZERO) };
    static STR_EXTRA_CACHE: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
}

fn record_gc_time(elapsed: std::time::Duration) {
    let elapsed_us = elapsed.as_micros() as u64;
    GC_TIME_STATS.with(|cell| {
        let mut s = cell.get();
        s.total_us += elapsed_us;
        s.count += 1;
        if elapsed_us > s.max_us {
            s.max_us = elapsed_us;
        }
        cell.set(s);
    });
}

pub fn print_gc_time_summary() {
    if !gc_stats_enabled() {
        return;
    }
    GC_TIME_STATS.with(|cell| {
        let s = cell.get();
        if s.count == 0 {
            return;
        }
        let avg_us = s.total_us / s.count;
        eprintln!(
            "[GC time] count={} total={:.3}s avg={:.3}ms max={:.3}ms | \
             str_sweep={:.3}s str_for_each={:.3}s mark_other={:.3}s",
            s.count,
            s.total_us as f64 / 1_000_000.0,
            avg_us as f64 / 1000.0,
            s.max_us as f64 / 1000.0,
            s.str_sweep_us as f64 / 1_000_000.0,
            s.str_for_each_us as f64 / 1_000_000.0,
            (s.total_us
                .saturating_sub(s.str_sweep_us)
                .saturating_sub(s.str_for_each_us)) as f64
                / 1_000_000.0,
        );
    });
}

// ============================================================================
// RawTValue — worklist 的原始值项
// ============================================================================

/// TValue 的字节拷贝，不做引用计数。
/// GC 期间所有对象有效（sweep 在 mark 完成后），所以 push/pop 无需 incq/decq。
/// 消除 worklist 的 TValue clone + drop_glue 开销。
pub(crate) type RawTValue<'a> = std::mem::MaybeUninit<TValue<'a>>;

#[inline(always)]
pub(crate) unsafe fn raw_from_tvalue<'a>(val: &TValue<'a>) -> RawTValue<'a> {
    std::mem::MaybeUninit::new(std::ptr::read(val as *const TValue))
}

#[inline(always)]
pub(crate) unsafe fn raw_as_tvalue<'a, 'b>(raw: &'b RawTValue<'a>) -> &'b TValue<'a> {
    raw.assume_init_ref()
}

/// 只处理需要 GC 标记的类型（Table/LClosure/CClosure/UserData/Thread/RustClosure）。
#[inline]
fn needs_gc_mark<'a>(val: &TValue<'a>) -> bool {
    matches!(
        val,
        TValue::Table(_)
            | TValue::LClosure(_)
            | TValue::CClosure(_)
            | TValue::UserData(_)
            | TValue::Thread(_)
            | TValue::RustClosure(_)
    )
}

/// 判断值是否被 GC 标记为存活。
///
/// 有 `GCObjectId` 的类型（Table / LClosure / UserData）：读颜色。
/// 无 ID 的类型（CClosure / RustClosure / Thread）：读 `GCState.aux_marked`。
/// 非 GC 对象（字符串、数字、布尔、nil）：始终视为存活。
pub(crate) fn is_marked<'a>(val: &TValue<'a>, gc: &GCState) -> bool {
    match val {
        TValue::Table(t) => {
            if let Some(id) = t.data.borrow().gc_header.id() {
                !gc.is_white(id)
            } else {
                gc.aux_is_marked(t.data.borrow().gc_header.ptr_id as usize)
            }
        }
        TValue::LClosure(c) => {
            if let Some(id) = c.gc_header.id() {
                !gc.is_white(id)
            } else {
                gc.aux_is_marked(c.gc_header.ptr_id as usize)
            }
        }
        TValue::UserData(u) => {
            if let Some(id) = u.gc_header.id() {
                !gc.is_white(id)
            } else {
                gc.aux_is_marked(u.gc_header.ptr_id as usize)
            }
        }
        TValue::RustClosure(rc) => gc.aux_is_marked(rc.as_raw_ptr() as usize),
        TValue::Thread(t) => gc.aux_is_marked(Rc::as_ptr(&t.context) as usize),
        TValue::CClosure(cc) => gc.aux_is_marked(cc.as_raw_ptr() as usize),
        _ => true,
    }
}

// ============================================================================
// GCState — 全局 GC 状态
// ============================================================================

#[derive(Default)]
struct GcObjects<'a> {
    metas: Vec<Option<GCObjectMeta<'a>>>,
    free_ids: Vec<u32>,
    /// 活跃对象 ID 列表 — sweep 时只遍历此列表而非整个 metas 数组。
    /// register_object 时 push，sweep_unreachable 时重建（只保留可达对象）。
    all_objects: Vec<u32>,
}

pub struct GCState<'a> {
    pub phase: Cell<GCPhase>,
    pub mode: Cell<GCMode>,
    pub current_white: Cell<u8>,
    next_id: Cell<u32>,
    /// 对象存储 — metas + free_ids + all_objects 合并为单个 RefCell,
    /// register_object 每次只做 1 次 borrow (原 3 次)。
    objects: RefCell<GcObjects<'a>>,
    gray: RefCell<VecDeque<GCObjectId>>,
    gray_again: RefCell<VecDeque<GCObjectId>>,
    weak: RefCell<VecDeque<GCObjectId>>,
    all_weak: RefCell<VecDeque<GCObjectId>>,
    ephemeron: RefCell<VecDeque<GCObjectId>>,
    /// 无 GC ID 对象（CClosure/RustClosure/Thread）的辅助标记集合。
    /// 跨 GC 周期复用：`enter_cycle` 只 `clear()` 保留容量，GC 路径零 malloc。
    aux_marked: RefCell<HashSet<usize, FxBuildHasher>>,
    pub gc_debt: Cell<isize>,
    pub gc_estimate: Cell<usize>,
    /// 无 gc_header 对象（String/Proto/CClosure/RustClosure/LuaThread）的估算占用。
    /// 这些对象不调用 register_object，无法通过 gc_estimate 跟踪。
    /// 创建后由 collect_gc 通过 set_extra_estimate 重算（精确）。
    pub gc_extra_estimate: Cell<usize>,
    pub gc_stop: Cell<u8>,
    pub gc_params: [Cell<i32>; 6],
    next_collect_threshold: Cell<usize>,
    pub step_accum: Cell<usize>,
    /// GC 是否正在运行（collect_gc 进行中）— 用于阻止 finalizer 中重入
    pub gc_running: Cell<bool>,
    /// 紧急回收模式 —— 对应 C 的 `global_State.gcemergency`（lgc.c::luaC_fullgc
    /// 的 `isemergency` 参数）：内存分配失败后的回收必须**避免改变解释器状态**，
    /// 即不运行 finalizer（可能执行任意 Lua 代码并再次分配）、不缩栈。
    /// 该模式下需要 finalize 的对象仍会被标活（避免被扫掉），但延后到下一次
    /// 常规回收，与 C 中「紧急模式不把对象移入 tobefnz」一致。
    pub gcemergency: Cell<bool>,
    /// 所有 Lua 协程对象的弱引用 —— 对应 C 的 `g->allgc` 里 thread 链路。
    ///
    /// 线程没有 `gc_header`（无 GCObjectId），可达性由 `aux_marked` 记录，
    /// 键是 `Rc::as_ptr(&thread.context)`（见 `mark_tvalue` 的 Thread 分支）。
    /// 因为线程进不了 register_object/sweep 通道，必须在每次回收末尾用
    /// [`GCState::clear_dead_threads`] 单独清理：不可达线程的 `context` 换回
    /// 默认值 —— 这会 drop 协程的执行态（`Box<LuaState>` 及其栈/upvalue），
    /// 从而打断「Thread -> context.exec -> stack/upvalue -> Thread」自引用环。
    /// 否则该环由 Rc 计数自持，永不回收（C Lua 的 trace GC 能直接回收线程）。
    /// 主线程与一次性临时线程不登记。
    threads: RefCell<Vec<WeakRc<LuaThread<'a>, SlotAlloc>>>,
}

impl<'a> GCState<'a> {
    pub fn new(mode: GCMode) -> Self {
        let mut aux = HashSet::with_hasher(FxBuildHasher::default());
        aux.reserve(256);
        GCState {
            phase: Cell::new(GCPhase::Pause),
            mode: Cell::new(mode),
            current_white: Cell::new(1 << 0),
            next_id: Cell::new(1),
            objects: RefCell::new(GcObjects::default()),
            gray: RefCell::new(VecDeque::with_capacity(256)),
            gray_again: RefCell::new(VecDeque::with_capacity(64)),
            weak: RefCell::new(VecDeque::with_capacity(64)),
            all_weak: RefCell::new(VecDeque::with_capacity(64)),
            ephemeron: RefCell::new(VecDeque::with_capacity(64)),
            aux_marked: RefCell::new(aux),
            gc_debt: Cell::new(0),
            gc_estimate: Cell::new(0),
            gc_extra_estimate: Cell::new(0),
            gc_stop: Cell::new(0),
            gc_params: [
                Cell::new(0),
                Cell::new(0),
                Cell::new(0),
                Cell::new(0),
                Cell::new(0),
                Cell::new(0),
            ],
            next_collect_threshold: Cell::new(200000),
            step_accum: Cell::new(0),
            gc_running: Cell::new(false),
            gcemergency: Cell::new(false),
            threads: RefCell::new(Vec::new()),
        }
    }

    pub fn default_incremental() -> Self {
        let gc = GCState::new(GCMode::Incremental);
        gc.set_gc_param(Self::PARAM_PAUSE, 200);
        gc.set_gc_param(Self::PARAM_STEPMUL, 200);
        gc.set_gc_param(Self::PARAM_STEPSIZE, 100);
        gc
    }

    /// GC 参数索引（与 C 的 LUA_GCP* 一致）
    pub const PARAM_MINORMUL: usize = 0;
    pub const PARAM_MAJORMINOR: usize = 1;
    pub const PARAM_MINORMAJOR: usize = 2;
    pub const PARAM_PAUSE: usize = 3;
    pub const PARAM_STEPMUL: usize = 4;
    pub const PARAM_STEPSIZE: usize = 5;

    /// 切换 GC 模式，返回之前的模式
    pub fn set_mode(&self, mode: GCMode) -> GCMode {
        let old = self.mode.get();
        self.mode.set(mode);
        old
    }

    fn set_gc_param(&self, idx: usize, val: i32) {
        if idx < self.gc_params.len() {
            self.gc_params[idx].set(val);
        }
    }

    /// 查询 GC 参数，返回当前值
    pub fn get_gc_param(&self, idx: usize) -> i32 {
        if idx < self.gc_params.len() {
            self.gc_params[idx].get()
        } else {
            0
        }
    }

    /// 设置 GC 参数，返回之前的值
    pub fn swap_gc_param(&self, idx: usize, val: i32) -> i32 {
        if idx < self.gc_params.len() {
            let old = self.gc_params[idx].get();
            self.gc_params[idx].set(val);
            old
        } else {
            0
        }
    }

    // ========================================================================
    // 对象注册/注销
    // ========================================================================

    /// 登记一个 GC 对象 —— **可失败**（对象表/活跃表扩容失败时返回 `None`，
    /// 由调用方走「紧急 GC + 重试 → 内存错误」漏斗）。
    ///
    /// 为什么必须可失败：对象表（`metas` / `all_objects`）随注册按几何增长，
    /// 活跃对象上百万时需要几十 MB 的重新分配；此处若直接 abort（`Vec::push` 默认
    /// 行为），内存耗尽时就再也报不出内存错误 —— 200MB 限制下实测正是这一条先崩。
    /// 对应 C：C 的 GC 不随对象数分配内存（对象自带 allgc 链表），故无此问题。
    pub fn try_register_object(
        &self,
        size: usize,
        weak_value: WeakValue<'a>,
    ) -> Result<GCObjectId, WeakValue<'a>> {
        let mut meta = GCObjectMeta::new(size);
        meta.color = Self::new_object_color(self.current_white.get());
        meta.age = GCAge::New;
        meta.weak_value = weak_value;

        let mut objs = self.objects.borrow_mut();
        // 先备容量、再写入：失败时对象表保持原样（不留下半个条目）
        if objs.metas.len() + 1 > objs.metas.capacity() {
            if objs.metas.try_reserve(1).is_err() {
                return Err(meta.weak_value);
            }
        }
        if objs.all_objects.len() + 1 > objs.all_objects.capacity() {
            if objs.all_objects.try_reserve(1).is_err() {
                return Err(meta.weak_value);
            }
        }

        // 优先重用已释放的 ID 槽位，避免 metas 数组无限增长
        let id = if let Some(free_id) = objs.free_ids.pop() {
            GCObjectId(free_id)
        } else {
            let id_val = self.next_id.get();
            self.next_id.set(id_val + 1);
            while objs.metas.len() <= id_val as usize {
                objs.metas.push(None);
            }
            GCObjectId(id_val)
        };
        objs.metas[id.0 as usize] = Some(meta);
        objs.all_objects.push(id.0);

        let current = self.gc_debt.get();
        let charged = size.max(32) as isize;
        self.gc_debt.set(current - charged);
        let estimate = self.gc_estimate.get();
        self.gc_estimate.set(estimate + charged as usize);

        Ok(id)
    }

    /// 新建对象的初始颜色（与 `try_register_object` 共用）。
    #[inline]
    fn new_object_color(cw: u8) -> GCColor {
        if cw & 1 != 0 {
            GCColor::White0
        } else {
            GCColor::White1
        }
    }

    /// 登记一张表 —— 可失败（见 [`GCState::try_register_object`]）。
    pub fn try_register_table(&self, t: &Table<'a>) -> Result<(), crate::mem::AllocFailed> {
        // 幂等：同一张表（同一 TableData）只能注册一次。
        // 重复注册会给同一数据分配第二个 ID，旧 ID 永远保持白色 ——
        // mark 只把新 ID 染灰，sweep 见到旧 ID 白色就 clear_for_drop 这张
        // 仍然可达的表（package.loaded 曾被 registry._LOADED 二次注册清空）。
        if t.data.borrow().gc_header.id().is_some() {
            return Ok(());
        }
        let id = self
            .try_register_object(t.mem_size(), WeakValue::Table(t.downgrade()))
            .map_err(|_| crate::mem::AllocFailed)?;
        t.data.borrow().gc_header.set_id(id);
        Ok(())
    }

    pub(crate) fn meta(&self, id: GCObjectId) -> Option<std::cell::Ref<'_, GCObjectMeta<'a>>> {
        let objs = self.objects.borrow();
        if (id.0 as usize) < objs.metas.len() && objs.metas[id.0 as usize].is_some() {
            Some(std::cell::Ref::map(objs, |o| {
                o.metas[id.0 as usize].as_ref().unwrap()
            }))
        } else {
            None
        }
    }

    pub(crate) fn meta_mut(
        &self,
        id: GCObjectId,
    ) -> Option<std::cell::RefMut<'_, GCObjectMeta<'a>>> {
        let objs = self.objects.borrow_mut();
        if (id.0 as usize) < objs.metas.len() && objs.metas[id.0 as usize].is_some() {
            Some(std::cell::RefMut::map(objs, |o| {
                o.metas[id.0 as usize].as_mut().unwrap()
            }))
        } else {
            None
        }
    }

    /// 返回实际存活（非 None）的对象数 — O(1) 直接读 all_objects 长度
    pub fn active_count(&self) -> usize {
        self.objects.borrow().all_objects.len()
    }

    /// 返回当前 GC 触发阈值
    pub fn collect_threshold(&self) -> usize {
        self.next_collect_threshold.get()
    }

    /// 设置 GC 触发阈值
    pub fn set_collect_threshold(&self, threshold: usize) {
        self.next_collect_threshold.set(threshold);
    }

    // ------------------------------------------------------------------------
    // 无 gc_header 对象的计费
    // ------------------------------------------------------------------------

    /// 设置 extra estimate（collect_gc 后重算使用）。
    pub fn set_extra_estimate(&self, size: usize) {
        self.gc_extra_estimate.set(size);
    }

    /// 返回总估算占用 = gc_estimate（已注册对象）+ gc_extra_estimate（无 gc_header 对象）。
    /// collectgarbage("count") 和 GC 触发阈值应基于此值。
    pub fn total_estimate(&self) -> usize {
        self.gc_estimate
            .get()
            .saturating_add(self.gc_extra_estimate.get())
    }

    // ------------------------------------------------------------------------
    // 无 ID 对象的辅助标记集合（跨周期复用）
    // ------------------------------------------------------------------------

    /// 对无 ID 对象做标记；返回 true 表示首次标记。
    #[inline]
    pub(crate) fn aux_mark(&self, ptr: usize) -> bool {
        self.aux_marked.borrow_mut().insert(ptr)
    }

    #[inline]
    pub(crate) fn aux_is_marked(&self, ptr: usize) -> bool {
        self.aux_marked.borrow().contains(&ptr)
    }

    /// 登记新建的 Lua 协程对象（Lua 层 `coroutine.create` / `coroutine.wrap`）。
    ///
    /// 见 [`GCState::threads`] 字段说明：线程靠 `aux_marked` 判活，
    /// 必须登记才能被 [`GCState::clear_dead_threads`] 清理。
    pub fn register_thread(&self, thread: &MyRc<LuaThread<'a>, SlotAlloc>) {
        self.threads.borrow_mut().push(thread.downgrade());
    }

    /// 清理不可达协程的执行态 —— 打断「Thread -> context.exec -> ... -> Thread」自引用环。
    ///
    /// 必须在标记阶段结束后的同一周期内调用：`aux_marked` 中不含该线程的
    /// `context` 指针即为不可达（判定口径与 `mark_tvalue` 的 Thread 分支一致）。
    /// 不可达线程把 `context` 换回默认值，drop 掉其执行态与栈上对象；
    /// 环内的强引用随后归零，线程对象由 Rc 释放。
    pub(crate) fn clear_dead_threads(&self) {
        let mut list = self.threads.borrow_mut();
        list.retain(|weak| {
            let thread = match weak.upgrade() {
                Some(t) => t,
                None => return false, // 线程已释放：移除登记
            };
            if self.aux_is_marked(Rc::as_ptr(&thread.context) as usize) {
                true
            } else {
                thread.context.replace(ThreadContext::default());
                false
            }
        });
    }

    // ------------------------------------------------------------------------
    // 清扫 —— 无参，直接读颜色
    // ------------------------------------------------------------------------

    /// 清扫不可达对象 —— **不再接收外部 reachable 集合**。
    ///
    /// 标记阶段结束后的不变式：所有可达对象已是黑/灰，所有垃圾仍是当前白。
    /// 因此 `meta.color.is_white(current_white)` 一行即为生死判决。
    pub fn sweep_unreachable(&self) {
        let cw = self.current_white.get();
        let mut objs = self.objects.borrow_mut();
        let mut total_freed_size = 0usize;

        let mut write = 0usize;
        for read in 0..objs.all_objects.len() {
            let id = objs.all_objects[read];
            let i = id as usize;

            let is_dead = match objs.metas[i].as_ref() {
                Some(m) => m.color.is_white(cw),
                None => false,
            };

            if is_dead {
                if let Some(ref meta) = objs.metas[i] {
                    total_freed_size = total_freed_size.saturating_add(meta.size.max(32));
                    match meta.weak_value {
                        WeakValue::Table(ref weak_table) => {
                            if let Some(upgrade) = weak_table.upgrade() {
                                upgrade.clear_for_drop();
                            }
                        }
                        WeakValue::LClosure(ref closure) => {
                            if let Some(upgrade) = closure.upgrade() {
                                upgrade.upvals.replace(UpValVec::default());
                            };
                        }
                        WeakValue::CClosure(ref weak_rc) => {
                            if let Some(upgrade) = weak_rc.upgrade() {
                                upgrade.upvalue.borrow_mut().clear();
                            }
                        }
                        WeakValue::RustClosure(ref weak_rc) => {
                            if let Some(upgrade) = weak_rc.upgrade() {
                                upgrade.upvalues.borrow_mut().clear();
                            }
                        }
                        WeakValue::UserData(ref weak_rc) => {
                            if let Some(upgrade) = weak_rc.upgrade() {
                                upgrade.metatable.replace(None);
                            }
                        }
                        WeakValue::Thread(ref weak_rc) => {
                            if let Some(upgrade) = weak_rc.upgrade() {
                                upgrade.context.replace(ThreadContext::default());
                            }
                        }
                        _ => {}
                    }
                }
                objs.metas[i] = None;
                // 记录空闲槽位（可失败）：容量不足时放弃记录 —— 该槽位保持 None
                // 且不可达，仅失去复用机会，不会泄漏用户内存，更不能因此 abort。
                if objs.free_ids.len() + 1 <= objs.free_ids.capacity()
                    || objs.free_ids.try_reserve(1).is_ok()
                {
                    objs.free_ids.push(id);
                }
            } else {
                objs.all_objects[write] = id;
                write += 1;
            }
        }
        objs.all_objects.truncate(write);
        let estimate = self.gc_estimate.get();
        self.gc_estimate
            .set(estimate.saturating_sub(total_freed_size));
    }

    // ========================================================================
    // 颜色操作
    // ========================================================================

    pub fn set_color(&self, id: GCObjectId, color: GCColor) {
        if let Some(mut meta) = self.meta_mut(id) {
            meta.color = color;
        }
    }

    pub fn is_white(&self, id: GCObjectId) -> bool {
        let cw = self.current_white.get();
        self.meta(id).map(|m| m.color.is_white(cw)).unwrap_or(false)
    }

    pub fn is_black(&self, id: GCObjectId) -> bool {
        self.meta(id).map(|m| m.color.is_black()).unwrap_or(false)
    }

    // ========================================================================
    // 灰色链表操作
    // ========================================================================

    pub fn make_gray(&self, id: GCObjectId) {
        if let Some(mut meta) = self.meta_mut(id) {
            if meta.color.is_gray() {
                return;
            }
            meta.color = GCColor::Gray;
        }
        self.gray.borrow_mut().push_back(id);
    }

    pub fn pop_gray(&self) -> Option<GCObjectId> {
        self.gray.borrow_mut().pop_front()
    }

    pub fn make_gray_again(&self, id: GCObjectId) {
        self.gray_again.borrow_mut().push_back(id);
    }

    pub fn pop_gray_again(&self) -> Option<GCObjectId> {
        self.gray_again.borrow_mut().pop_front()
    }

    pub fn gray_is_empty(&self) -> bool {
        self.gray.borrow().is_empty()
    }

    // ========================================================================
    // 写屏障 (Write Barriers)
    // ========================================================================

    /// 后向写屏障 (luaC_barrierback_)
    /// 当黑色对象 p 被写入时，将其加入 grayagain 队列
    pub fn barrier_back(&self, p: GCObjectId) {
        if self.is_black(p) {
            if self.mode.get() == GCMode::Generational {
                if let Some(mut meta) = self.meta_mut(p) {
                    if meta.age == GCAge::Old || meta.age == GCAge::Touched2 {
                        meta.age = GCAge::Touched1;
                    }
                }
            }
            self.make_gray_again(p);
            // 去重 (等效 C 的 grayagain 语义): 置灰后 is_black 为假, 同一对象不会被
            // 重复入队 —— 否则 gray_again 长度只受 write barrier 次数限制 (无界),
            // 其预留会失效并在增长时经全局分配器 abort。
            self.set_color(p, GCColor::Gray);
        }
    }

    /// 对象后向写屏障: if p is black and o is white → barrier_back(p)
    pub fn obj_barrier_back(&self, p: GCObjectId, o: GCObjectId) {
        if self.is_black(p) && self.is_white(o) {
            self.barrier_back(p);
        }
    }

    // ========================================================================
    // GC 不变式
    // ========================================================================

    pub fn is_running(&self) -> bool {
        self.gc_stop.get() == 0
    }

    /// GC 是否正在执行 collect_gc（用于阻止 finalizer 中重入）
    pub fn is_gc_running(&self) -> bool {
        self.gc_running.get()
    }

    // ========================================================================
    // GC 循环控制
    // ========================================================================

    pub fn cond_gc(&mut self) {
        // 增量 step 机器无根标记 (enter_cycle 不标记根, sweep 为空), 实际回收由
        // maybe_collect_gc (阈值触发 full collect_gc) 完成。这里仅在确有 gray 工作
        // 或机器处于周期中途时推进, 避免每次分配都空转 phase 机器 (~130ns/次)。
        if self.gc_debt.get() <= 0 {
            let phase = self.phase.get();
            if phase != GCPhase::Pause || !self.gray_is_empty() {
                self.step();
            }
        }
    }

    pub fn step(&mut self) {
        if !self.is_running() {
            return;
        }

        match self.phase.get() {
            GCPhase::Pause => self.enter_cycle(),
            GCPhase::Propagate => self.propagate_one(),
            GCPhase::EnterAtomic => {
                self.phase.set(GCPhase::Atomic);
                self.enter_atomic();
            }
            GCPhase::Atomic => self.enter_sweep(),
            GCPhase::SweepAllGC => self.sweep_step(),
            GCPhase::SweepEnd => self.end_cycle(),
        }
    }

    // ========================================================================
    // 标记阶段
    // ========================================================================

    /// 进入新周期：翻转 white、清空所有工作队列（含 aux_marked）。
    ///
    /// `pub(crate)`：供 `LuaState::collect_gc_inner` 手动进入完整 GC 周期。
    /// GC 内部队列容量预留（见 `LuaState::prepare_gc_queues`）；任一失败即返回 false。
    pub(crate) fn reserve_queues(&self, active: usize) -> bool {
        let plan: [(&RefCell<VecDeque<GCObjectId>>, usize); 5] = [
            (&self.gray, active),
            (&self.gray_again, active / 4 + 64),
            (&self.weak, active / 8 + 64),
            (&self.all_weak, active / 8 + 64),
            (&self.ephemeron, active / 8 + 64),
        ];
        for (q, want) in plan {
            let mut q = q.borrow_mut();
            let need = want.saturating_sub(q.len());
            if q.capacity() < q.len() + need && q.try_reserve(need).is_err() {
                return false;
            }
        }
        true
    }

    pub(crate) fn enter_cycle(&self) {
        let cw = self.current_white.get();
        let new_cw = cw ^ 0x3;
        self.current_white.set(new_cw);

        let new_color = if new_cw & 1 != 0 {
            GCColor::White0
        } else {
            GCColor::White1
        };
        {
            let mut objs = self.objects.borrow_mut();
            let GcObjects {
                metas, all_objects, ..
            } = &mut *objs;
            for &id in all_objects.iter() {
                let i = id as usize;
                if let Some(ref mut m) = metas[i] {
                    m.color = new_color;
                }
            }
        }
        self.gray.borrow_mut().clear();
        self.gray_again.borrow_mut().clear();
        self.weak.borrow_mut().clear();
        self.all_weak.borrow_mut().clear();
        self.ephemeron.borrow_mut().clear();
        self.aux_marked.borrow_mut().clear();

        self.phase.set(GCPhase::Propagate);
        self.update_debt(0);
    }

    fn propagate_one(&self) {
        if let Some(id) = self.pop_gray() {
            self.set_color(id, GCColor::Black);
        } else {
            self.phase.set(GCPhase::EnterAtomic);
        }
    }

    fn enter_atomic(&self) {
        while let Some(id) = self.pop_gray_again() {
            self.make_gray(id);
            while let Some(gid) = self.pop_gray() {
                self.set_color(gid, GCColor::Black);
            }
        }
        self.process_weak_tables();
        self.enter_sweep();
    }

    fn process_weak_tables(&self) {
        // step 机器路径下弱表清理由 collect_gc_inner 完成
    }

    // ========================================================================
    // 清扫阶段
    // ========================================================================

    fn enter_sweep(&self) {
        self.phase.set(GCPhase::SweepAllGC);
    }

    fn sweep_step(&mut self) {
        self.phase.set(GCPhase::SweepEnd);
    }

    fn end_cycle(&self) {
        let cw = self.current_white.get();
        self.current_white.set(cw ^ 0x3);

        if self.mode.get() == GCMode::Generational {
            self.bump_ages();
        }

        self.phase.set(GCPhase::Pause);
        self.update_debt(0);
    }

    fn bump_ages(&self) {
        let mut objs = self.objects.borrow_mut();
        for meta_opt in objs.metas.iter_mut() {
            if let Some(ref mut meta) = meta_opt {
                match meta.age {
                    GCAge::New => meta.age = GCAge::Survival,
                    GCAge::Survival => meta.age = GCAge::Old1,
                    GCAge::Old1 => meta.age = GCAge::Old,
                    GCAge::Touched1 => meta.age = GCAge::Touched2,
                    GCAge::Touched2 => meta.age = GCAge::Old,
                    GCAge::Old => {}
                }
            }
        }
    }

    // ========================================================================
    // 债务管理
    // ========================================================================

    fn update_debt(&self, _debt: isize) {
        let step_size = self
            .gc_params
            .get(Self::PARAM_STEPSIZE)
            .map(|c| c.get())
            .unwrap_or(100) as isize;
        self.gc_debt.set(step_size);
    }

    pub fn set_debt(&self, debt: isize) {
        self.gc_debt.set(debt);
    }

    // ========================================================================
    // 对象大小
    // ========================================================================

    pub fn set_obj_size(&self, id: GCObjectId, size: usize) {
        if let Some(mut meta) = self.meta_mut(id) {
            // 用 .max(32) 保持与 register_object/sweep_unreachable 一致的 charge 口径
            // 否则当 old < 32 时，register 收取 32 但此处只按 old 调整，导致 gc_estimate 虚高
            let old_charged = meta.size.max(32);
            let new_charged = size.max(32);
            meta.size = size;
            let estimate = self.gc_estimate.get();
            self.gc_estimate.set(estimate + new_charged - old_charged);
        }
    }

    // ========================================================================
    // 标记
    // ========================================================================

    pub fn mark_object(&self, id: GCObjectId) {
        if self.is_white(id) {
            self.make_gray(id);
        }
    }
}

// ============================================================================
// impl LuaState — GC 入口 / 标记 / 弱表 / finalizer
// ============================================================================
//
// 全部 GC 逻辑集中在 gc.rs，state.rs 不保留任何 GC 实现。

impl<'a> LuaState<'a> {
    /// 紧急全量回收 —— 对应 C lmem.c `tryagain` 里的 `luaC_fullgc(L, 1)`。
    ///
    /// 分配失败（且 `cantryagain(g)`）时由 [`LuaState::retry_alloc`] 调用：
    /// 尽力释放内存，让调用方重试分配。紧急模式（`gcemergency`）下
    /// 不运行 finalizer、不改变解释器状态（见 [`GCState::gcemergency`]）。
    pub fn emergency_gc(&mut self) {
        // 状态未建好 / GC 进行中 / 已处于紧急模式时不做（C: cantryagain）。
        if !self.can_retry_alloc() || unsafe { self.gc_mut().gcemergency.get() } {
            return;
        }
        unsafe { self.gc_mut().gcemergency.set(true) };
        self.collect_gc();
        unsafe { self.gc_mut().gcemergency.set(false) };
    }

    /// 登记 GC 对象漏斗 —— 对象表扩容失败先紧急 GC 再重试，仍失败即内存错误。
    ///
    /// 对象创建路径必须用它（对应 C 的 `luaC_newobj` 不失败；Rust 侧对象表要增长，
    /// 故与其它分配点一样走「先 GC 再重试」漏斗，绝不 abort）。
    pub fn register_object(
        &mut self,
        size: usize,
        weak_value: crate::gc::WeakValue<'a>,
    ) -> Result<crate::gc::GCObjectId, VmError<'a>> {
        match unsafe { self.gc_mut() }.try_register_object(size, weak_value) {
            Ok(id) => Ok(id),
            Err(wv) => {
                if self.can_retry_alloc() {
                    self.emergency_gc();
                    if let Ok(id) = unsafe { self.gc_mut() }.try_register_object(size, wv) {
                        return Ok(id);
                    }
                }
                Err(VmError::MemoryError)
            }
        }
    }

    /// 登记表漏斗（见 [`LuaState::register_object`]）。
    pub fn register_table(&mut self, t: &crate::table::Table<'a>) -> Result<(), VmError<'a>> {
        match unsafe { self.gc_mut() }.try_register_table(t) {
            Ok(()) => Ok(()),
            Err(_) => {
                if self.can_retry_alloc() {
                    self.emergency_gc();
                    if unsafe { self.gc_mut() }.try_register_table(t).is_ok() {
                        return Ok(());
                    }
                }
                Err(VmError::MemoryError)
            }
        }
    }

    /// 完整 GC —— 对应 C 的 `luaC_fullgc`。
    pub fn collect_gc(&mut self) {
        // 不可重入：finalizer 运行期间（call_finalizers -> pcall -> 用户代码）的分配
        // 可能经 maybe_collect_gc / concat_gc_check / step_gc 再次进入。
        // 重入会再次翻转 white 并清空标记队列（enter_cycle），使外层周期已完成的标记
        // 全部作废 —— 外层随后的 clear_weak_tables / sweep_unreachable 将基于被破坏的
        // 颜色状态，可能误清活对象。收集中的分配由本次 GC 收尾时的阈值重设覆盖。
        if unsafe { self.gc_mut().is_gc_running() } {
            return;
        }
        let gc_start = if gc_stats_enabled() {
            Some(std::time::Instant::now())
        } else {
            None
        };
        // 标记 GC 正在运行 —— 阻止 finalizer 中重入 collectgarbage("collect")
        unsafe { self.gc_mut().gc_running.set(true) };
        // 临时禁用 hook
        let saved_allowhook = self.allowhook;
        self.allowhook = false;

        // 先备好 GC 内部队列（**可失败**）：拿不到就跳过本次回收。
        if !self.prepare_gc_queues() {
            unsafe { self.gc_mut().gc_running.set(false) };
            self.allowhook = saved_allowhook;
            return;
        }

        // 先备好标记工作栈（**可失败**）：拿不到就跳过本次回收。
        // 绝不在此 abort —— 紧急回收路径正是内存耗尽时被调用的，若它自己再失败
        // 就会崩溃而不是把内存错误交给调用方（对应 C 的回收不需要新分配）。
        if !self.prepare_gc_worklist() {
            unsafe { self.gc_mut().gc_running.set(false) };
            self.allowhook = saved_allowhook;
            return;
        }

        let prev_estimate = unsafe { self.gc_mut().total_estimate() };
        let mut worklist = std::mem::take(&mut self.gc_worklist);
        worklist.clear();
        let result = self.collect_gc_inner(&mut worklist);
        worklist.clear();
        self.gc_worklist = worklist;

        self.allowhook = saved_allowhook;
        unsafe { self.gc_mut().gc_running.set(false) };

        let cur_estimate = unsafe { self.gc_mut().total_estimate() };
        if prev_estimate > cur_estimate + 1024 * 1024 {
            #[cfg(target_os = "linux")]
            unsafe {
                libc::malloc_trim(0);
            }
        }
        self.g_mut().last_gc_estimate = cur_estimate;
        if let Some(start) = gc_start {
            record_gc_time(start.elapsed());
        }
        result
    }

    /// 备好本周期需要的 GC 内部队列容量（**进入周期前**调用）。
    ///
    /// 标记过程中队列的 `push_back` 会按几何增长 —— 内存耗尽时那就是 abort。
    /// 与 worklist 同理：先在安全点（进入周期前）把容量谈妥，谈不妥就跳过本次回收。
    /// 上界：gray ⊆ 可达对象数；其余队列远小于该上界（取 1/4、1/8 留裕量）。
    fn prepare_gc_queues(&mut self) -> bool {
        let active = unsafe { self.gc_mut().active_count() };
        unsafe { self.gc_mut() }.reserve_queues(active)
    }

    /// 备好本周期需要的 worklist 容量（跨周期复用，容量不足时 `try_reserve`）。
    ///
    /// 返回 false 表示容量不足且无法分配 —— 调用方跳过本次回收（内存耗尽时的
    /// 紧急回收会因此把「内存错误」留给调用方，而不是 abort 进程）。
    fn prepare_gc_worklist(&mut self) -> bool {
        let active = unsafe { self.gc_mut().active_count() };
        // 与旧的 `Vec::with_capacity((active*3/5).max(64) * 2)` 取同一上界
        let need = (active * 3 / 5).max(64) * 2;
        let have = self.gc_worklist.capacity();
        if have >= need {
            return true;
        }
        let r = self.gc_worklist.try_reserve(need - have).is_ok();
        if !r {}
        r
    }

    /// 阈值触发 GC（对应原 `maybe_collect_gc`）。
    pub fn maybe_collect_gc(&mut self) {
        let (running, over) = unsafe {
            let g = self.gc_mut();
            (g.is_running(), g.total_estimate() > g.collect_threshold())
        };
        if running && over {
            gc_trigger_inc(GcTrigger::Maybe);
            self.collect_gc();
        }
    }

    /// 字符串拼接压力触发（对应原 `concat_gc_check`）。
    pub fn concat_gc_check(&mut self) {
        let cnt = self.concat_gc_counter.get() + 1;
        if cnt >= self.concat_gc_interval.get() {
            gc_trigger_inc(GcTrigger::Concat);
            self.collect_gc();
            self.concat_gc_counter.set(0);
        } else {
            self.concat_gc_counter.set(cnt);
        }
    }

    /// 增量 step（对应原 `step_gc`）。
    pub fn step_gc(&mut self, siz: usize) -> bool {
        if siz == 0 {
            gc_trigger_inc(GcTrigger::Step);
            self.collect_gc();
            unsafe { self.gc_mut().step_accum.set(0) };
            true
        } else {
            let acc = unsafe { self.gc_mut().step_accum.get() } + siz;
            let threshold = unsafe { self.gc_mut().active_count().max(1) };
            if acc >= threshold {
                gc_trigger_inc(GcTrigger::Step);
                self.collect_gc();
                unsafe { self.gc_mut().step_accum.set(0) };
                true
            } else {
                unsafe { self.gc_mut().step_accum.set(acc) };
                false
            }
        }
    }

    /// 完整 GC 实现 —— 标记、ephemeron 收敛、finalizer、弱表清理、sweep。
    fn collect_gc_inner(&mut self, worklist: &mut Vec<RawTValue<'a>>) {
        let active = unsafe { self.gc_mut().active_count() };
        if gc_stats_enabled() {
            reset_gc_type_stats();
        }

        // 进入新周期：翻转 white、清空所有队列（含 aux_marked）
        unsafe { self.gc_mut().enter_cycle() };

        let mut extra_size: usize = 0;

        // 根：本线程
        self.collect_roots_into(worklist);

        // 根：globals / registry
        {
            let tv = TValue::Table(self.globals.clone());
            unsafe { worklist.push(raw_from_tvalue(&tv)) };
            drop(tv);
        }
        {
            let tv = TValue::Table(self.registry.clone());
            unsafe { worklist.push(raw_from_tvalue(&tv)) };
            drop(tv);
        }
        // 根：内存错误消息（C 的 G(L)->memerrmsg 同样是 GC 根 —— 否则长字符串
        // 形态的消息会被回收，内存错误路径再取到悬垂/已清对象）
        if let Some(mm) = self.g().memerrmsg.as_ref() {
            if needs_gc_mark(mm) {
                unsafe { worklist.push(raw_from_tvalue(mm)) };
            }
        }
        {
            let file_handles = self.file_handles.borrow();
            match self.io_input_handle.get() {
                Some(handle) => {
                    let tv_opt = file_handles.get(&handle);
                    if let Some(tv) = tv_opt {
                        unsafe { worklist.push(raw_from_tvalue(tv)) };
                    }
                }
                _ => {}
            }
            match self.io_output_handle.get() {
                Some(handle) => {
                    let tv_opt = file_handles.get(&handle);
                    if let Some(tv) = tv_opt {
                        unsafe { worklist.push(raw_from_tvalue(tv)) };
                    }
                }
                _ => {}
            }
            for handle in self.popen_handles.borrow().iter() {
                let tv_opt = file_handles.get(handle);
                if let Some(tv) = tv_opt {
                    unsafe { worklist.push(raw_from_tvalue(tv)) };
                }
            }
        }

        // 根：基本类型的默认元表 (dmt) — 对应 C 的 markmt(g) 遍历 g->mt[]。
        // 这些表只被 dmt 强引用 (不挂在 globals/registry 上)，漏标会让 sweep
        // 对其 clear_for_drop —— 字符串的算术元方法、io 的 FILE* 元表随即失效。
        for mt in self.g().dmt.iter_tables() {
            let tv = TValue::Table(mt.clone());
            unsafe { worklist.push(raw_from_tvalue(&tv)) };
            drop(tv);
        }

        // 根：主线程
        let main_ptr = self.g().main;
        let self_ptr = self as *const LuaState<'a> as *mut LuaState<'a>;
        if !main_ptr.is_null() && main_ptr != self_ptr {
            unsafe { (*main_ptr).collect_roots_into(worklist) };
        }
        // 主线程对象自身的 function
        let main_thread = self.main_thread.clone();
        self.collect_thread_roots(&main_thread, worklist);

        // 标记阶段
        while let Some(raw) = worklist.pop() {
            let val = unsafe { raw_as_tvalue(&raw) };
            self.mark_tvalue(val, worklist, &mut extra_size);
        }

        // ephemeron 传递性
        self.process_ephemerons(worklist, &mut extra_size);

        // 紧急回收（内存耗尽后的重试）：不运行 finalizer —— 它会执行任意 Lua 代码
        // 并可能再次分配（C 同样在 gcemergency 下跳过 finalizer 的分离与执行）。
        let emergency = unsafe { self.gc_mut().gcemergency.get() };

        // finalizer 复活
        let to_finalize = if emergency {
            self.collect_finalizers_deferred(worklist, &mut extra_size);
            Vec::new()
        } else {
            self.collect_finalizers(worklist, &mut extra_size)
        };

        // 复活后可能引入新 ephemeron 关系
        self.process_ephemerons(worklist, &mut extra_size);

        // 弱表清理
        self.g_mut().clear_weak_tables();

        // 执行 finalizer（紧急模式下 to_finalize 为空 → 跳过，避免无谓分配）
        if !to_finalize.is_empty() {
            self.call_finalizers(to_finalize);
        }

        // 清理不可达协程的执行态（自引用环；须在标记之后、同一周期内）
        unsafe { self.gc_mut().clear_dead_threads() };

        // 清扫（无参，读颜色）
        unsafe { self.gc_mut().sweep_unreachable() };

        // 字符串表清理
        let str_sweep_start = if gc_stats_enabled() {
            Some(std::time::Instant::now())
        } else {
            None
        };
        self.string_table.sweep();
        if let Some(start) = str_sweep_start {
            let elapsed = start.elapsed().as_micros() as u64;
            GC_TIME_STATS.with(|cell| {
                let mut s = cell.get();
                s.str_sweep_us += elapsed;
                cell.set(s);
            });
        }

        // 计费：短串遍历
        let str_for_each_start = if gc_stats_enabled() {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let nuse = self.string_table.count();
        let table_ptr = &self.string_table as *const _ as usize;
        let cached = STR_EXTRA_CACHE.with(|c| c.get());
        let short_string_count: u64;
        if cached.0 == table_ptr && cached.1 == nuse {
            extra_size += cached.2;
            short_string_count = nuse as u64;
        } else {
            let mut str_extra: usize = 0;
            let mut count: u64 = 0;
            self.string_table.for_each(|ss| {
                str_extra +=
                    std::mem::size_of::<crate::strings::ShortStringFixed>() + ss.len as usize + 16;
                count += 1;
            });
            extra_size += str_extra;
            STR_EXTRA_CACHE.with(|c| c.set((table_ptr, nuse, str_extra)));
            short_string_count = count;
        }
        if let Some(start) = str_for_each_start {
            let elapsed = start.elapsed().as_micros() as u64;
            GC_TIME_STATS.with(|cell| {
                let mut s = cell.get();
                s.str_for_each_us += elapsed;
                cell.set(s);
            });
        }
        gc_stats_inc(|s| {
            s.short_string += short_string_count;
        });

        // 重算 extra_estimate
        unsafe { self.gc_mut().set_extra_estimate(extra_size) };

        // 自适应阈值
        let pause = unsafe { self.gc_mut().get_gc_param(GCState::PARAM_PAUSE).max(1) } as usize;
        let active_after = unsafe { self.gc_mut().active_count() };
        let freed = active.saturating_sub(active_after);
        let freed_ratio = if active > 0 {
            freed as f64 / active as f64
        } else {
            1.0
        };
        let effective_pause = if freed_ratio < 0.05 {
            (pause * 2).min(1000)
        } else if freed_ratio < 0.20 {
            (pause * 3 / 2).min(1000)
        } else {
            pause
        };
        let new_threshold = unsafe { self.gc_mut().total_estimate() } * effective_pause / 100;
        unsafe {
            let g = self.gc_mut();
            g.set_collect_threshold(new_threshold);
            g.set_debt(100);
            g.step_accum.set(0);
        }

        if gc_stats_enabled() {
            // 更新 reachable_total（用 active_after 近似可达数）
            GC_STATS.with(|cell| {
                let mut s = cell.get();
                s.reachable_total = active_after as u64;
                cell.set(s);
            });
            dump_gc_stats(active);
        }
    }

    /// 收集本执行态可达的根值（栈/上值/帧闭包/hook/wrap 调用者栈）。
    pub(crate) fn collect_roots_into(&self, worklist: &mut Vec<RawTValue<'a>>) {
        // 根扫描范围（务必按 C 帧活跃与否区分）：
        // - 外部 C 帧活跃时（CClosure/LCFn，见 LuaState::c_frame_depth）：C 代码只经 C API
        //   压栈 —— `lua_pushxxx` 只增长 `stack.len()`，不推进 `state.top`，因此 [top, len)
        //   是 C 代码刚压入的**活值**（如 lua_cjson_new 在 C 函数内建到一半的模块表）。
        //   若只扫到 top，C 调用期间触发的 GC（lua_createtable / lua_pushcclosure 内的
        //   maybe_collect_gc）会把这些值当垃圾清扫（sweep 对死表 clear_for_drop），
        //   随后 lua_getfield 读到 nil —— CI 上 lua-cjson `cjson.safe` 上值变 nil、
        //   skynet e2e 失败即此。
        // - 其余场合用 VM 的 top：VM 会刻意把可回收的旧值留在 top 之上
        //   （execute.rs::smart_clear_stack，仅保证 trivial 值），扫上去会让本应回收的
        //   对象继续存活（tests_lua/gc.lua 弱表断言即依赖此精度）。
        let stack_top = if self.c_frame_depth > 0 {
            self.stack.len()
        } else {
            self.top.min(self.stack.len())
        };
        for val in &self.stack[..stack_top] {
            if needs_gc_mark(val) {
                unsafe { worklist.push(raw_from_tvalue(val)) };
            }
        }

        for uv_ref in self.closure_upvals.borrow().iter() {
            let uv = uv_ref.borrow();
            match &*uv {
                UpVal::Closed { value } => {
                    if needs_gc_mark(value) {
                        unsafe { worklist.push(raw_from_tvalue(value)) };
                    }
                }
                UpVal::Open { stack_index, .. } => {
                    if *stack_index < self.stack.len() {
                        let val = &self.stack[*stack_index];
                        if needs_gc_mark(val) {
                            unsafe { worklist.push(raw_from_tvalue(val)) };
                        }
                    }
                }
            }
        }

        for frame in &self.call_stack {
            for val in &frame.constants[..] {
                if needs_gc_mark(val) {
                    unsafe { worklist.push(raw_from_tvalue(val)) };
                }
            }
            for uv_ref in frame.closure_upvals.borrow().iter() {
                let uv = uv_ref.borrow();
                if let UpVal::Closed { value } = &*uv {
                    if needs_gc_mark(value) {
                        unsafe { worklist.push(raw_from_tvalue(value)) };
                    }
                }
            }
        }

        for ci in &self.call_info {
            if let Some(closure) = &ci.closure {
                let tv = TValue::LClosure(closure.clone());
                unsafe { worklist.push(raw_from_tvalue(&tv)) };
                drop(tv);
            }
        }

        if let Some(hook) = &self.hook_func {
            if needs_gc_mark(hook) {
                unsafe { worklist.push(raw_from_tvalue(hook)) };
            }
        }

        for stack in &self.caller_gc_stacks {
            for val in stack {
                if needs_gc_mark(val) {
                    unsafe { worklist.push(raw_from_tvalue(val)) };
                }
            }
        }

        // C 安全 keepalive 区: 对应 C Lua 中失败 pcall 后仍留在栈数组里的值 ——
        // C 代码可能通过裸指针访问其数据区，故必须视为可达（否则标记阶段会清空其内容）。
        for val in &self.c_safety_keepalive {
            if needs_gc_mark(val) {
                unsafe { worklist.push(raw_from_tvalue(val)) };
            }
        }
    }

    /// 收集协程的根对象。
    pub(crate) fn collect_thread_roots(
        &self,
        thread: &LuaThread<'a>,
        worklist: &mut Vec<RawTValue<'a>>,
    ) {
        if let Some(ref func) = thread.function {
            if needs_gc_mark(func) {
                unsafe { worklist.push(raw_from_tvalue(func)) };
            }
        }
        let ctx = thread.context.borrow();
        let exec = &*ctx.exec;
        for val in &exec.stack {
            if needs_gc_mark(val) {
                unsafe { worklist.push(raw_from_tvalue(val)) };
            }
        }
        for val in &exec.constants[..] {
            if needs_gc_mark(val) {
                unsafe { worklist.push(raw_from_tvalue(val)) };
            }
        }
        for uv_ref in exec.closure_upvals.borrow().iter() {
            let uv = uv_ref.borrow();
            match &*uv {
                UpVal::Closed { value } => {
                    if needs_gc_mark(value) {
                        unsafe { worklist.push(raw_from_tvalue(value)) };
                    }
                }
                UpVal::Open { stack_index, .. } => {
                    if *stack_index < exec.stack.len() {
                        let val = &exec.stack[*stack_index];
                        if needs_gc_mark(val) {
                            unsafe { worklist.push(raw_from_tvalue(val)) };
                        }
                    }
                }
            }
        }
        for frame in &exec.call_stack {
            for val in &frame.constants[..] {
                if needs_gc_mark(val) {
                    unsafe { worklist.push(raw_from_tvalue(val)) };
                }
            }
            for uv_ref in frame.closure_upvals.borrow().iter() {
                let uv = uv_ref.borrow();
                if let UpVal::Closed { value } = &*uv {
                    if needs_gc_mark(value) {
                        unsafe { worklist.push(raw_from_tvalue(value)) };
                    }
                }
            }
        }
        if let Some(ref hook) = exec.hook_func {
            if needs_gc_mark(hook) {
                unsafe { worklist.push(raw_from_tvalue(hook)) };
            }
        }
        for ci in &exec.call_info {
            if let Some(ref closure) = ci.closure {
                let tv = TValue::LClosure(closure.clone());
                unsafe { worklist.push(raw_from_tvalue(&tv)) };
                drop(tv);
            }
        }
    }

    /// 子值入队（**带认领去重**）—— 对应 C 里 `markvalue` 的「已灰就不再入 gray 链」。
    ///
    /// 未去重的入队会让 worklist 峰值等于「可达值的**总数**」（一张百万元素表就
    /// 几百万条），内存耗尽时 `Vec::push` 只能 abort —— 200MB 限制下实测正是
    /// 标记中途的 worklist 扩容先崩。认领后峰值 ≤ 存活**对象**数，与 C 的 gray
    /// 链同级；顺带减少重复出队，标记更快。
    ///
    /// 认领键与各变体的判定口径一致（见 `mark_tvalue` / `mark_tvalue` 的 aux 键）：
    /// 表/闭包/userdata 用 `ptr_id`，CClosure/RustClosure 用自身块地址，
    /// Thread 用 `Rc::as_ptr(&context)`。
    fn push_mark(&self, _gc: &GCState, val: &TValue<'a>, worklist: &mut Vec<RawTValue<'a>>) {
        // 只做「值得标记」的过滤并入队；**去重仍由 mark_tvalue 内的 aux_mark 负责**
        // （aux_marked 同时是 is_marked 的可达性判定依据，两处必须是同一标记集）。
        if needs_gc_mark(val) {
            unsafe { worklist.push(raw_from_tvalue(val)) };
        }
    }

    /// 标记一个 TValue —— 有 ID 写颜色，无 ID 走 aux_mark。
    ///
    /// 由 worklist 出队调用：入队时已认领（见 [`Self::push_mark`]），故这里**不做
    /// 早退** —— 认领只表示「已排定遍历」，真正的遍历必须发生，否则子对象漏标会被
    /// sweep 误判为垃圾。可达对象因此保证被遍历（重复入队的根会重复遍历，无害）。
    fn mark_tvalue(
        &self,
        val: &TValue<'a>,
        worklist: &mut Vec<RawTValue<'a>>,
        extra_size: &mut usize,
    ) {
        let gc: &GCState = unsafe { &*self.gc_ptr() };
        match val {
            TValue::Table(t) => {
                let ptr_id = t.data.borrow().gc_header.ptr_id as usize;
                // 去重: 已标记对象不再重复遍历 (aux_marked 同时是 is_marked 的判定集)
                if !gc.aux_mark(ptr_id) {
                    return;
                }

                // 已注册的额外染色（供 sweep_unreachable 使用）
                if let Some(id) = t.data.borrow().gc_header.id() {
                    gc.set_color(id, GCColor::Gray);
                }

                let data = t.data.borrow();
                let (weak_k, weak_v) = match &data.metatable {
                    Some(mt) => {
                        let mode_key = self.mode_key_cached();
                        match mt.get(&mode_key) {
                            Some(s @ (TValue::LongStr(_) | TValue::ShortStr(_))) => {
                                let mode = lua_string_as_str(&s);
                                (mode.contains('k'), mode.contains('v'))
                            }
                            _ => (false, false),
                        }
                    }
                    None => (false, false),
                };
                let has_mt = data.metatable.is_some();
                gc_stats_inc(|s| {
                    s.table += 1;
                    if has_mt {
                        s.table_with_mt += 1;
                    }
                });
                for v in data.array.iter() {
                    if !weak_v {
                        self.push_mark(gc, v, worklist);
                    }
                }
                for (k, v) in data.hash_buckets.iter() {
                    if !weak_k {
                        self.push_mark(gc, k, worklist);
                    }
                    if !weak_k && !weak_v {
                        self.push_mark(gc, v, worklist);
                    }
                }
                if let Some(ref mt) = data.metatable {
                    let mt_ptr = mt.data.borrow().gc_header.ptr_id as usize;
                    let _ = mt_ptr;
                    let tv = TValue::Table((**mt).clone());
                    self.push_mark(gc, &tv, worklist);
                    drop(tv);
                }
            }
            TValue::LClosure(c) => {
                let ptr_id = c.gc_header.ptr_id as usize;
                if !gc.aux_mark(ptr_id) {
                    return;
                }
                if let Some(id) = c.gc_header.id() {
                    gc.set_color(id, GCColor::Gray);
                }

                *extra_size += c.proto.gc_mem_size();
                let upvals = c.upvals.borrow();
                let upvals_len = upvals.len();
                gc_stats_inc(|s| {
                    s.lclosure += 1;
                    s.proto += 1;
                    if upvals_len > 0 {
                        s.lclosure_with_upvals += 1;
                    }
                });
                for uv_ref in upvals.iter() {
                    let uv = uv_ref.borrow();
                    match &*uv {
                        UpVal::Closed { value } => {
                            self.push_mark(gc, value, worklist);
                        }
                        UpVal::Open { stack_index, .. } => {
                            if *stack_index < self.stack.len() {
                                let val = &self.stack[*stack_index];
                                self.push_mark(gc, val, worklist);
                            }
                        }
                    }
                }
            }
            TValue::CClosure(cc) => {
                let ptr = cc.as_raw_ptr() as usize;
                if !gc.aux_mark(ptr) {
                    return;
                }
                *extra_size += cc.gc_mem_size();
                gc_stats_inc(|s| {
                    s.cclosure += 1;
                });
                let upvalue = cc.upvalue.borrow();
                for uv in upvalue.iter() {
                    self.push_mark(gc, uv, worklist);
                }
            }
            TValue::RustClosure(rc) => {
                let ptr = rc.as_raw_ptr() as usize;
                if !gc.aux_mark(ptr) {
                    return;
                }
                *extra_size += rc.gc_mem_size();
                gc_stats_inc(|s| {
                    s.rust_closure += 1;
                });
                let upvals = rc.upvalues.borrow();
                for uv in upvals.iter() {
                    self.push_mark(gc, uv, worklist);
                }
            }
            TValue::UserData(u) => {
                let ptr_id = u.gc_header.ptr_id as usize;
                if !gc.aux_mark(ptr_id) {
                    return;
                }
                if let Some(id) = u.gc_header.id() {
                    gc.set_color(id, GCColor::Gray);
                }
                gc_stats_inc(|s| {
                    s.userdata += 1;
                });
                if let Some(ref mt) = &*u.metatable.borrow() {
                    let tv = TValue::Table((*mt).clone());
                    self.push_mark(gc, &tv, worklist);
                    drop(tv);
                }
                for uv in &u.user_values {
                    self.push_mark(gc, uv, worklist);
                }
            }
            TValue::Thread(t) => {
                let ptr = Rc::as_ptr(&t.context) as usize;
                if !gc.aux_mark(ptr) {
                    return;
                }
                *extra_size += t.gc_mem_size();
                gc_stats_inc(|s| {
                    s.thread += 1;
                });
                self.collect_thread_roots(t, worklist);
            }
            _ => {}
        }
    }

    /// ephemeron 表传递性处理 —— 迭代直到收敛。
    fn process_ephemerons(&self, worklist: &mut Vec<RawTValue<'a>>, extra_size: &mut usize) {
        let gc: &GCState = unsafe { &*self.gc_ptr() };
        let mode_key = self.mode_key_cached();
        let mut changed = true;
        while changed {
            changed = false;
            for weak_rc in self.weak_tables.borrow().iter() {
                let data_rc = match weak_rc.upgrade() {
                    None => continue,
                    Some(rc) => rc,
                };
                let (weak_k, weak_v) = {
                    let data = data_rc.borrow();
                    match &data.metatable {
                        Some(mt) => match mt.get(&mode_key) {
                            Some(s @ (TValue::LongStr(_) | TValue::ShortStr(_))) => {
                                let mode = lua_string_as_str(&s);
                                (mode.contains('k'), mode.contains('v'))
                            }
                            _ => (false, false),
                        },
                        None => (false, false),
                    }
                };
                if !(weak_k && !weak_v) {
                    continue;
                }
                let data = data_rc.borrow();
                for (k, v) in data.hash_buckets.iter() {
                    let k_reachable = is_marked(k, gc);
                    let v_reachable = is_marked(v, gc);
                    if v_reachable && !k_reachable {
                        let v_is_gc = matches!(
                            v,
                            TValue::Table(_) | TValue::LClosure(_) | TValue::UserData(_)
                        );
                        if v_is_gc {
                            let k_id = match k {
                                TValue::Table(t) => t.data.borrow().gc_header.id(),
                                TValue::LClosure(c) => c.gc_header.id(),
                                TValue::UserData(u) => u.gc_header.id(),
                                _ => None,
                            };
                            if let Some(id) = k_id {
                                gc.set_color(id, GCColor::Gray);
                            }
                            unsafe { worklist.push(raw_from_tvalue(k)) };
                            changed = true;
                        }
                    }
                    if k_reachable && !v_reachable {
                        unsafe { worklist.push(raw_from_tvalue(v)) };
                        changed = true;
                    }
                }
            }
            while let Some(raw) = worklist.pop() {
                let val = unsafe { raw_as_tvalue(&raw) };
                self.mark_tvalue(val, worklist, extra_size);
            }
        }
    }

    /// 收集需要 finalize 的对象并复活它们。
    fn collect_finalizers(
        &mut self,
        worklist: &mut Vec<RawTValue<'a>>,
        extra_size: &mut usize,
    ) -> Vec<TValue<'a>> {
        self.collect_finalizers_impl(worklist, extra_size, false)
    }

    /// 紧急模式下的 finalizer 处理 —— 对应 C 在 `gcemergency` 下「不把对象移入
    /// tobefnz」：只把不可达的待 finalize 对象**标活并保留在列表中**（否则紧接着的
    /// sweep 会把它们释放，finalizer 永远丢失），真正的分离/执行推迟到下次常规回收。
    fn collect_finalizers_deferred(
        &mut self,
        worklist: &mut Vec<RawTValue<'a>>,
        extra_size: &mut usize,
    ) {
        let _ = self.collect_finalizers_impl(worklist, extra_size, true);
    }

    fn collect_finalizers_impl(
        &mut self,
        worklist: &mut Vec<RawTValue<'a>>,
        extra_size: &mut usize,
        deferred: bool,
    ) -> Vec<TValue<'a>> {
        let gc: &GCState = unsafe { &*self.gc_ptr() };
        let gc_key = self.gc_key_cached();
        let mut to_finalize: Vec<TValue> = Vec::new();
        let mut keep: Vec<Table> = Vec::new();

        for t in self.finobj_list.borrow_mut().drain(..) {
            let is_reachable = t
                .data
                .borrow()
                .gc_header
                .id()
                .map_or(false, |id| !gc.is_white(id));
            if is_reachable {
                keep.push(t);
                continue;
            }
            let has_gc = {
                let data = t.data.borrow();
                if let Some(ref mt) = data.metatable {
                    mt.get(&gc_key).is_some()
                } else {
                    false
                }
            };
            if has_gc {
                if deferred {
                    // 标活 + 保留：对象仍持有强引用（`keep`），故 worklist 里的原始指针有效
                    let tv = TValue::Table(t.clone());
                    unsafe { worklist.push(raw_from_tvalue(&tv)) };
                    drop(tv);
                    keep.push(t);
                } else {
                    to_finalize.push(TValue::Table(t.clone()));
                    let tv = TValue::Table(t);
                    unsafe { worklist.push(raw_from_tvalue(&tv)) };
                    drop(tv);
                }
            }
        }
        *self.finobj_list.borrow_mut() = keep;

        let mut ud_keep: Vec<MyRc<crate::objects::Udata, SlotAlloc>> = Vec::new();
        for u in self.ud_finobj_list.borrow_mut().drain(..) {
            let is_reachable = u.gc_header.id().map_or(false, |id| !gc.is_white(id));
            if is_reachable {
                ud_keep.push(u);
                continue;
            }
            let has_gc = {
                if let Some(mt) = u.metatable.borrow().as_ref() {
                    mt.get(&gc_key).is_some()
                } else {
                    false
                }
            };
            if has_gc {
                if deferred {
                    let tv = TValue::UserData(u.clone());
                    unsafe { worklist.push(raw_from_tvalue(&tv)) };
                    drop(tv);
                    ud_keep.push(u);
                } else {
                    to_finalize.push(TValue::UserData(u.clone()));
                    let tv = TValue::UserData(u);
                    unsafe { worklist.push(raw_from_tvalue(&tv)) };
                    drop(tv);
                }
            }
        }
        *self.ud_finobj_list.borrow_mut() = ud_keep;

        while let Some(raw) = worklist.pop() {
            let val = unsafe { raw_as_tvalue(&raw) };
            self.mark_tvalue(val, worklist, extra_size);
        }

        to_finalize
    }

    /// 调用 finalizer（对 to_finalize 列表中的每个对象重新检查 __gc 并调用）。
    pub(crate) fn call_finalizers(&mut self, to_finalize: Vec<TValue<'a>>) {
        let gc_key = self.gc_key_cached();

        // 构建 upval_origins 映射
        let mut upval_origins_map: std::collections::HashMap<usize, usize, FxBuildHasher> =
            std::collections::HashMap::with_hasher(FxBuildHasher::default());
        for val in self.stack.iter() {
            if let TValue::Thread(t) = val {
                let origins = t.context.borrow().upval_origins.clone();
                for (uv_ref, stack_index) in origins {
                    let ptr = Rc::as_ptr(&uv_ref) as usize;
                    upval_origins_map.insert(ptr, stack_index);
                }
            }
        }

        for obj_val in to_finalize {
            let gc_func = match &obj_val {
                TValue::Table(t) => {
                    let data = t.data.borrow();
                    if let Some(ref mt) = data.metatable {
                        mt.get(&gc_key)
                    } else {
                        None
                    }
                }
                TValue::UserData(u) => {
                    if let Some(mt) = u.metatable.borrow().as_ref() {
                        mt.get(&gc_key)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            let gc_func = match gc_func {
                Some(f) => f,
                None => continue,
            };

            // 收集 finalizer 函数的 upvalue Rc 指针
            let finalizer_uv_ptrs: Vec<usize> = if let TValue::LClosure(c) = &gc_func {
                c.upvals
                    .borrow()
                    .iter()
                    .map(|uv_ref| Rc::as_ptr(uv_ref) as usize)
                    .collect()
            } else {
                Vec::new()
            };

            let stack_base = self.stack.len();
            let saved_base = self.base;
            let saved_pc = self.pc;
            let saved_closure_upvals = Rc::clone(&self.closure_upvals);
            let saved_proto_flag = self.proto_flag;
            let saved_nextraargs = self.nextraargs;
            self.stack.push(gc_func);
            self.stack.push(obj_val);
            self.top = self.stack.len();

            self.call_info.push(CallInfoEntry {
                is_c: false,
                closure: None,
                base: self.base,
                saved_pc: self.pc,
                name: Some("__gc"),
                namewhat: "metamethod",
                proto_flag: self.proto_flag,
                nextraargs: self.nextraargs,
                is_tailcall: false,
            });

            let status = self.pcall(1, 0, 0);
            if status != 0 && self.exit_requested.is_none() {
                let msg = match self.stack.last() {
                    Some(s @ (TValue::LongStr(_) | TValue::ShortStr(_))) => {
                        lua_string_as_str(s).to_string()
                    }
                    _ => "error object is not a string".to_string(),
                };
                self.g_mut().warning("error in ", true);
                self.g_mut().warning("__gc", true);
                self.g_mut().warning(" (", true);
                self.g_mut().warning(&msg, true);
                self.g_mut().warning(")", false);
            }

            self.truncate_live(stack_base);
            self.call_info.pop();
            self.top = stack_base;
            self.base = saved_base;
            self.pc = saved_pc;
            self.closure_upvals = saved_closure_upvals;
            self.proto_flag = saved_proto_flag;
            self.nextraargs = saved_nextraargs;

            // 同步 finalizer 修改的 closed upvalue 值回主线程栈
            for uv_ptr in &finalizer_uv_ptrs {
                if let Some(&stack_index) = upval_origins_map.get(uv_ptr) {
                    let mut found_val = None;
                    'outer: for val in self.stack.iter() {
                        if let TValue::Thread(t) = val {
                            let origins = t.context.borrow().upval_origins.clone();
                            for (uv_ref, _idx) in &origins {
                                if Rc::as_ptr(uv_ref) as usize == *uv_ptr {
                                    let uv = uv_ref.borrow();
                                    if let UpVal::Closed { value } = &*uv {
                                        found_val = Some(value.clone());
                                    }
                                    break 'outer;
                                }
                            }
                        }
                    }
                    if let Some(val) = found_val {
                        if stack_index < self.stack.len() {
                            self.stack[stack_index] = val;
                        }
                    }
                }
            }
        }
    }
}

// ============================================================================
// impl GlobalState — 弱表清理 / finalizer 注册 / 缓存键
// ============================================================================

impl<'a> GlobalState<'a> {
    /// 获取缓存的 "__mode" 字符串键。
    pub(crate) fn mode_key_cached(&self) -> TValue<'a> {
        if let Some(ref k) = *self.cached_mode_key.borrow() {
            return k.clone();
        }
        let k = self.intern_str("__mode");
        *self.cached_mode_key.borrow_mut() = Some(k.clone());
        k
    }

    /// 获取缓存的 "__gc" 字符串键。
    pub(crate) fn gc_key_cached(&self) -> TValue<'a> {
        if let Some(ref k) = *self.cached_gc_key.borrow() {
            return k.clone();
        }
        let k = self.intern_str("__gc");
        *self.cached_gc_key.borrow_mut() = Some(k.clone());
        k
    }

    /// 注册弱引用表。
    pub fn register_weak_table(&mut self, t: &Table<'a>) {
        self.weak_tables.borrow_mut().push(t.data.downgrade());
    }

    /// 注册有 __gc 元方法的 Table。
    pub fn register_finobj(&mut self, t: &Table<'a>) {
        if self.gc_closing {
            return;
        }
        let ptr_id = t.data.borrow().gc_header.ptr_id;
        let already = self
            .finobj_list
            .borrow()
            .iter()
            .any(|x| x.data.borrow().gc_header.ptr_id == ptr_id);
        if !already {
            self.finobj_list.borrow_mut().push(t.clone());
        }
    }

    /// 注册有 __gc 元方法的 UserData。
    pub fn register_ud_finobj(&mut self, u: &MyRc<crate::objects::Udata<'a>, SlotAlloc>) {
        if self.gc_closing {
            return;
        }
        let ptr_id = u.gc_header.ptr_id;
        let already = self
            .ud_finobj_list
            .borrow()
            .iter()
            .any(|x| x.gc_header.ptr_id == ptr_id);
        if !already {
            self.ud_finobj_list.borrow_mut().push(u.clone());
        }
    }

    /// 清理弱引用表 —— 基于颜色/aux_marked 判断键/值是否死亡。
    ///
    /// **无参**：可达性直接读 `GCState`（有 ID 的读颜色，无 ID 的读 `aux_marked`）。
    pub(crate) fn clear_weak_tables(&mut self) {
        let gc: &GCState = unsafe { &*self.gc.as_ptr() };
        let mode_key = self.mode_key_cached();
        let mut tables = std::mem::take(&mut *self.weak_tables.borrow_mut());
        let mut i = 0;
        while i < tables.len() {
            match tables[i].upgrade() {
                None => {
                    tables.swap_remove(i);
                }
                Some(data_rc) => {
                    let (weak_k, weak_v) = {
                        let data = data_rc.borrow();
                        match &data.metatable {
                            Some(mt) => match mt.get(&mode_key) {
                                Some(s @ (TValue::LongStr(_) | TValue::ShortStr(_))) => {
                                    let mode = lua_string_as_str(&s);
                                    (mode.contains('k'), mode.contains('v'))
                                }
                                _ => (false, false),
                            },
                            None => (false, false),
                        }
                    };
                    if weak_k || weak_v {
                        let mut to_clear_array: Vec<usize> = Vec::new();
                        let mut to_clear_hash: Vec<TValue> = Vec::new();
                        {
                            let data = data_rc.borrow();
                            if weak_v {
                                for (idx, val) in data.array.iter().enumerate() {
                                    if matches!(val, TValue::Nil(NilKind::Empty)) {
                                        continue;
                                    }
                                    let marked = is_marked(val, gc);
                                    if !marked {
                                        to_clear_array.push(idx);
                                    }
                                }
                            }
                            for (k, v) in data.hash_buckets.iter() {
                                let k_dead = weak_k && !is_marked(k, gc);
                                let v_dead = weak_v && !is_marked(v, gc);
                                let remove = if weak_k && weak_v {
                                    k_dead || v_dead
                                } else if weak_k {
                                    k_dead
                                } else {
                                    v_dead
                                };
                                if remove {
                                    to_clear_hash.push(k.clone());
                                }
                            }
                        }
                        if !to_clear_array.is_empty() || !to_clear_hash.is_empty() {
                            let mut data = data_rc.borrow_mut();
                            for idx in to_clear_array {
                                data.array[idx] = TValue::Nil(NilKind::Empty);
                            }
                            for k in to_clear_hash {
                                if let Some(i) = data.idx_remove(&k) {
                                    let last_idx = data.hash_buckets.len() - 1;
                                    if i != last_idx {
                                        let key = data.hash_buckets.last().unwrap().0.clone();
                                        data.hash_buckets.swap_remove(i);
                                        data.idx_remove(&key);
                                        data.idx_insert(&key, i);
                                    } else {
                                        data.hash_buckets.pop();
                                    }
                                }
                            }
                        }
                    }
                    i += 1;
                }
            }
        }
        *self.weak_tables.borrow_mut() = tables;
    }
}

// ============================================================================
// 测试 (TDD)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gc_object_id_unique() {
        let gc = GCState::default_incremental();
        let id1 = gc
            .try_register_object(100, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        let id2 = gc
            .try_register_object(200, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        assert_ne!(id1, id2);
        assert!(id1.0 > 0);
        assert!(id2.0 > id1.0);
    }

    #[test]
    fn test_gc_object_id_clone_copy() {
        let id = GCObjectId(42);
        let id2 = id;
        assert_eq!(id, id2);
    }

    #[test]
    fn test_gc_color_white_detection() {
        assert!(GCColor::White0.is_white(1));
        assert!(!GCColor::White1.is_white(1));
        assert!(!GCColor::Black.is_white(1));
        assert!(!GCColor::Gray.is_white(1));
    }

    #[test]
    fn test_gc_color_white_switched() {
        assert!(!GCColor::White0.is_white(2));
        assert!(GCColor::White1.is_white(2));
    }

    #[test]
    fn test_gc_color_is_gray() {
        assert!(GCColor::Gray.is_gray());
        assert!(!GCColor::Black.is_gray());
        assert!(!GCColor::White0.is_gray());
    }

    #[test]
    fn test_gc_color_is_black() {
        assert!(GCColor::Black.is_black());
        assert!(!GCColor::Gray.is_black());
    }

    #[test]
    fn test_gc_state_creation() {
        let gc = GCState::default_incremental();
        assert_eq!(gc.phase.get(), GCPhase::Pause);
        assert!(gc.gray_is_empty());
        assert!(gc.is_running());
    }

    #[test]
    fn test_register_object_color() {
        let gc = GCState::default_incremental();
        let id = gc
            .try_register_object(128, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        assert!(gc.is_white(id));
        assert!(!gc.is_black(id));
    }

    #[test]
    fn test_register_object_estimate() {
        let gc = GCState::default_incremental();
        let initial_estimate = gc.gc_estimate.get();
        let _id = gc
            .try_register_object(500, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        assert!(gc.gc_estimate.get() >= initial_estimate + 500);
    }

    #[test]
    fn test_make_gray_and_pop() {
        let gc = GCState::default_incremental();
        let id1 = gc
            .try_register_object(100, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        let id2 = gc
            .try_register_object(200, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        gc.make_gray(id1);
        gc.make_gray(id2);
        assert_eq!(gc.pop_gray(), Some(id1));
        assert_eq!(gc.pop_gray(), Some(id2));
        assert_eq!(gc.pop_gray(), None);
    }

    #[test]
    fn test_make_gray_idempotent() {
        let gc = GCState::default_incremental();
        let id = gc
            .try_register_object(100, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        gc.make_gray(id);
        gc.make_gray(id);
        assert_eq!(gc.pop_gray(), Some(id));
        assert_eq!(gc.pop_gray(), None);
    }

    #[test]
    fn test_barrier_back() {
        let gc = GCState::default_incremental();
        let p = gc
            .try_register_object(100, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        gc.set_color(p, GCColor::Black);
        gc.barrier_back(p);
        assert_eq!(gc.pop_gray_again(), Some(p));
    }

    #[test]
    fn test_barrier_back_not_black() {
        let gc = GCState::default_incremental();
        let p = gc
            .try_register_object(100, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        gc.set_color(p, GCColor::Gray);
        gc.barrier_back(p);
        assert_eq!(gc.pop_gray_again(), None);
    }

    #[test]
    fn test_enter_cycle() {
        let mut gc = GCState::default_incremental();
        let initial_white = gc.current_white.get();
        gc.phase.set(GCPhase::Pause);
        gc.step();
        assert_eq!(gc.phase.get(), GCPhase::Propagate);
        assert_ne!(gc.current_white.get(), initial_white);
    }

    #[test]
    fn test_gc_stop_and_running() {
        let gc = GCState::default_incremental();
        assert!(gc.is_running());
        gc.gc_stop.set(1);
        assert!(!gc.is_running());
    }

    #[test]
    fn test_gc_object_header_new() {
        let header = GCObjectHeader::new();
        assert!(header.id().is_none());
    }

    #[test]
    fn test_gc_object_header_set_get() {
        let header = GCObjectHeader::new();
        header.set_id(GCObjectId(42));
        assert_eq!(header.id(), Some(GCObjectId(42)));
    }

    #[test]
    fn test_aux_mark_and_reuse() {
        let gc = GCState::default_incremental();
        assert!(gc.aux_mark(12345));
        assert!(!gc.aux_mark(12345)); // 第二次返回 false
        assert!(gc.aux_is_marked(12345));
        gc.enter_cycle();
        assert!(!gc.aux_is_marked(12345)); // clear 后未标记
    }

    #[test]
    fn test_sweep_unreachable_by_color() {
        let gc = GCState::default_incremental();
        let id_keep = gc
            .try_register_object(100, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        let id_free = gc
            .try_register_object(200, WeakValue::Nil(NilKind::Strict))
            .ok()
            .expect("测试: 登记 GC 对象失败");
        // 标记 id_keep 为可达（染黑）
        gc.set_color(id_keep, GCColor::Black);
        // id_free 保持白色
        gc.sweep_unreachable();
        assert!(gc.meta(id_keep).is_some());
        assert!(gc.meta(id_free).is_none());
        assert_eq!(gc.active_count(), 1);
    }
}
