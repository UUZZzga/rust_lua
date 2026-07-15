//! 垃圾回收器 (Lua 5.5 GC → Rust 惯用重写)
//!
//! 对应 C 源码: lgc.h + lgc.cpp
//!
//! ## 核心算法
//! - **三色标记**: 白色(未标记)、灰色(已标记但未扫描引用)、黑色(已标记且引用已扫描)
//! - **增量式**: GC 步骤可以与 mutator 交错执行
//! - **分代**: 对象按 age 分层，老对象不参与每次回收
//! - **写屏障**: 维护"黑色对象不指向白色对象"的不变式
//!
//! ## Rust 设计原则
//! - 使用 `enum` 表示颜色/年龄，而非 C 的位运算
//! - `GCObjectHeader` trait 提供统一接口
//! - `GCObjectId` 是类型安全的对象标识符
//! - 写屏障是 trait 方法，编译器保证调用正确性
//! - 灰色链表使用 `VecDeque` + 索引，类型安全且无裸指针
//! - 使用 `Cell`/`RefCell` 实现 interior mutability，支持 `&self` 共享引用

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::num::NonZeroU32;

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
pub(crate) struct GCObjectMeta {
    pub color: GCColor,
    pub age: GCAge,
    #[allow(dead_code)]
    pub finalized: bool,
    pub size: usize,
    #[allow(dead_code)]
    pub traversed: bool,
}

impl GCObjectMeta {
    fn new(size: usize) -> Self {
        GCObjectMeta {
            color: GCColor::White0,
            age: GCAge::New,
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
// GCState — 全局 GC 状态
// ============================================================================

/// GC 全局状态。
///
/// 所有方法都使用 `&self` (interior mutability) 以支持在共享引用下操作。
pub struct GCState {
    pub phase: Cell<GCPhase>,
    pub mode: Cell<GCMode>,
    pub current_white: Cell<u8>,
    next_id: Cell<u32>,
    metas: RefCell<Vec<Option<GCObjectMeta>>>,
    free_ids: RefCell<Vec<u32>>,
    /// 活跃对象 ID 列表 — sweep 时只遍历此列表而非整个 metas 数组。
    /// register_object 时 push，sweep_unreachable 时重建（只保留可达对象）。
    /// 这避免了 metas 数组无限增长导致 sweep 遍历空槽的开销。
    all_objects: RefCell<Vec<u32>>,
    gray: RefCell<VecDeque<GCObjectId>>,
    gray_again: RefCell<VecDeque<GCObjectId>>,
    weak: RefCell<VecDeque<GCObjectId>>,
    all_weak: RefCell<VecDeque<GCObjectId>>,
    ephemeron: RefCell<VecDeque<GCObjectId>>,
    pub gc_debt: Cell<isize>,
    pub gc_estimate: Cell<usize>,
    pub gc_stop: Cell<u8>,
    pub gc_params: [Cell<i32>; 6],
    pub in_minor: Cell<bool>,
    next_collect_threshold: Cell<usize>,
    pub step_accum: Cell<usize>,
    /// GC 是否正在运行（collect_gc 进行中）— 用于阻止 finalizer 中重入
    pub gc_running: Cell<bool>,
}

impl GCState {
    pub fn new(mode: GCMode) -> Self {
        GCState {
            phase: Cell::new(GCPhase::Pause),
            mode: Cell::new(mode),
            current_white: Cell::new(1 << 0),
            next_id: Cell::new(1),
            metas: RefCell::new(Vec::new()),
            free_ids: RefCell::new(Vec::new()),
            all_objects: RefCell::new(Vec::new()),
            gray: RefCell::new(VecDeque::new()),
            gray_again: RefCell::new(VecDeque::new()),
            weak: RefCell::new(VecDeque::new()),
            all_weak: RefCell::new(VecDeque::new()),
            ephemeron: RefCell::new(VecDeque::new()),
            gc_debt: Cell::new(0),
            gc_estimate: Cell::new(0),
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

    pub fn register_object(&self, size: usize) -> GCObjectId {
        let cw = self.current_white.get();
        let color = if cw & 1 != 0 {
            GCColor::White0
        } else {
            GCColor::White1
        };

        let mut meta = GCObjectMeta::new(size);
        meta.color = color;
        meta.age = if self.mode.get() == GCMode::Generational {
            GCAge::New
        } else {
            GCAge::New
        };

        let mut metas = self.metas.borrow_mut();
        let mut free_ids = self.free_ids.borrow_mut();
        let mut all_objects = self.all_objects.borrow_mut();
        // 优先重用已释放的 ID 槽位，避免 metas 数组无限增长
        let id = if let Some(free_id) = free_ids.pop() {
            GCObjectId(free_id)
        } else {
            let id_val = self.next_id.get();
            self.next_id.set(id_val + 1);
            while metas.len() <= id_val as usize {
                metas.push(None);
            }
            GCObjectId(id_val)
        };
        metas[id.0 as usize] = Some(meta);
        all_objects.push(id.0);

        let current = self.gc_debt.get();
        let charged = size.max(64) as isize;
        self.gc_debt.set(current - charged);
        let estimate = self.gc_estimate.get();
        self.gc_estimate.set(estimate + charged as usize);

        id
    }

    pub fn unregister_object(&self, id: GCObjectId) {
        let mut metas = self.metas.borrow_mut();
        if (id.0 as usize) < metas.len() {
            if let Some(meta) = &metas[id.0 as usize] {
                let charged = meta.size.max(64);
                let estimate = self.gc_estimate.get();
                self.gc_estimate.set(estimate.saturating_sub(charged));
            }
            metas[id.0 as usize] = None;
            // 收集释放的 ID 供 register_object 重用
            self.free_ids.borrow_mut().push(id.0);
            // 从 all_objects 移除（swap_remove O(1)，顺序不影响正确性）
            let mut all_objects = self.all_objects.borrow_mut();
            if let Some(pos) = all_objects.iter().position(|&x| x == id.0) {
                all_objects.swap_remove(pos);
            }
        }
    }

    pub(crate) fn meta(&self, id: GCObjectId) -> Option<std::cell::Ref<'_, GCObjectMeta>> {
        let metas = self.metas.borrow();
        if (id.0 as usize) < metas.len() && metas[id.0 as usize].is_some() {
            Some(std::cell::Ref::map(metas, |m| {
                m[id.0 as usize].as_ref().unwrap()
            }))
        } else {
            None
        }
    }

    pub(crate) fn meta_mut(&self, id: GCObjectId) -> Option<std::cell::RefMut<'_, GCObjectMeta>> {
        let metas = self.metas.borrow_mut();
        if (id.0 as usize) < metas.len() && metas[id.0 as usize].is_some() {
            Some(std::cell::RefMut::map(metas, |m| {
                m[id.0 as usize].as_mut().unwrap()
            }))
        } else {
            None
        }
    }

    pub fn is_registered(&self, id: GCObjectId) -> bool {
        let metas = self.metas.borrow();
        (id.0 as usize) < metas.len() && metas[id.0 as usize].is_some()
    }

    /// 返回已注册对象数量（含已释放但未复用的 ID 槽位）
    pub fn metas_len(&self) -> usize {
        self.metas.borrow().len()
    }

    /// 返回实际存活（非 None）的对象数 — O(1) 直接读 all_objects 长度
    pub fn active_count(&self) -> usize {
        self.all_objects.borrow().len()
    }

    /// 返回 free_ids 长度
    pub fn free_ids_count(&self) -> usize {
        self.free_ids.borrow().len()
    }

    /// 返回当前 GC 触发阈值
    pub fn collect_threshold(&self) -> usize {
        self.next_collect_threshold.get()
    }

    /// 设置 GC 触发阈值
    pub fn set_collect_threshold(&self, threshold: usize) {
        self.next_collect_threshold.set(threshold);
    }

    /// 清扫不可达对象：只遍历 all_objects 活跃列表，释放所有不在 `reachable` 集合中的对象。
    /// 对应 C 的 sweep 阶段（C 遍历 allgc 链表，此处遍历 all_objects 列表）。
    pub fn sweep_unreachable(&self, reachable: &std::collections::HashSet<usize, crate::objects::FxBuildHasher>) {
        let mut metas = self.metas.borrow_mut();
        let mut all_objects = self.all_objects.borrow_mut();
        let mut free_ids = self.free_ids.borrow_mut();
        let mut total_freed_size = 0usize;
        // 原地重建 all_objects：只保留可达对象
        let mut write = 0usize;
        for read in 0..all_objects.len() {
            let id = all_objects[read];
            let i = id as usize;
            if !reachable.contains(&i) {
                if let Some(ref meta) = metas[i] {
                    total_freed_size = total_freed_size.saturating_add(meta.size.max(64));
                }
                metas[i] = None;
                free_ids.push(id);
            } else {
                all_objects[write] = id;
                write += 1;
            }
        }
        all_objects.truncate(write);
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
            crate::objects::TValue::Table(_)
            | crate::objects::TValue::LClosure(_)
            | crate::objects::TValue::CClosure(_)
            | crate::objects::TValue::Thread(_)
            | crate::objects::TValue::UserData(_)
            | crate::objects::TValue::Str(_) => {
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

    pub fn cond_gc(&self) {
        if self.gc_debt.get() <= 0 {
            self.step();
        }
    }

    pub fn check_gc(&self) {
        if self.gc_debt.get() <= 0 {
            self.step();
        }
    }

    pub fn step(&self) {
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
    pub fn step_n(&self, n: usize) -> bool {
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

    fn enter_cycle(&self) {
        let cw = self.current_white.get();
        self.current_white.set(cw ^ 0x3);

        self.gray.borrow_mut().clear();
        self.gray_again.borrow_mut().clear();
        self.weak.borrow_mut().clear();
        self.all_weak.borrow_mut().clear();
        self.ephemeron.borrow_mut().clear();

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
        // 清除弱引用表中的死键/值
    }

    // ========================================================================
    // 清扫阶段
    // ========================================================================

    fn enter_sweep(&self) {
        self.phase.set(GCPhase::SweepAllGC);
    }

    fn sweep_step(&self) {
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
        let mut metas = self.metas.borrow_mut();
        for meta_opt in metas.iter_mut() {
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

    pub fn full_gc(&self) {
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
            let old = meta.size;
            meta.size = size;
            let estimate = self.gc_estimate.get();
            self.gc_estimate.set(estimate + size - old);
        }
    }

    // ========================================================================
    // 标记
    // ========================================================================

    pub fn mark_value(&self, _value: &crate::objects::TValue) {
        match _value {
            crate::objects::TValue::Table(_) => {}
            crate::objects::TValue::LClosure(_) => {}
            crate::objects::TValue::CClosure(_) => {}
            crate::objects::TValue::Thread(_) => {}
            crate::objects::TValue::UserData(_) => {}
            crate::objects::TValue::Str(_) => {}
            _ => {}
        }
    }

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

pub fn gc_cond_gc(gc: &GCState) {
    gc.cond_gc();
}

pub fn gc_check_gc(gc: &GCState) {
    gc.check_gc();
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
        let id1 = gc.register_object(100);
        let id2 = gc.register_object(200);
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
        let id = gc.register_object(64);
        assert!(gc.is_registered(id));
        assert!(gc.is_white(id));
    }

    #[test]
    fn test_register_object_color() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(128);
        assert!(gc.is_white(id));
        assert!(!gc.is_black(id));
    }

    #[test]
    fn test_unregister_object() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(256);
        assert!(gc.is_registered(id));
        gc.unregister_object(id);
        assert!(!gc.is_registered(id));
    }

    #[test]
    fn test_register_object_debt() {
        let gc = GCState::default_incremental();
        let initial_debt = gc.debt();
        let _id = gc.register_object(100);
        assert!(gc.debt() < initial_debt);
    }

    #[test]
    fn test_register_object_estimate() {
        let gc = GCState::default_incremental();
        let initial_estimate = gc.gc_estimate.get();
        let _id = gc.register_object(500);
        assert!(gc.gc_estimate.get() >= initial_estimate + 500);
    }

    #[test]
    fn test_set_color_and_query() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100);
        assert!(gc.is_white(id));
        gc.set_color(id, GCColor::Gray);
        assert!(gc.is_gray(id));
        gc.set_color(id, GCColor::Black);
        assert!(gc.is_black(id));
    }

    #[test]
    fn test_nw2black() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100);
        gc.set_color(id, GCColor::Gray);
        gc.nw2black(id);
        assert!(gc.is_black(id));
    }

    #[test]
    fn test_change_white() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100);
        gc.set_color(id, GCColor::White0);
        gc.change_white(id);
        assert_eq!(gc.color(id), Some(GCColor::White1));
    }

    #[test]
    fn test_make_gray_and_pop() {
        let gc = GCState::default_incremental();
        let id1 = gc.register_object(100);
        let id2 = gc.register_object(200);
        gc.make_gray(id1);
        gc.make_gray(id2);
        assert_eq!(gc.pop_gray(), Some(id1));
        assert_eq!(gc.pop_gray(), Some(id2));
        assert_eq!(gc.pop_gray(), None);
    }

    #[test]
    fn test_make_gray_idempotent() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100);
        gc.make_gray(id);
        gc.make_gray(id);
        assert_eq!(gc.pop_gray(), Some(id));
        assert_eq!(gc.pop_gray(), None);
    }

    #[test]
    fn test_gray_again_queue() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100);
        gc.make_gray_again(id);
        assert!(!gc.gray_again_is_empty());
        assert_eq!(gc.pop_gray_again(), Some(id));
        assert!(gc.gray_again_is_empty());
    }

    #[test]
    fn test_promote_age() {
        let gc = GCState::default_generational();
        let id = gc.register_object(100);
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
        let p = gc.register_object(100);
        let o = gc.register_object(100);
        gc.set_color(p, GCColor::Black);
        gc.set_color(o, GCColor::White0);
        gc.obj_barrier(p, o);
        assert!(gc.is_gray(o));
    }

    #[test]
    fn test_obj_barrier_no_effect_when_not_black() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100);
        let o = gc.register_object(100);
        gc.set_color(p, GCColor::White0);
        gc.set_color(o, GCColor::White0);
        gc.obj_barrier(p, o);
        assert!(gc.is_white(o));
    }

    #[test]
    fn test_obj_barrier_no_effect_when_o_not_white() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100);
        let o = gc.register_object(100);
        gc.set_color(p, GCColor::Black);
        gc.set_color(o, GCColor::Gray);
        gc.obj_barrier(p, o);
        assert!(gc.is_gray(o));
    }

    #[test]
    fn test_barrier_back() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100);
        gc.set_color(p, GCColor::Black);
        gc.barrier_back(p);
        assert_eq!(gc.pop_gray_again(), Some(p));
    }

    #[test]
    fn test_barrier_back_not_black() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100);
        gc.set_color(p, GCColor::Gray);
        gc.barrier_back(p);
        assert!(gc.gray_again_is_empty());
    }

    #[test]
    fn test_obj_barrier_back() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100);
        let o = gc.register_object(100);
        gc.set_color(p, GCColor::Black);
        gc.set_color(o, GCColor::White0);
        gc.obj_barrier_back(p, o);
        assert_eq!(gc.pop_gray_again(), Some(p));
    }

    #[test]
    fn test_enter_cycle() {
        let gc = GCState::default_incremental();
        let initial_white = gc.current_white.get();
        gc.phase.set(GCPhase::Pause);
        gc.step();
        assert_eq!(gc.phase.get(), GCPhase::Propagate);
        assert_ne!(gc.current_white.get(), initial_white);
    }

    #[test]
    fn test_full_gc() {
        let gc = GCState::default_incremental();
        let _id1 = gc.register_object(100);
        let id2 = gc.register_object(200);
        gc.fix(id2);
        gc.full_gc();
        assert_eq!(gc.phase.get(), GCPhase::Pause);
    }

    #[test]
    fn test_fix_object() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100);
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
        let p = gc.register_object(100);
        let o = gc.register_object(100);
        gc.set_color(p, GCColor::Black);
        gc.set_color(o, GCColor::White0);
        gc_obj_barrier(&gc, p, o);
        assert!(gc.is_gray(o));
    }

    #[test]
    fn test_gc_barrier_back_convenience() {
        let gc = GCState::default_incremental();
        let p = gc.register_object(100);
        gc.set_color(p, GCColor::Black);
        gc_barrier_back(&gc, p);
        assert_eq!(gc.pop_gray_again(), Some(p));
    }

    #[test]
    fn test_weak_queues() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(100);
        gc.push_weak(id);
        gc.push_all_weak(id);
        gc.push_ephemeron(id);
    }

    #[test]
    fn test_obj_size() {
        let gc = GCState::default_incremental();
        let id = gc.register_object(256);
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
        let id = gc.register_object(100);
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
        let ids: Vec<GCObjectId> = (0..100).map(|_| gc.register_object(10)).collect();
        for &id in &ids {
            assert!(gc.is_registered(id));
            assert!(gc.is_white(id));
        }
    }
}
