//! Lua 内存管理器
//!
//! 对应 C 源码: lmem.h + lmem.cpp
//!
//! 核心职责:
//! - 封装所有内存分配/释放/重分配操作

use crate::execute::VmError;
use crate::objects::TValue;
use std::alloc::{self, Layout};
use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

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
/// # 为什么宿主是共享的（`Rc`）而不是各持一份
///
/// 槽的**有效性**绑定宿主的**存活期**：对象把槽内联在引用计数块里，块的释放
/// 就发生在槽指向的内层 Box 上。同一份宿主有两类持有者 ——
/// `GlobalState::mem`（lmem 路由）与 [`crate::state::GlobalShared`]（GC 对象
/// 存储：`GcObjects` 里的 `WeakRc`/`WeakValue` 引用，其析构正是经槽回到这里）。
/// 若宿主只挂在 `GlobalState` 上，则当共享块句柄比 `GlobalState` 活得久
/// （`Rc<RefCell<GlobalShared>>` 被外部持有，或同块被多个执行态共享注册对象）
/// 时，GC 存储中的弱引用会按**已析构**的宿主释放块 = use-after-free
/// （Miri: `constructing invalid value of type &Box<dyn Allocator>`）。
///
/// 共享同一宿主后，宿主至少活到**最后一个**持有者；`GlobalShared` 自身持有
/// 一份（声明在 `gc`/`memerrmsg` 之后 ⇒ 晚于块内对象析构），于是
/// 「对象存储晚于其释放宿主析构」成为结构性不变量，与 `GlobalState` 无关。
/// [`Self::set`] 原地替换内层值，所有持有者与已发出的槽一起跟随，语义与
/// C 的 `g->frealloc` 一致。
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
/// `set` 原地覆盖都始终合法，且不会因其他访问而失效。所有权仍由宿主持有
/// （[`AllocInner`] 的 `Drop`），内层值只回收一次（不是 `Box::leak`，无泄漏）。
#[derive(Clone)]
pub struct AllocOwner {
    /// 共享宿主内部（`Rc` 自身用 std 分配，不参与 Lua allocf 路由）。
    inner: Rc<AllocInner>,
}

/// 宿主内部：内层分配器 Box 的根裸指针 + 唯一回收点。
struct AllocInner {
    /// 指向堆上 `Box<dyn Allocator>` 的根裸指针（始终非 null）。
    ptr: NonNull<Box<dyn Allocator>>,
}

impl Drop for AllocInner {
    fn drop(&mut self) {
        // SAFETY: ptr 来自 Box::into_raw，是内层值的唯一所有权点，只回收一次。
        unsafe { drop(Box::from_raw(self.ptr.as_ptr())) };
    }
}

impl AllocOwner {
    /// 接管一个分配器（内层 Box 立即获得稳定堆地址）。
    #[inline]
    pub fn new(allocator: Box<dyn Allocator>) -> Self {
        Self {
            inner: Rc::new(AllocInner {
                ptr: NonNull::new(Box::into_raw(Box::new(allocator)))
                    .expect("Box::into_raw 不得返回 null"),
            }),
        }
    }

    /// 稳定的分配器槽 —— 构造期即可用，且永不为 null。
    ///
    /// 只做指针转换，**不创建对目标值的引用**（见类型级文档）。
    #[inline]
    pub fn slot(&self) -> crate::strings::AllocSlot {
        self.inner.ptr.as_ptr() as *const Box<dyn Allocator>
    }

    /// 原地替换分配器（槽地址不变，所有共享持有者与已发出的槽自动跟随）。
    #[inline]
    pub fn set(&mut self, allocator: Box<dyn Allocator>) {
        // SAFETY: 根裸指针非 null，且指向内层值的唯一所有权点；覆盖即析构旧值。
        unsafe { *self.inner.ptr.as_ptr() = allocator };
    }
}

impl Allocator for AllocOwner {
    #[inline]
    fn alloc(&self, ptr: *mut u8, old_size: usize, new_size: usize, align: usize) -> *mut u8 {
        // SAFETY: 根裸指针非 null 且指向存活的 Box。
        unsafe { (**self.inner.ptr.as_ptr()).alloc(ptr, old_size, new_size, align) }
    }

    #[inline]
    fn allocf_parts(&self) -> Option<(LuaCAlloc, *mut std::ffi::c_void)> {
        // SAFETY: 同上。
        unsafe { (**self.inner.ptr.as_ptr()).allocf_parts() }
    }

    #[inline]
    fn set_allocf(&mut self, f: LuaCAlloc, ud: *mut std::ffi::c_void) -> bool {
        // SAFETY: 同上；`&mut self` 保证与 [`Self::set`] 互斥。
        unsafe { (**self.inner.ptr.as_ptr()).set_allocf(f, ud) }
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
// 分配失败重试漏斗 — 对应 C lmem.c 的 `luaM_realloc_` / `tryagain` / `luaM_error`
// ============================================================================

/// 分配失败标记 —— 对应 C lmem.c 中分配原语返回 `NULL` 的失败分支
/// （`luaM_realloc_` 的 `newblock == NULL && nsize > 0`）。
///
/// 只作内部控制流信号；对 Lua 可见的错误是 [`VmError::MemoryError`]
/// （消息 "not enough memory"，与 C 的 `MEMERRMSG` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocFailed;

/// 内存错误抛出的 panic 载荷 —— 供**没有错误通道**的分配点使用
/// （对应 C 中 `luaM_error` 直接触发 `luaD_throw(L, LUA_ERRMEM)`）。
///
/// 由 `LuaState::raise_memory_error` 抛出，在保护点（`pcall_c_function` 的
/// catch_unwind / longjmp、`LuaState::pcall` 的 catch）转换回
/// [`crate::execute::VmError::MemoryError`]。错误对象是**预注册**的
/// `G(L)->memerrmsg`，抛出与转换全程不分配内存。
///
/// longjmp 模式下 `LuaState::pcall` 的保护点是 `ProtectedExec` + `protected_exec_trampoline`
/// 建立的 setjmp 边界（`state.rs`，对应 C 的 `luaD_pcall`）；而帧入口/栈扩容的容量预留
/// 不抛错 —— 它们在任何 state 变更之前走漏斗，失败直接返回 `ERR_MEM` / `MemoryError`。
#[derive(Debug)]
pub struct MemoryErrorPanic;

impl<'a> crate::state::LuaState<'a> {
    /// 对应的 C 判定 `cantryagain(g)`（lmem.c）：
    /// `completestate(g) && !g->gcstopem`
    /// —— 状态尚未建好（`lua_newstate` 的 `f_luaopen` 之前）或 GC 正在运行时
    /// 都不能做紧急回收（收集器可能还没初始化 / 已经处于收集中）。
    #[inline]
    pub fn can_retry_alloc(&self) -> bool {
        self.g().mem.complete_state && !unsafe { self.gc_mut().is_gc_running() }
    }

    /// C lmem.c: `luaM_realloc_` + `tryagain` —— 通用分配漏斗。
    ///
    /// 首次尝试 →（可重试时）紧急全量 GC（`luaC_fullgc(L, 1)`）→ 重试 →
    /// 仍失败则返回 [`VmError::MemoryError`]（对应 `luaM_error(L)` = `luaD_throw(L, LUA_ERRMEM)`）。
    ///
    /// `op` 必须**可重复执行**且失败时不得留下部分写入：C 的分配原语失败时
    /// 解释器状态与调用前一致（`luaM_saferealloc_` 只在成功后才更新 `*psize`），
    /// 这里同样要求「先扩容再写入」（见 `Table::try_reserve_for` + `set_unchecked`）。
    pub fn retry_alloc<T>(
        &mut self,
        mut op: impl FnMut() -> Result<T, AllocFailed>,
    ) -> Result<T, VmError<'a>> {
        if let Ok(v) = op() {
            return Ok(v);
        }
        core::hint::cold_path();
        if self.can_retry_alloc() {
            self.emergency_gc();
            if let Ok(v) = op() {
                return Ok(v);
            }
            core::hint::cold_path();
        }
        Err(VmError::MemoryError)
    }

    /// `MyRc` 对象块分配漏斗（对应 C 的 `luaM_newobject`）：
    /// 失败 → 紧急 GC → 重试 → 内存错误。
    ///
    /// `value` 在两次尝试之间交还给分配器（失败分支由 [`crate::alloc::MyRc::try_new_in`]
    /// 原样归还），因此没有额外克隆/构造开销。
    pub fn new_object<T: crate::alloc::RcPayload>(
        &mut self,
        value: T,
    ) -> Result<crate::alloc::MyRc<T, SlotAlloc>, VmError<'a>> {
        let slot = self.alloc_slot();
        let mut pending = Some(value);
        self.retry_alloc(move || {
            let v = pending.take().ok_or(AllocFailed)?;
            match crate::alloc::MyRc::try_new_in(slot, v) {
                Ok(rc) => Ok(rc),
                Err(v) => {
                    pending = Some(v);
                    Err(AllocFailed)
                }
            }
        })
    }

    /// 建空表漏斗 —— 对应 C 的 `luaH_new`（块经 `luaM_newobject`）。
    pub fn new_table(&mut self) -> Result<crate::table::Table<'a>, VmError<'a>> {
        let slot = self.alloc_slot();
        self.retry_alloc(move || crate::table::Table::try_new_in(slot))
    }

    /// 带容量建表漏斗 —— 对应 C 的 `lua_createtable`（先分配数组/哈希容量）。
    pub fn new_table_with_capacity(
        &mut self,
        narray: usize,
        nhash: usize,
    ) -> Result<crate::table::Table<'a>, VmError<'a>> {
        let slot = self.alloc_slot();
        self.retry_alloc(move || crate::table::Table::try_with_capacity_in(slot, narray, nhash))
    }

    /// 表写入漏斗 —— 对应 C 的 `luaH_set`（先扩容再写；失败 → 紧急 GC → 重试 → 内存错误）。
    pub fn table_set(
        &mut self,
        t: &crate::table::Table<'a>,
        key: TValue<'a>,
        value: TValue<'a>,
    ) -> Result<(), VmError<'a>> {
        let is_nil = matches!(&value, TValue::Nil(crate::objects::NilKind::Strict));
        self.retry_alloc(|| t.try_reserve_for(&key, is_nil))?;
        t.set_unchecked(key, value);
        Ok(())
    }

    /// 整型键表写入漏斗（见 [`LuaState::table_set`]）。
    pub fn table_set_int(
        &mut self,
        t: &crate::table::Table<'a>,
        key: crate::config::LuaInteger,
        value: TValue<'a>,
    ) -> Result<(), VmError<'a>> {
        let is_nil = matches!(&value, TValue::Nil(crate::objects::NilKind::Strict));
        self.retry_alloc(|| t.try_reserve_for_int(key, is_nil))?;
        t.set_int_unchecked(key, value);
        Ok(())
    }

    /// 建 userdata 漏斗 —— 一次完成「变长块分配 + 初始化 + GC 登记 + id 写回」。
    ///
    /// 对应 C 的 `luaS_newudata`（`luaC_newobj` 建 `sizeudata(nuvalue, len)` 大小的
    /// 单块对象并立即 `setgclist`）：userdata 是**变长对象**，固定头 / 用户值 /
    /// 原始数据同处一块（见 [`crate::objects::Udata`]），没有独立堆分配。
    /// 分配失败与其它分配点同走「紧急 GC → 重试 → 内存错误」漏斗。
    ///
    /// `len` 是给 C 侧看的逻辑字节长度（`lua_rawlen`），数据区按 8 字节向上取整
    /// （C 代码可能在数据区放指针数组，需 8 字节对齐）。
    /// 数据区初始化为 0，用户值初始化为 nil；元表在分配成功后写入。
    pub fn new_udata(
        &mut self,
        nuvalue: usize,
        len: usize,
        metatable: Option<crate::table::Table<'a>>,
    ) -> Result<crate::alloc::MyRc<crate::objects::Udata<'a>, SlotAlloc>, VmError<'a>> {
        use crate::objects::Udata;
        // 头部 `nuvalue` 字段是 u16（对应 C 的 `cast(unsigned short, nuvalue)`，C 侧
        // 另有 api_check 拒绝 >= SHRT_MAX）。这里一次夹紧，使「payload 大小 /
        // 头部字段 / 用户值初始化循环」三者恒一致 —— 否则 `as_ref` 由头部算出的
        // DST 元数据会与分配大小不符，释放时用错 layout。
        let nuvalue = nuvalue.min(u16::MAX as usize);
        let slot = self.alloc_slot();
        // 大小上限（对应 C 的 `luaS_newudata` → `luaM_toobig`：块过大时报
        // "memory allocation error: block too big"，是**常规**运行时错误而非 OOM）。
        let Some(payload_size) = Udata::checked_payload_size(nuvalue, len) else {
            return Err(VmError::RuntimeError(
                "memory allocation error: block too big".to_string(),
            ));
        };
        let ud = self.retry_alloc(move || {
            crate::alloc::MyRc::<Udata<'a>, SlotAlloc>::try_new_unsized_in(
                slot,
                payload_size,
                |p| unsafe { Udata::init_payload(p, nuvalue, len) },
            )
            .map_err(|_| AllocFailed)
        })?;
        // 元表在分配成功后写入（失败路径不会消耗它）
        unsafe { Udata::metatable_of(ud.as_raw_ptr()).replace(metatable) };
        // 用整块大小计费（含 header / 分配器槽 / 用户值 / 数据区）
        let id = self.register_object(
            ud.gc_mem_size(),
            crate::objects::WeakValue::UserData(ud.downgrade()),
        )?;
        ud.gc_header.set_id(id);
        Ok(ud)
    }

    /// 建 Lua 闭包漏斗 —— 一次完成「构造 + 对象块分配 + GC 登记 + id 写回」。
    ///
    /// 对应 C 的 `luaF_newLclosure`（`luaC_newobj` 建对象）。登记使 `gc_estimate`
    /// 增长（`maybe_collect_gc` 阈值检查依赖它），并让 sweep 能回收不可达闭包。
    pub fn new_closure(
        &mut self,
        proto: Rc<crate::objects::Proto<'a>>,
        upvals: Rc<RefCell<crate::objects::UpValVec<'a>>>,
    ) -> Result<crate::alloc::MyRc<crate::objects::LClosure<'a>, SlotAlloc>, VmError<'a>> {
        let closure = self.new_object(crate::objects::LClosure {
            gc_header: crate::gc::GCObjectHeader::new(),
            proto,
            upvals,
        })?;
        // 用 gc_mem_size() 计费含 upvals 容量，比 size_of::<LClosure>() 更接近真实占用
        let id = self.register_object(
            closure.gc_mem_size(),
            crate::objects::WeakValue::LClosure(closure.downgrade()),
        )?;
        closure.gc_header.set_id(id);
        Ok(closure)
    }

    /// 通用漏斗（闭包可取 `&mut self`）—— 用于需要同时改自身状态的分配点，
    /// 例如栈扩容（`luaD_growstack` → `luaM_reallocvector`）。
    pub fn retry_alloc_mut<T>(
        &mut self,
        mut op: impl FnMut(&mut crate::state::LuaState<'a>) -> Result<T, AllocFailed>,
    ) -> Result<T, VmError<'a>> {
        if let Ok(v) = op(self) {
            return Ok(v);
        }
        if self.can_retry_alloc() {
            self.emergency_gc();
            if let Ok(v) = op(self) {
                return Ok(v);
            }
        }
        Err(VmError::MemoryError)
    }

    /// 栈容量预留漏斗 —— 对应 C 的 `luaD_growstack`（栈数组经 `luaM_reallocvector`，
    /// 失败先紧急 GC 再重试，仍失败即内存错误）。
    pub fn reserve_stack(&mut self, additional: usize) -> Result<(), VmError<'a>> {
        self.retry_alloc_mut(|st| st.stack.try_reserve(additional).map_err(|_| AllocFailed))
    }

    /// 抛出内存错误 —— 对应 C 的 `luaM_error(L)` = `luaD_throw(L, LUA_ERRMEM)`。
    ///
    /// 供**没有错误通道**（返回 `()`，无法向上传递 `Result`）的分配点使用，例如帧入口
    /// 的栈扩容与表增长辅助函数。机制与 `capi::raise_mem_error` / `do_lua_error` 完全一致：
    /// 先把预注册的 `memerrmsg` 放进 `pending_error`（**不分配内存**），再按构建模式
    /// panic（unwind 构建）或 longjmp（`panic=abort` 的 bench 构建）跳到最近保护点。
    /// 保护点把 `pending_error` 变回 [`VmError::MemoryError`]。
    #[cold]
    #[inline(never)]
    pub fn raise_memory_error(&mut self) -> ! {
        let msg = self.memerrmsg_value();
        self.last_error_value = Some(msg.clone());
        self.last_error_is_memerr.set(true);
        self.set_memerrmsg_text();
        self.pending_error = Some(msg);
        #[cfg(not(lua_use_longjmp))]
        {
            std::panic::panic_any(MemoryErrorPanic);
        }
        #[cfg(lua_use_longjmp)]
        {
            let Some(&buf) = self.error_jmp_bufs.last() else {
                // 栈空: 顶层错误, 打印后退出 (对齐 C Lua 行为)
                let msg = self.last_error_msg.clone();
                unsafe {
                    libc::write(1, b"lua ".as_ptr() as *const libc::c_void, 4);
                    libc::write(1, msg.as_ptr() as *const libc::c_void, msg.len() as _);
                    libc::write(1, b"\n".as_ptr() as *const libc::c_void, 1);
                }
                std::process::exit(1);
            };
            unsafe { crate::state::lua_rs_longjmp(buf) }
        }
    }

    /// 栈扩容或抛内存错误 —— 对应 C 的 `luaD_growstack`（`luaM_reallocvector` 失败即
    /// `luaM_error`）。容量已够时直接返回，不分配。
    #[inline]
    pub fn grow_stack_or_throw(&mut self, additional: usize) {
        if self.stack.capacity() >= self.stack.len() + additional {
            return;
        }
        if self.reserve_stack(additional).is_err() {
            self.raise_memory_error();
        }
    }
}

// ============================================================================
// 内存状态 — 对应 C 中 global_State 的内存相关字段
// ============================================================================

pub struct MemState<'a, A: Allocator = DefaultAllocator> {
    pub allocator: A,
    pub complete_state: bool,
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl Default for MemState<'static, DefaultAllocator> {
    fn default() -> Self {
        Self {
            allocator: DefaultAllocator,
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
}
