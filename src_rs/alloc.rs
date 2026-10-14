#[cfg(test)]
use std::sync::Arc;
use std::{
    alloc::{alloc, dealloc, handle_alloc_error, Layout},
    cell::Cell,
    fmt::{Debug, Formatter},
    marker::PhantomData,
    ops::Deref,
    ptr::NonNull,
    sync::atomic::{fence, AtomicUsize, Ordering},
};

// ---------- Header ----------

#[repr(C)]
pub struct RcHeader {
    pub strong: Cell<usize>,
    pub weak: Cell<usize>,
}

#[repr(C)]
pub struct ArcHeader {
    pub strong: AtomicUsize,
    pub weak: AtomicUsize,
}

#[repr(C)]
pub struct SimpleRcHeader {
    pub strong: Cell<usize>,
}

#[repr(C)]
pub struct SimpleArcHeader {
    pub strong: AtomicUsize,
}

impl Default for RcHeader {
    fn default() -> Self {
        Self {
            strong: std::cell::Cell::new(1),
            weak: std::cell::Cell::new(1),
        }
    }
}

impl Default for ArcHeader {
    fn default() -> Self {
        Self {
            strong: std::sync::atomic::AtomicUsize::new(1),
            weak: std::sync::atomic::AtomicUsize::new(1),
        }
    }
}

impl Default for SimpleRcHeader {
    fn default() -> Self {
        Self {
            strong: std::cell::Cell::new(1),
        }
    }
}

impl Default for SimpleArcHeader {
    fn default() -> Self {
        Self {
            strong: std::sync::atomic::AtomicUsize::new(1),
        }
    }
}

// ---------- RcPayload ----------

/// # Safety
/// - `as_ref` 返回的引用必须在 `payload_size` 范围内有效
/// - `payload_size` 必须与分配时传入 `*_block_layout` 的 payload_size 一致
/// - `drop_payload` 必须正确析构 payload 且不越界
pub unsafe trait RcPayload {
    /// 编译期常量：payload 的对齐。
    /// 对 `Sized` 类型等于 `align_of::<Self>()`；
    /// 对 DST 由实现者按已知固定字段给出。
    const ALIGN: usize;

    unsafe fn as_ref<'a>(payload: *const u8) -> &'a Self;
    unsafe fn drop_payload(payload: *mut u8);
    unsafe fn payload_size(payload: *const u8) -> usize;
}

/// Sized 类型：默认按 `size_of`/`align_of` 处理。
///
/// 分配器不进本 trait：它由智能指针的 `A: Allocator` 类型参数表达
/// （见 [`MyRc`]/[`SimpleRc`]），块布局与释放路径由该参数决定，
/// 因此本 blanket impl 得以保留，任意 Sized 类型开箱即用。
unsafe impl<T: Sized> RcPayload for T {
    const ALIGN: usize = std::mem::align_of::<T>();

    #[inline]
    unsafe fn as_ref<'a>(payload: *const u8) -> &'a T {
        &*(payload as *const T)
    }
    #[inline]
    unsafe fn drop_payload(payload: *mut u8) {
        std::ptr::drop_in_place(payload as *mut T);
    }
    #[inline]
    unsafe fn payload_size(_: *const u8) -> usize {
        std::mem::size_of::<T>()
    }
}

// ---------- 常量 ----------

pub const RC_HEADER_SIZE: usize = std::mem::size_of::<RcHeader>();
pub const ARC_HEADER_SIZE: usize = std::mem::size_of::<ArcHeader>();
pub const SIMPLE_RC_HEADER_SIZE: usize = std::mem::size_of::<SimpleRcHeader>();
pub const SIMPLE_ARC_HEADER_SIZE: usize = std::mem::size_of::<SimpleArcHeader>();

// ---------- repr(C) 布局参照（仅用于布局测试） ----------

#[repr(C)]
pub struct RcBox<T: ?Sized> {
    pub header: RcHeader,
    pub value: T,
}

#[repr(C)]
pub struct SimpleRcBox<T: ?Sized + RcPayload> {
    pub header: SimpleRcHeader,
    pub value: T,
}

#[repr(C)]
pub struct SimpleArcBox<T: ?Sized + RcPayload> {
    pub header: SimpleArcHeader,
    pub value: T,
}

// ---------- 布局计算 ----------

#[inline]
pub fn rc_payload_offset(payload_align: usize) -> usize {
    let a = payload_align.max(std::mem::align_of::<RcHeader>());
    (RC_HEADER_SIZE + a - 1) & !(a - 1)
}
#[inline]
pub fn rc_block_layout(payload_size: usize, payload_align: usize) -> Layout {
    let offset = rc_payload_offset(payload_align);
    let align = payload_align.max(std::mem::align_of::<RcHeader>());
    let size = (offset + payload_size + align - 1) & !(align - 1);
    Layout::from_size_align(size.max(align), align).unwrap()
}

#[inline]
pub fn arc_payload_offset(payload_align: usize) -> usize {
    let a = payload_align.max(std::mem::align_of::<ArcHeader>());
    (ARC_HEADER_SIZE + a - 1) & !(a - 1)
}
#[inline]
pub fn arc_block_layout(payload_size: usize, payload_align: usize) -> Layout {
    let offset = arc_payload_offset(payload_align);
    let align = payload_align.max(std::mem::align_of::<ArcHeader>());
    let size = (offset + payload_size + align - 1) & !(align - 1);
    Layout::from_size_align(size.max(align), align).unwrap()
}

#[inline]
pub fn simple_rc_payload_offset(payload_align: usize) -> usize {
    let a = payload_align.max(std::mem::align_of::<SimpleRcHeader>());
    (SIMPLE_RC_HEADER_SIZE + a - 1) & !(a - 1)
}
#[inline]
pub fn simple_rc_block_layout(payload_size: usize, payload_align: usize) -> Layout {
    let offset = simple_rc_payload_offset(payload_align);
    let align = payload_align.max(std::mem::align_of::<SimpleRcHeader>());
    let size = (offset + payload_size + align - 1) & !(align - 1);
    Layout::from_size_align(size.max(align), align).unwrap()
}

#[inline]
pub fn simple_arc_payload_offset(payload_align: usize) -> usize {
    let a = payload_align.max(std::mem::align_of::<SimpleArcHeader>());
    (SIMPLE_ARC_HEADER_SIZE + a - 1) & !(a - 1)
}
#[inline]
pub fn simple_arc_block_layout(payload_size: usize, payload_align: usize) -> Layout {
    let offset = simple_arc_payload_offset(payload_align);
    let align = payload_align.max(std::mem::align_of::<SimpleArcHeader>());
    let size = (offset + payload_size + align - 1) & !(align - 1);
    Layout::from_size_align(size.max(align), align).unwrap()
}

// ---------- 旧「allocator 路由块」辅助函数已并入智能指针的泛型布局 ----------
// 见本文件后段 `rc_alloc_off` / `rc_data_off` / `rc_layout`（`MyRc`）
// 与 `simple_alloc_off` / `simple_data_off` / `simple_layout`（`SimpleRc`/`SimpleArc`）：
// 分配器类型参数 `A` 的字段偏移取代了原先硬编码的 AllocSlot 槽偏移。

// ==================== JIT 运行时（extern "C"） ====================

// ---------- MyRc ----------

#[no_mangle]
pub unsafe extern "C" fn myrc_alloc(payload_size: usize, payload_align: usize) -> *mut u8 {
    let layout = rc_block_layout(payload_size, payload_align);
    let base = alloc(layout);
    if base.is_null() {
        handle_alloc_error(layout);
    }
    std::ptr::write(
        base as *mut RcHeader,
        RcHeader {
            strong: Cell::new(1),
            weak: Cell::new(1),
        },
    );
    base.add(rc_payload_offset(payload_align))
}

#[no_mangle]
pub unsafe extern "C" fn myrc_clone(payload: *mut u8, payload_align: usize) -> *mut u8 {
    let hdr = payload.sub(rc_payload_offset(payload_align)) as *mut RcHeader;
    (*hdr).strong.set((*hdr).strong.get() + 1);
    payload
}

#[no_mangle]
pub unsafe extern "C" fn myrc_drop(
    payload: *mut u8,
    payload_size: usize,
    payload_align: usize,
    drop_fn: Option<unsafe extern "C" fn(*mut u8)>,
) {
    let offset = rc_payload_offset(payload_align);
    let base = payload.sub(offset);
    let hdr = base as *mut RcHeader;
    let c = (*hdr).strong.get();

    if c == 1 {
        if let Some(f) = drop_fn {
            f(payload);
        }
        (*hdr).strong.set(0);

        let w = (*hdr).weak.get();
        debug_assert!(w > 0, "myrc_drop 时 weak 不应为 0");
        (*hdr).weak.set(w - 1);

        if w == 1 {
            dealloc(base, rc_block_layout(payload_size, payload_align));
        }
    } else {
        (*hdr).strong.set(c - 1);
    }
}

#[no_mangle]
pub unsafe extern "C" fn myrc_downgrade(payload: *mut u8, payload_align: usize) -> *mut u8 {
    let hdr = payload.sub(rc_payload_offset(payload_align)) as *mut RcHeader;
    (*hdr).weak.set((*hdr).weak.get() + 1);
    payload
}

#[no_mangle]
pub unsafe extern "C" fn myrc_weak_clone(payload: *mut u8, payload_align: usize) -> *mut u8 {
    let hdr = payload.sub(rc_payload_offset(payload_align)) as *mut RcHeader;
    (*hdr).weak.set((*hdr).weak.get() + 1);
    payload
}

#[no_mangle]
pub unsafe extern "C" fn myrc_weak_drop(
    payload: *mut u8,
    payload_size: usize,
    payload_align: usize,
) {
    let offset = rc_payload_offset(payload_align);
    let base = payload.sub(offset);
    let hdr = base as *mut RcHeader;
    let w = (*hdr).weak.get();
    (*hdr).weak.set(w - 1);
    if w == 1 {
        dealloc(base, rc_block_layout(payload_size, payload_align));
    }
}

#[no_mangle]
pub unsafe extern "C" fn myrc_weak_upgrade(payload: *mut u8, payload_align: usize) -> *mut u8 {
    let hdr = payload.sub(rc_payload_offset(payload_align)) as *mut RcHeader;
    let s = (*hdr).strong.get();
    if s == 0 {
        return std::ptr::null_mut();
    }
    (*hdr).strong.set(s + 1);
    payload
}

// ---------- MyArc ----------

#[no_mangle]
pub unsafe extern "C" fn myarc_alloc(payload_size: usize, payload_align: usize) -> *mut u8 {
    let layout = arc_block_layout(payload_size, payload_align);
    let base = alloc(layout);
    if base.is_null() {
        handle_alloc_error(layout);
    }
    std::ptr::write(
        base as *mut ArcHeader,
        ArcHeader {
            strong: AtomicUsize::new(1),
            weak: AtomicUsize::new(1),
        },
    );
    base.add(arc_payload_offset(payload_align))
}

#[no_mangle]
pub unsafe extern "C" fn myarc_clone(payload: *mut u8, payload_align: usize) -> *mut u8 {
    let hdr = payload.sub(arc_payload_offset(payload_align)) as *mut ArcHeader;
    (*hdr).strong.fetch_add(1, Ordering::Relaxed);
    payload
}

#[no_mangle]
pub unsafe extern "C" fn myarc_drop(
    payload: *mut u8,
    payload_size: usize,
    payload_align: usize,
    drop_fn: Option<unsafe extern "C" fn(*mut u8)>,
) {
    let offset = arc_payload_offset(payload_align);
    let base = payload.sub(offset);
    let hdr = base as *mut ArcHeader;

    if (*hdr).strong.fetch_sub(1, Ordering::Release) == 1 {
        fence(Ordering::Acquire);
        if let Some(f) = drop_fn {
            f(payload);
        }
        if (*hdr).weak.fetch_sub(1, Ordering::Release) == 1 {
            fence(Ordering::Acquire);
            dealloc(base, arc_block_layout(payload_size, payload_align));
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn myarc_downgrade(payload: *mut u8, payload_align: usize) -> *mut u8 {
    let hdr = payload.sub(arc_payload_offset(payload_align)) as *mut ArcHeader;
    (*hdr).weak.fetch_add(1, Ordering::Relaxed);
    payload
}

#[no_mangle]
pub unsafe extern "C" fn myarc_weak_clone(payload: *mut u8, payload_align: usize) -> *mut u8 {
    let hdr = payload.sub(arc_payload_offset(payload_align)) as *mut ArcHeader;
    (*hdr).weak.fetch_add(1, Ordering::Relaxed);
    payload
}

#[no_mangle]
pub unsafe extern "C" fn myarc_weak_drop(
    payload: *mut u8,
    payload_size: usize,
    payload_align: usize,
) {
    let offset = arc_payload_offset(payload_align);
    let base = payload.sub(offset);
    let hdr = base as *mut ArcHeader;
    if (*hdr).weak.fetch_sub(1, Ordering::Release) == 1 {
        fence(Ordering::Acquire);
        dealloc(base, arc_block_layout(payload_size, payload_align));
    }
}

// ---------- SimpleRc ----------

#[no_mangle]
pub unsafe extern "C" fn simple_rc_alloc(payload_size: usize, payload_align: usize) -> *mut u8 {
    let layout = simple_rc_block_layout(payload_size, payload_align);
    let base = alloc(layout);
    if base.is_null() {
        handle_alloc_error(layout);
    }
    std::ptr::write(
        base as *mut SimpleRcHeader,
        SimpleRcHeader {
            strong: Cell::new(1),
        },
    );
    base.add(simple_rc_payload_offset(payload_align))
}

#[no_mangle]
pub unsafe extern "C" fn simple_rc_clone(payload: *mut u8, payload_align: usize) -> *mut u8 {
    let hdr = payload.sub(simple_rc_payload_offset(payload_align)) as *mut SimpleRcHeader;
    (*hdr).strong.set((*hdr).strong.get() + 1);
    payload
}

#[no_mangle]
pub unsafe extern "C" fn simple_rc_drop(
    payload: *mut u8,
    payload_size: usize,
    payload_align: usize,
    drop_fn: Option<unsafe extern "C" fn(*mut u8)>,
) {
    let offset = simple_rc_payload_offset(payload_align);
    let base = payload.sub(offset);
    let hdr = base as *mut SimpleRcHeader;
    let c = (*hdr).strong.get();
    if c == 1 {
        if let Some(f) = drop_fn {
            f(payload);
        }
        dealloc(base, simple_rc_block_layout(payload_size, payload_align));
    } else {
        (*hdr).strong.set(c - 1);
    }
}

// ---------- SimpleArc ----------

#[no_mangle]
pub unsafe extern "C" fn simple_arc_alloc(payload_size: usize, payload_align: usize) -> *mut u8 {
    let layout = simple_arc_block_layout(payload_size, payload_align);
    let base = alloc(layout);
    if base.is_null() {
        handle_alloc_error(layout);
    }
    std::ptr::write(
        base as *mut SimpleArcHeader,
        SimpleArcHeader {
            strong: AtomicUsize::new(1),
        },
    );
    base.add(simple_arc_payload_offset(payload_align))
}

#[no_mangle]
pub unsafe extern "C" fn simple_arc_clone(payload: *mut u8, payload_align: usize) -> *mut u8 {
    let hdr = payload.sub(simple_arc_payload_offset(payload_align)) as *mut SimpleArcHeader;
    (*hdr).strong.fetch_add(1, Ordering::Relaxed);
    payload
}

#[no_mangle]
pub unsafe extern "C" fn simple_arc_drop(
    payload: *mut u8,
    payload_size: usize,
    payload_align: usize,
    drop_fn: Option<unsafe extern "C" fn(*mut u8)>,
) {
    let offset = simple_arc_payload_offset(payload_align);
    let base = payload.sub(offset);
    let hdr = base as *mut SimpleArcHeader;
    if (*hdr).strong.fetch_sub(1, Ordering::Release) == 1 {
        fence(Ordering::Acquire);
        if let Some(f) = drop_fn {
            f(payload);
        }
        dealloc(base, simple_arc_block_layout(payload_size, payload_align));
    }
}

// ==================== Rust 智能指针 ====================
// ==================== Rust 智能指针（allocator_api2 风格） ====================
//
// 共享指针统一接受分配器类型参数 `A: Allocator`，默认 `Global`（全局分配器），
// 与 `allocator_api2::boxed::Box<T, A = Global>` / `allocator_api2::vec::Vec<T, A = Global>`
// 同形。运行态用 `crate::mem::SlotAlloc`（8 字节，路由到 MemState 的 allocf 槽，
// 即 C Lua 的 `global_State.allocf`），测试/构造期用默认的 `Global`。
//
// 与 `Box` 的唯一差别：分配器存放在**分配块内部**（header 与 payload 之间），
// 而不是句柄内。原因：句柄必须保持 8 字节 thin —— `TValue` 总宽 16 字节，句柄一旦
// 变成 (ptr, allocator) 胖指针，`TValue` 会膨胀到 24 字节，内存流量直接翻倍。
// 释放时从块内取回 `A`（strong 归零且无 weak，或 weak 归零路径）。
//
// `A = Global` 是 ZST（size 0 / align 1），块布局退化为 header + payload，与不含
// 分配器时逐字节一致（零开销）；`A = SlotAlloc` 时块布局 = header + A(8B) + payload。

#[cfg(not(feature = "nightly-allocator"))]
pub use allocator_api2::alloc::{Allocator, Global};
#[cfg(feature = "nightly-allocator")]
pub use core::alloc::{Allocator, Global};

/// 向上取整到 `a` 的倍数（`a` 为 2 的幂）。
#[inline(always)]
fn align_up(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

/// `MyRc` 块内分配器字段的偏移（紧随 header）。
#[inline(always)]
fn rc_alloc_off<A>() -> usize {
    align_up(RC_HEADER_SIZE, std::mem::align_of::<A>())
}
/// `MyRc` 块内业务 payload 的偏移。
#[inline(always)]
fn rc_data_off<A, T: ?Sized + RcPayload>() -> usize {
    align_up(rc_alloc_off::<A>() + std::mem::size_of::<A>(), T::ALIGN)
}
/// `MyRc` 整块布局。
#[inline(always)]
fn rc_layout<A, T: ?Sized + RcPayload>(payload_size: usize) -> Layout {
    let align = T::ALIGN
        .max(std::mem::align_of::<A>())
        .max(std::mem::align_of::<RcHeader>());
    Layout::from_size_align(
        align_up(rc_data_off::<A, T>() + payload_size, align).max(align),
        align,
    )
    .unwrap()
}

/// `SimpleRc`/`SimpleArc` 块内分配器字段的偏移（紧随 header）。
#[inline(always)]
pub(crate) fn simple_alloc_off<A>() -> usize {
    align_up(SIMPLE_RC_HEADER_SIZE, std::mem::align_of::<A>())
}
/// `SimpleRc`/`SimpleArc` 块内业务 payload 的偏移。
#[inline(always)]
pub(crate) fn simple_data_off<A, T: ?Sized + RcPayload>() -> usize {
    align_up(simple_alloc_off::<A>() + std::mem::size_of::<A>(), T::ALIGN)
}
/// `SimpleRc`/`SimpleArc` 整块布局。
#[inline(always)]
pub(crate) fn simple_layout<A, T: ?Sized + RcPayload>(payload_size: usize) -> Layout {
    let align = T::ALIGN
        .max(std::mem::align_of::<A>())
        .max(std::mem::align_of::<SimpleRcHeader>());
    Layout::from_size_align(
        align_up(simple_data_off::<A, T>() + payload_size, align).max(align),
        align,
    )
    .unwrap()
}

pub trait IsArcRc {
    type Box: ?Sized;
    type Header;
}

// ---------- MyRc ----------

/// Rc 语义共享指针（strong + weak），分配器 `A` 存于块内，默认 [`Global`]。
///
/// 块布局 = [`RcHeader`] + `A` + payload；`A = Global` 时 `size_of::<A>() == 0`，
/// 布局与 `[`RcBox`]` 一致。
pub struct MyRc<T: ?Sized + RcPayload, A: Allocator = Global> {
    /// 块基址（thin 指针；payload 经 `T::as_ref` 重建）
    ptr: NonNull<u8>,
    _p: PhantomData<(A, T)>,
}

impl<T: ?Sized + RcPayload, A: Allocator> MyRc<T, A> {
    #[inline]
    pub fn as_raw_ptr(&self) -> *mut u8 {
        unsafe { self.ptr.as_ptr().add(rc_data_off::<A, T>()) }
    }

    pub fn clone(&self) -> Self {
        unsafe {
            let h = &*(self.ptr.as_ptr() as *const RcHeader);
            h.strong.set(h.strong.get() + 1);
        }
        MyRc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }

    #[inline]
    pub fn as_ptr(&self) -> *const T {
        unsafe { T::as_ref(self.as_raw_ptr()) as *const T }
    }
    #[inline]
    pub fn as_ref(&self) -> &T {
        unsafe { T::as_ref(self.as_raw_ptr()) }
    }
    #[inline]
    pub fn downgrade(&self) -> WeakRc<T, A> {
        WeakRc::new(self)
    }
    #[inline]
    pub fn strong_count(&self) -> usize {
        unsafe { (*(self.ptr.as_ptr() as *const RcHeader)).strong.get() }
    }
    #[inline]
    pub fn weak_count(&self) -> usize {
        unsafe { (*(self.ptr.as_ptr() as *const RcHeader)).weak.get() }
    }
    #[inline]
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        std::ptr::addr_eq(this.ptr.as_ptr(), other.ptr.as_ptr())
    }
    /// 取出块内分配器的副本（`A: Copy` 时零成本；[`Global`] 为 ZST）。
    #[inline]
    pub fn allocator(&self) -> A {
        unsafe { std::ptr::read(self.ptr.as_ptr().add(rc_alloc_off::<A>()) as *const A) }
    }

    /// # Safety
    /// - `base` 必须是 [`rc_layout`] 分配并已初始化的块基址
    /// - 调用方转移给它一份 strong 引用份额
    pub unsafe fn from_raw_base(base: *mut u8) -> Self {
        Self {
            ptr: NonNull::new_unchecked(base),
            _p: PhantomData,
        }
    }
}

impl<T: RcPayload, A: Allocator> MyRc<T, A> {
    /// 用分配器 `alloc` 创建共享块。
    pub fn new_in(alloc: A, value: T) -> Self {
        unsafe {
            let layout = rc_layout::<A, T>(std::mem::size_of::<T>());
            let base = alloc
                .allocate(layout)
                .unwrap_or_else(|_| handle_alloc_error(layout))
                .cast::<u8>()
                .as_ptr();
            std::ptr::write(base as *mut RcHeader, RcHeader::default());
            std::ptr::write(base.add(rc_alloc_off::<A>()) as *mut A, alloc);
            std::ptr::write(base.add(rc_data_off::<A, T>()) as *mut T, value);
            MyRc {
                ptr: NonNull::new_unchecked(base),
                _p: PhantomData,
            }
        }
    }
}

impl<T: RcPayload> MyRc<T, Global> {
    /// 用全局分配器创建共享块。
    pub fn new(value: T) -> Self {
        Self::new_in(Global, value)
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Drop for MyRc<T, A> {
    fn drop(&mut self) {
        unsafe {
            let base = self.ptr.as_ptr();
            let hdr = &*(base as *const RcHeader);
            let s = hdr.strong.get();
            debug_assert!(s > 0, "MyRc::drop 时 strong 不应为 0");
            hdr.strong.set(s - 1);

            if s == 1 {
                let payload = base.add(rc_data_off::<A, T>());
                T::drop_payload(payload);
                let payload_size = T::payload_size(payload);

                // strong 隐含持有 1 份 weak 份额（RcHeader::default weak=1）
                let w = hdr.weak.get();
                debug_assert!(w > 0);
                hdr.weak.set(w - 1);

                if w == 1 {
                    let layout = rc_layout::<A, T>(payload_size);
                    let alloc = std::ptr::read(base.add(rc_alloc_off::<A>()) as *const A);
                    alloc.deallocate(NonNull::new_unchecked(base), layout);
                    drop(alloc);
                }
            }
        }
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Clone for MyRc<T, A> {
    #[inline]
    fn clone(&self) -> Self {
        MyRc::clone(self)
    }
}
impl<T: ?Sized + RcPayload, A: Allocator> Deref for MyRc<T, A> {
    type Target = T;
    fn deref(&self) -> &T {
        self.as_ref()
    }
}
impl<T: ?Sized + RcPayload, A: Allocator> Debug for MyRc<T, A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MyRc").field("ptr", &self.ptr).finish()
    }
}
impl<T: ?Sized + RcPayload, A: Allocator> IsArcRc for MyRc<T, A> {
    type Box = RcBox<T>;
    type Header = RcHeader;
}

// ---------- WeakRc ----------

pub struct WeakRc<T: ?Sized + RcPayload, A: Allocator = Global> {
    ptr: NonNull<u8>,
    _p: PhantomData<(A, T)>,
}

impl<T: ?Sized + RcPayload, A: Allocator> WeakRc<T, A> {
    pub fn new(r: &MyRc<T, A>) -> Self {
        unsafe {
            let hdr = &*(r.ptr.as_ptr() as *const RcHeader);
            hdr.weak.set(hdr.weak.get() + 1);
        }
        WeakRc {
            ptr: r.ptr,
            _p: PhantomData,
        }
    }
    pub fn clone(&self) -> Self {
        unsafe {
            let hdr = &*(self.ptr.as_ptr() as *const RcHeader);
            hdr.weak.set(hdr.weak.get() + 1);
        }
        WeakRc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }
    pub fn upgrade(&self) -> Option<MyRc<T, A>> {
        unsafe {
            let hdr = &*(self.ptr.as_ptr() as *const RcHeader);
            let s = hdr.strong.get();
            if s == 0 {
                return None;
            }
            hdr.strong.set(s + 1);
            Some(MyRc {
                ptr: self.ptr,
                _p: PhantomData,
            })
        }
    }
    #[inline]
    pub fn strong_count(&self) -> usize {
        unsafe { (*(self.ptr.as_ptr() as *const RcHeader)).strong.get() }
    }
    #[inline]
    pub fn weak_count(&self) -> usize {
        unsafe { (*(self.ptr.as_ptr() as *const RcHeader)).weak.get() }
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Drop for WeakRc<T, A> {
    fn drop(&mut self) {
        unsafe {
            let base = self.ptr.as_ptr();
            let hdr = &*(base as *const RcHeader);
            let w = hdr.weak.get();
            debug_assert!(w > 0, "WeakRc::drop 时 weak 不应为 0");
            hdr.weak.set(w - 1);

            if w == 1 {
                debug_assert_eq!(hdr.strong.get(), 0, "weak 归零时 strong 应已为 0");
                // payload 已随 strong 归零 drop，仅计算布局并释放块
                let payload_size = T::payload_size(base.add(rc_data_off::<A, T>()));
                let layout = rc_layout::<A, T>(payload_size);
                let alloc = std::ptr::read(base.add(rc_alloc_off::<A>()) as *const A);
                alloc.deallocate(NonNull::new_unchecked(base), layout);
                drop(alloc);
            }
        }
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Clone for WeakRc<T, A> {
    #[inline]
    fn clone(&self) -> Self {
        WeakRc::clone(self)
    }
}
impl<T: ?Sized + RcPayload, A: Allocator> Debug for WeakRc<T, A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakRc").field("ptr", &self.ptr).finish()
    }
}

// ==================== SimpleRc（薄指针 + RcPayload） ====================

pub struct SimpleRc<T: ?Sized + RcPayload, A: Allocator = Global> {
    ptr: NonNull<u8>,
    _p: PhantomData<(A, T)>,
}

impl<T: ?Sized + RcPayload, A: Allocator> SimpleRc<T, A> {
    #[inline]
    pub fn as_raw_ptr(&self) -> *mut u8 {
        unsafe { self.ptr.as_ptr().add(simple_data_off::<A, T>()) }
    }
    /// payload 起点（薄指针）。
    pub fn data_ptr(&self) -> *mut u8 {
        self.as_raw_ptr()
    }
    #[inline]
    pub fn as_ptr(&self) -> *const T {
        unsafe { T::as_ref(self.as_raw_ptr()) as *const T }
    }
    #[inline]
    pub fn as_ref(&self) -> &T {
        unsafe { T::as_ref(self.as_raw_ptr()) }
    }

    pub fn clone(&self) -> Self {
        unsafe {
            let hdr = self.ptr.as_ptr().cast::<SimpleRcHeader>();
            let s = (*hdr).strong.get();
            (*hdr).strong.set(s + 1);
        }
        SimpleRc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }

    #[inline]
    pub fn strong_count(&self) -> usize {
        unsafe { (*(self.ptr.as_ptr() as *const SimpleRcHeader)).strong.get() }
    }

    #[inline]
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        this.ptr == other.ptr
    }

    /// 取出块内分配器的副本（`A: Copy` 时零成本；[`Global`] 为 ZST）。
    #[inline]
    pub fn allocator(&self) -> A {
        unsafe { std::ptr::read(self.ptr.as_ptr().add(simple_alloc_off::<A>()) as *const A) }
    }

    /// # Safety
    /// - `base` 必须是 [`simple_layout`] 分配并已初始化的块基址
    /// - 调用方转移给它一份 strong 引用份额
    pub unsafe fn from_raw_base(base: *mut u8) -> Self {
        Self {
            ptr: NonNull::new_unchecked(base),
            _p: PhantomData,
        }
    }
}

impl<T: RcPayload, A: Allocator> SimpleRc<T, A> {
    /// 用分配器 `alloc` 创建薄共享块。
    pub fn new_in(alloc: A, value: T) -> Self {
        unsafe {
            let layout = simple_layout::<A, T>(std::mem::size_of::<T>());
            let base = alloc
                .allocate(layout)
                .unwrap_or_else(|_| handle_alloc_error(layout))
                .cast::<u8>()
                .as_ptr();
            std::ptr::write(base as *mut SimpleRcHeader, SimpleRcHeader::default());
            std::ptr::write(base.add(simple_alloc_off::<A>()) as *mut A, alloc);
            std::ptr::write(base.add(simple_data_off::<A, T>()) as *mut T, value);
            SimpleRc {
                ptr: NonNull::new_unchecked(base),
                _p: PhantomData,
            }
        }
    }
}

impl<T: RcPayload> SimpleRc<T, Global> {
    pub fn new(value: T) -> Self {
        Self::new_in(Global, value)
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Drop for SimpleRc<T, A> {
    fn drop(&mut self) {
        unsafe {
            let base = self.ptr.as_ptr();
            let hdr = base as *mut SimpleRcHeader;
            let s = (*hdr).strong.get();
            debug_assert!(s > 0);
            (*hdr).strong.set(s - 1);

            if s == 1 {
                let payload = base.add(simple_data_off::<A, T>());
                T::drop_payload(payload);
                let payload_size = T::payload_size(payload);
                let layout = simple_layout::<A, T>(payload_size);
                let alloc = std::ptr::read(base.add(simple_alloc_off::<A>()) as *const A);
                alloc.deallocate(NonNull::new_unchecked(base), layout);
                drop(alloc);
            }
        }
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Clone for SimpleRc<T, A> {
    #[inline]
    fn clone(&self) -> Self {
        SimpleRc::clone(self)
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Deref for SimpleRc<T, A> {
    type Target = T;
    fn deref(&self) -> &T {
        self.as_ref()
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Debug for SimpleRc<T, A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimpleRc").field("ptr", &self.ptr).finish()
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> IsArcRc for SimpleRc<T, A> {
    type Box = SimpleRcBox<T>;
    type Header = SimpleRcHeader;
}

// ==================== SimpleArc（薄指针 + RcPayload） ====================

pub struct SimpleArc<T: ?Sized + RcPayload, A: Allocator = Global> {
    ptr: NonNull<u8>,
    _p: PhantomData<(A, T)>,
}

unsafe impl<T: ?Sized + RcPayload + Send + Sync, A: Allocator + Send + Sync> Send
    for SimpleArc<T, A>
{
}
unsafe impl<T: ?Sized + RcPayload + Send + Sync, A: Allocator + Send + Sync> Sync
    for SimpleArc<T, A>
{
}

impl<T: ?Sized + RcPayload, A: Allocator> SimpleArc<T, A> {
    #[inline]
    pub fn as_raw_ptr(&self) -> *mut u8 {
        unsafe { self.ptr.as_ptr().add(simple_data_off::<A, T>()) }
    }
    pub fn data_ptr(&self) -> *mut u8 {
        self.as_raw_ptr()
    }
    pub fn clone(&self) -> Self {
        unsafe {
            let hdr = self.ptr.as_ptr() as *mut SimpleArcHeader;
            (*hdr).strong.fetch_add(1, Ordering::Relaxed);
        }
        SimpleArc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }
    #[inline]
    pub fn as_ptr(&self) -> *const T {
        unsafe { T::as_ref(self.as_raw_ptr()) as *const T }
    }
    #[inline]
    pub fn as_ref(&self) -> &T {
        unsafe { T::as_ref(self.as_raw_ptr()) }
    }
    #[inline]
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        this.ptr == other.ptr
    }
    #[inline]
    pub fn strong_count(&self) -> usize {
        unsafe {
            (*(self.ptr.as_ptr() as *const SimpleArcHeader))
                .strong
                .load(Ordering::Relaxed)
        }
    }
    /// # Safety
    /// - `base` 必须是 [`simple_layout`] 分配并已初始化的块基址
    pub unsafe fn from_raw_base(base: *mut u8) -> Self {
        Self {
            ptr: NonNull::new_unchecked(base),
            _p: PhantomData,
        }
    }
}

impl<T: RcPayload, A: Allocator> SimpleArc<T, A> {
    pub fn new_in(alloc: A, value: T) -> Self {
        unsafe {
            let layout = simple_layout::<A, T>(std::mem::size_of::<T>());
            let base = alloc
                .allocate(layout)
                .unwrap_or_else(|_| handle_alloc_error(layout))
                .cast::<u8>()
                .as_ptr();
            std::ptr::write(base as *mut SimpleArcHeader, SimpleArcHeader::default());
            std::ptr::write(base.add(simple_alloc_off::<A>()) as *mut A, alloc);
            std::ptr::write(base.add(simple_data_off::<A, T>()) as *mut T, value);
            SimpleArc {
                ptr: NonNull::new_unchecked(base),
                _p: PhantomData,
            }
        }
    }
}

impl<T: RcPayload> SimpleArc<T, Global> {
    pub fn new(value: T) -> Self {
        Self::new_in(Global, value)
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Drop for SimpleArc<T, A> {
    fn drop(&mut self) {
        unsafe {
            let base = self.ptr.as_ptr();
            let hdr = base as *mut SimpleArcHeader;
            if (*hdr).strong.fetch_sub(1, Ordering::Release) == 1 {
                fence(Ordering::Acquire);
                let payload = base.add(simple_data_off::<A, T>());
                T::drop_payload(payload);
                let payload_size = T::payload_size(payload);
                let layout = simple_layout::<A, T>(payload_size);
                let alloc = std::ptr::read(base.add(simple_alloc_off::<A>()) as *const A);
                alloc.deallocate(NonNull::new_unchecked(base), layout);
                drop(alloc);
            }
        }
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Clone for SimpleArc<T, A> {
    #[inline]
    fn clone(&self) -> Self {
        SimpleArc::clone(self)
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Deref for SimpleArc<T, A> {
    type Target = T;
    fn deref(&self) -> &T {
        self.as_ref()
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> Debug for SimpleArc<T, A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimpleArc").field("ptr", &self.ptr).finish()
    }
}

impl<T: ?Sized + RcPayload, A: Allocator> IsArcRc for SimpleArc<T, A> {
    type Box = SimpleArcBox<T>;
    type Header = SimpleArcHeader;
}

// ==================== 测试 ====================

#[cfg(test)]
struct DropProbe(Arc<AtomicUsize>);

#[cfg(test)]
impl DropProbe {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let c = Arc::new(AtomicUsize::new(0));
        (DropProbe(c.clone()), c)
    }
}

#[cfg(test)]
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod test_helpers {
    use super::*;

    #[inline]
    unsafe fn header_from_payload<H>(payload: *mut u8, offset: usize) -> &'static H {
        &*(payload.sub(offset) as *const H)
    }

    pub fn rc_header<T>(r: &MyRc<T>, _align: usize) -> &RcHeader {
        unsafe {
            let payload =
                (r.ptr.as_ptr() as *mut u8).add(rc_payload_offset(std::mem::align_of::<T>()));
            header_from_payload::<RcHeader>(payload, rc_payload_offset(std::mem::align_of::<T>()))
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn header_sizes_are_fixed() {
        assert_eq!(size_of::<RcHeader>(), 16);
        assert_eq!(size_of::<ArcHeader>(), 16);
        assert_eq!(size_of::<SimpleRcHeader>(), 8);
        assert_eq!(size_of::<SimpleArcHeader>(), 8);
    }

    #[test]
    fn payload_offset_matches_repr_c() {
        assert_eq!(rc_payload_offset(1), 16);
        assert_eq!(rc_payload_offset(4), 16);
        assert_eq!(rc_payload_offset(8), 16);
        assert_eq!(rc_payload_offset(16), 16);
        assert_eq!(rc_payload_offset(32), 32);

        assert_eq!(simple_rc_payload_offset(1), 8);
        assert_eq!(simple_rc_payload_offset(8), 8);
        assert_eq!(simple_rc_payload_offset(16), 16);
        assert_eq!(simple_rc_payload_offset(32), 32);

        assert_eq!(simple_arc_payload_offset(16), 16);
    }

    #[test]
    fn block_layout_covers_header_and_payload() {
        let l = rc_block_layout(8, 8);
        assert!(l.size() >= 24);
        assert_eq!(l.align(), 8);

        let l = simple_rc_block_layout(0, 8);
        assert!(l.size() >= 8);

        let l = rc_block_layout(16, 64);
        assert_eq!(l.align(), 64);
        assert!(l.size() % 64 == 0);
    }

    #[test]
    fn repr_c_layout_matches_manual_offset() {
        let boxed: RcBox<u64> = RcBox {
            header: RcHeader {
                strong: Cell::new(0),
                weak: Cell::new(0),
            },
            value: 0,
        };
        let base = &boxed as *const _ as usize;
        let vaddr = &boxed.value as *const _ as usize;
        assert_eq!(vaddr - base, rc_payload_offset(std::mem::align_of::<u64>()));

        let boxed: SimpleRcBox<u64> = SimpleRcBox {
            header: SimpleRcHeader {
                strong: Cell::new(0),
            },
            value: 0,
        };
        let base = &boxed as *const _ as usize;
        let vaddr = &boxed.value as *const _ as usize;
        assert_eq!(
            vaddr - base,
            simple_rc_payload_offset(std::mem::align_of::<u64>())
        );
    }

    #[test]
    fn simple_rc_is_thin() {
        assert_eq!(std::mem::size_of::<SimpleRc<u64>>(), 8);
        assert_eq!(std::mem::size_of::<SimpleArc<u64>>(), 8);
    }

    /// 分配器类型参数 `A` 的布局契约：
    /// - `A = Global`（ZST）必须与旧「header + payload」布局逐字节一致；
    /// - `A = SlotAlloc`（8 字节）必须与旧「header + AllocSlot + payload」路由布局一致；
    /// - 句柄在两种 `A` 下都必须是 8 字节 thin 指针（`TValue` 总宽 16 字节的前提）。
    #[test]
    fn allocator_param_layout() {
        use crate::mem::SlotAlloc;

        // A = Global ≡ 旧 non-routed 布局
        assert_eq!(rc_alloc_off::<Global>(), RC_HEADER_SIZE);
        assert_eq!(rc_data_off::<Global, u64>(), rc_payload_offset(8));
        assert_eq!(
            rc_layout::<Global, u64>(8).size(),
            rc_block_layout(8, 8).size()
        );
        assert_eq!(
            rc_layout::<Global, u64>(8).align(),
            rc_block_layout(8, 8).align()
        );
        assert_eq!(
            simple_data_off::<Global, u64>(),
            simple_rc_payload_offset(8)
        );
        assert_eq!(
            simple_layout::<Global, u64>(8).size(),
            simple_rc_block_layout(8, 8).size()
        );

        // A = SlotAlloc ≡ 旧 routed 布局（header(16) + A(8) + payload）
        assert_eq!(rc_alloc_off::<SlotAlloc>(), RC_HEADER_SIZE);
        assert_eq!(rc_data_off::<SlotAlloc, u64>(), 24);
        assert_eq!(rc_layout::<SlotAlloc, u64>(8).size(), 32);
        assert_eq!(simple_data_off::<SlotAlloc, u64>(), 16);
        assert_eq!(simple_layout::<SlotAlloc, u64>(8).size(), 24);

        // 句柄 thin 不变量
        assert_eq!(std::mem::size_of::<MyRc<u64, Global>>(), 8);
        assert_eq!(std::mem::size_of::<MyRc<u64, SlotAlloc>>(), 8);
        assert_eq!(std::mem::size_of::<WeakRc<u64, SlotAlloc>>(), 8);
        assert_eq!(std::mem::size_of::<SimpleRc<u64, SlotAlloc>>(), 8);
        assert_eq!(std::mem::size_of::<SimpleArc<u64, SlotAlloc>>(), 8);

        // 句柄必须保持 thin 的最终目的：TValue 总宽 16 字节
        assert_eq!(std::mem::size_of::<crate::objects::TValue<'static>>(), 16);
    }

    /// 经 `new_in(SlotAlloc, ..)` 创建的块，其 payload 在两种分配器下都能
    /// 正确读写，且强/弱计数与释放路径一致（含 `Global` 默认路径）。
    #[test]
    fn allocator_param_roundtrip() {
        let alloc = crate::mem::TestAlloc::new();
        use crate::mem::SlotAlloc;

        let g = MyRc::<u64, Global>::new(7);
        assert_eq!(*g.as_ref(), 7);
        assert_eq!(g.strong_count(), 1);
        drop(g.clone());

        let slot = alloc.slot();
        let s = MyRc::<u64, SlotAlloc>::new_in(slot, 9);
        assert_eq!(*s.as_ref(), 9);
        // 存进块内、释放时取用的就是传入的那个分配器（非 null 不变量）
        assert_eq!(s.allocator().slot(), slot.slot());
        let w = s.downgrade();
        drop(s);
        assert_eq!(w.strong_count(), 0);
        assert!(w.upgrade().is_none());

        let sr_slot = alloc.slot();
        let sr = SimpleRc::<u64, SlotAlloc>::new_in(sr_slot, 11);
        assert_eq!(*sr.as_ref(), 11);
        assert_eq!(sr.allocator().slot(), sr_slot.slot());
        let sr2 = sr.clone();
        drop(sr);
        assert_eq!(*sr2.as_ref(), 11);
    }
}

#[cfg(test)]
mod simple_rc_tests {
    use super::*;

    #[test]
    fn new_and_get() {
        let r = SimpleRc::new(42u32);
        assert_eq!(*r.as_ref(), 42);
    }

    #[test]
    fn clone_increments_count() {
        let r = SimpleRc::new(7u8);
        let base =
            unsafe { r.as_raw_ptr().sub(simple_rc_payload_offset(1)) } as *const SimpleRcHeader;
        assert_eq!(unsafe { (*base).strong.get() }, 1);

        let r2 = r.clone();
        assert_eq!(unsafe { (*base).strong.get() }, 2);

        let r3 = r2.clone();
        assert_eq!(unsafe { (*base).strong.get() }, 3);

        drop(r3);
        assert_eq!(unsafe { (*base).strong.get() }, 2);
        drop(r2);
        assert_eq!(unsafe { (*base).strong.get() }, 1);
    }

    #[test]
    fn drop_only_once_at_zero() {
        let (probe, counter) = DropProbe::new();
        let r = SimpleRc::new(probe);
        let r2 = r.clone();
        drop(r);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        drop(r2);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn jit_style_alloc_clone_drop() {
        unsafe {
            let payload = simple_rc_alloc(4, 4);
            *(payload as *mut u32) = 0xDEAD_BEEF;

            let p2 = simple_rc_clone(payload, 4);
            assert_eq!(p2, payload);

            extern "C" fn dtor(p: *mut u8) {
                unsafe {
                    *(p as *mut u32) = 0;
                }
            }

            simple_rc_drop(payload, 4, 4, Some(dtor));
            simple_rc_drop(p2, 4, 4, None);
        }
    }

    #[test]
    fn jit_alloc_zero_size_payload() {
        unsafe {
            let p = simple_rc_alloc(0, 1);
            assert!(!p.is_null());
            simple_rc_drop(p, 0, 1, None);
        }
    }

    #[test]
    fn high_alignment_payload() {
        #[repr(align(64))]
        struct Aligned64(u8);

        let r = SimpleRc::new(Aligned64(9));
        assert_eq!(r.as_raw_ptr() as usize % 64, 0);
        assert_eq!(r.as_ref().0, 9);
    }
}

#[cfg(test)]
mod weak_rc_tests {
    use super::*;
    use crate::alloc::test_helpers::rc_header;

    #[test]
    fn downgrade_increments_weak() {
        let r = MyRc::new(1u32);
        let h = rc_header(&r, 4);
        assert_eq!(h.strong.get(), 1);
        assert_eq!(h.weak.get(), 1);

        let w = r.downgrade();
        assert_eq!(h.strong.get(), 1);
        assert_eq!(h.weak.get(), 2);

        drop(w);
        assert_eq!(h.weak.get(), 1);
    }

    #[test]
    fn weak_clone_and_drop() {
        let r = MyRc::new(2u64);
        let w = r.downgrade();
        let h = rc_header(&r, 8);
        assert_eq!(h.weak.get(), 2);

        let w2 = w.clone();
        let w3 = w.clone();
        assert_eq!(h.weak.get(), 4);

        drop(w2);
        assert_eq!(h.weak.get(), 3);
        drop(w3);
        assert_eq!(h.weak.get(), 2);
        drop(w);
        assert_eq!(h.weak.get(), 1);
    }

    #[test]
    fn upgrade_succeeds_while_strong_alive() {
        let r = MyRc::new(42u32);
        let w = r.downgrade();

        let up = w.upgrade().expect("strong > 0");
        assert_eq!(*up.as_ref(), 42);
        assert_eq!(r.strong_count(), 2);
        assert_eq!(r.weak_count(), 2);
    }

    #[test]
    fn upgrade_fails_after_strong_dropped() {
        let r = MyRc::new(42u32);
        let w = r.downgrade();

        drop(r);
        assert_eq!(w.strong_count(), 0);
        assert_eq!(w.weak_count(), 1);

        assert!(w.upgrade().is_none());
    }

    #[test]
    fn upgrade_then_drop_returns_to_zero() {
        let r = MyRc::new(7u8);
        let w = r.downgrade();
        {
            let up = w.upgrade().unwrap();
            assert_eq!(*up.as_ref(), 7);
        }
        assert_eq!(r.strong_count(), 1);
        assert_eq!(r.weak_count(), 2);

        drop(r);
        drop(w);
    }

    #[test]
    fn payload_dropped_when_strong_zero_even_if_weak_alive() {
        let (probe, counter) = DropProbe::new();
        let r = MyRc::new(probe);
        let w = r.downgrade();

        assert_eq!(counter.load(Ordering::SeqCst), 0);
        drop(r);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(w.strong_count(), 0);
        assert_eq!(w.weak_count(), 1);

        drop(w);
    }

    #[test]
    fn memory_freed_only_when_weak_zero() {
        let r = MyRc::new(vec![1u8, 2, 3]);
        let w = r.downgrade();
        drop(r);

        assert_eq!(w.weak_count(), 1);
        assert_eq!(w.strong_count(), 0);
        assert!(w.upgrade().is_none());

        drop(w);
    }

    #[test]
    fn multiple_weaks_extend_lifetime() {
        let (probe, counter) = DropProbe::new();
        let r = MyRc::new(probe);
        let w1 = r.downgrade();
        let w2 = r.downgrade();
        let w3 = r.downgrade();

        drop(r);
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        drop(w1);
        drop(w2);
        drop(w3);
    }

    #[test]
    fn weak_cycle_no_leak() {
        let (probe, counter) = DropProbe::new();
        {
            let r = MyRc::new(probe);
            let w = r.downgrade();
            drop(r);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            drop(w);
        }
    }

    #[test]
    fn jit_downgrade_clone_upgrade_drop() {
        unsafe {
            let p = myrc_alloc(4, 4);
            *(p as *mut u32) = 99;

            let w = myrc_downgrade(p, 4);
            let w2 = myrc_weak_clone(w, 4);

            let up = myrc_weak_upgrade(w, 4);
            assert!(!up.is_null());
            assert_eq!(up, p);
            assert_eq!(*(up as *const u32), 99);

            myrc_drop(up, 4, 4, None);
            myrc_drop(p, 4, 4, None);

            myrc_weak_drop(w2, 4, 4);
            myrc_weak_drop(w, 4, 4);
        }
    }

    #[test]
    fn jit_upgrade_fails_after_strong_gone() {
        unsafe {
            let p = myrc_alloc(4, 4);
            *(p as *mut u32) = 5;

            let w = myrc_downgrade(p, 4);
            myrc_drop(p, 4, 4, None);

            let up = myrc_weak_upgrade(w, 4);
            assert!(up.is_null());

            myrc_weak_drop(w, 4, 4);
        }
    }

    #[test]
    fn jit_weak_zero_releases_memory() {
        unsafe {
            let p = myrc_alloc(16, 8);
            let w1 = myrc_downgrade(p, 8);
            let w2 = myrc_weak_clone(p, 8);
            myrc_drop(p, 16, 8, None);
            myrc_weak_drop(w1, 16, 8);
            myrc_weak_drop(w2, 16, 8);
        }
    }
}

#[cfg(test)]
mod myrc_tests {
    use super::*;

    #[test]
    fn strong_and_weak_counts() {
        let r = MyRc::new(123u64);
        let base = unsafe { r.as_raw_ptr().sub(rc_payload_offset(8)) } as *const RcHeader;
        unsafe {
            assert_eq!((*base).strong.get(), 1);
            assert_eq!((*base).weak.get(), 1);
        }
        let r2 = r.clone();
        unsafe {
            assert_eq!((*base).strong.get(), 2);
            assert_eq!((*base).weak.get(), 1);
        }
        drop(r2);
        unsafe {
            assert_eq!((*base).strong.get(), 1);
        }
    }

    #[test]
    fn drop_once_when_strong_zero() {
        let (probe, c) = DropProbe::new();
        let r = MyRc::new(probe);
        let r2 = r.clone();
        drop(r);
        assert_eq!(c.load(Ordering::SeqCst), 0);
        drop(r2);
        assert_eq!(c.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn jit_roundtrip() {
        unsafe {
            let p = myrc_alloc(8, 8);
            *(p as *mut u64) = 0x1234_5678_9ABC_DEF0;

            let p2 = myrc_clone(p, 8);
            assert_eq!(p, p2);

            myrc_drop(p, 8, 8, None);
            myrc_drop(p2, 8, 8, None);
        }
    }
}

#[cfg(test)]
mod simple_arc_single_thread {
    use super::*;

    #[test]
    fn clone_and_drop() {
        let (probe, c) = DropProbe::new();
        let a = SimpleArc::new(probe);
        let b = a.clone();
        let d = a.clone();
        drop(b);
        assert_eq!(c.load(Ordering::SeqCst), 0);
        drop(d);
        assert_eq!(c.load(Ordering::SeqCst), 0);
        drop(a);
        assert_eq!(c.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn jit_roundtrip() {
        unsafe {
            let p = simple_arc_alloc(16, 8);
            *(p as *mut u64) = 11;
            *(p.add(8) as *mut u64) = 22;

            let p2 = simple_arc_clone(p, 8);
            assert_eq!(p, p2);

            simple_arc_drop(p2, 16, 8, None);
            simple_arc_drop(p, 16, 8, None);
        }
    }
}

#[cfg(test)]
mod arc_thread_tests {
    use super::*;
    use std::{sync::Barrier, thread};

    #[test]
    fn simple_arc_concurrent_clone_drop() {
        const N: usize = 8;
        let (probe, counter) = DropProbe::new();
        let a = SimpleArc::new(probe);

        let barrier = Arc::new(Barrier::new(N));
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let a = a.clone();
                let b = barrier.clone();
                thread::spawn(move || {
                    b.wait();
                    let clones: Vec<_> = (0..100).map(|_| a.clone()).collect();
                    drop(clones);
                    drop(a);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        drop(a);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn simple_arc_cross_thread_jit_drop() {
        let (probe, counter) = DropProbe::new();
        let a = SimpleArc::new(probe);
        let p_addr = a.as_raw_ptr() as usize;

        let h = thread::spawn(move || {
            let p = p_addr as *mut u8;
            unsafe {
                let p2 = simple_arc_clone(p, std::mem::align_of::<DropProbe>());
                extern "C" fn dtor(ptr: *mut u8) {
                    unsafe {
                        std::ptr::drop_in_place(ptr as *mut DropProbe);
                    }
                }
                simple_arc_drop(
                    p2,
                    std::mem::size_of::<DropProbe>(),
                    std::mem::align_of::<DropProbe>(),
                    Some(dtor),
                );
            }
        });
        h.join().unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        drop(a);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod interop_tests {
    use super::*;

    #[test]
    fn rust_to_jit_to_rust_simple_rc() {
        let r = SimpleRc::new(99u32);
        let p = r.as_raw_ptr();

        let p2 = unsafe { simple_rc_clone(p, 4) };
        assert_eq!(p, p2);

        unsafe {
            assert_eq!(*(p2 as *const u32), 99);
            simple_rc_drop(p2, 4, 4, None);
        }
        drop(r);
    }

    #[test]
    fn ptr_stability_across_clone() {
        let r = SimpleArc::new([1u64, 2, 3]);
        let p = r.as_raw_ptr();
        let r2 = r.clone();
        assert_eq!(r.as_raw_ptr(), p);
        assert_eq!(r2.as_raw_ptr(), p);
    }
}
