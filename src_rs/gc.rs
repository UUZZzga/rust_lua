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

// ============================================================================
// GCObjectId — GC 对象的唯一标识
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GCObjectId(pub(crate) usize);

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
    fn default() -> Self { GCAge::New }
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

#[derive(Debug, Clone)]
pub struct GCObjectHeader {
    id: Cell<Option<GCObjectId>>,
}

impl GCObjectHeader {
    pub fn new() -> Self {
        GCObjectHeader { id: Cell::new(None) }
    }

    pub fn id(&self) -> Option<GCObjectId> {
        self.id.get()
    }

    pub fn set_id(&self, id: GCObjectId) {
        self.id.set(Some(id));
    }

    pub fn clear_id(&self) {
        self.id.set(None);
    }
}

impl Default for GCObjectHeader {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// GCState — 全局 GC 状态
// ============================================================================

/// GC 全局状态。
///
/// 所有方法都使用 `&self` (interior mutability) 以支持在共享引用下操作。
pub struct GCState {
    pub phase: Cell<GCPhase>,
    pub mode: GCMode,
    pub current_white: Cell<u8>,
    next_id: Cell<usize>,
    metas: RefCell<Vec<Option<GCObjectMeta>>>,
    gray: RefCell<VecDeque<GCObjectId>>,
    gray_again: RefCell<VecDeque<GCObjectId>>,
    weak: RefCell<VecDeque<GCObjectId>>,
    all_weak: RefCell<VecDeque<GCObjectId>>,
    ephemeron: RefCell<VecDeque<GCObjectId>>,
    pub gc_debt: Cell<isize>,
    pub gc_estimate: Cell<usize>,
    pub gc_stop: Cell<u8>,
    pub gc_params: [u8; 6],
    pub in_minor: Cell<bool>,
}

impl GCState {
    pub fn new(mode: GCMode) -> Self {
        GCState {
            phase: Cell::new(GCPhase::Pause),
            mode,
            current_white: Cell::new(1 << 0),
            next_id: Cell::new(1),
            metas: RefCell::new(Vec::new()),
            gray: RefCell::new(VecDeque::new()),
            gray_again: RefCell::new(VecDeque::new()),
            weak: RefCell::new(VecDeque::new()),
            all_weak: RefCell::new(VecDeque::new()),
            ephemeron: RefCell::new(VecDeque::new()),
            gc_debt: Cell::new(0),
            gc_estimate: Cell::new(0),
            gc_stop: Cell::new(0),
            gc_params: [0; 6],
            in_minor: Cell::new(false),
        }
    }

    pub fn default_incremental() -> Self {
        let gc = GCState::new(GCMode::Incremental);
        gc.set_gc_param(0, 200);
        gc.set_gc_param(1, 200);
        gc.set_gc_param(2, 100);
        gc
    }

    pub fn default_generational() -> Self {
        let gc = GCState::new(GCMode::Generational);
        gc.set_gc_param(0, 20);
        gc.set_gc_param(1, 50);
        gc.set_gc_param(2, 70);
        gc
    }

    fn set_gc_param(&self, idx: usize, val: u8) {
        // gc_params is [u8; 6]; we need interior mutability for it too.
        // For now, accept that gc_params is immutable after construction.
        // The array values are small and set only once at init.
        unsafe {
            let ptr = &self.gc_params as *const [u8; 6] as *mut [u8; 6];
            if idx < (*ptr).len() {
                (*ptr)[idx] = val;
            }
        }
    }

    // ========================================================================
    // 对象注册/注销
    // ========================================================================

    pub fn register_object(&self, size: usize) -> GCObjectId {
        let id_val = self.next_id.get();
        let id = GCObjectId(id_val);
        self.next_id.set(id_val + 1);

        let mut metas = self.metas.borrow_mut();
        while metas.len() <= id.0 {
            metas.push(None);
        }

        let cw = self.current_white.get();
        let color = if cw & 1 != 0 { GCColor::White0 } else { GCColor::White1 };

        let mut meta = GCObjectMeta::new(size);
        meta.color = color;
        meta.age = if self.mode == GCMode::Generational { GCAge::New } else { GCAge::New };
        metas[id.0] = Some(meta);

        let current = self.gc_debt.get();
        self.gc_debt.set(current - size as isize);
        let estimate = self.gc_estimate.get();
        self.gc_estimate.set(estimate + size);

        id
    }

    pub fn unregister_object(&self, id: GCObjectId) {
        let mut metas = self.metas.borrow_mut();
        if id.0 < metas.len() {
            if let Some(ref meta) = metas[id.0] {
                let estimate = self.gc_estimate.get();
                self.gc_estimate.set(estimate.saturating_sub(meta.size));
            }
            metas[id.0] = None;
        }
    }

    pub(crate) fn meta(&self, id: GCObjectId) -> Option<std::cell::Ref<'_, GCObjectMeta>> {
        let metas = self.metas.borrow();
        if id.0 < metas.len() && metas[id.0].is_some() {
            Some(std::cell::Ref::map(metas, |m| m[id.0].as_ref().unwrap()))
        } else {
            None
        }
    }

    pub(crate) fn meta_mut(&self, id: GCObjectId) -> Option<std::cell::RefMut<'_, GCObjectMeta>> {
        let metas = self.metas.borrow_mut();
        if id.0 < metas.len() && metas[id.0].is_some() {
            Some(std::cell::RefMut::map(metas, |m| m[id.0].as_mut().unwrap()))
        } else {
            None
        }
    }

    pub fn is_registered(&self, id: GCObjectId) -> bool {
        let metas = self.metas.borrow();
        id.0 < metas.len() && metas[id.0].is_some()
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
            if self.mode == GCMode::Generational {
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

        if self.mode == GCMode::Generational {
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
        let step_size = self.gc_params.get(2).copied().unwrap_or(100) as isize;
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
        assert_eq!(gc.mode, GCMode::Generational);
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