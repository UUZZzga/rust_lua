use std::process::Command;
use lua_rs::cli::Interpreter;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::sync::{Arc, Mutex};

fn lua_path() -> String {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target/debug/lua");
    path.to_str().unwrap().to_string()
}

// 可共享的 writer，用于捕获输出
struct SharedWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (Self { buffer: buffer.clone() }, buffer)
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run_lua(args: &[&str]) -> std::process::Output {
    let mut interpreter = Interpreter::new().unwrap();

    // 注入自定义 writer 捕获输出
    let (stdout_writer, stdout_buffer) = SharedWriter::new();
    let (stderr_writer, stderr_buffer) = SharedWriter::new();
    interpreter.set_stdout(Box::new(stdout_writer));
    interpreter.set_stderr(Box::new(stderr_writer));

    // 执行 Interpreter（argv[0] 需要是程序名）
    let mut args_vec: Vec<String> = vec!["lua".to_string()];
    args_vec.extend(args.iter().map(|s| s.to_string()));
    let success = interpreter.pmain(&args_vec);

    let stdout_buf = stdout_buffer.lock().unwrap().clone();
    let stderr_buf = stderr_buffer.lock().unwrap().clone();

    // 打印输出
    let stdout_str = String::from_utf8_lossy(&stdout_buf);
    let stderr_str = String::from_utf8_lossy(&stderr_buf);
    if !stdout_str.is_empty() {
        println!("STDOUT:\n{}", stdout_str);
    }
    if !stderr_str.is_empty() {
        eprintln!("STDERR:\n{}", stderr_str);
    }

    std::process::Output {
        status: std::process::ExitStatus::from_raw(if success { 0 } else { 1 }),
        stdout: stdout_buf,
        stderr: stderr_buf,
    }
}

fn run_lua_input(args: &[&str], stdin: &str) -> std::process::Output {
    use std::process::Stdio;
    let mut child = Command::new(lua_path())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn lua binary");
    {
        use std::io::Write;
        let child_stdin = child.stdin.as_mut().unwrap();
        child_stdin.write_all(stdin.as_bytes()).unwrap();
    }
    child.wait_with_output().expect("failed to wait on lua")
}

#[test]
fn test_version() {
    let output = run_lua(&["-v"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Lua 5.5.0"));
    assert!(stdout.contains("Rust Edition"));
}

#[test]
fn test_execute_string() {
    let output = run_lua(&["-e", "print('hello')"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello"));
}

#[test]
fn test_math_expression() {
    let output = run_lua(&["-e", "print(2+2)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4"));
}

#[test]
fn test_multiple_expressions() {
    let output = run_lua(&["-e", "x=10; y=20; print(x+y)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("30"));
}

#[test]
fn test_error_traceback() {
    let output = run_lua(&["-e", "error('test error')"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("test error"));
    assert!(stderr.contains("stack traceback"));
}

#[test]
fn test_syntax_error() {
    let output = run_lua(&["-e", "print(++++"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.is_empty());
}

#[test]
fn test_table_operations() {
    let output = run_lua(&["-e", "t={a=1,b=2}; print(t.a+t.b)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"));
}

#[test]
fn test_for_loop() {
    let output = run_lua(&["-e", "s=0; for i=1,100 do s=s+i end; print(s)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("5050"));
}

#[test]
fn test_function_definition() {
    let output = run_lua(&["-e", "function add(a,b) return a+b end; print(add(3,4))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("7"));
}

#[test]
fn test_string_library() {
    let output = run_lua(&["-e", "print(string.upper('hello'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("HELLO"));
}

#[test]
fn test_math_library() {
    let output = run_lua(&["-e", "print(math.abs(-42))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"));
}

#[test]
fn test_coroutine() {
    let output = run_lua(&[
        "-e",
        "co=coroutine.create(function(x) return x*2 end); print(coroutine.status(co))",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("suspended"));
}

#[test]
fn test_nil_and_bool() {
    let output = run_lua(&["-e", "print(nil==nil, true~=false)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("true"));
}

#[test]
fn test_concat() {
    let output = run_lua(&["-e", "print('hello' .. ' ' .. 'world')"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello world"));
}

#[test]
fn test_collect_args_e_option() {
    let output = run_lua(&[
        "-e", "print('x')",
        "-e", "print('y')",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("x"));
    assert!(stdout.contains("y"));
}

#[test]
fn test_usage_error() {
    let output = run_lua(&["-x"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unrecognized option"));
}

#[test]
fn test_e_needs_arg() {
    let output = run_lua(&["-e"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("needs argument"));
}

#[test]
fn test_inline_e_expression() {
    let output = run_lua(&["-eprint('inline')"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("inline"));
}

#[test]
fn test_stdin_execution() {
    let output = run_lua_input(&[], "print('from_stdin')\n");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("from_stdin"));
}

#[test]
fn test_empty_input() {
    let output = run_lua(&["-e", ""]);
    assert!(output.status.success());
}

#[test]
fn test_closures() {
    let output = run_lua(&[
        "-e",
        "function counter() local n=0; return function() n=n+1; return n end end; c=counter(); print(c(), c(), c())",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1"));
    assert!(stdout.contains("2"));
    assert!(stdout.contains("3"));
}