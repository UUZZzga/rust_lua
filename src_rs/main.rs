use lua_rs::lua_ffi::*;

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{self, BufRead, IsTerminal, Write};
use std::mem::ManuallyDrop;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

const LUA_PROGNAME: &str = "lua";
const LUA_INIT_VAR: &str = "LUA_INIT";
const LUA_VERSUFFIX: &str = "_5_5";
const LUA_PROMPT: &str = "> ";
const LUA_PROMPT2: &str = ">> ";
const LUA_MAXINPUT: usize = 512;
const EOFMARK: &str = "<eof>";

const HAS_ERROR: c_int = 1;
const HAS_I: c_int = 2;
const HAS_V: c_int = 4;
const HAS_E: c_int = 8;
const HAS_EE: c_int = 16;

static GLOBAL_L: AtomicPtr<lua_State> = AtomicPtr::new(ptr::null_mut());

// ============================================================================
// Utility functions
// ============================================================================

fn writestring(s: &str) {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(s.as_bytes());
}

fn writestring_error(s: &str) {
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(s.as_bytes());
}

fn writeline() {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
}

fn print_version() {
    writestring(concat!(
        "Lua 5.5.0  Copyright (C) 1994-2025 Lua.org, PUC-Rio [Rust Edition]\n"
    ));
}

// ============================================================================
// Interpreter - wraps lua_State with safe Rust methods
// ============================================================================

struct Interpreter {
    l: *mut lua_State,
    progname: String,
}

impl Interpreter {
    unsafe fn new() -> Option<Self> {
        let l = luaL_newstate();
        if l.is_null() {
            eprintln!("cannot create state: not enough memory");
            return None;
        }
        lua_gc(l, LUA_GCSTOP);
        Some(Interpreter {
            l,
            progname: LUA_PROGNAME.to_string(),
        })
    }

    unsafe extern "C" fn msghandler(l: *mut lua_State) -> c_int {
        let msg_ptr = lua_tolstring(l, 1, ptr::null_mut());
        if msg_ptr.is_null() {
            if luaL_callmeta(l, 1, c"__tostring".as_ptr()) != 0
                && lua_type(l, -1) == LUA_TSTRING
            {
                return 1;
            } else {
                let type_name = lua_typename(l, lua_type(l, 1));
                let type_str = CStr::from_ptr(type_name).to_str().unwrap_or("unknown");
                let fmt = format!("(error object is a {} value)", type_str);
                let c_fmt = CString::new(fmt).unwrap();
                lua_pushfstring(l, c_fmt.as_ptr());
                return 1;
            }
        }
        luaL_traceback(l, l, msg_ptr, 1);
        1
    }

    fn report(l: *mut lua_State, status: c_int) -> c_int {
        if status != LUA_OK {
            let msg_ptr = unsafe { lua_tolstring(l, -1, ptr::null_mut()) };
            let msg = if msg_ptr.is_null() {
                "(error message not a string)"
            } else {
                unsafe { CStr::from_ptr(msg_ptr).to_str().unwrap_or("(error)") }
            };
            let mut stderr = io::stderr().lock();
            let _ = write!(stderr, "{}: {}\n", LUA_PROGNAME, msg);
            unsafe { lua_pop(l, 1) };
        }
        status
    }

    unsafe fn docall(&mut self, narg: c_int, nres: c_int) -> c_int {
        let base = lua_gettop(self.l) - narg;
        lua_pushcfunction(self.l, Self::msghandler);
        lua_rotate(self.l, base, 1);
        GLOBAL_L.store(self.l, Ordering::SeqCst);
        setup_signal_handler();
        let status = lua_pcall(self.l, narg, nres, base);
        reset_signal_handler();
        lua_rotate(self.l, base, -1);
        lua_pop(self.l, 1);
        status
    }

    unsafe fn dochunk(&mut self, status: c_int) -> c_int {
        let status = if status == LUA_OK {
            self.docall(0, 0)
        } else {
            status
        };
        Self::report(self.l, status)
    }

    unsafe fn dostring(&mut self, s: &str, name: &str) -> c_int {
        let c_s = CString::new(s).unwrap();
        let c_name = CString::new(name).unwrap();
        let status = luaL_loadbuffer(self.l, c_s.as_ptr(), s.len(), c_name.as_ptr());
        self.dochunk(status)
    }

    unsafe fn dofile(&mut self, fname_ptr: *const c_char) -> c_int {
        let status = luaL_loadfile(self.l, fname_ptr);
        self.dochunk(status)
    }

    unsafe fn dolibrary(&mut self, globname: &str) -> c_int {
        let g = globname.to_string();
        let eq_pos = g.find('=');
        let (glob_part, mod_part): (&str, &str) = if let Some(pos) = eq_pos {
            let (a, b) = g.split_at(pos);
            (a, &b[1..])
        } else {
            (&g, &g)
        };

        lua_getglobal(self.l, c"require".as_ptr());
        let c_modname = CString::new(mod_part).unwrap();
        lua_pushstring(self.l, c_modname.as_ptr());
        let status = self.docall(1, 1);

        if status == LUA_OK {
            let global_name = if eq_pos.is_none() {
                if let Some(dash_pos) = glob_part.rfind('-') {
                    &glob_part[..dash_pos]
                } else {
                    glob_part
                }
            } else {
                glob_part
            };
            let c_global = CString::new(global_name).unwrap();
            lua_setglobal(self.l, c_global.as_ptr());
        }
        Self::report(self.l, status)
    }

    unsafe fn handle_luainit(&mut self) -> c_int {
        let init_var = format!("{}{}", LUA_INIT_VAR, LUA_VERSUFFIX);
        if let Ok(init) = std::env::var(&init_var) {
            return self.doinit(&init, &init_var);
        }
        if let Ok(init) = std::env::var(LUA_INIT_VAR) {
            return self.doinit(&init, LUA_INIT_VAR);
        }
        LUA_OK
    }

    unsafe fn doinit(&mut self, init: &str, varname: &str) -> c_int {
        if let Some(stripped) = init.strip_prefix('@') {
            let c_name = CString::new(stripped).unwrap();
            self.dochunk(luaL_loadfile(self.l, c_name.as_ptr()))
        } else {
            let name = format!("={}", varname);
            self.dostring(init, &name)
        }
    }

    unsafe fn run_args(&mut self, argv: &[CString], n: usize) -> bool {
        let c_off = c"@off";
        lua_warning(self.l, c_off.as_ptr(), 0);
        let mut i = 1;
        while i < n {
            let bytes = argv[i].to_bytes();
            if bytes.len() < 2 || bytes[0] != b'-' {
                i += 1;
                continue;
            }
            match bytes[1] {
                b'e' | b'l' => {
                    let extra = if bytes.len() > 2 {
                        &argv[i].to_str().unwrap()[2..]
                    } else {
                        i += 1;
                        argv[i].to_str().unwrap()
                    };
                    let status = if bytes[1] == b'e' {
                        self.dostring(extra, "=(command line)")
                    } else {
                        self.dolibrary(extra)
                    };
                    if status != LUA_OK {
                        return false;
                    }
                }
                b'W' => {
                    lua_warning(self.l, c"@on".as_ptr(), 0);
                }
                _ => {}
            }
            i += 1;
        }
        true
    }

    unsafe fn push_args(&mut self) -> c_int {
        let n = luaL_len(self.l, -1) as c_int;
        luaL_checkstack(self.l, n + 3, c"too many arguments to script".as_ptr());
        for i in 1..=n {
            lua_rawgeti(self.l, -i, i as i64);
        }
        lua_rotate(self.l, -(n + 1), -1);
        lua_pop(self.l, 1);
        n
    }

    unsafe fn handle_script(&mut self, argv: &[CString], script: isize, after_dash: bool) -> c_int {
        let fname = argv[script as usize].to_str().unwrap();
        let c_fname;
        let fname_ptr = if fname == "-" && !after_dash {
            ptr::null()
        } else {
            c_fname = CString::new(fname).unwrap();
            c_fname.as_ptr()
        };
        let status = luaL_loadfile(self.l, fname_ptr);
        if status == LUA_OK {
            if lua_getglobal(self.l, c"arg".as_ptr()) != LUA_TTABLE {
                lua_pushstring(self.l, c"'arg' is not a table".as_ptr());
                return Self::report(self.l, LUA_ERRRUN);
            }
            let n = self.push_args();
            let call_status = self.docall(n, LUA_MULTRET);
            Self::report(self.l, call_status)
        } else {
            Self::report(self.l, status)
        }
    }

    // ========================================================================
    // REPL
    // ========================================================================

    fn readline(prompt: &str) -> Option<String> {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(prompt.as_bytes());
        let _ = stdout.flush();
        let stdin = io::stdin();
        let mut line = String::with_capacity(LUA_MAXINPUT);
        match stdin.lock().read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => {
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                Some(line)
            }
            Err(_) => None,
        }
    }

    fn check_local(line: &str) {
        let trimmed = line.trim_start_matches(&[' ', '\t']);
        if trimmed.starts_with("local ") {
            writestring_error("warning: locals do not survive across lines in interactive mode\n");
        }
    }

    unsafe fn incomplete(&self, status: c_int) -> bool {
        if status == LUA_ERRSYNTAX {
            let mut lmsg: usize = 0;
            let msg_ptr = lua_tolstring(self.l, -1, &mut lmsg);
            if !msg_ptr.is_null()
                && lmsg >= EOFMARK.len()
                && unsafe {
                    CStr::from_ptr(msg_ptr.add(lmsg - EOFMARK.len())).to_bytes()
                        == EOFMARK.as_bytes()
                }
            {
                return true;
            }
        }
        false
    }

    unsafe fn pushline(&mut self, firstline: bool) -> bool {
        let prompt = if firstline { LUA_PROMPT } else { LUA_PROMPT2 };
        match Self::readline(prompt) {
            None => false,
            Some(line) => {
                let c_line = CString::new(line.as_str()).unwrap();
                lua_pushlstring(self.l, c_line.as_ptr(), line.len());
                true
            }
        }
    }

    unsafe fn addreturn(&mut self) -> c_int {
        let mut len: usize = 0;
        let line_ptr = lua_tolstring(self.l, -1, &mut len);
        if line_ptr.is_null() {
            return LUA_ERRSYNTAX;
        }
        let line = std::str::from_utf8(std::slice::from_raw_parts(line_ptr as *const u8, len))
            .unwrap_or("");
        let retline = format!("return {};", line);
        let c_retline = CString::new(retline.as_str()).unwrap();
        let status = luaL_loadbuffer(
            self.l,
            c_retline.as_ptr(),
            retline.len(),
            c"=stdin".as_ptr(),
        );
        if status == LUA_OK {
        } else {
            lua_pop(self.l, 1);
        }
        status
    }

    unsafe fn multiline(&mut self) -> c_int {
        let mut l: usize = 0;
        let mut line_ptr = lua_tolstring(self.l, 1, &mut l);
        let first_line = std::str::from_utf8(std::slice::from_raw_parts(
            line_ptr as *const u8,
            l.min(1024),
        ))
        .unwrap_or("");
        Self::check_local(first_line);

        loop {
            let status =
                luaL_loadbuffer(self.l, line_ptr as *const c_char, l, c"=stdin".as_ptr());
            if !self.incomplete(status) || !self.pushline(false) {
                return status;
            }
            lua_rotate(self.l, -2, -1);
            lua_pop(self.l, 1);
            lua_pushlstring(self.l, c"\n".as_ptr(), 1);
            lua_rotate(self.l, -2, 1);
            lua_concat(self.l, 3);
            let mut new_len: usize = 0;
            line_ptr = lua_tolstring(self.l, 1, &mut new_len);
            l = new_len;
        }
    }

    unsafe fn loadline(&mut self) -> c_int {
        lua_settop(self.l, 0);
        if !self.pushline(true) {
            return -1;
        }
        let status = self.addreturn();
        let status = if status != LUA_OK {
            self.multiline()
        } else {
            status
        };
        lua_rotate(self.l, 1, -1);
        lua_pop(self.l, 1);
        status
    }

    unsafe fn l_print(&mut self) {
        let n = lua_gettop(self.l);
        if n > 0 {
            luaL_checkstack(self.l, LUA_MINSTACK, c"too many results to print".as_ptr());
            lua_getglobal(self.l, c"print".as_ptr());
            if lua_type(self.l, -1) != LUA_TFUNCTION {
                lua_pop(self.l, n + 1);
                let mut stderr = io::stderr().lock();
                let _ = write!(
                    stderr,
                    "{}: error calling 'print' (value is not a function)\n",
                    self.progname
                );
                return;
            }
            lua_rotate(self.l, 1, 1);
            if lua_pcall(self.l, n, 0, 0) != LUA_OK {
                let err_ptr = lua_tolstring(self.l, -1, ptr::null_mut());
                let err_msg = if !err_ptr.is_null() {
                    unsafe { CStr::from_ptr(err_ptr).to_str().unwrap_or("(error)") }
                } else {
                    "(error)"
                };
                let mut stderr = io::stderr().lock();
                let _ = write!(
                    stderr,
                    "{}: error calling 'print' ({})\n",
                    self.progname, err_msg
                );
            }
        }
    }

    unsafe fn do_repl(&mut self) {
        loop {
            let status = self.loadline();
            if status == -1 {
                break;
            }
            let status = if status == LUA_OK {
                self.docall(0, LUA_MULTRET)
            } else {
                status
            };
            if status == LUA_OK {
                self.l_print();
            } else {
                Self::report(self.l, status);
            }
        }
        lua_settop(self.l, 0);
        writeline();
    }

    // ========================================================================
    // collect_args
    // ========================================================================

    fn collect_args(argv: &[CString], out_script: &mut isize) -> c_int {
        let mut args: c_int = 0;

        if argv.is_empty() {
            *out_script = -1;
            return 0;
        }

        let mut i: usize = 1;
        while i < argv.len() {
            let arg = &argv[i];
            *out_script = i as isize;
            let bytes = arg.to_bytes();
            if bytes.is_empty() || bytes[0] != b'-' {
                return args;
            }
            match bytes.get(1) {
                None | Some(b'\0') => return args,
                Some(b'-') if bytes.get(2).is_none() || bytes.get(2) == Some(&b'\0') => {
                    *out_script = if i + 1 < argv.len() {
                        (i + 1) as isize
                    } else {
                        0
                    };
                    return args;
                }
                Some(b'-') => return HAS_ERROR,
                Some(b'E') if bytes.get(2).is_none() || bytes.get(2) == Some(&b'\0') => {
                    args |= HAS_EE;
                }
                Some(b'E') => return HAS_ERROR,
                Some(b'W') if bytes.get(2).is_none() || bytes.get(2) == Some(&b'\0') => {}
                Some(b'W') => return HAS_ERROR,
                Some(b'i') if bytes.get(2).is_none() || bytes.get(2) == Some(&b'\0') => {
                    args |= HAS_I | HAS_V;
                }
                Some(b'i') => return HAS_ERROR,
                Some(b'v') if bytes.get(2).is_none() || bytes.get(2) == Some(&b'\0') => {
                    args |= HAS_V;
                }
                Some(b'v') => return HAS_ERROR,
                Some(b'e') => {
                    args |= HAS_E;
                    if bytes.get(2).is_none() || bytes.get(2) == Some(&b'\0') {
                        i += 1;
                        if i >= argv.len()
                            || argv[i].to_bytes().first() == Some(&b'-')
                        {
                            return HAS_ERROR;
                        }
                    }
                }
                Some(b'l') => {
                    if bytes.get(2).is_none() || bytes.get(2) == Some(&b'\0') {
                        i += 1;
                        if i >= argv.len()
                            || argv[i].to_bytes().first() == Some(&b'-')
                        {
                            return HAS_ERROR;
                        }
                    }
                }
                _ => return HAS_ERROR,
            }
            i += 1;
        }
        *out_script = 0;
        args
    }

    fn print_usage(badoption: &str) {
        let mut stderr = io::stderr().lock();
        let _ = write!(stderr, "{}: ", LUA_PROGNAME);
        match badoption.chars().nth(1) {
            Some('e' | 'l') => {
                let _ = writeln!(stderr, "'{}' needs argument", badoption);
            }
            _ => {
                let _ = writeln!(stderr, "unrecognized option '{}'", badoption);
            }
        }
        let _ = write!(
            stderr,
            "usage: {} [options] [script [args]]\n\
             Available options are:\n\
               -e stat   execute string 'stat'\n\
               -i        enter interactive mode after executing 'script'\n\
               -l mod    require library 'mod' into global 'mod'\n\
               -l g=mod  require library 'mod' into global 'g'\n\
               -v        show version information\n\
               -E        ignore environment variables\n\
               -W        turn warnings on\n\
               --        stop handling options\n\
               -         stop handling options and execute stdin\n",
            LUA_PROGNAME
        );
    }

    // ========================================================================
    // pmain - protected main body (called via lua_pcall from C)
    // ========================================================================

    unsafe extern "C" fn pmain(l: *mut lua_State) -> c_int {
        let argc = lua_tointegerx(l, 1, ptr::null_mut()) as usize;
        let argv_ptr = lua_touserdata(l, 2);
        assert!(!argv_ptr.is_null(), "argv must not be null");

        let mut ip = ManuallyDrop::new(Interpreter {
            l,
            progname: LUA_PROGNAME.to_string(),
        });

        let argv_cstrings: Vec<CString> = {
            let raw_argv = argv_ptr as *mut *mut c_char;
            (0..argc)
                .map(|i| CStr::from_ptr(*raw_argv.add(i)).to_owned())
                .collect()
        };

        let mut script: isize = 0;
        let args = Self::collect_args(&argv_cstrings, &mut script);
        let optlim = if script > 0 {
            script as usize
        } else {
            argc
        };

        if args & HAS_ERROR != 0 {
            let bad_idx = script as usize;
            let bad = argv_cstrings[bad_idx].to_str().unwrap_or("?");
            Self::print_usage(bad);
            return 0;
        }

        luaL_checkversion(l);

        if args & HAS_V != 0 {
            print_version();
        }

        if args & HAS_EE != 0 {
            lua_pushboolean(l, 1);
            lua_setfield(l, LUA_REGISTRYINDEX, c"LUA_NOENV".as_ptr());
        }

        luaL_openselectedlibs(l, !0, 0);

        let narg = argc as isize - (script + 1);
        lua_createtable(l, narg as c_int, (script + 1) as c_int);
        for (i, arg) in argv_cstrings.iter().enumerate() {
            lua_pushstring(l, arg.as_ptr());
            lua_rawseti(l, -2, i as i64 - script as i64);
        }
        lua_setglobal(l, c"arg".as_ptr());

        lua_gc(l, LUA_GCRESTART);
        lua_gc(l, LUA_GCGEN);

        if args & HAS_EE == 0 {
            if ip.handle_luainit() != LUA_OK {
                return 0;
            }
        }

        if !ip.run_args(&argv_cstrings, optlim) {
            return 0;
        }

        if script > 0 {
            let after_dash =
                script > 1 && argv_cstrings[script as usize - 1].to_bytes() == b"--";
            if ip.handle_script(&argv_cstrings, script, after_dash) != LUA_OK {
                return 0;
            }
        }

        if args & HAS_I != 0 {
            ip.do_repl();
        } else if script < 1 && args & (HAS_E | HAS_V) == 0 {
            if io::stdin().is_terminal() {
                print_version();
                ip.do_repl();
            } else {
                ip.dofile(ptr::null());
            }
        }

        lua_pushboolean(l, 1);
        1
    }

    unsafe fn run(&mut self, argc: usize, argv: *mut *mut c_char) -> bool {
        GLOBAL_L.store(self.l, Ordering::SeqCst);
        lua_pushcfunction(self.l, Self::pmain);
        lua_pushinteger(self.l, argc as i64);
        lua_pushlightuserdata(self.l, argv as *mut c_void);

        let status = lua_pcall(self.l, 2, 1, 0);
        let result = lua_toboolean(self.l, -1);
        Self::report(self.l, status);
        GLOBAL_L.store(ptr::null_mut(), Ordering::SeqCst);
        result != 0 && status == LUA_OK
    }
}

impl Drop for Interpreter {
    fn drop(&mut self) {
        if !self.l.is_null() {
            unsafe {
                lua_close(self.l);
            }
        }
    }
}

// ============================================================================
// Signal handling
// ============================================================================

unsafe extern "C" fn lstop(_l: *mut lua_State, _ar: *mut c_void) {
    let msg = c"interrupted!";
    luaL_error(_l, msg.as_ptr());
}

unsafe extern "C" fn laction(_sig: c_int) {
    let flag = LUA_MASKCALL | LUA_MASKRET | LUA_MASKLINE | LUA_MASKCOUNT;
    let l = GLOBAL_L.load(Ordering::SeqCst);
    if !l.is_null() {
        lua_sethook(l, Some(lstop), flag, 1);
    }
}

fn setup_signal_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = laction as *const () as usize;
        sa.sa_flags = 0;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGINT, &sa, ptr::null_mut());
    }
}

fn reset_signal_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = libc::SIG_DFL;
        libc::sigaction(libc::SIGINT, &sa, ptr::null_mut());
    }
}

// ============================================================================
// main
// ============================================================================

fn main() {
    let args: Vec<CString> = std::env::args()
        .map(|a| CString::new(a).unwrap())
        .collect();

    let argc = args.len();
    let mut argv_ptrs: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv_ptrs.push(ptr::null_mut());

    let mut interpreter = match unsafe { Interpreter::new() } {
        Some(ip) => ip,
        None => {
            std::process::exit(1);
        }
    };

    let result = unsafe { interpreter.run(argc, argv_ptrs.as_mut_ptr()) };
    if !result {
        std::process::exit(1);
    }
}