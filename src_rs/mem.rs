//! Lua 内存管理器
//!
//! 对应 C 源码: lmem.h + lmem.cpp
//!
//! 核心职责:
//! - 封装所有内存分配/释放/重分配操作
//! - 跟踪 GC 债务 (GCdebt)，用于触发垃圾回收
//! - 在分配失败时触发紧急 GC，然后重试
//! - 提供类型安全的 vector 扩容/缩容
//! - 溢出检查

use std::alloc::{self, Layout};
use std::mem::{self, ManuallyDrop};
use std::ptr::NonNull;

use crate::config::LuaMem;

// ============================================================================
// 自定义分配器 trait
// ============================================================================

pub type LuaCAlloc = Option<
    unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *mut std::ffi::c_void,
        usize,
        usize,
    ) -> *mut std::ffi::c_void,
>;

pub trait Allocator {
    /// C 风格 realloc 语义：new_size==0 时释放并返回 NULL。
    /// `align` 为分配/释放的对齐要求（LongString 块为 8）；CapiAllocator
    /// 透传给宿主 allocf（依赖 malloc 式 max-align，与 C Lua 一致）。
    /// `&self`：内置实现无内部状态；有状态的自定义分配器用 Cell/Atomic。
    fn alloc(&self, ptr: *mut u8, old_size: usize, new_size: usize, align: usize) -> *mut u8;

    /// 若该分配器包装了 C 的 lua_Alloc（如 CapiAllocator），返回 (allocf, ud)
    /// 供 lua_getallocf 取回；纯 Rust 分配器返回 None。
    fn allocf_parts(&self) -> Option<(LuaCAlloc, *mut std::ffi::c_void)> {
        None
    }

    /// lua_setallocf 支持：若为 C API 兼容分配器（CapiAllocator）则原地更新
    /// allocf/ud 并返回 true；纯 Rust 分配器返回 false（由调用方整体替换为
    /// CapiAllocator）。
    fn set_allocf(&mut self, _f: LuaCAlloc, _ud: *mut std::ffi::c_void) -> bool {
        false
    }
}

/// 允许 MemState 以 `Box<dyn Allocator>` 形式持有任意分配器
/// （GlobalState 必须是具体类型，无法对 A 泛型化，故用 trait 对象动态分发）。
impl Allocator for Box<dyn Allocator> {
    fn alloc(&self, ptr: *mut u8, old_size: usize, new_size: usize, align: usize) -> *mut u8 {
        (**self).alloc(ptr, old_size, new_size, align)
    }

    fn allocf_parts(&self) -> Option<(LuaCAlloc, *mut std::ffi::c_void)> {
        (**self).allocf_parts()
    }

    fn set_allocf(&mut self, f: LuaCAlloc, ud: *mut std::ffi::c_void) -> bool {
        (**self).set_allocf(f, ud)
    }
}

// ============================================================================
// 默认分配器：基于 std::alloc
// ============================================================================

pub struct DefaultAllocator;

/// 默认对齐：1 字节，用于非类型化分配（C 的 realloc 语义）。
/// 类型化分配（new_vec / grow_vec / shrink_vec）直接用 std::alloc API
/// 并传入 T 的对齐，确保与 Vec<T> 的 drop（用 std::alloc::Global 释放）一致。
impl Allocator for DefaultAllocator {
    fn alloc(&self, ptr: *mut u8, old_size: usize, new_size: usize, align: usize) -> *mut u8 {
        if new_size == 0 {
            if old_size != 0 && !ptr.is_null() {
                unsafe {
                    let layout = Layout::from_size_align_unchecked(old_size, align);
                    alloc::dealloc(ptr, layout);
                }
            }
            return std::ptr::null_mut();
        }

        if ptr.is_null() || old_size == 0 {
            let layout = match Layout::from_size_align(new_size, align) {
                Ok(l) => l,
                Err(_) => return std::ptr::null_mut(),
            };
            unsafe { alloc::alloc(layout) }
        } else {
            let old_layout = unsafe { Layout::from_size_align_unchecked(old_size, align) };
            unsafe { alloc::realloc(ptr, old_layout, new_size) }
        }
    }
}

// ============================================================================
// C API 分配器：包装 lua_Alloc 函数指针
// ============================================================================

/// C API 分配器 — 对应 C global_State 的 allocf/ud。
/// lua_newstate / lua_setallocf 写入 allocf/allocf_ud，lmem 层的 C 风格
/// 分配原语（realloc/free/malloc）经此路由到宿主提供的 allocator（如 skynet）。
/// allocf 为 None 时兜底 DefaultAllocator（C l_alloc 的 realloc 语义）。
/// 注意：经此分配器分配的内存只能经此释放（C 的 realloc 语义，1 字节对齐），
/// 不能交给 Box<T>/Vec<T> 的 std drop 路径管理。
pub struct CapiAllocator {
    pub allocf: LuaCAlloc,
    pub allocf_ud: *mut std::ffi::c_void,
}

impl Default for CapiAllocator {
    fn default() -> Self {
        Self {
            allocf: None,
            allocf_ud: std::ptr::null_mut(),
        }
    }
}

impl CapiAllocator {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Allocator for CapiAllocator {
    fn alloc(&self, ptr: *mut u8, old_size: usize, new_size: usize, _align: usize) -> *mut u8 {
        // SAFETY: allocf 必须满足 lua_Alloc 约定（C 的 realloc 语义）：
        // nsize==0 时释放并返回 NULL；否则分配/重分配，失败返回 NULL。
        // align 不透传：lua_Alloc 接口无对齐参数，依赖宿主 malloc 的
        // max-align 保证（与 C Lua 对 LongString/TString 的处理一致）。
        match self.allocf {
            Some(f) => unsafe {
                f(
                    self.allocf_ud,
                    ptr as *mut std::ffi::c_void,
                    old_size,
                    new_size,
                ) as *mut u8
            },
            None => DefaultAllocator.alloc(ptr, old_size, new_size, _align),
        }
    }

    fn allocf_parts(&self) -> Option<(LuaCAlloc, *mut std::ffi::c_void)> {
        Some((self.allocf, self.allocf_ud))
    }

    fn set_allocf(&mut self, f: LuaCAlloc, ud: *mut std::ffi::c_void) -> bool {
        self.allocf = f;
        self.allocf_ud = ud;
        true
    }
}

// ============================================================================
// Allocator-aware 容器：标准库优先，回退 allocator_api2
// ============================================================================

#[cfg(not(feature = "nightly-allocator"))]
pub use allocator_api2::boxed::Box as AllocBox;
/// 标准库的 `Box<T, A>` / `Vec<T, A>`（自定义分配器）受 unstable `allocator_api`
/// 特性门控，stable 不可用。因此这里按 crate feature 选择实现：
///
/// - 启用 `nightly-allocator`（需 nightly）：直接指向标准库 [`std::boxed::Box`] /
///   [`std::vec::Vec`]；此时 [`SlotAlloc`] 额外实现 `core::alloc::Allocator`；
/// - 未启用（stable 默认）：回退到 [`allocator_api2`] 的兼容实现，API 完全一致。
///
/// 两种情形下 [`SlotAlloc`] 都实现了容器所需的分配器 trait，容器代码无需区分。
/// 注意：Cargo 无法按工具链自动开关依赖 feature，nightly 上需显式
/// `cargo build --features nightly-allocator`（crate 根的 `#![feature(allocator_api)]`
/// 已由 `cfg_attr` 自动启用）。
#[cfg(feature = "nightly-allocator")]
pub use std::boxed::Box as AllocBox;

#[cfg(not(feature = "nightly-allocator"))]
pub use allocator_api2::vec::Vec as AllocVec;
#[cfg(feature = "nightly-allocator")]
pub use std::vec::Vec as AllocVec;

// ============================================================================
// AllocOwner — 运行期分配器的稳定宿主；SlotAlloc — 面向 allocator-aware 容器
// （Vec/Box/HashTable）的适配器，把分配路由到分配器槽（AllocSlot）
// ============================================================================

/// 运行期分配器宿主：为 `Box<dyn Allocator>` 提供一个稳定地址。
///
/// 存在理由：分配器槽（[`crate::strings::AllocSlot`]）指向 `Box<dyn Allocator>`
/// **值本身**。若该值内联在 `GlobalState::mem` 中，`GlobalState` 构造期还没有
/// 固定地址，就只能在构造期退化成 null 槽——而 null 槽意味着「类型上带了分配器
/// 参数、运行时却回退 std::alloc」，与 allocator_api2 的参数化语义自相矛盾。
///
/// 本类型把内层 Box 放在独立堆块上：槽自 [`Self::new`] 起即有效，
/// `GlobalState` 装箱/移动不改变该地址；[`Self::set`] 原地替换内容后，
/// 已发出的槽自动跟随。
///
/// # 为什么持有根裸指针而不是 `Box<Box<dyn Allocator>>`
///
/// 若用 `Box` 持有内层值，`slot()` 只能写成 `&*self.inner as *const _` ——
/// 一条**从共享引用派生**的地址。随后 [`Self::set`] 的原地写入会被 Tree
/// Borrows 视为 foreign write，把该共享引用派生的标签置为 `Disabled`；
/// 已发出的槽再被使用（例如析构时释放）即为 UB（Miri:
/// `reborrow ... is forbidden`，tag `Disabled due to a foreign write`）。
///
/// [`Box::into_raw`] 取得的是**根裸指针**，不派生自任何引用：读、写、以及
/// `set` 原地覆盖都始终合法，且不会因其他访问而失效。所有权仍由本类型持有，
/// [`Drop`] 负责回收（不是 `Box::leak`，无泄漏）。
pub struct AllocOwner {
    /// 指向堆上 `Box<dyn Allocator>` 的根裸指针（始终非 null）。
    inner: NonNull<Box<dyn Allocator>>,
}

impl AllocOwner {
    /// 接管一个分配器（内层 Box 立即获得稳定堆地址）。
    #[inline]
    pub fn new(allocator: Box<dyn Allocator>) -> Self {
        Self {
            inner: NonNull::new(Box::into_raw(Box::new(allocator)))
                .expect("Box::into_raw 不得返回 null"),
        }
    }

    /// 稳定的分配器槽 —— 构造期即可用，且永不为 null。
    ///
    /// 只做指针转换，**不创建对目标值的引用**（见类型级文档）。
    #[inline]
    pub fn slot(&self) -> crate::strings::AllocSlot {
        self.inner.as_ptr() as *const Box<dyn Allocator>
    }

    /// 原地替换分配器（槽地址不变，已发出的槽自动跟随）。
    #[inline]
    pub fn set(&mut self, allocator: Box<dyn Allocator>) {
        // SAFETY: inner 非 null，且指向本类型独占持有的 Box；覆盖即析构旧值。
        unsafe { *self.inner.as_ptr() = allocator };
    }

    /// 当前分配器。
    #[inline]
    pub fn get(&self) -> &dyn Allocator {
        // SAFETY: inner 非 null 且指向存活、已初始化的 Box。
        unsafe { &**self.inner.as_ptr() }
    }
}

impl Drop for AllocOwner {
    fn drop(&mut self) {
        // SAFETY: inner 来自 Box::into_raw，本类型独占其所有权，且只回收一次。
        unsafe { drop(Box::from_raw(self.inner.as_ptr())) };
    }
}

impl Allocator for AllocOwner {
    #[inline]
    fn alloc(&self, ptr: *mut u8, old_size: usize, new_size: usize, align: usize) -> *mut u8 {
        // SAFETY: inner 非 null 且指向存活的 Box。
        unsafe { (**self.inner.as_ptr()).alloc(ptr, old_size, new_size, align) }
    }

    #[inline]
    fn allocf_parts(&self) -> Option<(LuaCAlloc, *mut std::ffi::c_void)> {
        // SAFETY: 同上。
        unsafe { (**self.inner.as_ptr()).allocf_parts() }
    }

    #[inline]
    fn set_allocf(&mut self, f: LuaCAlloc, ud: *mut std::ffi::c_void) -> bool {
        // SAFETY: 同上；`&mut self` 保证独占。
        unsafe { (**self.inner.as_ptr()).set_allocf(f, ud) }
    }
}

// ============================================================================
// SlotAlloc — 面向 allocator-aware 容器的适配器（上方注释保留）
// ============================================================================

/// 把 [`crate::strings::AllocSlot`]（指向 `Box<dyn Allocator>` 的稳定地址）适配为
/// allocator-aware 容器的分配器（[`AllocVec`] / [`AllocBox`] /
/// [`hashbrown::HashTable`]，分别对应 C 的数组部分 / 盒 / Node 数组）：
/// - [`AllocVec`]（标准库 `Vec<T, A>`，回退 `allocator_api2::vec::Vec`）
/// - [`AllocBox`]（标准库 `Box<T, A>`，回退 `allocator_api2::boxed::Box`）
/// - [`hashbrown::HashTable`]（`A = SlotAlloc`，对应 C Lua Node 数组）
///
/// 全部分配/重分配/释放都经槽指向的分配器（对应 C 的 `global_State.allocf`）。
///
/// 不变量：**槽永不为 null**。只能经 [`SlotAlloc::from_slot`] /
/// [`SlotAlloc::from_owner`]（运行期，配合 [`AllocOwner`] 或
/// `LuaState::alloc_slot`）构造；测试用 `TestAlloc`（每实例独立宿主）。
///
/// `Copy`：仅持有一个指针，容器（Vec/Box/HashTable）要求 `A: Clone` 时零成本复制。
#[derive(Clone, Copy)]
pub struct SlotAlloc(crate::strings::AllocSlot);

impl SlotAlloc {
    /// 从分配器宿主取槽。
    #[inline]
    pub fn from_owner(owner: &AllocOwner) -> Self {
        Self(owner.slot())
    }

    /// 从裸槽构造。
    ///
    /// # Safety
    /// `slot` 必须非 null，且其指向的 `Box<dyn Allocator>` 必须在本分配器的
    /// **全部**使用期内保持有效（由 [`AllocOwner`] 或 `GlobalState::mem` 保证）。
    #[inline]
    pub unsafe fn from_slot(slot: crate::strings::AllocSlot) -> Self {
        debug_assert!(!slot.is_null(), "SlotAlloc 槽不得为 null");
        Self(slot)
    }

    /// 底层槽（非 null）。
    #[inline]
    pub fn slot(self) -> crate::strings::AllocSlot {
        self.0
    }

    /// 经槽分配/释放；`new_size == 0` 时释放并返回 null（C realloc 语义）。
    #[inline]
    unsafe fn raw_realloc(
        &self,
        ptr: *mut u8,
        old_size: usize,
        new_size: usize,
        align: usize,
    ) -> *mut u8 {
        (*self.0).alloc(ptr, old_size, new_size, align)
    }
}

/// 测试用独立分配器：**每个实例自带一份** [`DefaultAllocator`]，实例之间
/// 互不共享。
///
/// 为什么不是「每线程一份」的单例：同一线程可以有多个 `LuaState`，各自挂
/// 不同的 `allocf`（`lua_newstate` / `lua_setallocf`），它们之间可能还要
/// 交叉引用对象。把同线程的所有分配钉在同一个分配器上，正是必须避免的假定。
///
/// 生命周期：`TestAlloc` 必须**活得比使用它槽的对象更久**。约定用法是在
/// 测试函数（或测试辅助作用域）开头绑定：
///
/// ```ignore
/// let alloc = TestAlloc::new();
/// let t = Table::new_in(alloc.slot());
/// ```
///
/// 禁止在「返回表/闭包等持有槽的对象」的函数里局部绑定 `TestAlloc` ——
/// 函数返回后宿主被析构，槽即成悬垂指针。
#[cfg(test)]
pub(crate) struct TestAlloc(AllocOwner);

#[cfg(test)]
impl TestAlloc {
    /// 新建一份独立分配器（内层 Box 的地址在 `self` 存活期内稳定）。
    #[inline]
    pub(crate) fn new() -> Self {
        Self(AllocOwner::new(Box::new(DefaultAllocator)))
    }

    /// 稳定槽 —— 引用者不得比 `self` 活得久。
    #[inline]
    pub(crate) fn slot(&self) -> SlotAlloc {
        SlotAlloc::from_owner(&self.0)
    }
}

// SAFETY: 所有方法都满足 Allocator 契约——返回的块由同一分配器（槽或 std::alloc）
// 分配，且 deallocate/grow/shrink 使用与分配时相同的 layout。
unsafe impl allocator_api2::alloc::Allocator for SlotAlloc {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, allocator_api2::alloc::AllocError> {
        let ptr =
            unsafe { self.raw_realloc(std::ptr::null_mut(), 0, layout.size(), layout.align()) };
        let nn = NonNull::new(ptr).ok_or(allocator_api2::alloc::AllocError)?;
        Ok(NonNull::slice_from_raw_parts(nn, layout.size()))
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.raw_realloc(ptr.as_ptr(), layout.size(), 0, layout.align());
    }

    #[inline]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, allocator_api2::alloc::AllocError> {
        debug_assert!(new_layout.size() >= old_layout.size());
        let new_ptr = self.raw_realloc(
            ptr.as_ptr(),
            old_layout.size(),
            new_layout.size(),
            new_layout.align(),
        );
        let nn = NonNull::new(new_ptr).ok_or(allocator_api2::alloc::AllocError)?;
        Ok(NonNull::slice_from_raw_parts(nn, new_layout.size()))
    }

    #[inline]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, allocator_api2::alloc::AllocError> {
        debug_assert!(new_layout.size() <= old_layout.size());
        let new_ptr = self.raw_realloc(
            ptr.as_ptr(),
            old_layout.size(),
            new_layout.size(),
            new_layout.align(),
        );
        let nn = NonNull::new(new_ptr).ok_or(allocator_api2::alloc::AllocError)?;
        Ok(NonNull::slice_from_raw_parts(nn, new_layout.size()))
    }
}

// 标准库 `Vec<T, A>` / `Box<T, A>` 所需的分配器契约（`core::alloc::Allocator`，
// unstable `allocator_api`，仅 `nightly-allocator` 时编译）。
//
// 这与上方 `allocator_api2::alloc::Allocator` 是**两个不同的 trait**（后者是
// stable 下的兼容 shim），各写各的 impl 互不冲突：`AllocBox`/`AllocVec` 走本 impl，
// `hashbrown::HashTable`（`A = SlotAlloc`）走 shim impl。方法体复用
// [`SlotAlloc::raw_realloc`]，全部满足 Allocator 契约（同一分配器分配/释放，
// deallocate/grow/shrink 使用与分配时相同的 layout）。
#[cfg(feature = "nightly-allocator")]
unsafe impl core::alloc::Allocator for SlotAlloc {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        let ptr =
            unsafe { self.raw_realloc(std::ptr::null_mut(), 0, layout.size(), layout.align()) };
        let nn = NonNull::new(ptr).ok_or(core::alloc::AllocError)?;
        Ok(NonNull::slice_from_raw_parts(nn, layout.size()))
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.raw_realloc(ptr.as_ptr(), layout.size(), 0, layout.align());
    }

    #[inline]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        debug_assert!(new_layout.size() >= old_layout.size());
        let new_ptr = self.raw_realloc(
            ptr.as_ptr(),
            old_layout.size(),
            new_layout.size(),
            new_layout.align(),
        );
        let nn = NonNull::new(new_ptr).ok_or(core::alloc::AllocError)?;
        Ok(NonNull::slice_from_raw_parts(nn, new_layout.size()))
    }

    #[inline]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        debug_assert!(new_layout.size() <= old_layout.size());
        let new_ptr = self.raw_realloc(
            ptr.as_ptr(),
            old_layout.size(),
            new_layout.size(),
            new_layout.align(),
        );
        let nn = NonNull::new(new_ptr).ok_or(core::alloc::AllocError)?;
        Ok(NonNull::slice_from_raw_parts(nn, new_layout.size()))
    }
}

// ============================================================================
// 内存错误类型
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemError {
    pub msg: String,
}

impl std::fmt::Display for MemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "memory error: {}", self.msg)
    }
}

// 体积优先: 不实现 std::error::Error trait, 避免 Box<dyn Error> 引入 StringError
#[cfg(not(size_optimized))]
impl std::error::Error for MemError {}

// ============================================================================
// 内存状态 — 对应 C 中 global_State 的内存相关字段
// ============================================================================

pub struct MemState<'a, A: Allocator = DefaultAllocator> {
    pub allocator: A,
    pub gc_debt: LuaMem,
    pub gc_stop_em: bool,
    pub complete_state: bool,
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl Default for MemState<'static, DefaultAllocator> {
    fn default() -> Self {
        Self {
            allocator: DefaultAllocator,
            gc_debt: 0,
            gc_stop_em: false,
            complete_state: true,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<'a, A: Allocator> MemState<'a, A> {
    /// 创建使用自定义 allocator 的 MemState（创建中：complete_state = false）。
    pub fn new(allocator: A) -> Self {
        Self {
            allocator,
            gc_debt: 0,
            gc_stop_em: false,
            complete_state: false,
            _phantom: std::marker::PhantomData,
        }
    }

    /// 创建运行态 MemState（complete_state = true，对应 C lua_newstate
    /// 完成 f_luaopen 后的状态，紧急 GC 重试路径可用）。
    pub fn new_complete(allocator: A) -> Self {
        let mut m = Self::new(allocator);
        m.complete_state = true;
        m
    }

    fn can_try_again(&self) -> bool {
        self.complete_state && !self.gc_stop_em
    }

    // ========================================================================
    // 底层分配原语
    // ========================================================================

    fn call_alloc(&mut self, block: *mut u8, osize: usize, nsize: usize) -> *mut u8 {
        // 非类型化路径对应 C 的 realloc 语义，1 字节对齐
        self.allocator.alloc(block, osize, nsize, 1)
    }

    fn first_try(&mut self, block: *mut u8, osize: usize, nsize: usize) -> *mut u8 {
        self.call_alloc(block, osize, nsize)
    }

    fn try_again(&mut self, block: *mut u8, osize: usize, nsize: usize) -> *mut u8 {
        if !self.can_try_again() {
            return std::ptr::null_mut();
        }
        self.call_alloc(block, osize, nsize)
    }

    // ========================================================================
    // 原始 realloc — 对应 luaM_realloc_
    // ========================================================================

    pub fn realloc(&mut self, block: *mut u8, old_size: usize, new_size: usize) -> *mut u8 {
        debug_assert_eq!(old_size == 0, block.is_null());
        let mut new_block = self.first_try(block, old_size, new_size);
        if new_block.is_null() && new_size > 0 {
            new_block = self.try_again(block, old_size, new_size);
            if new_block.is_null() {
                return std::ptr::null_mut();
            }
        }
        debug_assert_eq!(new_size == 0, new_block.is_null());
        self.gc_debt -= (new_size as LuaMem) - (old_size as LuaMem);
        new_block
    }

    // ========================================================================
    // 安全 realloc — 对应 luaM_saferealloc_。失败时返回 Err
    // ========================================================================

    pub fn safe_realloc(
        &mut self,
        block: *mut u8,
        old_size: usize,
        new_size: usize,
    ) -> Result<*mut u8, MemError> {
        let new_block = self.realloc(block, old_size, new_size);
        if new_block.is_null() && new_size > 0 {
            return Err(MemError {
                msg: "allocation failed".into(),
            });
        }
        Ok(new_block)
    }

    // ========================================================================
    // 释放 — 对应 luaM_free_
    // ========================================================================

    pub fn free(&mut self, block: *mut u8, size: usize) {
        debug_assert_eq!(size == 0, block.is_null());
        self.call_alloc(block, size, 0);
        self.gc_debt += size as LuaMem;
    }

    // ========================================================================
    // 分配 — 对应 luaM_malloc_。失败时返回 Err
    // ========================================================================

    pub fn malloc(&mut self, size: usize) -> Result<*mut u8, MemError> {
        if size == 0 {
            return Ok(std::ptr::null_mut());
        }
        let mut new_block = self.first_try(std::ptr::null_mut(), 0, size);
        if new_block.is_null() {
            new_block = self.try_again(std::ptr::null_mut(), 0, size);
            if new_block.is_null() {
                return Err(MemError {
                    msg: "allocation failed".into(),
                });
            }
        }
        self.gc_debt -= size as LuaMem;
        Ok(new_block)
    }

    // ========================================================================
    // 类型安全的分配
    // ========================================================================

    pub fn new_box<T>(&mut self) -> Result<Box<T>, MemError> {
        let layout = Layout::new::<T>();
        let ptr = unsafe { alloc::alloc(layout) };
        if ptr.is_null() {
            return Err(MemError {
                msg: "allocation failed".into(),
            });
        }
        self.gc_debt -= layout.size() as LuaMem;
        Ok(unsafe { Box::from_raw(ptr as *mut T) })
    }

    pub fn new_vec<T>(&mut self, n: usize) -> Result<Vec<T>, MemError> {
        if n == 0 {
            return Ok(Vec::new());
        }
        if self.overflow_check::<T>(n) {
            return Err(MemError {
                msg: "block too big".into(),
            });
        }
        let layout = Layout::array::<T>(n).map_err(|_| MemError {
            msg: "block too big".into(),
        })?;
        let ptr = unsafe { alloc::alloc(layout) };
        if ptr.is_null() {
            return Err(MemError {
                msg: "allocation failed".into(),
            });
        }
        self.gc_debt -= layout.size() as LuaMem;
        Ok(unsafe { Vec::from_raw_parts(ptr as *mut T, n, n) })
    }

    // ========================================================================
    // 溢出检查 — 对应 luaM_testsize / luaM_checksize
    // ========================================================================

    /// 检查 n * sizeof(T) 是否溢出
    pub fn overflow_check<T>(&self, n: usize) -> bool {
        let max = usize::MAX / mem::size_of::<T>();
        n > max
    }

    // ========================================================================
    // Vector 扩容 — 对应 luaM_growaux_ / luaM_growvector
    // ========================================================================

    pub fn grow_vec<T>(
        &mut self,
        v: Vec<T>,
        nelems: usize,
        limit: usize,
        what: &str,
    ) -> Result<Vec<T>, MemError> {
        let mut size = v.capacity();
        if nelems + 1 <= size {
            return Ok(v);
        }
        if size >= limit / 2 {
            if size >= limit {
                return Err(MemError {
                    msg: format!("too many {} (limit is {})", what, limit),
                });
            }
            size = limit;
        } else {
            size *= 2;
            if size < MINSIZE_ARRAY {
                size = MINSIZE_ARRAY;
            }
        }
        debug_assert!(nelems + 1 <= size && size <= limit);
        // 直接用 std::alloc API 并传入 T 的对齐，确保与 Vec<T> 的 drop
        // （用 std::alloc::Global 释放）对齐一致，避免 Miri 检测到对齐不匹配 UB。
        // 不通过 self.safe_realloc（DefaultAllocator 用 1 字节对齐，与 Vec<T> 的 drop 不匹配）。
        let new_layout = Layout::array::<T>(size).map_err(|_| MemError {
            msg: "block too big".into(),
        })?;
        let old_layout = Layout::array::<T>(v.capacity()).unwrap();
        let mut v = ManuallyDrop::new(v);
        let old_ptr = v.as_mut_ptr() as *mut u8;
        let new_ptr = unsafe { alloc::realloc(old_ptr, old_layout, new_layout.size()) };
        if new_ptr.is_null() {
            return Err(MemError {
                msg: "allocation failed".into(),
            });
        }
        self.gc_debt -= (new_layout.size() as LuaMem) - (old_layout.size() as LuaMem);
        let new_vec = unsafe { Vec::from_raw_parts(new_ptr as *mut T, nelems, size) };
        Ok(new_vec)
    }

    // ========================================================================
    // Vector 缩容 — 对应 luaM_shrinkvector_
    // ========================================================================

    pub fn shrink_vec<T>(&mut self, v: Vec<T>, final_n: usize) -> Result<Vec<T>, MemError> {
        let old_cap = v.capacity();
        if old_cap == final_n {
            return Ok(v);
        }
        let new_layout = Layout::array::<T>(final_n).map_err(|_| MemError {
            msg: "block too big".into(),
        })?;
        let old_layout = Layout::array::<T>(old_cap).unwrap();
        debug_assert!(new_layout.size() <= old_layout.size());
        let mut v = ManuallyDrop::new(v);
        let old_ptr = v.as_mut_ptr() as *mut u8;
        let new_ptr = unsafe { alloc::realloc(old_ptr, old_layout, new_layout.size()) };
        if new_ptr.is_null() {
            return Err(MemError {
                msg: "allocation failed".into(),
            });
        }
        self.gc_debt -= (new_layout.size() as LuaMem) - (old_layout.size() as LuaMem);
        let new_vec = unsafe { Vec::from_raw_parts(new_ptr as *mut T, final_n, final_n) };
        Ok(new_vec)
    }
}

// ============================================================================
// 常量
// ============================================================================

const MINSIZE_ARRAY: usize = 4;

// ============================================================================
// 简单的 block 类型（对应 lmem.h 中 luaM_newblock）
// ============================================================================

pub struct Block {
    ptr: NonNull<u8>,
    size: usize,
}

impl Block {
    pub unsafe fn new(ptr: *mut u8, size: usize) -> Self {
        Self {
            ptr: NonNull::new_unchecked(ptr),
            size,
        }
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub fn size(&self) -> usize {
        self.size
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 分配器槽的**非 null 不变量**：`AllocOwner` 与 `SlotAlloc` 的任何构造
    /// 入口都不得产生 null 槽（null 会让分配静默回退 std::alloc，使
    /// allocator 参数失去意义）。
    #[test]
    fn alloc_slot_is_never_null() {
        let alloc = crate::mem::TestAlloc::new();
        let owner = AllocOwner::new(Box::new(DefaultAllocator));
        assert!(!owner.slot().is_null());

        let slot = SlotAlloc::from_owner(&owner);
        assert!(!slot.slot().is_null());
        assert!(!alloc.slot().slot().is_null());

        // 经槽分配/释放可用（证明槽指向真实分配器）
        unsafe {
            let boxed: &Box<dyn Allocator> = &*slot.slot();
            let p = boxed.alloc(std::ptr::null_mut(), 0, 32, 8);
            assert!(!p.is_null());
            let _ = boxed.alloc(p, 32, 0, 8);
        }
    }
    /// `AllocOwner::set` 原地替换：槽地址不变（已发出的槽自动跟随新分配器）。
    #[test]
    fn alloc_owner_set_keeps_slot_address() {
        let mut owner = AllocOwner::new(Box::new(DefaultAllocator));
        let before = owner.slot();
        owner.set(Box::new(DefaultAllocator));
        assert_eq!(before, owner.slot(), "替换分配器不得改变槽地址");
    }

    /// 分配器**逐实例独立**：同一线程内两份宿主必须给出不同槽地址。
    ///
    /// 反例（本节禁止）：把同线程的所有分配钉在同一个分配器上 —— 那样
    /// 同一线程里的多个 `LuaState` 就无法各自使用不同的 `allocf`。
    #[test]
    fn allocators_are_per_instance_not_per_thread() {
        let a = AllocOwner::new(Box::new(DefaultAllocator));
        let b = AllocOwner::new(Box::new(DefaultAllocator));
        assert_ne!(a.slot(), b.slot(), "两份 AllocOwner 不得共用槽");

        let ta = TestAlloc::new();
        let tb = TestAlloc::new();
        assert_ne!(
            ta.slot().slot(),
            tb.slot().slot(),
            "两份 TestAlloc 不得共用槽"
        );
        assert_ne!(
            ta.slot().slot(),
            a.slot(),
            "TestAlloc 槽不得与 AllocOwner 复用"
        );
    }

    #[test]
    fn test_alloc_free() {
        let mut mem = MemState::default();
        let ptr = mem.malloc(128).unwrap();
        assert!(!ptr.is_null());
        mem.free(ptr, 128);
    }

    #[test]
    fn test_zero_alloc() {
        let mut mem = MemState::default();
        let ptr = mem.malloc(0).unwrap();
        assert!(ptr.is_null());
    }

    #[test]
    fn test_overflow_check() {
        let mem = MemState::default();
        assert!(mem.overflow_check::<[u8; 64]>(usize::MAX));
        assert!(!mem.overflow_check::<u8>(0));
        assert!(!mem.overflow_check::<u8>(100));
    }

    #[test]
    fn test_realloc_grow() {
        let mut mem = MemState::default();
        let ptr = mem.malloc(64).unwrap();
        let new_ptr = mem.realloc(ptr, 64, 128);
        assert!(!new_ptr.is_null());
        mem.free(new_ptr, 128);
    }

    #[test]
    fn test_realloc_shrink() {
        let mut mem = MemState::default();
        let ptr = mem.malloc(128).unwrap();
        let new_ptr = mem.realloc(ptr, 128, 64);
        assert!(!new_ptr.is_null());
        mem.free(new_ptr, 64);
    }

    #[test]
    fn test_realloc_free() {
        let mut mem = MemState::default();
        let ptr = mem.malloc(64).unwrap();
        let new_ptr = mem.realloc(ptr, 64, 0);
        assert!(new_ptr.is_null());
    }

    #[test]
    fn test_new_box() {
        let mut mem = MemState::default();
        let mut b: Box<i32> = mem.new_box().unwrap();
        *b = 42;
        assert_eq!(*b, 42);
    }

    #[test]
    fn test_new_vec() {
        let mut mem = MemState::default();
        let v: Vec<i32> = mem.new_vec(10).unwrap();
        assert_eq!(v.len(), 10);
    }

    #[test]
    fn test_gc_debt() {
        let mut mem = MemState::default();
        assert_eq!(mem.gc_debt, 0);
        let ptr = mem.malloc(100).unwrap();
        assert_eq!(mem.gc_debt, -100);
        mem.free(ptr, 100);
        assert_eq!(mem.gc_debt, 0);
    }

    #[test]
    fn test_grow_vec() {
        let mut mem = MemState::default();
        let v: Vec<i32> = mem.new_vec(4).unwrap();
        assert_eq!(v.capacity(), 4);
        let v = mem.grow_vec(v, 4, 100, "test").unwrap();
        assert_eq!(v.capacity(), 8);
    }

    #[test]
    fn test_custom_rust_allocator() {
        // Rust API 可注入任意自定义 Allocator（与 C API 的 CapiAllocator 无关）
        struct CountingAllocator {
            n: std::cell::Cell<usize>,
        }
        impl Allocator for CountingAllocator {
            fn alloc(&self, ptr: *mut u8, osize: usize, nsize: usize, align: usize) -> *mut u8 {
                if nsize > osize {
                    self.n.set(self.n.get() + 1);
                }
                DefaultAllocator.alloc(ptr, osize, nsize, align)
            }
        }

        let mut mem = MemState::new(CountingAllocator {
            n: std::cell::Cell::new(0),
        });
        mem.complete_state = true;
        let ptr = mem.malloc(64).unwrap();
        assert_eq!(mem.allocator.n.get(), 1);
        let ptr = mem.realloc(ptr, 64, 128);
        assert_eq!(mem.allocator.n.get(), 2);
        mem.free(ptr, 128);
        assert_eq!(mem.allocator.n.get(), 2);
    }

    #[test]
    fn test_capi_allocator() {
        // 自定义 allocator 经 ud 计数，验证 C 风格原语确实走 allocf
        unsafe extern "C" fn counting_alloc(
            ud: *mut std::ffi::c_void,
            ptr: *mut std::ffi::c_void,
            osize: usize,
            nsize: usize,
        ) -> *mut std::ffi::c_void {
            // 经 `Cell` 共享访问：Tree Borrows 允许 UnsafeCell 内部的可变别名，
            // 而裸指针 + 直接读 `count` 会被判定为冲突。
            let count = unsafe { &*(ud as *const std::cell::Cell<usize>) };
            if nsize > osize {
                count.set(count.get() + 1);
            }
            {
                DefaultAllocator.alloc(ptr as *mut u8, osize, nsize, 1) as *mut std::ffi::c_void
            }
        }

        let count: std::cell::Cell<usize> = std::cell::Cell::new(0);
        let mut mem = MemState::new_complete(CapiAllocator {
            allocf: Some(counting_alloc),
            allocf_ud: &count as *const std::cell::Cell<usize> as *mut std::ffi::c_void,
        });
        let ptr = mem.malloc(64).unwrap();
        assert_eq!(count.get(), 1);
        let ptr = mem.realloc(ptr, 64, 128);
        assert_eq!(count.get(), 2);
        mem.free(ptr, 128);
        assert_eq!(count.get(), 2);
    }
}
