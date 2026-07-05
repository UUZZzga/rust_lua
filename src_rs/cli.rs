//! Lua 命令行解释器
//!
//! 提供 Lua REPL 和脚本执行功能。

use crate::objects::{LuaType, TValue};
use crate::state::{LuaState, ERR_RUN, ERR_SYNTAX, MIN_STACK, MULT_RET};

use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

const LUA_PROGNAME: &str = "lua";
const LUA_INIT_VAR: &str = "LUA_INIT";
const LUA_VERSUFFIX: &str = "_5_5";
const LUA_PROMPT: &str = "> ";
const LUA_PROMPT2: &str = ">> ";
const LUA_MAXINPUT: usize = 512;
const EOFMARK: &str = "<eof>";

const HAS_ERROR: i32 = 1;
const HAS_I: i32 = 2;
const HAS_V: i32 = 4;
const HAS_E: i32 = 8;
const HAS_EE: i32 = 16;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub struct Interpreter {
    l: LuaState,
    progname: String,
    stderr: Box<dyn Write>,
}

impl Interpreter {
    pub fn new() -> Option<Self> {
        let l = LuaState::new();
        l.gc_stop();
        Some(Interpreter {
            l,
            progname: LUA_PROGNAME.to_string(),
            stderr: Box::new(io::stderr()),
        })
    }

    pub fn set_stdout(&mut self, writer: Box<dyn Write>) {
        self.l.stdout = writer;
    }

    pub fn set_stderr(&mut self, writer: Box<dyn Write>) {
        self.stderr = writer;
    }

    fn writestring(&mut self, s: &str) {
        let _ = self.l.stdout.write_all(s.as_bytes());
    }

    fn writestring_error(&mut self, s: &str) {
        let _ = self.stderr.write_all(s.as_bytes());
    }

    fn writeline(&mut self) {
        let _ = self.l.stdout.write_all(b"\n");
        let _ = self.l.stdout.flush();
    }

    fn print_version(&mut self) {
        self.writestring(concat!(
            "Lua 5.5.0 Rust Edition  Copyright (C) 1994-2025 Lua.org, PUC-Rio\n"
        ));
    }

    fn report(&mut self, status: i32) -> i32 {
        if status != 0 {
            let msg = self.l.to_string(-1).unwrap_or_else(|| "(error message not a string)".to_string());
            let _ = write!(self.stderr, "{}: {}\n", self.progname, msg);
            self.l.pop(1);
        }
        status
    }

    /// 消息处理器 — 对应 C 的 msghandler
    ///
    /// 当 pcall 发生错误时调用，负责：
    /// 1. 将错误对象转换为字符串
    /// 2. 追加堆栈回溯信息
    fn msghandler(&mut self) {
        // 尝试将错误对象转换为字符串
        let msg = match self.l.to_string(-1) {
            Some(s) => s,
            None => {
                // 错误对象不是字符串，尝试 __tostring 元方法
                if self.l.call_meta(-1, "__tostring") {
                    if let Some(s) = self.l.to_string(-1) {
                        self.l.pop(1);
                        s
                    } else {
                        self.l.pop(1);
                        format!("(error object is a {} value)", self.l.typename_at(-1))
                    }
                } else {
                    format!("(error object is a {} value)", self.l.typename_at(-1))
                }
            }
        };
        // 弹出原始错误对象
        self.l.pop(1);
        // 追加堆栈回溯（由 execute_loop 在错误发生时构建并存储）
        let traceback = if self.l.last_traceback.is_empty() {
            format!("{}\nstack traceback:\n\t[C]: in ?", msg)
        } else {
            format!("{}\n{}", msg, self.l.last_traceback)
        };
        self.l.push_string(&traceback);
    }

    fn docall(&mut self, narg: usize, nres: i32) -> i32 {
        let base = self.l.gettop() - narg;
        setup_signal_handler();
        let status = self.l.pcall(narg, nres, 0);
        reset_signal_handler();
        if status != 0 {
            // 调用消息处理器追加堆栈回溯
            self.msghandler();
        }
        let _ = base;
        status
    }

    fn dochunk(&mut self, status: i32) -> i32 {
        let status = if status == 0 {
            self.docall(0, 0)
        } else {
            status
        };
        self.report(status)
    }

    fn dostring(&mut self, s: &str, name: &str) -> i32 {
        let status = self.l.load_buffer(s, name);
        self.dochunk(status)
    }

    fn dofile(&mut self, fname: Option<&str>) -> i32 {
        let status = self.l.load_file(fname);
        self.dochunk(status)
    }

    fn dolibrary(&mut self, globname: &str) -> i32 {
        let g = globname.to_string();
        let eq_pos = g.find('=');
        let (glob_part, mod_part): (&str, &str) = if let Some(pos) = eq_pos {
            let (a, b) = g.split_at(pos);
            (a, &b[1..])
        } else {
            (&g, &g)
        };

        self.l.get_global("require");
        self.l.push_string(mod_part);
        let status = self.docall(1, 1);

        if status == 0 {
            let global_name = if eq_pos.is_none() {
                if let Some(dash_pos) = glob_part.rfind('-') {
                    &glob_part[..dash_pos]
                } else {
                    glob_part
                }
            } else {
                glob_part
            };
            self.l.set_global(global_name);
        }
        self.report(status)
    }

    fn handle_luainit(&mut self) -> i32 {
        let init_var = format!("{}{}", LUA_INIT_VAR, LUA_VERSUFFIX);
        if let Ok(init) = std::env::var(&init_var) {
            return self.doinit(&init, &init_var);
        }
        if let Ok(init) = std::env::var(LUA_INIT_VAR) {
            return self.doinit(&init, LUA_INIT_VAR);
        }
        0
    }

    fn doinit(&mut self, init: &str, varname: &str) -> i32 {
        if let Some(stripped) = init.strip_prefix('@') {
            let status = self.l.load_file(Some(stripped));
            self.dochunk(status)
        } else {
            let name = format!("={}", varname);
            self.dostring(init, &name)
        }
    }

    fn run_args(&mut self, argv: &[String], n: usize) -> bool {
        self.l.warning("@off", false);
        let mut i = 1;
        while i < n {
            let bytes = argv[i].as_bytes();
            if bytes.len() < 2 || bytes[0] != b'-' {
                i += 1;
                continue;
            }
            match bytes[1] {
                b'e' | b'l' => {
                    let extra = if bytes.len() > 2 {
                        &argv[i][2..]
                    } else {
                        i += 1;
                        &argv[i]
                    };
                    let status = if bytes[1] == b'e' {
                        self.dostring(extra, "=(command line)")
                    } else {
                        self.dolibrary(extra)
                    };
                    if status != 0 {
                        return false;
                    }
                }
                b'W' => {
                    self.l.warning("@on", false);
                }
                _ => {}
            }
            i += 1;
        }
        true
    }

    fn push_args(&mut self) -> usize {
        let n = self.l.len(-1);
        self.l.check_stack(n + 3);
        for i in 1..=n {
            self.l.raw_get_i(-(i as isize), i as i64);
        }
        self.l.rotate(-((n + 1) as isize), -1);
        self.l.pop(1);
        n
    }

    fn handle_script(&mut self, argv: &[String], script: isize, after_dash: bool) -> i32 {
        let fname = &argv[script as usize];
        let fname_opt = if fname == "-" && !after_dash {
            None
        } else {
            Some(fname.as_str())
        };
        let status = self.l.load_file(fname_opt);
        if status == 0 {
            if self.l.get_global("arg") != LuaType::Table {
                self.l.push_string("'arg' is not a table");
                return self.report(ERR_RUN);
            }
            let n = self.push_args();
            let call_status = self.docall(n, MULT_RET);
            self.report(call_status)
        } else {
            self.report(status)
        }
    }

    fn readline(&mut self, prompt: &str) -> Option<String> {
        let _ = self.l.stdout.write_all(prompt.as_bytes());
        let _ = self.l.stdout.flush();
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

    fn check_local(&mut self, line: &str) {
        let trimmed = line.trim_start_matches(&[' ', '\t']);
        if trimmed.starts_with("local ") {
            self.writestring_error("warning: locals do not survive across lines in interactive mode\n");
        }
    }

    fn incomplete(&self, status: i32) -> bool {
        if status == ERR_SYNTAX {
            if let Some(msg) = self.l.to_string(-1) {
                if msg.len() >= EOFMARK.len()
                    && &msg[msg.len() - EOFMARK.len()..] == EOFMARK
                {
                    return true;
                }
            }
        }
        false
    }

    fn get_prompt(&mut self, firstline: bool) -> String {
        let global_name = if firstline { "_PROMPT" } else { "_PROMPT2" };
        if self.l.get_global(global_name) == LuaType::Nil {
            (if firstline { LUA_PROMPT } else { LUA_PROMPT2 }).to_string()
        } else {
            let result = self.l.to_string(-1).unwrap_or_else(|| {
                (if firstline { LUA_PROMPT } else { LUA_PROMPT2 }).to_string()
            });
            self.l.pop(1);
            result
        }
    }

    fn pushline(&mut self, firstline: bool) -> bool {
        let prompt = self.get_prompt(firstline);
        self.l.pop(1);
        match self.readline(&prompt) {
            None => false,
            Some(line) => {
                let len = line.len();
                self.l.push_lstring(line.as_bytes());
                let _ = len;
                true
            }
        }
    }

    fn addreturn(&mut self) -> i32 {
        let text = self.l.to_string(-1).unwrap_or_default();
        if text.is_empty() {
            return ERR_SYNTAX;
        }
        let retline = format!("return {};", text);
        let status = self.l.load_buffer(&retline, "=stdin");
        if status != 0 {
            self.l.pop(1);
        }
        status
    }

    fn multiline(&mut self) -> i32 {
        let first_line = self.l.to_string(1).unwrap_or_default();
        self.check_local(&first_line);

        loop {
            let line = self.l.to_string(1).unwrap_or_default();
            let status = self.l.load_buffer(&line, "=stdin");
            if !self.incomplete(status) || !self.pushline(false) {
                return status;
            }
            self.l.rotate(-2, -1);
            self.l.pop(1);
            self.l.push_lstring(b"\n");
            self.l.rotate(-2, 1);
            self.l.pop(1);
            self.l.pop(1);
            self.l.push_string(&format!("{}{}", first_line, "\n"));
        }
    }

    fn loadline(&mut self) -> i32 {
        self.l.settop(0);
        if !self.pushline(true) {
            return -1;
        }
        let status = self.addreturn();
        let status = if status != 0 {
            self.multiline()
        } else {
            status
        };
        self.l.rotate(1, -1);
        self.l.pop(1);
        status
    }

    fn l_print(&mut self) {
        let n = self.l.gettop();
        if n > 0 {
            self.l.check_stack(MIN_STACK);
            self.l.get_global("print");
            self.l.rotate(1, 1);
            if self.l.pcall(n, 0, 0) != 0 {
                let err_msg = self.l.to_string(-1).unwrap_or_else(|| "(error)".to_string());
                let _ = write!(
                    self.stderr,
                    "{}: error calling 'print' ({})\n",
                    self.progname, err_msg
                );
            }
        }
    }

    fn do_repl(&mut self) {
        loop {
            let status = self.loadline();
            if status == -1 {
                break;
            }
            let status = if status == 0 {
                self.docall(0, MULT_RET)
            } else {
                status
            };
            if status == 0 {
                self.l_print();
            } else {
                self.report(status);
            }
        }
        self.l.settop(0);
        self.writeline();
    }

    fn collect_args(argv: &[String], out_script: &mut isize) -> i32 {
        let mut args: i32 = 0;

        if argv.is_empty() {
            *out_script = -1;
            return 0;
        }

        let mut i: usize = 1;
        while i < argv.len() {
            let arg = &argv[i];
            *out_script = i as isize;
            let bytes = arg.as_bytes();
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
                            || argv[i].as_bytes().first() == Some(&b'-')
                        {
                            return HAS_ERROR;
                        }
                    }
                }
                Some(b'l') => {
                    if bytes.get(2).is_none() || bytes.get(2) == Some(&b'\0') {
                        i += 1;
                        if i >= argv.len()
                            || argv[i].as_bytes().first() == Some(&b'-')
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

    fn print_usage(&mut self, badoption: &str) {
        let _ = write!(self.stderr, "{}: ", LUA_PROGNAME);
        match badoption.chars().nth(1) {
            Some('e' | 'l') => {
                let _ = writeln!(self.stderr, "'{}' needs argument", badoption);
            }
            _ => {
                let _ = writeln!(self.stderr, "unrecognized option '{}'", badoption);
            }
        }
        let _ = writeln!(
            self.stderr,
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

    pub fn pmain(&mut self, argv: &[String]) -> bool {
        let argc = argv.len();

        if !argv.is_empty() {
            self.progname = argv[0].clone();
        }

        let mut script: isize = 0;
        let args = Self::collect_args(argv, &mut script);
        let optlim = if script > 0 {
            script as usize
        } else {
            argc
        };

        if args & HAS_ERROR != 0 {
            let bad_idx = script as usize;
            let bad = argv.get(bad_idx).map(|s| s.as_str()).unwrap_or("?");
            self.print_usage(bad);
            return false;
        }

        self.l.check_version();

        if args & HAS_V != 0 {
            self.print_version();
        }

        if args & HAS_EE != 0 {
            let key = TValue::Str(self.l.intern_str("LUA_NOENV"));
            self.l.registry.set(key, TValue::Boolean(true));
        }

        self.l.open_selected_libs(-1, 0);

        let narg = argc as isize - (script + 1);
        self.l.create_table(narg as usize, (script + 1) as usize);
        for (i, arg) in argv.iter().enumerate() {
            self.l.push_string(arg);
            self.l.raw_set_i(-2, i as i64 - script as i64);
        }
        self.l.set_global("arg");

        self.l.gc_restart();
        self.l.gc_gen();

        if args & HAS_EE == 0 {
            if self.handle_luainit() != 0 {
                return false;
            }
        }

        if !self.run_args(argv, optlim) {
            return false;
        }

        if script > 0 {
            let after_dash =
                script > 1 && argv[script as usize - 1] == "--";
            if self.handle_script(argv, script, after_dash) != 0 {
                return false;
            }
        }

        if args & HAS_I != 0 {
            self.do_repl();
        } else if script < 1 && args & (HAS_E | HAS_V) == 0 {
            if io::stdin().is_terminal() {
                self.print_version();
                self.do_repl();
            } else {
                self.dofile(None);
            }
        }

        true
    }
}

unsafe extern "C" fn laction(_sig: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn setup_signal_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = laction as *const () as usize;
        sa.sa_flags = 0;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }
}

fn reset_signal_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = libc::SIG_DFL;
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }
}

/// CLI 入口点
pub fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut interpreter = match Interpreter::new() {
        Some(ip) => ip,
        None => {
            std::process::exit(1);
        }
    };

    if !interpreter.pmain(&args) {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpreter_new() {
        let interp = Interpreter::new();
        assert!(interp.is_some());
    }

    #[test]
    fn test_collect_args_empty() {
        let argv: Vec<String> = vec![];
        let mut script: isize = 0;
        let args = Interpreter::collect_args(&argv, &mut script);
        assert_eq!(args, 0);
        assert_eq!(script, -1);
    }

    #[test]
    fn test_collect_args_version() {
        let argv: Vec<String> = vec!["lua".to_string(), "-v".to_string()];
        let mut script: isize = 0;
        let args = Interpreter::collect_args(&argv, &mut script);
        assert_eq!(args & HAS_V, HAS_V);
    }
}
