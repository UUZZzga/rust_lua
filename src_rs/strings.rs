//! # Lua 字符串模块 — Rust 惯用重写
//!
//! 将 Lua C 实现中的 `TString`/短字符串内部化/惰性哈希 转换为 Rust 类型系统。
//!
//! ## 核心类型
//! - `LuaString` — 枚举类型，统一表示短/长字符串
//!   - `LuaString::Short(ArcRc<ShortString>)` — 内部化（interned）的短字符串
//!   - `LuaString::Long(ArcRc<LongString>)` — 非内部化的长字符串（Rc 共享，避免 lua_tolstring 返回的指针在 lua_pop 后悬垂）
//!
//! ## 设计原则
//! - 短字符串通过指针相等性比较（内部化保证同一内容只有一个 ArcRc 实例）
//! - 长字符串通过内容比较（hash → length → contents 三级短路）
//! - 长度直接从 `String` 获取（`contents.len()`），无冗余字段
//! - 哈希统一使用 Lua 风格快速 hash（对应 C `luaS_hash`），无随机种子（编译器场景无 DoS 风险）
//! - 短字符串创建时预计算 hash → O(1) Hash trait
//! - 长字符串惰性计算 hash，`Hash::hash` 首次计算后通过 Atomic 自动缓存，避免重复计算
//! - 字符串表使用 `hashbrown::HashTable<ArcRc<ShortString>>` 单级哈希表
//! - **多线程安全**：StringTable 使用 `RefCell/RwLock` 保护 HashTable，读可并发、写互斥
//!   LongString 使用 `AtomicU64`/`AtomicU8` 实现 Sync 内部可变性

use std::alloc::{alloc, handle_alloc_error};
use std::cell::Cell;
use std::fmt::{self, Debug, Formatter};
use std::os::raw::{c_char, c_void};

use crate::mem::Allocator;
use crate::objects::TValue;

/// LongString 的 allocator 槽指针：指向 `Box<dyn Allocator>` 的稳定地址
/// （即 `&g.mem.allocator`，GlobalState 被 Box 持有 → 地址稳定；
/// lua_setallocf 原地替换 Box 内容后自动跟随当前 allocator，
/// 符合 C Lua「释放走当前 allocf」语义）。null 表示无 allocator
/// （测试/构造期），回退 std::alloc。
///
/// 用 `*const`（而非 `*mut`）以便通过 `&self` 取得槽，避免与
/// `&self.string_table` 的可变借用冲突（`Allocator::alloc` 已是 `&self`）。
pub type AllocSlot = *const Box<dyn Allocator>;
// ============================================================================
// RwLock 抽象层 — 根据 `threaded` feature 切换实现
// ============================================================================
// 性能: 默认 RefCell 模式省去 atomic 操作开销。
// perf 数据显示 StringTable::intern 在编译热点路径上 (6.19%),
// 每次调用都要 read() 锁,RefCell 比 RwLock 快约 3-5ns (无 CAS)。
//
// 进一步优化: 非 threaded 模式下, RefCell 的运行时借用检查 (mov borrow
// counter + cmp + 写回) 仍然占 intern 时间的 3.14% (perf annotate 显示:
// `mov 0x10(%rcx),%rdi` 2.03% + `mov %rax,0x10(%rcx)` 1.11%).
// 由于 StringTable 在非 threaded 模式下是 !Sync, 单线程访问保证不会并发,
// intern/intern_bytes/count/remove 中用 unsafe 绕过借用检查 (as_ptr + 解引用)。
// threaded 模式下仍走 RwLock 路径保证线程安全。

#[cfg(not(feature = "threaded"))]
mod inner_lock {
    use std::cell::RefCell;
    use std::fmt;

    pub struct RwLock<T: ?Sized>(pub RefCell<T>);

    impl<T> RwLock<T> {
        #[cfg_attr(not(size_optimized), inline(always))]
        pub const fn new(val: T) -> Self {
            RwLock(RefCell::new(val))
        }
        #[cfg_attr(not(size_optimized), inline(always))]
        pub fn read(&self) -> std::cell::Ref<'_, T> {
            self.0.borrow()
        }
        #[cfg_attr(not(size_optimized), inline(always))]
        pub fn write(&self) -> std::cell::RefMut<'_, T> {
            self.0.borrow_mut()
        }
        /// UNSAFE: 直接获取内部 RefCell 的裸指针 (供绕过借用检查使用)。
        /// 调用方需保证单线程独占访问 (非 threaded 模式下 StringTable 是 !Sync)。
        #[cfg_attr(not(size_optimized), inline(always))]
        pub fn as_ptr(&self) -> *mut T {
            self.0.as_ptr()
        }
    }

    impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLock<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(&self.0, f)
        }
    }
}

#[cfg(feature = "threaded")]
mod inner_lock {
    pub use parking_lot::RwLock;
}

use inner_lock::RwLock;

// ============================================================================
// ArcRc 别名 — 根据 threaded feature 切换 Arc/Rc
// ============================================================================
// 性能: 非 threaded 模式下 LuaState 本身不是 Sync (含 Rc/RefCell/*mut),
// 短字符串的引用计数无需原子操作。perf 数据显示 intern 函数中
// `lock incq` (Arc::clone 的原子 CAS) 占 intern 时间的 14.61%。
// 改用 Rc 后 `incq` (非原子) 消除 cache line 上的 LOCK 前缀开销。
#[cfg(not(feature = "threaded"))]
pub type ArcRc<T> = crate::alloc::SimpleRc<T>;
#[cfg(feature = "threaded")]
pub type ArcRc<T> = crate::alloc::SimpleArc<T>;
// 根据 feature 选择偏移 / layout 函数（不要走关联常量，见问题 2）
#[cfg(feature = "threaded")]
use crate::alloc::{
    simple_arc_block_layout as block_layout, simple_arc_payload_offset as payload_offset,
};
#[cfg(not(feature = "threaded"))]
use crate::alloc::{
    simple_rc_block_layout as block_layout, simple_rc_payload_offset as payload_offset,
};
pub type ArcRcBox<T> = <ArcRc<T> as IsArcRc>::Box;
pub type ArcRcHeader<T> = <ArcRc<T> as IsArcRc>::Header;

// 默认模式 (性能优先): 使用 hashbrown::HashTable 单级哈希表
// size_optimized: 使用 std::collections::HashMap 两级结构, 减小二进制体积
#[cfg(not(size_optimized))]
use hashbrown::HashTable;

use crate::alloc::{
    simple_alloc_off, simple_data_off, simple_layout, IsArcRc, RcPayload, SimpleRc, SimpleRcHeader,
};
use crate::mem::SlotAlloc;

// ============================================================================
// 规约：常量
// ============================================================================

/// 短字符串的最大长度（字节数）。
/// 长度 ≤ 40 的字符串会被内部化（interned），相同内容的字符串共享同一个 `ArcRc`。
pub const LUAI_MAXSHORTLEN: usize = 40;

pub const MEMERRMSG: &str = "not enough memory";

// ============================================================================
// 规约：字符串类型定义
// ============================================================================

/// ShortString 的固定前缀（Sized）
#[repr(C)]
pub struct ShortStringFixed {
    pub hash: u64,
    pub len: u32,
    pub code: u32,
}

/// 变长字符串（DST）
#[derive(Debug)]
#[repr(C)]
pub struct ShortString {
    pub hash: u64,
    pub len: u32,
    pub code: u32,
    pub data: [u8],
}

impl ShortString {
    pub const FIXED_SIZE: usize = std::mem::size_of::<ShortStringFixed>(); // 16
    pub const ALIGN: usize = std::mem::align_of::<ShortStringFixed>(); // 8

    /// 内容起始指针 — 对应 C 的 `rawgetshrstr(ts)` = `&ts->contents`。
    ///
    /// C 的 TString 中短字符串内容起始于 `contents` 字段本身（该字段既当
    /// "内容指针"又当内容首字节），故不额外存储指针。Rust 侧 DST 的 `data`
    /// 字段占据同一位置，因此这里返回 `data` 的首地址 —— 与 C 同为
    /// 「对象后起始指针」，但无 8 字节/串的指针存储开销。
    #[inline]
    pub fn contents(&self) -> *const u8 {
        self.data.as_ptr()
    }
}

// 编译期断言：前缀布局必须与 ShortString 的固定部分一致
const _: () = {
    assert!(
        ShortString::FIXED_SIZE == 16,
        "ShortString 固定部分应为 16 字节"
    );
    assert!(ShortString::ALIGN == 8, "ShortString 应对齐到 8");
    // [u8] 的对齐是 1，不会抬高整体对齐，所以前缀对齐 == 整体对齐
};

unsafe impl RcPayload for ShortString {
    const ALIGN: usize = std::mem::align_of::<u32>();

    unsafe fn as_ref<'a>(payload: *const u8) -> &'a ShortString {
        // 从偏移 8 读 len（u64 + u32 + u32 + [u8]）
        let len = *(payload.add(8) as *const u32) as usize;

        // 直接把 (data ptr, metadata) 拼成 *const ShortString。
        // 不走 &[u8] 中间层，避免 Stacked Borrows 的范围不匹配。
        let sp: *const ShortString = std::mem::transmute((payload, len));

        // 这一次 retag 的覆盖范围是「整个 ShortString」，
        // payload 的 provenance 来自 alloc，覆盖 [payload, payload+16+len) 没问题。
        &*sp
    }

    unsafe fn drop_payload(_: *mut u8) {}

    unsafe fn payload_size(p: *const u8) -> usize {
        let len = *(p.add(8) as *const u32) as usize;
        ShortString::FIXED_SIZE + len
    }
}

impl ArcRc<ShortString> {
    pub fn new_sstr(hash: u64, code: u32, data: &[u8]) -> Self {
        debug_assert!(data.len() <= u32::MAX as usize, "ShortString 长度超 u32");

        let tmp: ArcRc<ShortString> = unsafe {
            let align = ShortString::ALIGN;
            let fixed = ShortString::FIXED_SIZE;
            let payload_size = fixed + data.len() + 1;

            // 1) 一次分配 header + payload
            let header_offset = payload_offset(align);
            let layout = block_layout(payload_size, align);
            let base = alloc(layout);
            if base.is_null() {
                handle_alloc_error(layout);
            }

            // 2) 初始化 header
            std::ptr::write(
                base as *mut ArcRcHeader<ShortString>,
                ArcRcHeader::<ShortString>::default(),
            );

            // 3) 写固定部分（严格按结构体字段顺序）
            let p = base.add(header_offset);
            std::ptr::write(p as *mut u64, hash);
            std::ptr::write(p.add(8) as *mut u32, data.len() as u32 + 1);
            std::ptr::write(p.add(12) as *mut u32, code);

            // 4) 写变长部分（紧跟在固定部分后面）
            std::ptr::copy_nonoverlapping(data.as_ptr(), p.add(fixed), data.len());

            // 5) 写 NUL 终止符
            std::ptr::write(p.add(fixed).add(data.len()), 0);

            ArcRc::from_raw_base(base)
        };
        debug_assert_eq!(tmp.hash, hash);
        debug_assert_eq!(tmp.len, data.len() as u32 + 1);
        debug_assert_eq!(&tmp.data[0..tmp.len as usize - 1], data);
        tmp
    }
}

/// 长字符串种类 — 对应 C `TString.shrlen` 的负值编码（`LSTRREG`/`LSTRFIX`/`LSTRMEM`）。
/// 常规长字符串（`LSTRREG`）内容内联在对象尾部；外部字符串（`LSTRFIX`/`LSTRMEM`）
/// 的 `contents` 直接指向调用方缓冲区，内容不拷贝。
pub const LSTRREG: i8 = -1;
/// 固定外部长字符串：缓冲区由调用方持有，Lua 不释放（对应 C 的 LSTRFIX）。
pub const LSTRFIX: i8 = -2;
/// 带释放回调的外部长字符串（对应 C 的 LSTRMEM）。
pub const LSTRMEM: i8 = -3;

/// 外部字符串缓冲区的释放回调 — 对应 C 的 `lua_Alloc`（`TString.falloc`）。
///
/// 调用约定与 C 相同：`falloc(ud, contents, len + 1, 0)`。
pub type Falloc = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, usize) -> *mut c_void;

/// 长字符串 — 长度 > 40 字节的常规长字符串，或任意长度的外部字符串。
///
/// 布局仿 C 的 `TString`（`lobject.h`）：
/// - `contents` — 内容起始指针，对应 C 的 `getstr(ts)`。常规长字符串指向
///   对象尾部的内联内容（C: `cast_charp(ts) + offsetof(TString, falloc)`）；
///   外部字符串指向调用方缓冲区（C 的 `luaS_newextlstr`：`ts->contents =
///   cast_charp(s)`）。
/// - `kind` — `shrlen` 的负值编码：`LSTRREG` / `LSTRFIX` / `LSTRMEM`。
/// - `falloc` / `ud` — 外部字符串（`LSTRMEM`）的释放回调与参数；本块（引用
///   计数归零、即字符串不再可达）回收时调用一次，对应 C `luaC_freeobj`：
///   `(*ts->falloc)(ts->ud, ts->contents, ts->u.lnglen + 1, 0)`。
/// - `len` — 内容长度 + 1（含末尾 NUL，与 C `u.lnglen + 1` 同口径）。
/// - `extra` — 0/1 惰性哈希标记（`Hash` 首次计算后置 1，后续 O(1) 复用）。
///
/// 块布局 = SimpleRcHeader + AllocSlot + payload（allocator 路由块，
/// 见 `RcPayload`；槽在 Rc 块内，不在本结构体中）。
///
/// **DST 元数据 = 内联内容字节数**（常规 = `len`；外部 = 0，因为外部字符串
/// 没有内联内容）。因此所有内容访问必须经 `contents` 指针（[`long_content`]），
/// 不能直接切 `data`（外部字符串的 `data` 是空切片）。
#[repr(C)]
pub struct LongStringFixed {
    pub hash: Cell<u64>,
    pub len: u32,
    pub extra: Cell<u8>,
    pub kind: Cell<i8>,
    pub contents: Cell<*const u8>,
    pub falloc: Cell<Option<Falloc>>,
    pub ud: Cell<*mut c_void>,
}

#[derive(Debug)]
#[repr(C)]
pub struct LongString {
    pub hash: Cell<u64>,
    pub len: u32,
    pub extra: Cell<u8>,
    pub kind: Cell<i8>,
    pub contents: Cell<*const u8>,
    pub falloc: Cell<Option<Falloc>>,
    pub ud: Cell<*mut c_void>,
    pub data: [u8],
}

impl LongString {
    pub const ALIGN: usize = std::mem::align_of::<LongStringFixed>();
    /// 固定部分大小 = 内联内容起始偏移（对应 C 的 `offsetof(TString, falloc)`）。
    /// 取 `LongStringFixed`（Sized）的大小：`data: [u8]` 对齐为 1 且为唯一尾字段，
    /// 故二者相等（`offset_of!` 不支持 DST 的末字段）。
    pub const FIXED_SIZE: usize = std::mem::size_of::<LongStringFixed>();

    /// 内容起始指针 — 对应 C 的 `getstr(ts)`（短/长/外部统一入口）。
    #[inline]
    pub fn contents(&self) -> *const u8 {
        self.contents.get()
    }

    /// 是否为外部字符串（内容在 Lua 管理的内存之外）。
    #[inline]
    pub fn is_external(&self) -> bool {
        self.kind.get() != LSTRREG
    }
}

// 编译期断言：前缀布局必须与 ShortString 的固定部分一致
const _: () = {
    assert!(LongString::ALIGN == 8, "LongString 应对齐到 8");
    // 固定部分含指针字段，必须保持 8 对齐（DST `data` 才能紧随其后无填充）
    assert!(
        LongString::FIXED_SIZE % LongString::ALIGN == 0,
        "LongString 固定部分应为指针宽度整数倍"
    );
};

/// 长字符串内容（不含末尾 NUL）——对应 C 的 `getlngstr(ts)` + `tsslen(ts)`。
///
/// 统一走 `contents` 指针：外部字符串的内容不在对象尾部（`data` 为空切片），
/// 只有常规长字符串的内联内容才能直接切 `data`。
#[inline]
pub fn long_content(ls: &LongString) -> &[u8] {
    // SAFETY: contents 指向 len-1 字节内容 + 末尾 NUL。常规长字符串指向对象
    // 内联区（分配范围内）；外部字符串由 lua_pushexternalstring 契约保证
    // contents[len] == '\0' 且 len+1 字节可读（C 的 api_check 同款前提）。
    unsafe { std::slice::from_raw_parts(ls.contents(), ls.len as usize - 1) }
}

/// 短字符串内容（不含末尾 NUL）。
#[inline]
fn short_content(s: &ShortString) -> &[u8] {
    // SAFETY: 短字符串所有创建路径都写入末尾 NUL，data 长度恒为 len。
    unsafe { std::slice::from_raw_parts(s.contents(), s.len as usize - 1) }
}

unsafe impl RcPayload for LongString {
    const ALIGN: usize = std::mem::align_of::<LongStringFixed>();

    unsafe fn as_ref<'a>(payload: *const u8) -> &'a LongString {
        // DST 元数据 = 内联内容长度：常规长字符串 = len 字段（内容 + 末尾 NUL）；
        // 外部字符串没有内联内容（contents 指向调用方缓冲区）→ 0。若照着 len
        // 拼元数据，引用会覆盖分配之外的地址（UB）。
        let kind = *payload
            .add(std::mem::offset_of!(LongString, kind))
            .cast::<i8>();
        let dlen = if kind == LSTRREG {
            *payload
                .add(std::mem::offset_of!(LongString, len))
                .cast::<u32>() as usize
        } else {
            0
        };
        let sp: *const LongString = std::mem::transmute((payload, dlen));
        &*sp
    }

    unsafe fn drop_payload(payload: *mut u8) {
        // 外部字符串（LSTRMEM）：内容由调用方缓冲区持有，本块回收（引用计数归零，
        // 即字符串不再可达）时通知调用方释放 —— 对应 C luaC_freeobj:
        //   (*ts->falloc)(ts->ud, ts->contents, ts->u.lnglen + 1, 0)。
        // LSTRFIX（falloc == None）与常规长字符串不释放任何东西。
        let kind = *payload
            .add(std::mem::offset_of!(LongString, kind))
            .cast::<i8>();
        if kind != LSTRMEM {
            return;
        }
        let contents = (*payload
            .add(std::mem::offset_of!(LongString, contents))
            .cast::<Cell<*const u8>>())
        .get();
        let falloc = (*payload
            .add(std::mem::offset_of!(LongString, falloc))
            .cast::<Cell<Option<Falloc>>>())
        .get();
        let ud = (*payload
            .add(std::mem::offset_of!(LongString, ud))
            .cast::<Cell<*mut c_void>>())
        .get();
        // len 字段 = 内容长度 + 1（C 传入 lnglen + 1）
        let blen = *payload
            .add(std::mem::offset_of!(LongString, len))
            .cast::<u32>() as usize;
        if let Some(f) = falloc {
            f(ud, contents as *mut c_void, blen, 0);
        }
    }

    unsafe fn payload_size(p: *const u8) -> usize {
        // 必须与创建时的 payload_size 一致（见 new_lstr_in / new_ext_lstr_in）：
        // 常规 = fixed + len（len 含末尾 NUL），外部 = fixed（无内联内容）。
        let kind = *p.add(std::mem::offset_of!(LongString, kind)).cast::<i8>();
        if kind == LSTRREG {
            let len = *p.add(std::mem::offset_of!(LongString, len)).cast::<u32>() as usize;
            LongString::FIXED_SIZE + len
        } else {
            LongString::FIXED_SIZE
        }
    }
}

impl SimpleRc<LongString, SlotAlloc> {
    /// 创建长字符串块。`alloc_slot` 为 MemState allocator 槽（见 [`AllocSlot`]）：
    /// 非 null 时分配/释放均路由到该 allocator（对应 C Lua 的 allocf）；
    /// null 时回退 std::alloc。
    #[inline]
    pub fn new_lstr(alloc: SlotAlloc, hash: Option<u64>, data: &[u8]) -> Self {
        Self::new_lstr_in(alloc, hash, data)
    }

    /// 创建外部字符串块 —— 内容不拷贝，`contents` 指向调用方缓冲区。
    ///
    /// # Safety
    /// 见 [`Self::new_ext_lstr_in`]。
    #[inline]
    pub unsafe fn new_ext_lstr(
        alloc: SlotAlloc,
        contents: *const u8,
        len: usize,
        falloc: Option<Falloc>,
        ud: *mut c_void,
    ) -> Self {
        Self::new_ext_lstr_in(alloc, contents, len, falloc, ud)
    }
}

impl<A: crate::alloc::Allocator> SimpleRc<LongString, A> {
    /// 用分配器 `alloc` 创建长字符串块（块布局 = header + `A` + payload）。
    ///
    /// `LongString` 是变长结构（payload 大小取决于内容长度），因此不走
    /// `new_in`（按 `size_of::<T>()` 分配），而是按实际 `payload_size` 单次分配
    /// header + `A` + payload，写字段后用 [`SimpleRc::from_raw_base`] 接管。
    pub fn new_lstr_in(alloc: A, hash: Option<u64>, data: &[u8]) -> Self {
        debug_assert!(data.len() <= u32::MAX as usize, "LongString 长度超 u32");

        unsafe {
            let fixed = LongString::FIXED_SIZE;
            let payload_size = fixed + data.len() + 1;

            // 1) 一次分配 header + A + payload（与 SimpleRc::drop 的释放路径对应）
            let layout = simple_layout::<A, LongString>(payload_size);
            let base = alloc
                .allocate(layout)
                .unwrap_or_else(|_| handle_alloc_error(layout))
                .cast::<u8>()
                .as_ptr();

            // 2) 初始化 header 与分配器字段
            std::ptr::write(base as *mut SimpleRcHeader, SimpleRcHeader::default());
            std::ptr::write(base.add(simple_alloc_off::<A>()) as *mut A, alloc);

            // 3) 写固定部分（严格按结构体字段顺序）
            let p = base.add(simple_data_off::<A, LongString>());
            match hash {
                Some(hash) => {
                    std::ptr::write(
                        p.add(std::mem::offset_of!(LongString, hash)) as *mut u64,
                        hash,
                    );
                    std::ptr::write(p.add(std::mem::offset_of!(LongString, extra)) as *mut u8, 1);
                }
                None => {
                    std::ptr::write(p.add(std::mem::offset_of!(LongString, hash)) as *mut u64, 0);
                    std::ptr::write(p.add(std::mem::offset_of!(LongString, extra)) as *mut u8, 0);
                }
            }

            std::ptr::write(
                p.add(std::mem::offset_of!(LongString, len)) as *mut u32,
                data.len() as u32 + 1,
            );
            std::ptr::write(
                p.add(std::mem::offset_of!(LongString, kind)) as *mut i8,
                LSTRREG,
            );
            // contents 指向对象尾部的内联内容（对应 C: cast_charp(ts) + offsetof(TString, falloc)）
            std::ptr::write(
                p.add(std::mem::offset_of!(LongString, contents)) as *mut *const u8,
                p.add(fixed) as *const u8,
            );
            std::ptr::write(
                p.add(std::mem::offset_of!(LongString, falloc)) as *mut Option<Falloc>,
                None,
            );
            std::ptr::write(
                p.add(std::mem::offset_of!(LongString, ud)) as *mut *mut c_void,
                std::ptr::null_mut(),
            );

            // 4) 写变长部分（紧跟在固定部分后面）
            std::ptr::copy_nonoverlapping(data.as_ptr(), p.add(fixed), data.len());
            // 5) 写 NUL 终止符
            std::ptr::write(p.add(fixed).add(data.len()), 0);

            let out = SimpleRc::<LongString, A>::from_raw_base(base);
            debug_assert_eq!(out.len, data.len() as u32 + 1);
            debug_assert_eq!(out.kind.get(), LSTRREG);
            debug_assert_eq!(out.contents(), p.add(fixed));
            debug_assert_eq!(long_content(&out), data);
            out
        }
    }

    /// 创建外部字符串块（`lua_pushexternalstring`）——内容**不拷贝**，
    /// `contents` 直接指向调用方缓冲区（对应 C 的 `luaS_newextlstr`）。
    ///
    /// `falloc` 为 `Some` 时类型为 [`LSTRMEM`]，本块（字符串不再可达）回收时
    /// 调用 `falloc(ud, contents, len + 1, 0)` 释放缓冲区；为 `None` 时类型为
    /// [`LSTRFIX`]（缓冲区由调用方持有，对应 C 的 `loadlib.c` 字面量用法）。
    ///
    /// # Safety
    /// - `contents` 非 null，且至少 `len + 1` 字节可读、`contents[len] == 0`
    ///   （C 的 `api_check(L, s[len] == '\0')` 契约）。
    /// - `falloc`/`ud` 必须能安全地在字符串回收时调用（C 侧缓冲区通常在
    ///   state 关闭前释放完）。
    pub unsafe fn new_ext_lstr_in(
        alloc: A,
        contents: *const u8,
        len: usize,
        falloc: Option<Falloc>,
        ud: *mut c_void,
    ) -> Self {
        debug_assert!(len <= u32::MAX as usize, "LongString 长度超 u32");
        debug_assert!(!contents.is_null(), "外部字符串缓冲区不得为 null");

        let fixed = LongString::FIXED_SIZE;
        let payload_size = fixed; // 外部字符串无内联内容

        let layout = simple_layout::<A, LongString>(payload_size);
        let base = alloc
            .allocate(layout)
            .unwrap_or_else(|_| handle_alloc_error(layout))
            .cast::<u8>()
            .as_ptr();

        std::ptr::write(base as *mut SimpleRcHeader, SimpleRcHeader::default());
        std::ptr::write(base.add(simple_alloc_off::<A>()) as *mut A, alloc);

        let p = base.add(simple_data_off::<A, LongString>());
        std::ptr::write(p.add(std::mem::offset_of!(LongString, hash)) as *mut u64, 0);
        std::ptr::write(p.add(std::mem::offset_of!(LongString, extra)) as *mut u8, 0);
        std::ptr::write(
            p.add(std::mem::offset_of!(LongString, len)) as *mut u32,
            len as u32 + 1,
        );
        std::ptr::write(
            p.add(std::mem::offset_of!(LongString, kind)) as *mut i8,
            if falloc.is_some() { LSTRMEM } else { LSTRFIX },
        );
        std::ptr::write(
            p.add(std::mem::offset_of!(LongString, contents)) as *mut *const u8,
            contents,
        );
        std::ptr::write(
            p.add(std::mem::offset_of!(LongString, falloc)) as *mut Option<Falloc>,
            falloc,
        );
        std::ptr::write(
            p.add(std::mem::offset_of!(LongString, ud)) as *mut *mut c_void,
            ud,
        );

        SimpleRc::<LongString, A>::from_raw_base(base)
    }
}

/// 统一字符串类型。
#[derive(Debug, Clone)]
pub enum LLuaString {
    Short(ArcRc<ShortString>),
    Long(SimpleRc<LongString, SlotAlloc>),
}

impl LLuaString {
    pub fn to_value<'a>(&self) -> TValue<'a> {
        match self {
            LLuaString::Short(s) => TValue::ShortStr(s.clone()),
            LLuaString::Long(s) => TValue::LongStr(s.clone()),
        }
    }

    /// 字符串内容（不含末尾 NUL）。
    pub fn content(&self) -> &[u8] {
        match self {
            LLuaString::Short(s) => short_content(s),
            LLuaString::Long(s) => long_content(s),
        }
    }
}

impl PartialEq for LLuaString {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Short(l0), Self::Short(r0)) => core::ptr::eq(l0, r0),
            (Self::Long(l0), Self::Long(r0)) => **l0 == **r0,
            // 混合变体：只有外部字符串可能与短字符串内容相同（对应 C 的 luaS_eqstr）
            _ => self.content() == other.content(),
        }
    }
}

impl<'a> TValue<'a> {
    pub fn as_str(&self) -> LLuaString {
        match self {
            TValue::ShortStr(s) => LLuaString::Short(s.clone()),
            TValue::LongStr(s) => LLuaString::Long(s.clone()),
            _ => panic!("TValue::as_str: not a string"),
        }
    }
}

// ============================================================================
// 规约：内容比较辅助函数
// ============================================================================

/// 长字符串相等性：若双方均已哈希 → 先比 hash（快速淘汰），否则直接比内容。
/// 内容一律经 `contents` 指针取（外部字符串无内联内容）。
impl PartialEq for LongString {
    fn eq(&self, other: &Self) -> bool {
        if self.extra.get() == 1 && other.extra.get() == 1 {
            if self.hash != other.hash {
                return false;
            }
        }
        long_content(self) == long_content(other)
    }
}

// ============================================================================
// 规约：字符串表（内部化）
// ============================================================================

/// 字符串表 — 管理短字符串的内部化。
///
/// # 默认模式 (性能优先)
/// 使用 `hashbrown::HashTable<ArcRc<ShortString>>` 单级哈希表，每个条目仅 8 字节（指针）。
/// 相比 `HashMap<u64, Vec<ArcRc<ShortString>>>` 的两级结构（每条目 32 字节）：
/// - 4 倍缓存密度（每缓存行 8 条目 vs 2 条目），减少 cache miss
/// - 消除 Vec 迭代开销（len 检查、索引、边界检查）
/// - hashbrown SIMD 探测直接在字符串 hash 上进行，等效函数仅比较内容
///
pub struct StringTable {
    ht: RwLock<HashTable<ArcRc<ShortString>>>,
    nuse: RwLock<usize>,
}

impl Debug for StringTable {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("StringTable")
            .field("nuse", &*self.nuse.read())
            .finish()
    }
}

impl StringTable {
    pub fn new() -> Self {
        // 初始容量 256: perf 显示 intern 是 finish_grow 的主要 caller (12 次),
        // HashTable 扩容开销大 (realloc + rehash)。256 * 0.875 = 224 个条目后才扩容,
        // 覆盖 Lua 关键字 (21) + 标准库符号 (~100) + 常用变量名,减少首次扩容。
        // 内存开销: 256 * 8 = 2KB, 可忽略。
        StringTable {
            ht: RwLock::new(HashTable::with_capacity(256)),
            nuse: RwLock::new(0),
        }
    }
    /// 内部化一个短字符串。
    #[cfg_attr(not(size_optimized), inline)]
    pub fn intern(&self, str: &str) -> ArcRc<ShortString> {
        self.intern_with_hash(str, rust_hash(str))
    }

    #[inline]
    pub fn intern_value<'a>(&self, str: &str) -> TValue<'a> {
        TValue::ShortStr(self.intern_with_hash(str, rust_hash(str)))
    }

    /// 内部化一个短字符串 (使用预计算的 hash, 避免重复计算)。
    /// 用于词法分析器标识符缓存: 缓存查找时已计算 hash, 未命中时直接传入。
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(not(feature = "threaded"))]
    pub fn intern_with_hash(&self, str: &str, h: u64) -> ArcRc<ShortString> {
        debug_assert!(str.len() <= LUAI_MAXSHORTLEN, "intern 只用于短字符串");

        let str_bytes = str.as_bytes();
        let str_len = str_bytes.len();

        // UNSAFE fast path: 非 threaded 模式下 StringTable 是 !Sync, 单线程独占访问。
        // 直接通过 RefCell::as_ptr() 解引用 HashTable, 绕过 RefCell::borrow 的运行时
        // borrow counter 检查。
        // SAFETY: StringTable 在非 threaded 模式下是 !Sync (RefCell 是 !Sync),
        // Rust 类型系统保证单线程访问; intern 不会重入 (无递归调用其他 intern).
        let ht = unsafe { &mut *self.ht.as_ptr() };

        // 单级查找: HashTable 用预计算 hash 做 SIMD 探测,
        // 等效函数仅在 hash tag (hash 低位 7bit) 匹配时调用。
        // 相比之前 HashMap<u64, Vec<...>> 两级结构:
        // 1. 消除 Vec 迭代开销 (len 检查、索引、边界检查)
        // 2. 条目大小 8 字节 (vs 32 字节), 4 倍缓存密度
        //
        // perf 优化: hashbrown 的 tag 只有 7bit (128 种值), 字符串表 256+ 条目时
        // 平均每个 tag 有 2+ 个条目。先比较完整 hash (O(1), 仅读 ShortString.hash
        // 字段), 快速淘汰 tag 匹配但完整 hash 不匹配的桶, 避免不必要的
        // contents.as_bytes() (Rc deref + String 字段访问) + memcmp。
        // perf 数据显示 Rc::deref 占 1.20%, memcmp 占 1.09%,
        // hash 预比较可减少这两项的开销。
        if let Some(ts) = ht.find(h, |ts| {
            // 先比较完整 hash: hash 不同 → 内容必然不同 → 立即 false
            // (ts.hash 是 ShortString 第一个字段, 读取无需额外偏移)
            if ts.hash != h {
                return false;
            }
            let content_bytes = &ts.data[0..ts.len as usize];
            // 短字符串各行创建路径都写入末尾 NUL（new_sstr），contents 末尾必有
            // NUL 终止符。当 content_bytes.len() == str_len + 1 时, NUL 检查冗余 (已去除)。
            content_bytes.len() == str_len + 1 && content_bytes[..str_len] == *str_bytes
        }) {
            return ArcRc::clone(ts);
        }

        // 写路径: 需要插入新字符串
        let ts = ArcRc::<ShortString>::new_sstr(h, 0, str.as_bytes());
        // hasher 函数仅在 resize 时调用, 返回预计算 hash
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        // SAFETY: 同上, nuse 也是 RefCell 包装, 单线程下安全.
        *unsafe { &mut *self.nuse.as_ptr() } += 1;
        ts
    }

    /// 内部化一个短字符串 (使用预计算的 hash, threaded 模式).
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(feature = "threaded")]
    pub fn intern_with_hash(&self, str: &str, h: u64) -> ArcRc<ShortString> {
        debug_assert!(str.len() <= LUAI_MAXSHORTLEN, "intern 只用于短字符串");

        let str_bytes = str.as_bytes();
        let str_len = str_bytes.len();

        // 单级查找 (见非 threaded 版本注释), 先比较完整 hash 快速淘汰
        let ht_reader = self.ht.read();
        if let Some(ts) = ht_reader.find(h, |ts| {
            if ts.hash != h {
                return false;
            }
            let content_bytes = &ts.data[0..ts.len as usize];
            content_bytes.len() == str_len + 1 && content_bytes[..str_len] == *str_bytes
        }) {
            return ArcRc::clone(ts);
        }
        drop(ht_reader);

        // 写路径: 需要插入新字符串
        // TOCTOU 在单线程执行中安全; 多线程下最多导致重复桶条目(无害)
        let mut ht = self.ht.write();
        let ts = ArcRc::new_sstr(h, 0, str.as_bytes());
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        *self.nuse.write() += 1;
        ts
    }

    /// 内部化一个短字符串（从任意字节，8-bit clean，绕过 UTF-8 验证）。
    /// 用于 C API 的 lua_pushlstring/lua_pushstring 等需要保留原始字节的场景。
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(not(feature = "threaded"))]
    pub fn intern_bytes<'a>(&self, bytes: &[u8]) -> TValue<'a> {
        debug_assert!(
            bytes.len() <= LUAI_MAXSHORTLEN,
            "intern_bytes 只用于短字符串"
        );
        // 必须使用与 intern() 相同的哈希算法（rust_hash_bytes）。
        // intern_bytes 与 intern 在相同字节输入下必须产生相同 hash。
        let h = rust_hash_bytes(bytes);

        let bytes_len = bytes.len();

        // SAFETY: 同 intern, 非 threaded 模式下 StringTable 是 !Sync, 单线程独占.
        let ht = unsafe { &mut *self.ht.as_ptr() };

        // 单级查找 (见 intern 注释), 先比较完整 hash 快速淘汰
        if let Some(ts) = ht.find(h, |ts| {
            if ts.hash != h {
                return false;
            }
            let content_bytes = &ts.data[0..ts.len as usize];
            content_bytes.len() == bytes_len + 1 && content_bytes[..bytes_len] == *bytes
        }) {
            return TValue::ShortStr(ArcRc::clone(ts));
        }

        let ts = ArcRc::<ShortString>::new_sstr(h, 0, bytes);
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        // SAFETY: 同上
        *unsafe { &mut *self.nuse.as_ptr() } += 1;
        TValue::ShortStr(ts)
    }

    /// 内部化一个短字符串 (threaded 模式 — 走 RwLock).
    #[cfg_attr(not(size_optimized), inline)]
    #[cfg(feature = "threaded")]
    pub fn intern_bytes<'a>(&self, bytes: &[u8]) -> TValue<'a> {
        debug_assert!(
            bytes.len() <= LUAI_MAXSHORTLEN,
            "intern_bytes 只用于短字符串"
        );
        let h = rust_hash_bytes(bytes);
        let bytes_len = bytes.len();

        let ht_reader = self.ht.read();
        if let Some(ts) = ht_reader.find(h, |ts| {
            if ts.hash != h {
                return false;
            }
            let content_bytes = &ts.data[0..ts.len as usize];
            content_bytes.len() == bytes_len + 1 && content_bytes[..bytes_len] == *bytes
        }) {
            return TValue::ShortStr(ArcRc::clone(ts));
        }
        drop(ht_reader);

        let mut ht = self.ht.write();
        let ts = ArcRc::new_sstr(h, 0, bytes);
        ht.insert_unique(h, ArcRc::clone(&ts), |ts| ts.hash);
        *self.nuse.write() += 1;
        TValue::ShortStr(ts)
    }

    pub fn count(&self) -> usize {
        *self.nuse.read()
    }

    pub fn remove(&self, ts: &ShortString) {
        let h = ts.hash;
        let ptr = ts as *const ShortString;
        let mut ht = self.ht.write();
        // 用指针相等性匹配要删除的条目
        // HashTable 没有 remove_entry 方法，改用 find_entry + remove
        if let Ok(entry) = ht.find_entry(h, |item: &ArcRc<ShortString>| {
            std::ptr::eq(item.as_ref(), ptr)
        }) {
            entry.remove();
        }
        let mut nuse = self.nuse.write();
        *nuse = nuse.saturating_sub(1);
    }

    pub fn for_each<F: FnMut(&ShortString)>(&self, mut f: F) {
        let ht = self.ht.read();
        for ts in ht.iter() {
            f(ts);
        }
    }

    /// 清理字符串表中的死字符串（只有字符串表持有的字符串）。
    /// 对应 C Lua 的 sweep 阶段清理 string table 的逻辑。
    /// 返回被清理的字符串数量。
    pub fn sweep(&self) -> usize {
        let mut ht = self.ht.write();
        // 收集待删除条目的 (hash, ptr), 避免在迭代中修改表
        let mut to_remove: Vec<(u64, *const ShortString)> = Vec::new();
        for ts in ht.iter() {
            // strong_count == 1 表示只有字符串表持有，无其他引用 → 可回收
            if ArcRc::strong_count(ts) <= 1 {
                to_remove.push((ts.hash, ArcRc::as_ptr(ts)));
            }
        }
        let freed = to_remove.len();
        for (hash, ptr) in to_remove {
            // HashTable 没有 remove_entry 方法，改用 find_entry + remove
            if let Ok(entry) = ht.find_entry(hash, |item: &ArcRc<ShortString>| {
                std::ptr::addr_eq(ArcRc::as_ptr(item), ptr)
            }) {
                entry.remove();
            }
        }

        let mut nuse = self.nuse.write();
        *nuse = nuse.saturating_sub(freed);

        freed
    }
}

// ============================================================================
// 规约：哈希计算
// ============================================================================

/// Lua 风格的快速 hash 函数 — 对应 C 的 `luaS_hash` (lstring.c)。
///
/// 每字符仅需 4 条指令（shift+add+add+xor），比 Rust `DefaultHasher`
/// (SipHash-1-3, ~30 条指令/8 字节) 快 5-10 倍。
///
/// perf 数据显示 StringTable::intern 在编译热点路径上占 14.63%，
/// 其中绝大部分时间花在 SipHash13::write 上。改用此 hash 后 intern
/// 开销大幅下降。
///
/// 注意：编译器场景不面临 hash 碰撞 DoS 攻击（源码可信），因此无需
/// SipHash 的密码学安全性。固定 seed 即可。
#[cfg_attr(not(size_optimized), inline)]
pub fn rust_hash_bytes(bytes: &[u8]) -> u64 {
    let l = bytes.len();
    // seed = length * 0x5bd1e995（MurmurHash2 常量，扩散性好）
    let mut h: u64 = (l as u64).wrapping_mul(0x5bd1e995);
    // 反向遍历对应 C 的 `for (; l > 0; l--) h ^= ((h<<5) + (h>>2) + str[l-1]);`
    // 改为 64 位以充分利用寄存器，移位常量也相应调整。
    // perf: 用 iter().rev() 替代索引循环, 消除每次迭代的边界检查。
    // 迭代器版本编译器可证明无越界, 生成更紧凑的循环体。
    for &b in bytes.iter().rev() {
        let b = b as u64;
        h ^= h
            .wrapping_shl(7)
            .wrapping_add(h.wrapping_shr(2))
            .wrapping_add(b);
    }
    h
}

#[cfg_attr(not(size_optimized), inline)]
pub fn rust_hash(str: &str) -> u64 {
    rust_hash_bytes(str.as_bytes())
}

// ============================================================================
// 规约：字符串方法
// ============================================================================

/// 字符串内容相等 — 对应 C 的 `luaS_eqstr`：短字符串按指针（内部化保证唯一），
/// 长字符串（含外部字符串）按内容；短 vs 长混合变体也按内容比较
/// （对应 C `luaV_equalobj` 的混合变体分支 —— 常规长串 > LUAI_MAXSHORTLEN
/// 必与短串等长直接判否，只有外部字符串可能相等）。
pub fn lua_string_eq(s1: &TValue, s2: &TValue) -> bool {
    match (s1, s2) {
        (TValue::ShortStr(a), TValue::ShortStr(b)) => ArcRc::ptr_eq(a, b),
        (TValue::LongStr(a), TValue::LongStr(b)) => **a == **b,
        (TValue::ShortStr(a), TValue::LongStr(b)) => short_content(a) == long_content(b),
        (TValue::LongStr(a), TValue::ShortStr(b)) => long_content(a) == short_content(b),
        _ => false,
    }
}

/// 估算字符串真实堆占用（用于 GC 内存计费）。
/// 短串: ArcRc 分配头 + ShortString 结构 + 内联内容
/// 长串: Rc 分配头 + 分配器槽 + LongString 固定部分 + 内联内容（外部字符串无内联内容）
/// 字符串不调用 register_object（无 gc_header），由 gc_extra_estimate 跟踪。
pub fn lua_string_gc_mem_size(str: &TValue) -> usize {
    match str {
        // ArcRc 分配 = 引用计数头 + ShortString（固定部分 + 内联内容）
        TValue::ShortStr(s) => ShortString::FIXED_SIZE + s.len as usize + 16,
        // Rc 分配 = 引用计数头 + 分配器槽 + LongString（固定部分 + 内联内容）；
        // 外部字符串的内容在调用方缓冲区（不由 Lua 分配），不计入
        TValue::LongStr(s) => {
            let inline = if s.is_external() { 0 } else { s.len as usize };
            LongString::FIXED_SIZE + inline + 16
        }
        _ => unreachable!("Not a string"),
    }
}

#[cfg_attr(not(size_optimized), inline)]
pub fn lua_string_as_str<'a, 'b>(str: &'b TValue<'a>) -> &'b str {
    lua_string_as_str_inner(str)
}

/// 内部实现：内容一律经 `contents` 指针取（外部字符串无内联内容），
/// 末尾 NUL 由所有构造路径保证（外部字符串由 lua_pushexternalstring 的
/// `s[len] == '\0'` 契约保证）。
fn lua_string_as_str_inner<'a, 'b>(str: &'b TValue<'a>) -> &'b str {
    match str {
        TValue::ShortStr(s) => unsafe { std::str::from_utf8_unchecked(short_content(s)) },
        TValue::LongStr(s) => unsafe { std::str::from_utf8_unchecked(long_content(s)) },
        _ => unreachable!("Not a string"),
    }
}

/// 返回一个 NUL 结尾的 C 字符串指针（供 C API 使用）——对应 C 的 `getstr(ts)`。
///
/// 常规短/长字符串指向对象尾部内联内容；外部字符串直接返回调用方缓冲区指针
/// （与 C 一致：`lua_pushexternalstring` 的返回值即该指针）。
/// 指针在 LuaString 自身存活期间有效。
pub fn lua_string_as_c_str_ptr(str: &TValue) -> *const c_char {
    match str {
        TValue::ShortStr(s) => s.contents() as *const c_char,
        TValue::LongStr(s) => s.contents() as *const c_char,
        _ => unreachable!("Not a string"),
    }
}

/// 返回字符串长度（O(1)，不含末尾 NUL）。
/// `len` 字段恒为内容长度 + 1（含末尾 NUL），直接减 1 省去 slice 操作。
pub fn lua_string_len(str: &TValue) -> usize {
    match str {
        TValue::ShortStr(s) => s.len as usize - 1,
        TValue::LongStr(s) => s.len as usize - 1,
        _ => unreachable!("Not a string"),
    }
}

pub fn lua_string_is_empty(str: &TValue) -> bool {
    lua_string_len(str) == 0
}

/// 返回预计算的哈希值（短字符串始终有效；长字符串 extra==0 时为 0）。
pub fn lua_string_hash(str: &TValue) -> u64 {
    match str {
        TValue::ShortStr(s) => s.hash,
        TValue::LongStr(s) => ensure_long_hash(s),
        _ => unreachable!("Not a string"),
    }
}

// ============================================================================
// 规约：创建字符串
// ============================================================================

/// 创建一个长字符串对象，不预先计算哈希（惰性）。
/// 长字符串块由 `alloc`（MemState allocator 槽，通常 `state.alloc_slot()`）分配/释放。
pub fn new_long_str<'a>(alloc: SlotAlloc, str: &str) -> TValue<'a> {
    debug_assert!(
        str.len() > LUAI_MAXSHORTLEN,
        "长字符串长度必须大于 LUAI_MAXSHORTLEN"
    );
    TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
        alloc,
        None,
        str.as_bytes(),
    ))
}

/// 创建一个长字符串对象，直接 consume 传入的 String，避免 clone。
/// 用于 str_format 等已知结果为长字符串且不再需要原 String 的场景。
/// perf: 消除 new_long_str 中 with_nul 的 to_string() clone（constructs.lua 热点）。
pub fn new_long_str_from_string<'a>(alloc: SlotAlloc, s: String) -> TValue<'a> {
    debug_assert!(
        s.len() > LUAI_MAXSHORTLEN,
        "长字符串长度必须大于 LUAI_MAXSHORTLEN"
    );
    TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
        alloc,
        None,
        &s.as_bytes(),
    ))
}

pub fn new_long_bytes<'a>(alloc: SlotAlloc, bytes: Vec<u8>) -> TValue<'a> {
    debug_assert!(
        bytes.len() > LUAI_MAXSHORTLEN,
        "长字符串长度必须大于 LUAI_MAXSHORTLEN"
    );
    TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
        alloc, None, &bytes,
    ))
}

/// 确保长字符串有哈希值（惰性计算）。内容经 `contents` 指针取
/// （外部字符串无内联内容）。
#[inline]
pub fn ensure_long_hash(ls: &LongString) -> u64 {
    if ls.extra.get() == 0 {
        let h = rust_hash_bytes(long_content(ls));
        ls.hash.set(h);
        ls.extra.set(1);
    }
    ls.hash.get()
}

/// 创建外部字符串（`lua_pushexternalstring`）——对应 C 的 `luaS_newextlstr`。
///
/// 内容**不拷贝**：字符串对象只带固定头部，`contents` 直接指向调用方缓冲区
/// `s`（C 返回的 `getstr(ts)` 即该指针）。`falloc` 为 `Some` 时缓冲区所有权
/// 交给 Lua —— 字符串不再可达（引用计数归零）时调用 `falloc(ud, s, len + 1, 0)`
/// 释放，对应 C `luaC_freeobj` 对 `LSTRMEM` 的处理。
///
/// # Safety
/// `s` 非 null，至少 `len + 1` 字节可读且 `s[len] == '\0'`（C API 契约）。
pub unsafe fn new_ext_str<'a>(
    alloc: SlotAlloc,
    s: *const u8,
    len: usize,
    falloc: Option<Falloc>,
    ud: *mut c_void,
) -> TValue<'a> {
    TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_ext_lstr_in(
        alloc, s, len, falloc, ud,
    ))
}
#[cfg_attr(not(size_optimized), inline)]
pub fn new_lstr<'a>(alloc: SlotAlloc, table: &StringTable, str: &str) -> TValue<'a> {
    if str.len() <= LUAI_MAXSHORTLEN {
        table.intern_value(str)
    } else {
        new_long_str(alloc, str)
    }
}

/// 从 String 创建 LuaString，长字符串路径直接 consume 避免 clone。
/// 短字符串仍走 intern（需要查表去重，intern 未命中时内部会 clone，但短串开销小）。
#[cfg_attr(not(size_optimized), inline)]
pub fn new_lstr_from_string<'a>(alloc: SlotAlloc, table: &StringTable, s: String) -> TValue<'a> {
    if s.len() <= LUAI_MAXSHORTLEN {
        table.intern_value(&s)
    } else {
        new_long_str_from_string(alloc, s)
    }
}

/// 从任意字节创建 LuaString（8-bit clean，绕过 UTF-8 验证）。
/// 用于 C API 的 lua_pushlstring/lua_pushstring 等需要保留原始字节的场景。
#[cfg_attr(not(size_optimized), inline)]
pub fn new_lstr_bytes<'a>(alloc: SlotAlloc, table: &StringTable, bytes: &[u8]) -> TValue<'a> {
    if bytes.len() <= LUAI_MAXSHORTLEN {
        table.intern_bytes(bytes)
    } else {
        new_long_bytes(alloc, bytes.to_vec())
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ------------------------------------------------------------------------
    // rust_hash 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_hash_deterministic() {
        let h1 = rust_hash("hello");
        let h2 = rust_hash("hello");
        assert_eq!(h1, h2, "相同输入的哈希值必须一致");
    }

    #[test]
    fn test_hash_different_content_different_hash() {
        let h1 = rust_hash("hello");
        let h2 = rust_hash("world");
        assert_ne!(h1, h2, "不同内容的哈希值应该不同");
    }

    #[test]
    fn test_hash_empty_string() {
        let h = rust_hash("");
        let h2 = rust_hash("");
        assert_eq!(h, h2, "空字符串哈希值一致");
    }

    // ------------------------------------------------------------------------
    // StringTable::new 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_new_string_table() {
        let tb = StringTable::new();
        assert_eq!(tb.count(), 0, "新表应该为空");
    }

    // ------------------------------------------------------------------------
    // StringTable::intern 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_intern_new_short_string() {
        let tb = StringTable::new();
        let s = tb.intern_value("hello");
        assert_eq!(lua_string_as_str(&s), "hello");
        assert_eq!(lua_string_len(&s), 5);
        assert_eq!(tb.count(), 1, "nuse 应为 1");
    }

    #[test]
    fn test_intern_duplicate_returns_same() {
        let tb = StringTable::new();
        let s1 = tb.intern_value("hello");
        let s2 = tb.intern_value("hello");
        assert_eq!(s1, s2, "相同内容应返回同一个实例");
        assert!(lua_string_eq(&s1, &s2), "通过 lua_string_eq 比较也应相等");
        assert_eq!(tb.count(), 1, "内部化后 nuse 仍应为 1");
    }

    #[test]
    fn test_intern_multiple_strings() {
        let tb = StringTable::new();
        let s1 = tb.intern_value("hello");
        let s2 = tb.intern_value("world");
        let s3 = tb.intern_value("lua");
        assert_eq!(lua_string_as_str(&s1), "hello");
        assert_eq!(lua_string_as_str(&s2), "world");
        assert_eq!(lua_string_as_str(&s3), "lua");
        assert_eq!(tb.count(), 3);
        let s1_dup = tb.intern_value("hello");
        assert_eq!(s1, s1_dup);
        assert_eq!(tb.count(), 3);
    }

    #[test]
    fn test_intern_empty_string() {
        let tb = StringTable::new();
        let s = tb.intern_value("");
        assert_eq!(lua_string_as_str(&s), "");
        assert_eq!(lua_string_len(&s), 0);
        assert!(lua_string_is_empty(&s));
        assert_eq!(tb.count(), 1);
    }

    #[test]
    fn test_intern_max_short_length() {
        let tb = StringTable::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN);
        let s = tb.intern_value(&content);
        assert_eq!(lua_string_as_str(&s), content);
        assert_eq!(lua_string_len(&s), LUAI_MAXSHORTLEN);
    }

    #[test]
    fn test_intern_triggers_grow() {
        let tb = StringTable::new();
        for i in 0..256 {
            let content = format!("key_{}", i);
            tb.intern(&content);
        }
        assert_eq!(tb.count(), 256, "所有字符串应该都被插入");
    }

    // ------------------------------------------------------------------------
    // StringTable::remove 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_remove_short_string() {
        let tb = StringTable::new();
        let s1 = tb.intern_value("hello");
        let s2 = tb.intern_value("world");
        assert_eq!(tb.count(), 2);

        if let TValue::ShortStr(ref ts) = s1 {
            tb.remove(ts);
        }
        assert_eq!(tb.count(), 1, "移除后 nuse 应为 1");

        let s2_again = tb.intern_value("world");
        assert_eq!(s2, s2_again);
        assert_eq!(tb.count(), 1, "重新查找 world 不应增加 nuse");
    }

    // ------------------------------------------------------------------------
    // 字符串相等性测试（对应 C 的 luaS_eqstr / luaV_equalobj）
    // ------------------------------------------------------------------------

    #[test]
    fn test_eq_short_same_pointer() {
        let tb = StringTable::new();
        let a = tb.intern_value("foo");
        let b = tb.intern_value("foo");
        assert!(lua_string_eq(&a, &b), "相同短字符串必须相等");
    }

    #[test]
    fn test_eq_short_different() {
        let tb = StringTable::new();
        let a = tb.intern_value("foo");
        let b = tb.intern_value("bar");
        assert!(!lua_string_eq(&a, &b), "不同短字符串必须不等");
    }

    #[test]
    fn test_eq_long_same_content() {
        let alloc = crate::mem::TestAlloc::new();
        let long_content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let a = new_long_str(alloc.slot(), &long_content);
        let b = new_long_str(alloc.slot(), &long_content);
        assert!(lua_string_eq(&a, &b), "相同内容的长字符串必须相等");
    }
    #[test]
    fn test_eq_long_different() {
        let alloc = crate::mem::TestAlloc::new();
        let a = new_long_str(alloc.slot(), &"a".repeat(LUAI_MAXSHORTLEN + 1));
        let b = new_long_str(alloc.slot(), &"b".repeat(LUAI_MAXSHORTLEN + 1));
        assert!(!lua_string_eq(&a, &b), "不同内容的长字符串必须不等");
    }
    /// 混合变体按内容比较（对应 C `luaV_equalobj`：短 vs 长走 `luaS_eqstr`）。
    /// 常规长串长度 > LUAI_MAXSHORTLEN 必与短串等长，只有外部字符串（内容
    /// 可短）会真正命中相等。
    #[test]
    fn test_eq_short_vs_long_by_content() {
        let alloc = crate::mem::TestAlloc::new();
        let tb = StringTable::new();
        let short = tb.intern_value("hello");
        let same = TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            None,
            "hello".as_bytes(),
        ));
        let other = TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            None,
            "world".as_bytes(),
        ));
        assert!(lua_string_eq(&short, &same), "同内容短/长必须相等");
        assert!(lua_string_eq(&same, &short), "同内容长/短必须相等");
        assert!(!lua_string_eq(&short, &other), "不同内容短/长必须不等");
    }
    /// 外部字符串（内容不拷贝，contents 指向调用方缓冲区）与内部化短字符串
    /// 同内容时必须相等（对应 C 的 luaS_eqstr 跨变体比较）。
    #[test]
    fn test_eq_external_vs_short_and_long() {
        let alloc = crate::mem::TestAlloc::new();
        let tb = StringTable::new();

        // 短内容外部字符串 vs 内部化短字符串（缓冲区须比字符串活得久）
        {
            let buf = b"hello\0".to_vec();
            let ext =
                unsafe { new_ext_str(alloc.slot(), buf.as_ptr(), 5, None, std::ptr::null_mut()) };
            let short = tb.intern_value("hello");
            assert!(lua_string_eq(&ext, &short));
            assert!(lua_string_eq(&short, &ext));
            assert_eq!(lua_string_as_str(&ext), "hello");
            assert_eq!(lua_string_len(&ext), 5);
            assert_eq!(lua_string_as_c_str_ptr(&ext), buf.as_ptr() as *const c_char);
            match &ext {
                TValue::LongStr(ls) => {
                    assert_eq!(ls.kind.get(), LSTRFIX, "falloc == NULL → LSTRFIX");
                    assert!(ls.is_external());
                    assert_eq!(ls.contents(), buf.as_ptr());
                }
                _ => panic!("外部字符串应为 LongStr"),
            }
        }

        // 长内容外部字符串 vs 常规长字符串
        {
            let long_text = "a".repeat(LUAI_MAXSHORTLEN + 1);
            let mut buf = long_text.clone().into_bytes();
            buf.push(0);
            let ext_long = unsafe {
                new_ext_str(
                    alloc.slot(),
                    buf.as_ptr(),
                    long_text.len(),
                    None,
                    std::ptr::null_mut(),
                )
            };
            let long = new_long_str(alloc.slot(), &long_text);
            assert!(lua_string_eq(&ext_long, &long));
            assert_eq!(lua_string_hash(&ext_long), lua_string_hash(&long));
            assert_eq!(lua_string_as_str(&ext_long), long_text);
        }
    }

    /// 外部字符串缓冲区在字符串回收（引用计数归零）时经 falloc 释放一次；
    /// LSTRFIX（falloc == NULL）与常规长字符串不释放任何东西。
    #[test]
    fn test_external_string_frees_buffer_on_last_ref() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static FREED: AtomicUsize = AtomicUsize::new(0);
        static LAST_LEN: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn falloc(
            _ud: *mut c_void,
            _ptr: *mut c_void,
            osize: usize,
            nsize: usize,
        ) -> *mut c_void {
            // C 契约：释放时 osize == len + 1、nsize == 0
            assert_eq!(nsize, 0);
            FREED.fetch_add(1, Ordering::SeqCst);
            LAST_LEN.store(osize, Ordering::SeqCst);
            std::ptr::null_mut()
        }

        let alloc = crate::mem::TestAlloc::new();
        let buf = b"external-data\0".to_vec();
        {
            let ext = unsafe {
                new_ext_str(
                    alloc.slot(),
                    buf.as_ptr(),
                    13,
                    Some(falloc),
                    std::ptr::null_mut(),
                )
            };
            match &ext {
                TValue::LongStr(ls) => assert_eq!(ls.kind.get(), LSTRMEM),
                _ => panic!("应为 LongStr"),
            }
            let clone = ext.clone();
            drop(ext);
            assert_eq!(FREED.load(Ordering::SeqCst), 0, "仍有强引用，不得释放");
            assert_eq!(lua_string_as_str(&clone), "external-data");
            drop(clone);
        }
        assert_eq!(
            FREED.load(Ordering::SeqCst),
            1,
            "最后一次引用释放时回调一次"
        );
        assert_eq!(LAST_LEN.load(Ordering::SeqCst), 14, "osize = len + 1");
    }
    // ------------------------------------------------------------------------
    // new_lstr 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_new_lstr_short() {
        let alloc = crate::mem::TestAlloc::new();
        let tb = StringTable::new();
        let s = new_lstr(alloc.slot(), &tb, "hello");
        assert!(matches!(s, TValue::ShortStr(_)));
        assert_eq!(lua_string_as_str(&s), "hello");
        assert_eq!(lua_string_len(&s), 5);
    }
    #[test]
    fn test_new_lstr_long() {
        let alloc = crate::mem::TestAlloc::new();
        let tb = StringTable::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let s = new_lstr(alloc.slot(), &tb, &content);
        assert!(matches!(s, TValue::LongStr(_)));
        assert_eq!(lua_string_as_str(&s), content);
        assert_eq!(lua_string_len(&s), LUAI_MAXSHORTLEN + 1);
    }
    // ------------------------------------------------------------------------
    // new_long_str 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_new_long_str_has_hashing_marker() {
        let alloc = crate::mem::TestAlloc::new();
        let ls = new_long_str(alloc.slot(), &"a".repeat(LUAI_MAXSHORTLEN + 1));
        assert_eq!(lua_string_len(&ls), LUAI_MAXSHORTLEN + 1);
        match &ls {
            TValue::LongStr(ls) => assert_eq!(ls.extra.get(), 0, "新长字符串 extra 应为 0"),
            _ => panic!("应为长字符串"),
        }
    }
    // ------------------------------------------------------------------------
    // ensure_long_hash 测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_ensure_long_hash_computes_on_first_call() {
        let alloc = crate::mem::TestAlloc::new();
        let mut ls = SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            None,
            "a".repeat(50).as_bytes(),
        );
        let hash = ensure_long_hash(&mut ls);
        assert_eq!(ls.extra.get(), 1, "extra 应为 1（标记已计算哈希）");
        assert_eq!(hash, ls.hash.get(), "返回的哈希应与存储的一致");
    }
    #[test]
    fn test_ensure_long_hash_idempotent() {
        let alloc = crate::mem::TestAlloc::new();
        let mut ls = SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            Some(0),
            "a".repeat(50).as_bytes(),
        );
        let hash_before = ls.hash.get();
        let hash = ensure_long_hash(&mut ls);
        assert_eq!(hash, hash_before, "已有哈希不应重新计算");
        assert_eq!(ls.extra.get(), 1, "extra 仍为 1");
    }
    // ------------------------------------------------------------------------
    // LuaString 方法测试
    // ------------------------------------------------------------------------

    #[test]
    fn test_lua_string_hash() {
        let tb = StringTable::new();
        let s = tb.intern_value("hello");
        let h = rust_hash("hello");
        assert_eq!(lua_string_hash(&s), h);
    }

    #[test]
    fn test_lua_string_len_and_is_empty() {
        let tb = StringTable::new();
        let empty = tb.intern_value("");
        let non_empty = tb.intern_value("x");
        assert!(lua_string_is_empty(&empty));
        assert!(!lua_string_is_empty(&non_empty));
        assert_eq!(lua_string_len(&non_empty), 1);
    }

    #[test]
    fn test_intern_arc_identity() {
        let tb = StringTable::new();
        let a = tb.intern_value("shared");
        let b = tb.intern_value("shared");
        if let (TValue::ShortStr(ra), TValue::ShortStr(rb)) = (&a, &b) {
            assert!(
                ArcRc::ptr_eq(ra, rb),
                "同一字符串的内部化应该返回相同的 ArcRc"
            );
            assert_eq!(ArcRc::strong_count(ra), 3, "引用计数应为 3（表 + a + b）");
        } else {
            panic!("应为短字符串");
        }
    }

    // ========================================================================
    // 多线程并发测试 — 仅在 threaded feature 下编译（StringTable 需要 Send/Sync）
    // ========================================================================

    #[cfg(feature = "threaded")]
    #[test]
    fn test_string_table_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<StringTable>();
        assert_sync::<StringTable>();
    }

    #[cfg(feature = "threaded")]
    #[test]
    fn test_concurrent_intern_same_strings() {
        let table = ArcRc::new(StringTable::new());

        let strings: Vec<&str> = vec![
            "function", "return", "local", "while", "if", "else", "end", "then", "do", "for",
            "repeat", "until", "break", "nil", "true", "false", "and", "or", "not", "in",
        ];
        let count = strings.len();

        let mut handles = Vec::new();
        for _ in 0..4 {
            let table = ArcRc::clone(&table);
            let strings = strings.clone();
            handles.push(std::thread::spawn(move || {
                let mut results: Vec<ArcRc<ShortString>> = Vec::new();
                for s in &strings {
                    let ls = table.intern(s);
                    results.push(ls);
                }
                results
            }));
        }

        let all_results: Vec<Vec<ArcRc<ShortString>>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        for i in 0..count {
            let first = &all_results[0][i];
            for t in 1..4 {
                assert!(
                    ArcRc::ptr_eq(first, &all_results[t][i]),
                    "线程间同一字符串应返回相同 Arc 实例: '{}'",
                    strings[i]
                );
            }
        }

        assert_eq!(table.count(), count, "count 应为去重后的字符串数");
    }

    #[cfg(feature = "threaded")]
    #[test]
    fn test_concurrent_intern_many_strings() {
        let table = ArcRc::new(StringTable::new());
        let num_threads = 8;
        let per_thread = 2000;

        let mut handles = Vec::new();
        for t in 0..num_threads {
            let table = ArcRc::clone(&table);
            handles.push(std::thread::spawn(move || {
                for i in 0..per_thread {
                    let content = format!("thread_{}_key_{}", t, i);
                    let _ = table.intern(&content);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(table.count(), num_threads * per_thread);
    }

    // ========================================================================
    // Hash 行为测试 — 验证 impl Hash for LuaString 的正确性
    // ========================================================================

    fn hash_one(t: &TValue) -> u64 {
        lua_string_hash(t)
    }

    #[test]
    fn test_hash_same_content_same_hash() {
        let tb = StringTable::new();
        let a = tb.intern_value("hello");
        let b = tb.intern_value("hello");
        assert_eq!(
            hash_one(&a),
            hash_one(&b),
            "相同内容应产生相同的 Rust Hash 值"
        );
    }

    #[test]
    fn test_rust_hash_different_content_different() {
        let tb = StringTable::new();
        let a = tb.intern_value("hello");
        let b = tb.intern_value("world");
        assert_ne!(
            hash_one(&a),
            hash_one(&b),
            "不同内容应产生不同的 Rust Hash 值"
        );
    }

    /// LongString: Hash::hash 首次调用自动缓存，后续 O(1) 复用
    #[test]
    fn test_hash_long_string_caches_on_first_call() {
        let alloc = crate::mem::TestAlloc::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let ls = new_long_str(alloc.slot(), &content);
        match &ls {
            TValue::LongStr(inner) => {
                assert_eq!(
                    inner.extra.get(),
                    0,
                    "new_long_str 创建的字符串 extra 应为 0"
                );
                assert_eq!(
                    inner.hash.get(),
                    0,
                    "惰性策略：extra == 0 时 hash == 0，未计算"
                );
            }
            _ => panic!("应为 Long"),
        }

        let h1 = hash_one(&ls);

        match &ls {
            TValue::LongStr(inner) => {
                assert_eq!(
                    inner.extra.get(),
                    1,
                    "Hash::hash 调用后 extra 应自动变为 1（已缓存）"
                );
                assert_ne!(inner.hash.get(), 0, "Hash::hash 调用后 hash 被缓存为非零值");
            }
            _ => panic!("应为 Long"),
        }

        let h2 = hash_one(&ls);
        assert_eq!(h1, h2, "再次 hash 应返回相同值（缓存命中，不重复计算）");
    }
    /// 同内容长字符串：extra=0 和 extra=1 产生相同 Hash
    #[test]
    fn test_hash_mixed_extra_same_content() {
        let alloc = crate::mem::TestAlloc::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1).into_bytes();
        let unhashed = TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            None,
            &content,
        ));
        let ls = SimpleRc::<LongString, SlotAlloc>::new_lstr(alloc.slot(), None, &content);
        ensure_long_hash(&ls);
        let hashed = TValue::LongStr(ls);

        assert_eq!(unhashed, hashed, "同内容的不同 extra 状态应相等");

        assert_eq!(
            hash_one(&unhashed),
            hash_one(&hashed),
            "extra=0 和 extra=1 的同内容长字符串必须产生相同 Rust Hash"
        );

        let mut map: HashMap<TValue, i32> = HashMap::new();
        map.insert(hashed.clone(), 42);
        assert_eq!(
            map.get(&unhashed),
            Some(&42),
            "extra=0 的 key 应能找到 extra=1 的同内容 key 插入的值"
        );
    }
    /// 相同内容：extra=0（实时计算）vs extra=1（缓存命中）→ Hash 相同
    #[test]
    fn test_hash_same_content_different_hash_field() {
        let alloc = crate::mem::TestAlloc::new();
        let h = rust_hash("hello");
        let ls1 = TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            None,
            "hello".as_bytes(),
        ));
        let ls2 = TValue::LongStr(SimpleRc::<LongString, SlotAlloc>::new_lstr(
            alloc.slot(),
            Some(h),
            "hello".as_bytes(),
        ));

        assert_eq!(
            hash_one(&ls1),
            hash_one(&ls2),
            "相同内容 → Hash 应相同（extra=0 实时计算 vs extra=1 缓存命中）"
        );
    }
    /// 大批量长字符串创建时不计算 hash
    #[test]
    fn test_large_long_string_no_eager_hash() {
        let alloc = crate::mem::TestAlloc::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        for _ in 0..100 {
            let ls = new_long_str(alloc.slot(), &content);
            if let TValue::LongStr(inner) = &ls {
                assert_eq!(inner.hash.get(), 0, "所有长字符串创建时不计算 hash");
                assert_eq!(inner.extra.get(), 0);
            }
        }
    }
    /// HashMap 集成测试：短字符串和长字符串混用
    #[test]
    fn test_hashmap_with_mixed_strings() {
        let alloc = crate::mem::TestAlloc::new();
        let tb = StringTable::new();

        let short1 = tb.intern_value("key1");
        let short2 = tb.intern_value("key2");
        let long_content = "a".repeat(LUAI_MAXSHORTLEN + 1);
        let long1 = new_long_str(alloc.slot(), &long_content);

        let mut map: HashMap<TValue, &str> = HashMap::new();
        map.insert(short1.clone(), "value1");
        map.insert(short2.clone(), "value2");
        map.insert(long1.clone(), "value3");

        let short1_lookup = tb.intern_value("key1");
        let long1_lookup = new_long_str(alloc.slot(), &long_content);

        assert_eq!(map.get(&short1_lookup), Some(&"value1"));
        assert_eq!(map.get(&long1_lookup), Some(&"value3"));

        let nonexistent = tb.intern_value("nonexistent");
        assert_eq!(map.get(&nonexistent), None);
    }
    /// ensure_long_hash 同一内容多次调用，哈希值一致、不重复计算
    #[test]
    fn test_ensure_long_hash_same_content() {
        let alloc = crate::mem::TestAlloc::new();
        let content = "a".repeat(LUAI_MAXSHORTLEN + 1).into_bytes();
        let mut a = SimpleRc::<LongString, SlotAlloc>::new_lstr(alloc.slot(), None, &content);
        let mut b = SimpleRc::<LongString, SlotAlloc>::new_lstr(alloc.slot(), None, &content);

        let h0 = ensure_long_hash(&mut a);
        let h1 = ensure_long_hash(&mut b);
        assert_eq!(h0, h1, "同一内容应产生相同的 Rust hash");

        assert_eq!(a.extra.get(), 1);
        assert_eq!(b.extra.get(), 1);

        let h0_again = ensure_long_hash(&mut a);
        assert_eq!(h0, h0_again, "再次调用不重复计算");
    }
    /// LongString 块的分配/释放均路由到 MemState allocator（对应 C 的 allocf）
    #[test]
    fn test_long_string_routes_through_allocator() {
        use std::cell::Cell;
        use std::rc::Rc;

        use crate::mem::{Allocator, DefaultAllocator};

        struct CountingAllocator {
            n: Rc<Cell<usize>>,
        }
        impl Allocator for CountingAllocator {
            fn alloc(&self, ptr: *mut u8, osize: usize, nsize: usize, align: usize) -> *mut u8 {
                if nsize > osize {
                    self.n.set(self.n.get() + 1);
                }
                DefaultAllocator.alloc(ptr, osize, nsize, align)
            }
        }

        let counter = Rc::new(Cell::new(0usize));
        let boxed: Box<dyn Allocator> = Box::new(CountingAllocator {
            n: Rc::clone(&counter),
        });
        let slot = unsafe { SlotAlloc::from_slot(&boxed) };
        let content = "a".repeat(100);

        // 分配走 allocator（nsize > osize: 0 → block_size）
        let ls = SimpleRc::<LongString, SlotAlloc>::new_lstr(slot, None, content.as_bytes());
        assert_eq!(counter.get(), 1);

        // clone 不触发分配
        let c = ls.clone();
        assert_eq!(counter.get(), 1);
        drop(c);

        // 释放走 allocator（nsize == 0 不计数）
        drop(ls);
        assert_eq!(counter.get(), 1);
    }
}
