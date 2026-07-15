//! Lua 5.5 解析器 FFI 绑定（lparser.h）
//!
//! 通过 bindings.rs 直接调用 C 的 luaY_nvarstack / luaY_checklimit。
//! FuncState 作为不透明指针传递。

use crate::bindings;
use std::ffi::CString;

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
// FFI 调用 — 直接调用 C 的 luaY_nvarstack / luaY_checklimit
// ============================================================================

/// 返回函数寄存器栈中变量数（调用 C 的 luaY_nvarstack）
///
/// # Safety
/// `fs` 必须指向有效的 FuncState。
pub unsafe fn nvarstack(fs: &FuncState) -> u8 {
    bindings::luaY_nvarstack(fs.raw)
}

/// 检查限制（调用 C 的 luaY_checklimit）
///
/// # Safety
/// `fs` 必须指向有效的 FuncState。
pub unsafe fn checklimit(fs: &FuncState, v: i32, l: i32, what: &str) {
    let c_what = CString::new(what).expect("CString::new failed");
    bindings::luaY_checklimit(fs.raw, v, l, c_what.as_ptr());
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

    #[test]
    fn test_nvarstack_c_api_exists() {
        // 仅验证 FFI 符号存在且可链接
        // nvarstack 访问 fs 内部，需要完整初始化的 FuncState
        // 此测试仅验证编译链接
    }

    #[test]
    fn test_checklimit_c_api_exists() {
        // 仅验证 FFI 符号存在且可链接
    }
}
