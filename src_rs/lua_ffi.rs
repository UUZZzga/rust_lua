#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void, CStr};

pub type lua_State = c_void;
pub type lua_Number = f64;
pub type lua_Integer = i64;
pub type lua_Writer =
    unsafe extern "C" fn(L: *mut lua_State, p: *const c_void, sz: usize, ud: *mut c_void) -> c_int;

pub const LUA_OK: c_int = 0;

extern "C" {
    pub fn luaL_newstate() -> *mut lua_State;
    pub fn lua_close(L: *mut lua_State);
    pub fn luaL_checkversion_(L: *mut lua_State, ver: lua_Number, sz: usize);
    pub fn lua_tolstring(L: *mut lua_State, idx: c_int, len: *mut usize) -> *const c_char;
    pub fn luaL_loadbufferx(
        L: *mut lua_State,
        buff: *const c_char,
        sz: usize,
        name: *const c_char,
        mode: *const c_char,
    ) -> c_int;
    pub fn luaL_openselectedlibs(L: *mut lua_State, load: c_int, preload: c_int);
    pub fn lua_dump(
        L: *mut lua_State,
        writer: lua_Writer,
        data: *mut c_void,
        strip: c_int,
    ) -> c_int;
}

pub unsafe fn from_cstr<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        None
    } else {
        CStr::from_ptr(ptr).to_str().ok()
    }
}

pub unsafe fn luaL_checkversion(L: *mut lua_State) {
    luaL_checkversion_(
        L,
        505.0,
        std::mem::size_of::<lua_Integer>() * 16 + std::mem::size_of::<lua_Number>(),
    );
}
