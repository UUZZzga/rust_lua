#[cfg(test)]
use std::sync::Arc;
use std::{
    alloc::{alloc, dealloc, handle_alloc_error, Layout},
    cell::Cell,
    marker::PhantomData,
    ptr::NonNull,
    sync::atomic::{fence, AtomicUsize, Ordering},
}; // 这里 import 是为了 impl 内部能用 write_str 等方法

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

// 静态暴露给 JIT 的偏移常量（可选，方便 JIT 直接读计数）
pub const RC_HEADER_SIZE: usize = std::mem::size_of::<RcHeader>();
pub const ARC_HEADER_SIZE: usize = std::mem::size_of::<ArcHeader>();
pub const STRONG_OFFSET: usize = 0;
pub const WEAK_OFFSET: usize = std::mem::size_of::<usize>();
pub const SIMPLE_RC_HEADER_SIZE: usize = std::mem::size_of::<SimpleRcHeader>(); // == 8
pub const SIMPLE_ARC_HEADER_SIZE: usize = std::mem::size_of::<SimpleArcHeader>(); // == 8

// ---------- 带泛型的 Rust 布局 ----------

#[repr(C)]
pub struct RcBox<T> {
    pub header: RcHeader,
    pub value: T, // 编译器自动按 align_of::<T>() 填充
}

#[repr(C)]
pub struct ArcBox<T> {
    pub header: ArcHeader,
    pub value: T,
}

#[repr(C)]
pub struct SimpleRcBox<T> {
    pub header: SimpleRcHeader,
    pub value: T,
}

#[repr(C)]
pub struct SimpleArcBox<T> {
    pub header: SimpleArcHeader,
    pub value: T,
}

// ---------- 布局计算（和上面 repr(C) 完全一致） ----------

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

// ---------- Rc ----------

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
        // 1) strong 归零：先析构 payload
        if let Some(f) = drop_fn {
            f(payload);
        }
        (*hdr).strong.set(0);

        // 2) weak 减 1（strong 引用自己占一个 weak 份额）
        let w = (*hdr).weak.get();
        debug_assert!(w > 0, "myrc_drop 时 weak 不应为 0");
        (*hdr).weak.set(w - 1);

        // 3) weak 也归零才真正释放
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
    // 失败返回 null 指针
    let hdr = payload.sub(rc_payload_offset(payload_align)) as *mut RcHeader;
    let s = (*hdr).strong.get();
    if s == 0 {
        return std::ptr::null_mut();
    }
    (*hdr).strong.set(s + 1);
    payload
}

// ---------- Arc ----------

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

    // strong 减 1；归零则先析构 payload
    if (*hdr).strong.fetch_sub(1, Ordering::Release) == 1 {
        fence(Ordering::Acquire);
        if let Some(f) = drop_fn {
            f(payload);
        }

        // strong 归零后再减 weak；weak 归零才释放整块内存
        if (*hdr).weak.fetch_sub(1, Ordering::Release) == 1 {
            fence(Ordering::Acquire);
            dealloc(base, arc_block_layout(payload_size, payload_align));
        }
    }
}

// ---- Weak 相关：可选，若不需要 Weak 可以不导出 ----

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
    // Release 减 1；归零时 Acquire 保证看到其它线程的写
    if (*hdr).strong.fetch_sub(1, Ordering::Release) == 1 {
        fence(Ordering::Acquire);
        if let Some(f) = drop_fn {
            f(payload);
        }
        dealloc(base, simple_arc_block_layout(payload_size, payload_align));
    }
}

pub struct MyRc<T> {
    ptr: NonNull<RcBox<T>>,
    _p: PhantomData<RcBox<T>>,
}

impl<T> MyRc<T> {
    pub fn new(value: T) -> Self {
        unsafe {
            let layout = rc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>());
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
            let payload = base.add(rc_payload_offset(std::mem::align_of::<T>())) as *mut T;
            std::ptr::write(payload, value);
            MyRc {
                ptr: NonNull::new_unchecked(base as *mut RcBox<T>),
                _p: PhantomData,
            }
        }
    }

    pub fn as_ptr(&self) -> *mut u8 {
        unsafe { (self.ptr.as_ptr() as *mut u8).add(rc_payload_offset(std::mem::align_of::<T>())) }
    }

    pub fn clone(&self) -> Self {
        unsafe {
            let h = &(*self.ptr.as_ptr()).header;
            h.strong.set(h.strong.get() + 1);
        }
        MyRc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }

    pub fn get(&self) -> &T {
        unsafe { &(*self.ptr.as_ptr()).value }
    }

    pub fn downgrade(&self) -> WeakRc<T> {
        WeakRc::new(self)
    }

    pub fn strong_count(&self) -> usize {
        unsafe { (*self.ptr.as_ptr()).header.strong.get() }
    }

    pub fn weak_count(&self) -> usize {
        unsafe { (*self.ptr.as_ptr()).header.weak.get() }
    }
}

impl<T> Drop for MyRc<T> {
    fn drop(&mut self) {
        unsafe {
            let hdr = &(*self.ptr.as_ptr()).header;

            // 1) strong 减 1
            let s = hdr.strong.get();
            debug_assert!(s > 0, "MyRc::drop 时 strong 不应为 0");
            hdr.strong.set(s - 1);

            if s == 1 {
                // strong 归零：先析构 payload
                std::ptr::drop_in_place(&mut (*self.ptr.as_ptr()).value);

                // 然后 weak 也减 1（MyRc 自己占一个 weak 份额）
                let w = hdr.weak.get();
                debug_assert!(w > 0);
                hdr.weak.set(w - 1);

                if w == 1 {
                    // weak 也归零：真正释放
                    dealloc(
                        self.ptr.as_ptr() as *mut u8,
                        rc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>()),
                    );
                }
            }
        }
    }
}

pub struct WeakRc<T> {
    ptr: NonNull<RcBox<T>>,
    _p: PhantomData<RcBox<T>>,
}

impl<T> WeakRc<T> {
    /// 从 MyRc 降级得到 WeakRc（weak + 1）
    pub fn new(r: &MyRc<T>) -> Self {
        unsafe {
            let hdr = &(*r.ptr.as_ptr()).header;
            hdr.weak.set(hdr.weak.get() + 1);
        }
        WeakRc {
            ptr: r.ptr,
            _p: PhantomData,
        }
    }

    /// WeakRc 克隆（weak + 1）
    pub fn clone(&self) -> Self {
        unsafe {
            let hdr = &(*self.ptr.as_ptr()).header;
            hdr.weak.set(hdr.weak.get() + 1);
        }
        WeakRc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }

    /// 尝试升级：strong > 0 时 strong + 1 返回 Some(MyRc)；否则 None
    pub fn upgrade(&self) -> Option<MyRc<T>> {
        unsafe {
            let hdr = &(*self.ptr.as_ptr()).header;
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

    pub fn strong_count(&self) -> usize {
        unsafe { (*self.ptr.as_ptr()).header.strong.get() }
    }

    pub fn weak_count(&self) -> usize {
        unsafe { (*self.ptr.as_ptr()).header.weak.get() }
    }
}

impl<T> Drop for WeakRc<T> {
    fn drop(&mut self) {
        unsafe {
            let hdr = &(*self.ptr.as_ptr()).header;
            let w = hdr.weak.get();
            debug_assert!(w > 0, "WeakRc::drop 时 weak 不应为 0");
            hdr.weak.set(w - 1);

            if w == 1 {
                // weak 归零：此时 strong 必然也为 0（MyRc 的 drop 已经负责过）
                dealloc(
                    self.ptr.as_ptr() as *mut u8,
                    rc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>()),
                );
            }
        }
    }
}

pub struct MyArc<T> {
    ptr: NonNull<ArcBox<T>>,
    _p: PhantomData<ArcBox<T>>,
}

unsafe impl<T: Send + Sync> Send for MyArc<T> {}
unsafe impl<T: Send + Sync> Sync for MyArc<T> {}

impl<T> MyArc<T> {
    pub fn new(value: T) -> Self {
        unsafe {
            let layout = arc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>());
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
            let payload = base.add(arc_payload_offset(std::mem::align_of::<T>())) as *mut T;
            std::ptr::write(payload, value);
            MyArc {
                ptr: NonNull::new_unchecked(base as *mut ArcBox<T>),
                _p: PhantomData,
            }
        }
    }

    /// 返回给 JIT 的 payload 指针。
    pub fn as_ptr(&self) -> *mut u8 {
        unsafe { (self.ptr.as_ptr() as *mut u8).add(arc_payload_offset(std::mem::align_of::<T>())) }
    }

    pub fn clone(&self) -> Self {
        unsafe {
            (*self.ptr.as_ptr())
                .header
                .strong
                .fetch_add(1, Ordering::Relaxed);
        }
        MyArc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }

    pub fn get(&self) -> &T {
        unsafe { &(*self.ptr.as_ptr()).value }
    }

    /// 降级为 Weak，strong 引用释放后仍可观测 weak 计数。
    pub fn downgrade(&self) -> WeakArc<T> {
        unsafe {
            (*self.ptr.as_ptr())
                .header
                .weak
                .fetch_add(1, Ordering::Relaxed);
        }
        WeakArc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }

    /// 从 payload 指针重建，接管一个 strong 引用份额。
    ///
    /// # Safety
    /// - `payload` 必须是本类型 `as_ptr()` 返回的指针
    /// - 调用方必须拥有一个 strong 计数份额（所有权转移给它）
    pub unsafe fn from_raw(payload: *mut u8) -> Self {
        let offset = arc_payload_offset(std::mem::align_of::<T>());
        let base = payload.sub(offset) as *mut ArcBox<T>;
        MyArc {
            ptr: NonNull::new_unchecked(base),
            _p: PhantomData,
        }
    }

    /// 强引用计数（仅用于测试/调试）。
    pub fn strong_count(&self) -> usize {
        unsafe { (*self.ptr.as_ptr()).header.strong.load(Ordering::Relaxed) }
    }

    /// 弱引用计数（仅用于测试/调试）。
    pub fn weak_count(&self) -> usize {
        unsafe { (*self.ptr.as_ptr()).header.weak.load(Ordering::Relaxed) }
    }
}

impl<T> Drop for MyArc<T> {
    fn drop(&mut self) {
        unsafe {
            let hdr = &(*self.ptr.as_ptr()).header;
            if hdr.strong.fetch_sub(1, Ordering::Release) == 1 {
                fence(Ordering::Acquire);
                std::ptr::drop_in_place(&mut (*self.ptr.as_ptr()).value);

                if hdr.weak.fetch_sub(1, Ordering::Release) == 1 {
                    fence(Ordering::Acquire);
                    dealloc(
                        self.ptr.as_ptr() as *mut u8,
                        arc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>()),
                    );
                }
            }
        }
    }
}

pub struct WeakArc<T> {
    ptr: NonNull<ArcBox<T>>,
    _p: PhantomData<ArcBox<T>>,
}

unsafe impl<T: Send + Sync> Send for WeakArc<T> {}
unsafe impl<T: Send + Sync> Sync for WeakArc<T> {}

impl<T> WeakArc<T> {
    pub fn clone(&self) -> Self {
        unsafe {
            (*self.ptr.as_ptr())
                .header
                .weak
                .fetch_add(1, Ordering::Relaxed);
        }
        WeakArc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }

    /// 尝试升级为强引用；若 strong 已归零则返回 `None`。
    pub fn upgrade(&self) -> Option<MyArc<T>> {
        unsafe {
            let hdr = &(*self.ptr.as_ptr()).header;
            let mut cur = hdr.strong.load(Ordering::Relaxed);
            loop {
                if cur == 0 {
                    return None;
                }
                match hdr.strong.compare_exchange_weak(
                    cur,
                    cur + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        return Some(MyArc {
                            ptr: self.ptr,
                            _p: PhantomData,
                        })
                    }
                    Err(actual) => cur = actual,
                }
            }
        }
    }

    pub fn strong_count(&self) -> usize {
        unsafe { (*self.ptr.as_ptr()).header.strong.load(Ordering::Relaxed) }
    }

    pub fn weak_count(&self) -> usize {
        unsafe { (*self.ptr.as_ptr()).header.weak.load(Ordering::Relaxed) }
    }
}

impl<T> Drop for WeakArc<T> {
    fn drop(&mut self) {
        unsafe {
            let hdr = &(*self.ptr.as_ptr()).header;
            if hdr.weak.fetch_sub(1, Ordering::Release) == 1 {
                fence(Ordering::Acquire);
                dealloc(
                    self.ptr.as_ptr() as *mut u8,
                    arc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>()),
                );
            }
        }
    }
}

pub struct SimpleRc<T> {
    ptr: NonNull<SimpleRcBox<T>>,
    _p: PhantomData<SimpleRcBox<T>>,
}

impl<T> SimpleRc<T> {
    pub fn new(value: T) -> Self {
        unsafe {
            let layout =
                simple_rc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>());
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
            let payload = base.add(simple_rc_payload_offset(std::mem::align_of::<T>())) as *mut T;
            std::ptr::write(payload, value);
            SimpleRc {
                ptr: NonNull::new_unchecked(base as *mut SimpleRcBox<T>),
                _p: PhantomData,
            }
        }
    }

    /// 返回给 JIT 的 payload 指针。
    pub fn as_ptr(&self) -> *mut u8 {
        unsafe {
            (self.ptr.as_ptr() as *mut u8).add(simple_rc_payload_offset(std::mem::align_of::<T>()))
        }
    }

    pub fn clone(&self) -> Self {
        unsafe {
            let h = &(*self.ptr.as_ptr()).header;
            h.strong.set(h.strong.get() + 1);
        }
        SimpleRc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }

    pub fn get(&self) -> &T {
        unsafe { &(*self.ptr.as_ptr()).value }
    }
}

impl<T> Drop for SimpleRc<T> {
    fn drop(&mut self) {
        unsafe {
            let h = &(*self.ptr.as_ptr()).header;
            let c = h.strong.get();
            if c == 1 {
                std::ptr::drop_in_place(&mut (*self.ptr.as_ptr()).value);
                dealloc(
                    self.ptr.as_ptr() as *mut u8,
                    simple_rc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>()),
                );
            } else {
                h.strong.set(c - 1);
            }
        }
    }
}

pub struct SimpleArc<T> {
    ptr: NonNull<SimpleArcBox<T>>,
    _p: PhantomData<SimpleArcBox<T>>,
}

unsafe impl<T: Send + Sync> Send for SimpleArc<T> {}
unsafe impl<T: Send + Sync> Sync for SimpleArc<T> {}

impl<T> SimpleArc<T> {
    pub fn new(value: T) -> Self {
        unsafe {
            let layout =
                simple_arc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>());
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
            let payload = base.add(simple_arc_payload_offset(std::mem::align_of::<T>())) as *mut T;
            std::ptr::write(payload, value);
            SimpleArc {
                ptr: NonNull::new_unchecked(base as *mut SimpleArcBox<T>),
                _p: PhantomData,
            }
        }
    }

    pub fn as_ptr(&self) -> *mut u8 {
        unsafe {
            (self.ptr.as_ptr() as *mut u8).add(simple_arc_payload_offset(std::mem::align_of::<T>()))
        }
    }

    pub fn clone(&self) -> Self {
        unsafe {
            (*self.ptr.as_ptr())
                .header
                .strong
                .fetch_add(1, Ordering::Relaxed);
        }
        SimpleArc {
            ptr: self.ptr,
            _p: PhantomData,
        }
    }

    pub fn get(&self) -> &T {
        unsafe { &(*self.ptr.as_ptr()).value }
    }
}

impl<T> Drop for SimpleArc<T> {
    fn drop(&mut self) {
        unsafe {
            let hdr = &(*self.ptr.as_ptr()).header;
            if hdr.strong.fetch_sub(1, Ordering::Release) == 1 {
                fence(Ordering::Acquire);
                std::ptr::drop_in_place(&mut (*self.ptr.as_ptr()).value);
                dealloc(
                    self.ptr.as_ptr() as *mut u8,
                    simple_arc_block_layout(std::mem::size_of::<T>(), std::mem::align_of::<T>()),
                );
            }
        }
    }
}

/// 每次 Drop 时 +1，用于验证 payload 被析构且只被析构一次
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

    /// 从 payload 指针回溯到 header 的通用逻辑。
    /// 把 ptr 往回 sub offset，然后转成 header 引用。
    #[inline]
    unsafe fn header_from_payload<H>(payload: *mut u8, offset: usize) -> &'static H {
        // 生命周期被截断到 'static，调用方保证在智能指针存活期间使用
        &*(payload.sub(offset) as *const H)
    }

    // ---------- MyRc ----------

    pub fn rc_header<T>(r: &MyRc<T>, _align: usize) -> &RcHeader {
        unsafe {
            let payload =
                (r.ptr.as_ptr() as *mut u8).add(rc_payload_offset(std::mem::align_of::<T>()));
            header_from_payload::<RcHeader>(payload, rc_payload_offset(std::mem::align_of::<T>()))
        }
    }

    /// 直接从 payload 指针拿 header（用于 `as_ptr()` 之后）
    pub fn rc_header_from_payload(payload: *mut u8) -> &'static RcHeader {
        let align = std::mem::align_of::<RcHeader>(); // 仅用于偏移计算
        let offset = rc_payload_offset(align);
        unsafe { header_from_payload::<RcHeader>(payload, offset) }
    }

    // ---------- WeakRc ----------

    pub fn weak_rc_header<T>(w: &WeakRc<T>, _align: usize) -> &RcHeader {
        unsafe {
            let payload =
                (w.ptr.as_ptr() as *mut u8).add(rc_payload_offset(std::mem::align_of::<T>()));
            header_from_payload::<RcHeader>(payload, rc_payload_offset(std::mem::align_of::<T>()))
        }
    }

    // ---------- MyArc ----------

    pub fn arc_header<T>(a: &MyArc<T>, _align: usize) -> &ArcHeader {
        unsafe {
            let payload =
                (a.ptr.as_ptr() as *mut u8).add(arc_payload_offset(std::mem::align_of::<T>()));
            header_from_payload::<ArcHeader>(payload, arc_payload_offset(std::mem::align_of::<T>()))
        }
    }

    pub fn weak_arc_header<T>(w: &WeakArc<T>, _align: usize) -> &ArcHeader {
        unsafe {
            let payload =
                (w.ptr.as_ptr() as *mut u8).add(arc_payload_offset(std::mem::align_of::<T>()));
            header_from_payload::<ArcHeader>(payload, arc_payload_offset(std::mem::align_of::<T>()))
        }
    }

    // ---------- SimpleRc ----------

    pub fn simple_rc_header<T>(r: &SimpleRc<T>, _align: usize) -> &SimpleRcHeader {
        unsafe {
            let payload = (r.ptr.as_ptr() as *mut u8)
                .add(simple_rc_payload_offset(std::mem::align_of::<T>()));
            header_from_payload::<SimpleRcHeader>(
                payload,
                simple_rc_payload_offset(std::mem::align_of::<T>()),
            )
        }
    }

    // ---------- SimpleArc ----------

    pub fn simple_arc_header<T>(a: &SimpleArc<T>, _align: usize) -> &SimpleArcHeader {
        unsafe {
            let payload = (a.ptr.as_ptr() as *mut u8)
                .add(simple_arc_payload_offset(std::mem::align_of::<T>()));
            header_from_payload::<SimpleArcHeader>(
                payload,
                simple_arc_payload_offset(std::mem::align_of::<T>()),
            )
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn header_sizes_are_fixed() {
        assert_eq!(size_of::<RcHeader>(), 16, "带 weak Rc header");
        assert_eq!(size_of::<ArcHeader>(), 16, "带 weak Arc header");
        assert_eq!(size_of::<SimpleRcHeader>(), 8, "SimpleRc header");
        assert_eq!(size_of::<SimpleArcHeader>(), 8, "SimpleArc header");
    }

    #[test]
    fn payload_offset_matches_repr_c() {
        // 对任意对齐，手工计算的 offset 必须与 repr(C) 编译器排版一致
        macro_rules! check {
            ($box:ty, $off_fn:ident, $payload:ty) => {{
                let manual = $off_fn(align_of::<$payload>());
                let real = {
                    let base = 0usize;
                    let hdr_end = size_of::<<$box as Boxed>::Hdr>();
                    // 模拟 repr(C)：从 hdr_end 向上对齐到 payload 对齐
                    let a = align_of::<$payload>().max(align_of::<<$box as Boxed>::Hdr>());
                    let _ = base;
                    (hdr_end + a - 1) & !(a - 1)
                };
                assert_eq!(manual, real, "offset 与 repr(C) 不一致");
            }};
        }
        // 这里为了简洁，直接断言已知对齐的 payload
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
        // u64：offset 16 + size 8 = 24，总布局 >= 24
        let l = rc_block_layout(8, 8);
        assert!(l.size() >= 24);
        assert_eq!(l.align(), 8);

        // 8 字节对齐的 ZST：offset 16，size 至少到 16
        let l = simple_rc_block_layout(0, 8);
        assert!(l.size() >= 8);

        // 超对齐 payload
        let l = rc_block_layout(16, 64);
        assert_eq!(l.align(), 64);
        assert!(l.size() % 64 == 0);
    }

    #[test]
    fn repr_c_layout_matches_manual_offset() {
        // 用真实泛型结构体验证偏移
        let boxed: RcBox<u64> = RcBox {
            header: RcHeader {
                strong: std::cell::Cell::new(0),
                weak: std::cell::Cell::new(0),
            },
            value: 0,
        };
        let base = &boxed as *const _ as usize;
        let vaddr = &boxed.value as *const _ as usize;
        assert_eq!(vaddr - base, rc_payload_offset(std::mem::align_of::<u64>()));

        let boxed: SimpleRcBox<u64> = SimpleRcBox {
            header: SimpleRcHeader {
                strong: std::cell::Cell::new(0),
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
}

#[cfg(test)]
mod simple_rc_tests {
    use super::*;

    #[test]
    fn new_and_get() {
        let r = SimpleRc::new(42u32);
        assert_eq!(*r.get(), 42);
    }

    #[test]
    fn clone_increments_count() {
        let r = SimpleRc::new(7u8);
        let base = unsafe { r.as_ptr().sub(simple_rc_payload_offset(1)) } as *const SimpleRcHeader;
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
        assert_eq!(counter.load(Ordering::SeqCst), 0, "还有引用，不能析构");
        drop(r2);
        assert_eq!(counter.load(Ordering::SeqCst), 1, "归零时析构一次");
    }

    #[test]
    fn jit_style_alloc_clone_drop() {
        unsafe {
            let payload = simple_rc_alloc(4, 4);
            *(payload as *mut u32) = 0xDEAD_BEEF;

            let p2 = simple_rc_clone(payload, 4);
            assert_eq!(p2, payload, "clone 应返回同一 payload 指针");

            extern "C" fn dtor(p: *mut u8) {
                // 这里只是探针，无实际析构逻辑
                unsafe {
                    *(p as *mut u32) = 0;
                }
            }

            simple_rc_drop(payload, 4, 4, Some(dtor));
            // payload 已经不在有效状态，不再读
            simple_rc_drop(p2, 4, 4, None);
        }
    }

    #[test]
    fn jit_alloc_zero_size_payload() {
        // ZST 也不应 UB，指针非空且对齐
        unsafe {
            let p = simple_rc_alloc(0, 1);
            assert!(!p.is_null());
            assert_eq!(p as usize % 1, 0);
            simple_rc_drop(p, 0, 1, None);
        }
    }

    #[test]
    fn high_alignment_payload() {
        #[repr(align(64))]
        struct Aligned64(u8);

        let r = SimpleRc::new(Aligned64(9));
        assert_eq!(r.as_ptr() as usize % 64, 0, "payload 必须 64 字节对齐");
        assert_eq!(r.get().0, 9);
    }
}

#[cfg(test)]
mod weak_rc_tests {
    use super::*;
    use crate::alloc::test_helpers::rc_header;

    // ---------- 计数 ----------

    #[test]
    fn downgrade_increments_weak() {
        let r = MyRc::new(1u32);
        let h = rc_header(&r, 4);
        assert_eq!(h.strong.get(), 1);
        assert_eq!(h.weak.get(), 1);

        let w = r.downgrade();
        assert_eq!(h.strong.get(), 1, "downgrade 不动 strong");
        assert_eq!(h.weak.get(), 2, "downgrade 后 weak +1");

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

    // ---------- upgrade ----------

    #[test]
    fn upgrade_succeeds_while_strong_alive() {
        let r = MyRc::new(42u32);
        let w = r.downgrade();

        let up = w.upgrade().expect("strong > 0");
        assert_eq!(*up.get(), 42);
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

        assert!(w.upgrade().is_none(), "strong 归零后 upgrade 应返回 None");
    }

    #[test]
    fn upgrade_then_drop_returns_to_zero() {
        let r = MyRc::new(7u8);
        let w = r.downgrade();
        {
            let up = w.upgrade().unwrap();
            assert_eq!(*up.get(), 7);
        }
        assert_eq!(r.strong_count(), 1);
        assert_eq!(r.weak_count(), 2);

        drop(r);
        drop(w);
    }

    // ---------- 析构时机 ----------

    #[test]
    fn payload_dropped_when_strong_zero_even_if_weak_alive() {
        let (probe, counter) = DropProbe::new();
        let r = MyRc::new(probe);
        let w = r.downgrade();

        assert_eq!(counter.load(Ordering::SeqCst), 0);
        drop(r);
        // strong 归零，payload 应已析构；但 weak 仍 > 0，内存未释放
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(w.strong_count(), 0);
        assert_eq!(w.weak_count(), 1);

        drop(w); // 此时才 dealloc
    }

    #[test]
    fn memory_freed_only_when_weak_zero() {
        // 用 alloc 计数探针不可行，这里用「W 未释放时仍能读 header」
        // 的弱验证：w.weak_count() 直到 drop(w) 前都可读
        let r = MyRc::new(vec![1u8, 2, 3]);
        let w = r.downgrade();
        drop(r);

        // 内存仍存活，能安全读 header
        assert_eq!(w.weak_count(), 1);
        assert_eq!(w.strong_count(), 0);
        assert!(w.upgrade().is_none());

        drop(w); // 走到这里没崩，说明 weak 归零路径正确
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
        drop(w3); // 最后一个 weak 归零才 dealloc
    }

    #[test]
    fn weak_cycle_no_leak() {
        // 不构造循环，验证「strong 死后 weak 归零也能释放」
        let (probe, counter) = DropProbe::new();
        {
            let r = MyRc::new(probe);
            let w = r.downgrade();
            drop(r);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            drop(w);
        }
    }

    // ---------- JIT 接口 ----------

    #[test]
    fn jit_downgrade_clone_upgrade_drop() {
        unsafe {
            let p = myrc_alloc(4, 4);
            *(p as *mut u32) = 99;

            // JIT: downgrade
            let w = myrc_downgrade(p, 4);

            // JIT: weak_clone
            let w2 = myrc_weak_clone(w, 4);

            // JIT: 用 weak 尝试 upgrade
            let up = myrc_weak_upgrade(w, 4);
            assert!(!up.is_null());
            assert_eq!(up, p);
            assert_eq!(*(up as *const u32), 99);

            // 释放 upgrade 出来的 strong
            myrc_drop(up, 4, 4, None);

            // 释放强引用 → strong 归零
            myrc_drop(p, 4, 4, None);

            // 释放两个 weak
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
            myrc_drop(p, 4, 4, None); // strong 归零

            let up = myrc_weak_upgrade(w, 4);
            assert!(up.is_null(), "strong 归零后 upgrade 返回 null");

            myrc_weak_drop(w, 4, 4);
        }
    }

    #[test]
    fn jit_weak_zero_releases_memory() {
        // 依赖 Miri / ASan 检测释放是否正确；普通测试下走到最后即可
        unsafe {
            let p = myrc_alloc(16, 8);
            let w1 = myrc_downgrade(p, 8);
            let w2 = myrc_weak_clone(p, 8);
            myrc_drop(p, 16, 8, None);
            myrc_weak_drop(w1, 16, 8);
            myrc_weak_drop(w2, 16, 8); // 最后一个 weak → dealloc
        }
    }
}

#[cfg(test)]
mod myrc_tests {
    use super::*;

    #[test]
    fn strong_and_weak_counts() {
        let r = MyRc::new(123u64);
        let base = unsafe { r.as_ptr().sub(rc_payload_offset(8)) } as *const RcHeader;
        unsafe {
            assert_eq!((*base).strong.get(), 1);
            assert_eq!((*base).weak.get(), 1);
        }
        let r2 = r.clone();
        unsafe {
            assert_eq!((*base).strong.get(), 2);
            assert_eq!((*base).weak.get(), 1, "clone 不动 weak");
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
                    // 每个线程再克隆一批，然后释放
                    let clones: Vec<_> = (0..100).map(|_| a.clone()).collect();
                    drop(clones);
                    drop(a);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(counter.load(Ordering::SeqCst), 0, "主引用仍在，不能析构");
        drop(a);
        assert_eq!(counter.load(Ordering::SeqCst), 1, "全部释放后析构一次");
    }

    #[test]
    fn myarc_concurrent_clone_drop() {
        const N: usize = 8;
        let (probe, counter) = DropProbe::new();
        let a = MyArc::new(probe);

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
        // 模拟 JIT 侧：某线程克隆，另一线程释放
        let (probe, counter) = DropProbe::new();
        let a = SimpleArc::new(probe);
        let p_addr = a.as_ptr() as usize;

        let h = thread::spawn(move || {
            let p = p_addr as *mut u8;
            unsafe {
                // JIT 侧 clone
                let p2 = simple_arc_clone(p, std::mem::align_of::<DropProbe>());
                // JIT 侧 drop，但 payload 是 Rust DropProbe，用 Rust 析构
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
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "JIT clone 后引用计数仍 > 0"
        );
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
        let p = r.as_ptr();

        // 模拟 JIT 拿指针做一次 clone 并回传
        let p2 = unsafe { simple_rc_clone(p, 4) };
        assert_eq!(p, p2);

        // Rust 侧再从 p2 重建一个 SimpleRc（这需要 from_payload 构造函数）
        // 这里只是验证指针值一致，具体 from_payload 略
        unsafe {
            assert_eq!(*(p2 as *const u32), 99);
        }
        // 释放 JIT 侧引用的那次 clone
        unsafe {
            simple_rc_drop(p2, 4, 4, None);
        }
        // 再释放 Rust 侧
        drop(r);
    }

    #[test]
    fn ptr_stability_across_clone() {
        // clone 不移动 payload
        let r = SimpleArc::new([1u64, 2, 3]);
        let p = r.as_ptr();
        let r2 = r.clone();
        assert_eq!(r.as_ptr(), p);
        assert_eq!(r2.as_ptr(), p);
    }
}
