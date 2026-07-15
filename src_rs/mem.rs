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

pub trait Allocator {
    fn alloc(&mut self, ptr: *mut u8, old_size: usize, new_size: usize) -> *mut u8;
}

// ============================================================================
// 默认分配器：基于 std::alloc
// ============================================================================

pub struct DefaultAllocator;

impl Allocator for DefaultAllocator {
    fn alloc(&mut self, ptr: *mut u8, old_size: usize, new_size: usize) -> *mut u8 {
        if new_size == 0 {
            if old_size != 0 && !ptr.is_null() {
                unsafe {
                    let layout = Layout::from_size_align_unchecked(old_size, 1);
                    alloc::dealloc(ptr, layout);
                }
            }
            return std::ptr::null_mut();
        }

        if ptr.is_null() || old_size == 0 {
            let layout = match Layout::from_size_align(new_size, 1) {
                Ok(l) => l,
                Err(_) => return std::ptr::null_mut(),
            };
            unsafe { alloc::alloc(layout) }
        } else {
            let old_layout = unsafe { Layout::from_size_align_unchecked(old_size, 1) };
            unsafe { alloc::realloc(ptr, old_layout, new_size) }
        }
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
    pub fn new(allocator: A) -> Self {
        Self {
            allocator,
            gc_debt: 0,
            gc_stop_em: false,
            complete_state: false,
            _phantom: std::marker::PhantomData,
        }
    }

    fn can_try_again(&self) -> bool {
        self.complete_state && !self.gc_stop_em
    }

    // ========================================================================
    // 底层分配原语
    // ========================================================================

    fn call_alloc(&mut self, block: *mut u8, osize: usize, nsize: usize) -> *mut u8 {
        self.allocator.alloc(block, osize, nsize)
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
        let ptr = self.malloc(layout.size())?;
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
        let ptr = self.malloc(layout.size())? as *mut T;
        Ok(unsafe { Vec::from_raw_parts(ptr, n, n) })
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
        let new_size_bytes = size * mem::size_of::<T>();
        let old_size_bytes = v.capacity() * mem::size_of::<T>();
        self.safe_realloc(std::ptr::null_mut(), 0, new_size_bytes)?;
        let mut v = ManuallyDrop::new(v);
        let old_ptr = v.as_mut_ptr() as *mut u8;
        let new_block = self.safe_realloc(old_ptr, old_size_bytes, new_size_bytes)? as *mut T;
        let new_vec = unsafe { Vec::from_raw_parts(new_block, nelems, size) };
        Ok(new_vec)
    }

    // ========================================================================
    // Vector 缩容 — 对应 luaM_shrinkvector_
    // ========================================================================

    pub fn shrink_vec<T>(&mut self, v: Vec<T>, final_n: usize) -> Result<Vec<T>, MemError> {
        let old_cap = v.capacity();
        let old_size_bytes = old_cap * mem::size_of::<T>();
        let new_size_bytes = final_n * mem::size_of::<T>();
        debug_assert!(new_size_bytes <= old_size_bytes);
        if old_cap == final_n {
            return Ok(v);
        }
        let mut v = ManuallyDrop::new(v);
        let old_ptr = v.as_mut_ptr() as *mut u8;
        let new_block = self.safe_realloc(old_ptr, old_size_bytes, new_size_bytes)? as *mut T;
        let new_vec = unsafe { Vec::from_raw_parts(new_block, final_n, final_n) };
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
}
