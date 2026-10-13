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
    Old0 = 2,
    Old1 = 3,
    Old = 4,
    Touched1 = 5,
    Touched2 = 6,
}

impl GCAge {
    pub fn is_young(&self) -> bool {
        matches!(self, GCAge::New | GCAge::Survival)
    }

    pub fn is_old(&self) -> bool {
        *self > GCAge::Survival
    }
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
    SweepFinObj = 4,
    SweepToBeFnz = 5,
    SweepEnd = 6,
    CallFin = 7,
    Pause = 8,
}

impl GCPhase {
    pub fn is_sweep_phase(&self) -> bool {
        matches!(
            self,
            GCPhase::SweepAllGC | GCPhase::SweepFinObj | GCPhase::SweepToBeFnz | GCPhase::SweepEnd
        )
    }
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
// GCObject — GC 可追踪对象 trait
// ============================================================================

pub trait GCObject {
    fn gc_id(&self) -> GCObjectId;
    fn traverse(&self, gc: &GCState);
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

    pub fn clear_id(&self) {
        self.id.set(None);
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

#[cfg_attr(not(size_optimized), inline(always))]
pub(crate) unsafe fn raw_from_tvalue<'a>(val: &TValue<'a>) -> RawTValue<'a> {
    std::mem::MaybeUninit::new(std::ptr::read(val as *const TValue))
}

#[cfg_attr(not(size_optimized), inline(always))]
pub(crate) unsafe fn raw_as_tvalue<'a, 'b>(raw: &'b RawTValue<'a>) -> &'b TValue<'a> {
    raw.assume_init_ref()
}

/// 只处理需要 GC 标记的类型（Table/LClosure/CClosure/UserData/Thread/RustClosure）。
#[cfg_attr(not(size_optimized), inline)]
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
    /// 创建时通过 charge_extra 增加（触发 GC），collect_gc 后通过 set_extra_estimate 重算（精确）。
    pub gc_extra_estimate: Cell<usize>,
    pub gc_stop: Cell<u8>,
    pub gc_params: [Cell<i32>; 6],
    pub in_minor: Cell<bool>,
    next_collect_threshold: Cell<usize>,
    pub step_accum: Cell<usize>,
    /// GC 是否正在运行（collect_gc 进行中）— 用于阻止 finalizer 中重入
    pub gc_running: Cell<bool>,
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
            in_minor: Cell::new(false),
            next_collect_threshold: Cell::new(200000),
            step_accum: Cell::new(0),
            gc_running: Cell::new(false),
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

    pub fn default_generational() -> Self {
        let gc = GCState::new(GCMode::Generational);
        gc.set_gc_param(Self::PARAM_MINORMUL, 20);
        gc.set_gc_param(Self::PARAM_MAJORMINOR, 50);
        gc.set_gc_param(Self::PARAM_MINORMAJOR, 70);
        gc
    }

    /// GC 参数索引（与 C 的 LUA_GCP* 一致）
    pub const PARAM_MINORMUL: usize = 0;
    pub const PARAM_MAJORMINOR: usize = 1;
    pub const PARAM_MINORMAJOR: usize = 2;
    pub const PARAM_PAUSE: usize = 3;
    pub const PARAM_STEPMUL: usize = 4;
    pub const PARAM_STEPSIZE: usize = 5;

    /// 返回当前 GC 模式
    pub fn current_mode(&self) -> GCMode {
        self.mode.get()
    }

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

    pub fn register_object(&self, size: usize, weak_value: WeakValue<'a>) -> GCObjectId {
        let cw = self.current_white.get();
        let color = if cw & 1 != 0 {
            GCColor::White0
        } else {
            GCColor::White1
        };

        let mut meta = GCObjectMeta::new(size);
        meta.color = color;
        meta.age = GCAge::New;
        meta.weak_value = weak_value;

        let mut objs = self.objects.borrow_mut();
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

        id
    }

    pub fn register_table(&self, t: &Table<'a>) {
        // 幂等：同一张表（同一 TableData）只能注册一次。
        // 重复注册会给同一数据分配第二个 ID，旧 ID 永远保持白色 ——
        // mark 只把新 ID 染灰，sweep 见到旧 ID 白色就 clear_for_drop 这张
        // 仍然可达的表（package.loaded 曾被 registry._LOADED 二次注册清空）。
        if t.data.borrow().gc_header.id().is_some() {
            return;
        }
        let id = self.register_object(t.mem_size(), WeakValue::Table(t.downgrade()));
        t.data.borrow().gc_header.set_id(id);
    }

    pub fn unregister_object(&self, id: GCObjectId) {
        let mut objs = self.objects.borrow_mut();
        if (id.0 as usize) < objs.metas.len() {
            if let Some(meta) = &objs.metas[id.0 as usize] {
                let charged = meta.size.max(32);
                let estimate = self.gc_estimate.get();
                self.gc_estimate.set(estimate.saturating_sub(charged));
            }
            objs.metas[id.0 as usize] = None;
            // 收集释放的 ID 供 register_object 重用
            objs.free_ids.push(id.0);
            // 从 all_objects 移除（swap_remove O(1)，顺序不影响正确性）
            if let Some(pos) = objs.all_objects.iter().position(|&x| x == id.0) {
                objs.all_objects.swap_remove(pos);
            }
        }
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

    pub fn is_registered(&self, id: GCObjectId) -> bool {
        let objs = self.objects.borrow();
        (id.0 as usize) < objs.metas.len() && objs.metas[id.0 as usize].is_some()
    }

    /// 返回已注册对象数量（含已释放但未复用的 ID 槽位）
    pub fn metas_len(&self) -> usize {
        self.objects.borrow().metas.len()
    }

    /// 返回实际存活（非 None）的对象数 — O(1) 直接读 all_objects 长度
    pub fn active_count(&self) -> usize {
        self.objects.borrow().all_objects.len()
    }

    /// 返回 free_ids 长度
    pub fn free_ids_count(&self) -> usize {
        self.objects.borrow().free_ids.len()
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

    pub fn charge_extra(&self, size: usize) {
        let cur = self.gc_extra_estimate.get();
        self.gc_extra_estimate.set(cur + size);
    }

    /// 设置 extra estimate（collect_gc 后重算使用）。
    pub fn set_extra_estimate(&self, size: usize) {
        self.gc_extra_estimate.set(size);
    }

    /// 返回 extra estimate（无 gc_header 对象的估算占用）。
    pub fn extra_estimate(&self) -> usize {
        self.gc_extra_estimate.get()
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
                objs.free_ids.push(id);
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

    pub fn color(&self, id: GCObjectId) -> Option<GCColor> {
        self.meta(id).map(|m| m.color)
    }

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

    pub fn is_gray(&self, id: GCObjectId) -> bool {
        self.meta(id).map(|m| m.color.is_gray()).unwrap_or(false)
    }

    pub fn nw2black(&self, id: GCObjectId) {
        if let Some(mut meta) = self.meta_mut(id) {
            let cw = self.current_white.get();
            debug_assert!(!meta.color.is_white(cw), "nw2black: object is white");
            meta.color = GCColor::Black;
        }
    }

    pub fn change_white(&self, id: GCObjectId) {
        if let Some(mut meta) = self.meta_mut(id) {
            meta.color = match meta.color {
                GCColor::White0 => GCColor::White1,
                GCColor::White1 => GCColor::White0,
                other => other,
            };
        }
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

    pub fn gray_again_is_empty(&self) -> bool {
        self.gray_again.borrow().is_empty()
    }

    pub fn push_weak(&self, id: GCObjectId) {
        self.weak.borrow_mut().push_back(id);
    }

    pub fn push_all_weak(&self, id: GCObjectId) {
        self.all_weak.borrow_mut().push_back(id);
    }

    pub fn push_ephemeron(&self, id: GCObjectId) {
        self.ephemeron.borrow_mut().push_back(id);
    }

    // ========================================================================
    // 年龄操作 (分代模式)
    // ========================================================================

    pub fn age(&self, id: GCObjectId) -> Option<GCAge> {
        self.meta(id).map(|m| m.age)
    }

    pub fn set_age(&self, id: GCObjectId, age: GCAge) {
        if let Some(mut meta) = self.meta_mut(id) {
            meta.age = age;
        }
    }

    pub fn is_young(&self, id: GCObjectId) -> bool {
        self.meta(id).map(|m| m.age.is_young()).unwrap_or(false)
    }

    pub fn is_old(&self, id: GCObjectId) -> bool {
        self.meta(id).map(|m| m.age.is_old()).unwrap_or(false)
    }

    pub fn promote_age(&self, id: GCObjectId) {
        if let Some(mut meta) = self.meta_mut(id) {
            match meta.age {
                GCAge::New => meta.age = GCAge::Survival,
                GCAge::Survival => meta.age = GCAge::Old1,
                GCAge::Old0 => meta.age = GCAge::Old1,
                GCAge::Old1 => meta.age = GCAge::Old,
                GCAge::Touched1 => meta.age = GCAge::Touched2,
                GCAge::Touched2 => meta.age = GCAge::Old,
                GCAge::Old => {}
            }
        }
    }

    // ========================================================================
    // 写屏障 (Write Barriers)
    // ========================================================================

    pub fn obj_barrier(&self, p: GCObjectId, o: GCObjectId) {
        if self.is_black(p) && self.is_white(o) {
            self.make_gray(o);
        }
    }

    /// 值写屏障: 当 p 被赋值为可 GC 类型的 v 时调用
    pub fn barrier_value(&self, p: GCObjectId, v: &crate::objects::TValue) {
        if !self.is_black(p) {
            return;
        }
        match v {
            TValue::Table(_)
            | TValue::LClosure(_)
            | TValue::CClosure(_)
            | TValue::Thread(_)
            | TValue::UserData(_)
            | TValue::LongStr(_)
            | TValue::ShortStr(_) => {
                // 这些是 GC 对象类型，需要从 TValue 中提取 GCObjectId
                // 当前 TValue 变体持有具体类型值，不包含 ID。
                // 在完整集成时，需要将 TValue 改为持有 GCObjectId + Ref。
            }
            _ => {}
        }
    }

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

    pub fn keep_invariant(&self) -> bool {
        (self.phase.get() as u8) <= (GCPhase::Atomic as u8)
    }

    pub fn is_running(&self) -> bool {
        self.gc_stop.get() == 0
    }

    /// GC 是否正在执行 collect_gc（用于阻止 finalizer 中重入）
    pub fn is_gc_running(&self) -> bool {
        self.gc_running.get()
    }

    pub fn is_sweep_phase(&self) -> bool {
        self.phase.get().is_sweep_phase()
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

    pub fn check_gc(&mut self) {
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
            GCPhase::SweepFinObj => self.sweep_step(),
            GCPhase::SweepToBeFnz => self.sweep_step(),
            GCPhase::SweepEnd => self.end_cycle(),
            GCPhase::CallFin => self.phase.set(GCPhase::Pause),
        }
    }

    /// 执行最多 n 步 GC 工作，返回是否完成一个完整周期（phase 回到 Pause）
    pub fn step_n(&mut self, n: usize) -> bool {
        if !self.is_running() {
            return self.phase.get() == GCPhase::Pause;
        }
        let iters = n.max(1);
        for _ in 0..iters {
            self.step();
            if self.phase.get() == GCPhase::Pause {
                return true;
            }
        }
        false
    }

    /// 当前是否处于 Pause 阶段（即一个周期已完成）
    pub fn is_paused(&self) -> bool {
        self.phase.get() == GCPhase::Pause
    }

    // ========================================================================
    // 标记阶段
    // ========================================================================

    /// 进入新周期：翻转 white、清空所有工作队列（含 aux_marked）。
    ///
    /// `pub(crate)`：供 `LuaState::collect_gc_inner` 手动进入完整 GC 周期。
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
                    GCAge::Old0 => meta.age = GCAge::Old1,
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

    pub fn debt(&self) -> isize {
        self.gc_debt.get()
    }

    pub fn set_debt(&self, debt: isize) {
        self.gc_debt.set(debt);
    }

    // ========================================================================
    // 完整 GC (luaC_fullgc)
    // ========================================================================

    pub fn full_gc(&mut self) {
        if !self.is_running() {
            return;
        }

        self.enter_cycle();

        while !self.gray_is_empty() {
            while let Some(id) = self.pop_gray() {
                self.set_color(id, GCColor::Black);
            }
        }

        self.phase.set(GCPhase::EnterAtomic);
        self.enter_atomic();
        self.enter_sweep();
        self.sweep_step();
        self.end_cycle();
    }

    // ========================================================================
    // 对象大小
    // ========================================================================

    pub fn obj_size(&self, id: GCObjectId) -> Option<usize> {
        self.meta(id).map(|m| m.size)
    }

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

    pub fn mark_value(&self, _value: &crate::objects::TValue) {}

    pub fn mark_object(&self, id: GCObjectId) {
        if self.is_white(id) {
            self.make_gray(id);
        }
    }

    pub fn fix(&self, id: GCObjectId) {
        if self.is_white(id) {
            self.make_gray(id);
            self.set_color(id, GCColor::Black);
        }
    }

    pub fn register_root(&self, id: GCObjectId) {
        self.fix(id);
    }
}

// ============================================================================
// 便捷的写屏障函数
// ============================================================================

pub fn gc_obj_barrier(gc: &GCState, p_id: GCObjectId, o_id: GCObjectId) {
    gc.obj_barrier(p_id, o_id);
}

pub fn gc_barrier_back(gc: &GCState, p_id: GCObjectId) {
    gc.barrier_back(p_id);
}

pub fn gc_obj_barrier_back(gc: &GCState, p_id: GCObjectId, o_id: GCObjectId) {
    gc.obj_barrier_back(p_id, o_id);
}

pub fn gc_cond_gc(gc: &mut GCState) {
    gc.cond_gc();
}

pub fn gc_check_gc(gc: &mut GCState) {
    gc.check_gc();
}

// ============================================================================
// impl LuaState — GC 入口 / 标记 / 弱表 / finalizer
// ============================================================================
//
// 全部 GC 逻辑集中在 gc.rs，state.rs 不保留任何 GC 实现。

impl<'a> LuaState<'a> {
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

        let prev_estimate = unsafe { self.gc_mut().total_estimate() };
        let result = self.collect_gc_inner();

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
    fn collect_gc_inner(&mut self) {
        let active = unsafe { self.gc_mut().active_count() };
        if gc_stats_enabled() {
            reset_gc_type_stats();
        }

        // 进入新周期：翻转 white、清空所有队列（含 aux_marked）
        unsafe { self.gc_mut().enter_cycle() };

        // worklist 预分配
        let est = (active * 3 / 5).max(64);
        let mut worklist: Vec<RawTValue> = Vec::with_capacity(est * 2);
        let mut extra_size: usize = 0;

        // 根：本线程
        self.collect_roots_into(&mut worklist);

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
            unsafe { (*main_ptr).collect_roots_into(&mut worklist) };
        }
        // 主线程对象自身的 function
        let main_thread = self.main_thread.clone();
        self.collect_thread_roots(&main_thread, &mut worklist);

        // 标记阶段
        while let Some(raw) = worklist.pop() {
            let val = unsafe { raw_as_tvalue(&raw) };
            self.mark_tvalue(val, &mut worklist, &mut extra_size);
        }

        // ephemeron 传递性
        self.process_ephemerons(&mut worklist, &mut extra_size);

        // finalizer 复活
        let to_finalize = self.collect_finalizers(&mut worklist, &mut extra_size);

        // 复活后可能引入新 ephemeron 关系
        self.process_ephemerons(&mut worklist, &mut extra_size);

        // 弱表清理
        self.g_mut().clear_weak_tables();

        // 执行 finalizer
        self.call_finalizers(to_finalize);

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

    /// 标记一个 TValue —— 有 ID 写颜色，无 ID 走 aux_mark。
    ///
    /// 幂等：已灰/黑的对象会立即 return。
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
                // 无论是否注册，都用 ptr_id 做去重 —— 未注册对象只能靠这个
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
                    if !weak_v && needs_gc_mark(v) {
                        unsafe { worklist.push(raw_from_tvalue(v)) };
                    }
                }
                for (k, v) in data.hash_buckets.iter() {
                    if !weak_k && needs_gc_mark(k) {
                        unsafe { worklist.push(raw_from_tvalue(k)) };
                    }
                    if !weak_k && !weak_v && needs_gc_mark(v) {
                        unsafe { worklist.push(raw_from_tvalue(v)) };
                    }
                }
                if let Some(ref mt) = data.metatable {
                    let mt_ptr = mt.data.borrow().gc_header.ptr_id as usize;
                    let _ = mt_ptr;
                    let tv = TValue::Table((**mt).clone());
                    unsafe { worklist.push(raw_from_tvalue(&tv)) };
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
                    if needs_gc_mark(uv) {
                        unsafe { worklist.push(raw_from_tvalue(uv)) };
                    }
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
                    if needs_gc_mark(uv) {
                        unsafe { worklist.push(raw_from_tvalue(uv)) };
                    }
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
                    unsafe { worklist.push(raw_from_tvalue(&tv)) };
                    drop(tv);
                }
                for uv in &u.user_values {
                    if needs_gc_mark(uv) {
                        unsafe { worklist.push(raw_from_tvalue(uv)) };
                    }
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
                to_finalize.push(TValue::Table(t.clone()));
                let tv = TValue::Table(t);
                unsafe { worklist.push(raw_from_tvalue(&tv)) };
                drop(tv);
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
                to_finalize.push(TValue::UserData(u.clone()));
                let tv = TValue::UserData(u);
                unsafe { worklist.push(raw_from_tvalue(&tv)) };
                drop(tv);
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

            self.stack.truncate(stack_base);
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
        let id1 = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        let id2 = gc.register_object(200, WeakValue::Nil(NilKind::Strict));
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
    fn test_gc_age_is_young() {
        assert!(GCAge::New.is_young());
        assert!(GCAge::Survival.is_young());
        assert!(!GCAge::Old0.is_young());
        assert!(!GCAge::Old.is_young());
    }

    #[test]
    fn test_gc_age_is_old() {
        assert!(!GCAge::New.is_old());
        assert!(!GCAge::Survival.is_old());
        assert!(GCAge::Old0.is_old());
        assert!(GCAge::Old1.is_old());
        assert!(GCAge::Old.is_old());
        assert!(GCAge::Touched1.is_old());
        assert!(GCAge::Touched2.is_old());
    }

    #[test]
    fn test_gc_phase_sweep_detection() {
        assert!(GCPhase::SweepAllGC.is_sweep_phase());
        assert!(GCPhase::SweepFinObj.is_sweep_phase());
        assert!(GCPhase::SweepToBeFnz.is_sweep_phase());
        assert!(GCPhase::SweepEnd.is_sweep_phase());
        assert!(!GCPhase::Propagate.is_sweep_phase());
        assert!(!GCPhase::Pause.is_sweep_phase());
    }

    #[test]
    fn test_gc_state_creation() {
        let gc = GCState::default_incremental();
        assert_eq!(gc.phase.get(), GCPhase::Pause);
        assert!(gc.gray_is_empty());
        assert!(gc.is_running());
    }

    #[test]
    fn test_gc_state_generational() {
        let gc = GCState::default_generational();
        assert_eq!(gc.mode.get(), GCMode::Generational);
    }

    #[test]
    fn test_register_object() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(64, WeakValue::Nil(NilKind::Strict));
        assert!(gc.is_registered(id));
        assert!(gc.is_white(id));
    }

    #[test]
    fn test_register_object_color() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(128, WeakValue::Nil(NilKind::Strict));
        assert!(gc.is_white(id));
        assert!(!gc.is_black(id));
    }

    #[test]
    fn test_unregister_object() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(256, WeakValue::Nil(NilKind::Strict));
        assert!(gc.is_registered(id));
        gc.unregister_object(id);
        assert!(!gc.is_registered(id));
    }

    #[test]
    fn test_register_object_debt() {
        let gc = GCState::default_incremental();
        let initial_debt = gc.debt();
        let _id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        assert!(gc.debt() < initial_debt);
    }

    #[test]
    fn test_register_object_estimate() {
        let gc = GCState::default_incremental();
        let initial_estimate = gc.gc_estimate.get();
        let _id = gc.register_object(500, WeakValue::Nil(NilKind::Strict));
        assert!(gc.gc_estimate.get() >= initial_estimate + 500);
    }

    #[test]
    fn test_set_color_and_query() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        assert!(gc.is_white(id));
        gc.set_color(id, GCColor::Gray);
        assert!(gc.is_gray(id));
        gc.set_color(id, GCColor::Black);
        assert!(gc.is_black(id));
    }

    #[test]
    fn test_nw2black() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(id, GCColor::Gray);
        gc.nw2black(id);
        assert!(gc.is_black(id));
    }

    #[test]
    fn test_change_white() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(id, GCColor::White0);
        gc.change_white(id);
        assert_eq!(gc.color(id), Some(GCColor::White1));
    }

    #[test]
    fn test_make_gray_and_pop() {
        let gc = GCState::default_incremental();
        let id1 = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        let id2 = gc.register_object(200, WeakValue::Nil(NilKind::Strict));
        gc.make_gray(id1);
        gc.make_gray(id2);
        assert_eq!(gc.pop_gray(), Some(id1));
        assert_eq!(gc.pop_gray(), Some(id2));
        assert_eq!(gc.pop_gray(), None);
    }

    #[test]
    fn test_make_gray_idempotent() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.make_gray(id);
        gc.make_gray(id);
        assert_eq!(gc.pop_gray(), Some(id));
        assert_eq!(gc.pop_gray(), None);
    }

    #[test]
    fn test_gray_again_queue() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.make_gray_again(id);
        assert!(!gc.gray_again_is_empty());
        assert_eq!(gc.pop_gray_again(), Some(id));
        assert!(gc.gray_again_is_empty());
    }

    #[test]
    fn test_promote_age() {
        let gc = GCState::default_generational();
        let id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        assert!(gc.is_young(id));
        gc.promote_age(id);
        assert_eq!(gc.age(id), Some(GCAge::Survival));
        gc.promote_age(id);
        assert_eq!(gc.age(id), Some(GCAge::Old1));
        gc.promote_age(id);
        assert_eq!(gc.age(id), Some(GCAge::Old));
        gc.promote_age(id);
        assert_eq!(gc.age(id), Some(GCAge::Old));
    }

    #[test]
    fn test_obj_barrier_black_points_to_white() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        let o = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(p, GCColor::Black);
        gc.set_color(o, GCColor::White0);
        gc.obj_barrier(p, o);
        assert!(gc.is_gray(o));
    }

    #[test]
    fn test_obj_barrier_no_effect_when_not_black() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        let o = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(p, GCColor::White0);
        gc.set_color(o, GCColor::White0);
        gc.obj_barrier(p, o);
        assert!(gc.is_white(o));
    }

    #[test]
    fn test_obj_barrier_no_effect_when_o_not_white() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        let o = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(p, GCColor::Black);
        gc.set_color(o, GCColor::Gray);
        gc.obj_barrier(p, o);
        assert!(gc.is_gray(o));
    }

    #[test]
    fn test_barrier_back() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(p, GCColor::Black);
        gc.barrier_back(p);
        assert_eq!(gc.pop_gray_again(), Some(p));
    }

    #[test]
    fn test_barrier_back_not_black() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(p, GCColor::Gray);
        gc.barrier_back(p);
        assert!(gc.gray_again_is_empty());
    }

    #[test]
    fn test_obj_barrier_back() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        let o = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(p, GCColor::Black);
        gc.set_color(o, GCColor::White0);
        gc.obj_barrier_back(p, o);
        assert_eq!(gc.pop_gray_again(), Some(p));
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
    fn test_full_gc() {
        let mut gc = GCState::default_incremental();
        let _id1 = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        let id2 = gc.register_object(200, WeakValue::Nil(NilKind::Strict));
        gc.fix(id2);
        gc.full_gc();
        assert_eq!(gc.phase.get(), GCPhase::Pause);
    }

    #[test]
    fn test_fix_object() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        assert!(gc.is_white(id));
        gc.fix(id);
        assert!(gc.is_black(id));
    }

    #[test]
    fn test_gc_stop_and_running() {
        let gc = GCState::default_incremental();
        assert!(gc.is_running());
        gc.gc_stop.set(1);
        assert!(!gc.is_running());
    }

    #[test]
    fn test_keep_invariant() {
        let gc = GCState::default_incremental();
        gc.phase.set(GCPhase::Propagate);
        assert!(gc.keep_invariant());
        gc.phase.set(GCPhase::Atomic);
        assert!(gc.keep_invariant());
        gc.phase.set(GCPhase::SweepAllGC);
        assert!(!gc.keep_invariant());
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
    fn test_gc_object_header_clear() {
        let header = GCObjectHeader::new();
        header.set_id(GCObjectId(100));
        header.clear_id();
        assert!(header.id().is_none());
    }

    #[test]
    fn test_gc_obj_barrier_convenience() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        let o = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(p, GCColor::Black);
        gc.set_color(o, GCColor::White0);
        gc_obj_barrier(&gc, p, o);
        assert!(gc.is_gray(o));
    }

    #[test]
    fn test_gc_barrier_back_convenience() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.set_color(p, GCColor::Black);
        gc_barrier_back(&gc, p);
        assert_eq!(gc.pop_gray_again(), Some(p));
    }

    #[test]
    fn test_weak_queues() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.push_weak(id);
        gc.push_all_weak(id);
        gc.push_ephemeron(id);
    }

    #[test]
    fn test_obj_size() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(256, WeakValue::Nil(NilKind::Strict));
        assert_eq!(gc.obj_size(id), Some(256));
        gc.set_obj_size(id, 512);
        assert_eq!(gc.obj_size(id), Some(512));
    }

    #[test]
    fn test_debt_update() {
        let gc = GCState::default_incremental();
        gc.set_debt(100);
        assert_eq!(gc.debt(), 100);
    }

    #[test]
    fn test_register_root() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        gc.register_root(id);
        assert!(gc.is_black(id));
    }

    #[test]
    fn test_unregistered_object_queries() {
        let gc = GCState::default_incremental();
        let fake_id = GCObjectId(999);
        assert!(!gc.is_registered(fake_id));
        assert!(!gc.is_white(fake_id));
        assert!(!gc.is_black(fake_id));
        assert!(!gc.is_gray(fake_id));
    }

    #[test]
    fn test_many_objects() {
        let gc = GCState::default_incremental();
        let ids: Vec<GCObjectId> = (0..100)
            .map(|_| gc.register_object(10, WeakValue::Nil(NilKind::Strict)))
            .collect();
        for &id in &ids {
            assert!(gc.is_registered(id));
            assert!(gc.is_white(id));
        }
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
        let id_keep = gc.register_object(100, WeakValue::Nil(NilKind::Strict));
        let id_free = gc.register_object(200, WeakValue::Nil(NilKind::Strict));
        // 标记 id_keep 为可达（染黑）
        gc.set_color(id_keep, GCColor::Black);
        // id_free 保持白色
        gc.sweep_unreachable();
        assert!(gc.is_registered(id_keep));
        assert!(!gc.is_registered(id_free));
        assert_eq!(gc.active_count(), 1);
    }
}
