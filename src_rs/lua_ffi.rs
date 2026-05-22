#![allow(non_snake_case, non_camel_case_types, unused_imports)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;

pub type lua_State = c_void;
pub type lua_CFunction = unsafe extern "C" fn(L: *mut lua_State) -> c_int;
pub type lua_Hook = unsafe extern "C" fn(L: *mut lua_State, ar: *mut c_void);
pub type lua_Number = f64;
pub type lua_Integer = i64;
pub type lua_Unsigned = u64;
pub type lua_KContext = isize;
pub type lua_KFunction =
    unsafe extern "C" fn(L: *mut lua_State, status: c_int, ctx: lua_KContext) -> c_int;
pub type lua_Writer = unsafe extern "C" fn(L: *mut lua_State, p: *const c_void, sz: usize, ud: *mut c_void) -> c_int;

pub const LUA_OK: c_int = 0;
pub const LUA_ERRRUN: c_int = 2;
pub const LUA_ERRSYNTAX: c_int = 3;
pub const LUA_MULTRET: c_int = -1;
pub const LUA_REGISTRYINDEX: c_int = -(c_int::MAX / 2 + 1000);
pub const LUA_MINSTACK: c_int = 20;
pub const LUA_TNIL: c_int = 0;
pub const LUA_TBOOLEAN: c_int = 1;
pub const LUA_TNUMBER: c_int = 3;
pub const LUA_TSTRING: c_int = 4;
pub const LUA_TTABLE: c_int = 5;
pub const LUA_TFUNCTION: c_int = 6;
pub const LUA_NUMTYPES: c_int = 9;
pub const LUA_RIDX_GLOBALS: c_int = 2;
pub const LUA_GCSTOP: c_int = 0;
pub const LUA_GCRESTART: c_int = 1;
pub const LUA_GCGEN: c_int = 7;
pub const LUA_MASKCALL: c_int = 1 << 0;
pub const LUA_MASKRET: c_int = 1 << 1;
pub const LUA_MASKLINE: c_int = 1 << 2;
pub const LUA_MASKCOUNT: c_int = 1 << 3;

extern "C" {
    pub fn luaL_newstate() -> *mut lua_State;
    pub fn lua_close(L: *mut lua_State);
    pub fn luaL_checkversion_(L: *mut lua_State, ver: lua_Number, sz: usize);

    pub fn lua_gettop(L: *mut lua_State) -> c_int;
    pub fn lua_settop(L: *mut lua_State, idx: c_int);
    pub fn lua_rotate(L: *mut lua_State, idx: c_int, n: c_int);
    pub fn lua_type(L: *mut lua_State, idx: c_int) -> c_int;
    pub fn lua_typename(L: *mut lua_State, tp: c_int) -> *const c_char;

    pub fn lua_toboolean(L: *mut lua_State, idx: c_int) -> c_int;
    pub fn lua_tointegerx(L: *mut lua_State, idx: c_int, isnum: *mut c_int) -> lua_Integer;
    pub fn lua_tolstring(
        L: *mut lua_State,
        idx: c_int,
        len: *mut usize,
    ) -> *const c_char;
    pub fn lua_touserdata(L: *mut lua_State, idx: c_int) -> *mut c_void;

    pub fn lua_pushnil(L: *mut lua_State);
    pub fn lua_pushinteger(L: *mut lua_State, n: lua_Integer);
    pub fn lua_pushlstring(
        L: *mut lua_State,
        s: *const c_char,
        len: usize,
    ) -> *const c_char;
    pub fn lua_pushstring(L: *mut lua_State, s: *const c_char) -> *const c_char;
    pub fn lua_pushfstring(L: *mut lua_State, fmt: *const c_char, ...) -> *const c_char;
    pub fn lua_pushcclosure(L: *mut lua_State, f: lua_CFunction, n: c_int);
    pub fn lua_pushboolean(L: *mut lua_State, b: c_int);
    pub fn lua_pushlightuserdata(L: *mut lua_State, p: *mut c_void);

    pub fn lua_getglobal(L: *mut lua_State, name: *const c_char) -> c_int;
    pub fn lua_setglobal(L: *mut lua_State, name: *const c_char);
    pub fn lua_getfield(L: *mut lua_State, idx: c_int, k: *const c_char) -> c_int;
    pub fn lua_setfield(L: *mut lua_State, idx: c_int, k: *const c_char);
    pub fn lua_rawgeti(L: *mut lua_State, idx: c_int, n: lua_Integer) -> c_int;
    pub fn lua_rawseti(L: *mut lua_State, idx: c_int, n: lua_Integer);
    pub fn lua_createtable(L: *mut lua_State, narr: c_int, nrec: c_int);
    pub fn lua_concat(L: *mut lua_State, n: c_int);

    pub fn lua_pcallk(
        L: *mut lua_State,
        nargs: c_int,
        nresults: c_int,
        errfunc: c_int,
        ctx: lua_KContext,
        k: *const c_void,
    ) -> c_int;

    pub fn lua_gc(L: *mut lua_State, what: c_int, ...) -> c_int;

    pub fn lua_sethook(L: *mut lua_State, func: Option<lua_Hook>, mask: c_int, count: c_int) -> c_int;

    pub fn lua_warning(L: *mut lua_State, msg: *const c_char, tocont: c_int);

    pub fn lua_dump(
        L: *mut lua_State,
        writer: lua_Writer,
        data: *mut c_void,
        strip: c_int,
    ) -> c_int;

    pub fn luaL_loadfilex(
        L: *mut lua_State,
        fname: *const c_char,
        mode: *const c_char,
    ) -> c_int;
    pub fn luaL_loadbufferx(
        L: *mut lua_State,
        buff: *const c_char,
        sz: usize,
        name: *const c_char,
        mode: *const c_char,
    ) -> c_int;
    pub fn luaL_openselectedlibs(L: *mut lua_State, load: c_int, preload: c_int);
    pub fn luaL_callmeta(L: *mut lua_State, obj: c_int, e: *const c_char) -> c_int;
    pub fn luaL_tolstring(
        L: *mut lua_State,
        idx: c_int,
        len: *mut usize,
    ) -> *const c_char;
    pub fn luaL_traceback(
        L: *mut lua_State,
        L1: *mut lua_State,
        msg: *const c_char,
        level: c_int,
    );
    pub fn luaL_len(L: *mut lua_State, idx: c_int) -> lua_Integer;
    pub fn luaL_checkstack(L: *mut lua_State, sz: c_int, msg: *const c_char);
    pub fn luaL_error(L: *mut lua_State, fmt: *const c_char, ...) -> !;
    pub fn lua_error(L: *mut lua_State) -> !;
}

pub unsafe fn to_cstr(s: &str) -> CString {
    CString::new(s).unwrap()
}

pub unsafe fn from_cstr<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        None
    } else {
        CStr::from_ptr(ptr).to_str().ok()
    }
}

pub unsafe fn lua_pop(L: *mut lua_State, n: c_int) {
    lua_settop(L, -(n) - 1);
}

pub unsafe fn lua_pushcfunction(L: *mut lua_State, f: lua_CFunction) {
    lua_pushcclosure(L, f, 0);
}

pub unsafe fn luaL_loadfile(L: *mut lua_State, fname: *const c_char) -> c_int {
    luaL_loadfilex(L, fname, ptr::null())
}

pub unsafe fn luaL_loadbuffer(
    L: *mut lua_State,
    buff: *const c_char,
    sz: usize,
    name: *const c_char,
) -> c_int {
    luaL_loadbufferx(L, buff, sz, name, ptr::null())
}

pub unsafe fn lua_pcall(
    L: *mut lua_State,
    nargs: c_int,
    nresults: c_int,
    errfunc: c_int,
) -> c_int {
    lua_pcallk(L, nargs, nresults, errfunc, 0, ptr::null())
}

pub unsafe fn luaL_checkversion(L: *mut lua_State) {
    luaL_checkversion_(L, 505.0, std::mem::size_of::<lua_Integer>() * 16 + std::mem::size_of::<lua_Number>());
}