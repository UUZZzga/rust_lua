//! Lua 5.5 解析器 FFI 绑定（lparser.h）
//!
//! FuncState 作为不透明指针传递。

use crate::bindings;

/// FuncState 不透明句柄（内部持有 C 的 FuncState*）
pub struct FuncState {
    raw: *mut bindings::FuncState,
}

impl FuncState {
    /// 从原始 C 指针创建（调用方保证指针有效性）
    ///
    /// # Safety
    /// `ptr` 必须指向有效的 C FuncState。
    pub unsafe fn from_raw(ptr: *mut bindings::FuncState) -> Self {
        FuncState { raw: ptr }
    }

    /// 获取原始指针
    pub fn as_ptr(&self) -> *mut bindings::FuncState {
        self.raw
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings;

    /// 用 calloc 分配最小可用 FuncState（内部子指针为 NULL，不可实际调用）
    unsafe fn alloc_fs_zeroed() -> FuncState {
        let layout = std::alloc::Layout::new::<bindings::FuncState>();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) } as *mut bindings::FuncState;
        FuncState::from_raw(ptr)
    }

    /// 释放零分配的 FuncState
    unsafe fn free_fs(fs: FuncState) {
        let layout = std::alloc::Layout::new::<bindings::FuncState>();
        unsafe { std::alloc::dealloc(fs.raw as *mut u8, layout) }
    }

    #[test]
    fn test_funcstate_from_raw() {
        unsafe {
            let fs = alloc_fs_zeroed();
            assert!(!fs.raw.is_null());
            free_fs(fs);
        }
    }
}
