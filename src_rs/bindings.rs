//! Lua 5.5 C FFI 原始绑定
//!
//! 直接映射 lopcodes.h / lparser.h 中 extern "C" 声明的符号。
//! 不使用任何包装层。

// ============================================================================
// lopcodes.h — 全局数据
// ============================================================================

extern "C" {
    /// 操作模式表（luaP_opmodes 全局数组）
    #[link_name = "luaP_opmodes"]
    pub static LUA_P_OPMODES: [u8; 85];

    /// 操作码数量
    #[link_name = "NUM_OPCODES"]
    pub static LUA_NUM_OPCODES: i32;
}

// ============================================================================
// lopcodes.h — 函数
// ============================================================================

extern "C" {
    /// luaP_isOT — 检查指令是否设置 top
    #[link_name = "luaP_isOT"]
    pub fn luaP_isOT(i: u32) -> i32;

    /// luaP_isIT — 检查指令是否使用 top
    #[link_name = "luaP_isIT"]
    pub fn luaP_isIT(i: u32) -> i32;
}

// ============================================================================
// lparser.h — 不透明类型
// ============================================================================

#[repr(C)]
pub struct FuncState {
    _private: [u8; 0],
}

// ============================================================================
// lparser.h — 函数
// ============================================================================

extern "C" {
    /// luaY_nvarstack(fs) → lu_byte
    #[link_name = "luaY_nvarstack"]
    pub fn luaY_nvarstack(fs: *mut FuncState) -> u8;

    /// luaY_checklimit(fs, v, l, what)
    #[link_name = "luaY_checklimit"]
    pub fn luaY_checklimit(fs: *mut FuncState, v: i32, l: i32, what: *const std::os::raw::c_char);
}