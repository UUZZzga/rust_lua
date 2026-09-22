// src/lib.rs
use std::{
    io::{self, BufRead, Cursor, Write},
    ptr::NonNull,
};

pub trait Io {
    fn read_line(&mut self, buf: &mut String) -> io::Result<usize>;
    fn err(&mut self, s: &str) -> io::Result<()>;
    fn out(&mut self, s: &str) -> io::Result<()>;

    fn out_flush(&mut self) -> io::Result<()>;
    fn err_flush(&mut self) -> io::Result<()>;
}

pub struct RealIo;

pub fn lua_io() -> &'static mut RealIo {
    // SAFETY: RealIo 是 ZST，不存在实际内存，
    // 因此对它的 &mut 不可能产生别名冲突；NonNull::dangling 返回对齐的非空指针。
    unsafe { &mut *NonNull::<RealIo>::dangling().as_ptr() }
}

// 体积优先: 包装 Stdout/Stderr, 覆盖 write_fmt 方法
// 避免 default_write_fmt → Error::new → StringError → Unicode grapheme/whitespace 表 (~4KB)
// Box<dyn Write> 的 vtable 会引用 write_fmt, 即使不调用也会被链接器保留
#[cfg(size_optimized)]
pub struct LuaStdout(pub std::io::Stdout);

#[cfg(size_optimized)]
impl Write for LuaStdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.0.write_all(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
    // 覆盖 write_fmt: no-op (代码中仅使用 write_all + flush)
    fn write_fmt(&mut self, _: std::fmt::Arguments<'_>) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(size_optimized)]
pub struct LuaStderr(pub std::io::Stderr);

#[cfg(size_optimized)]
impl Write for LuaStderr {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.0.write_all(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
    fn write_fmt(&mut self, _: std::fmt::Arguments<'_>) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn lua_stdin() -> Box<dyn BufRead> {
    Box::new(std::io::stdin().lock())
}

#[cfg(size_optimized)]
pub fn lua_stdout() -> Box<dyn Write> {
    Box::new(LuaStdout(std::io::stdout()))
}

#[cfg(not(size_optimized))]
pub fn lua_stdout() -> Box<dyn Write> {
    Box::new(std::io::stdout())
}

#[cfg(size_optimized)]
pub fn lua_stderr() -> Box<dyn Write> {
    Box::new(LuaStdout(std::io::stderr()))
}

#[cfg(not(size_optimized))]
pub fn lua_stderr() -> Box<dyn Write> {
    Box::new(std::io::stderr())
}

impl Io for RealIo {
    fn read_line(&mut self, buf: &mut String) -> io::Result<usize> {
        io::stdin().lock().read_line(buf)
    }
    fn out(&mut self, s: &str) -> io::Result<()> {
        io::stdout().lock().write_all(s.as_bytes())
    }
    fn err(&mut self, s: &str) -> io::Result<()> {
        io::stderr().lock().write_all(s.as_bytes())
    }

    fn out_flush(&mut self) -> io::Result<()> {
        io::stdout().lock().flush()
    }

    fn err_flush(&mut self) -> io::Result<()> {
        io::stderr().lock().flush()
    }
}

// ---------- 测试用 ----------

pub struct BufferIo {
    stdin: Cursor<Vec<u8>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl BufferIo {
    pub fn new(stdin: impl Into<Vec<u8>>) -> Self {
        Self {
            stdin: Cursor::new(stdin.into()),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    // 只读访问器，测试断言用
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }
    pub fn stdout_str(&self) -> &str {
        std::str::from_utf8(&self.stdout).unwrap()
    }
    pub fn stderr_str(&self) -> &str {
        std::str::from_utf8(&self.stderr).unwrap()
    }
}

impl Io for BufferIo {
    fn read_line(&mut self, buf: &mut String) -> io::Result<usize> {
        self.stdin.read_line(buf)
    }
    fn out(&mut self, s: &str) -> io::Result<()> {
        self.stdout.write_all(s.as_bytes()) // Vec<u8>: Write
    }
    fn err(&mut self, s: &str) -> io::Result<()> {
        self.stderr.write_all(s.as_bytes())
    }
    fn out_flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn err_flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
