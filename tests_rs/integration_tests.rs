use std::process::Command;
use lua_rs::cli::Interpreter;
use std::os::unix::io::AsRawFd;
use std::io;
use std::os::unix::process::ExitStatusExt;

fn lua_path() -> String {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target/debug/lua");
    path.to_str().unwrap().to_string()
}

fn run_lua(args: &[&str]) -> std::process::Output {
    use std::io::{Read, Write};
    use std::os::unix::io::FromRawFd;

    // 创建管道用于捕获 stdout 和 stderr
    let mut stdout_pipe = [0i32; 2];
    let mut stderr_pipe = [0i32; 2];
    let mut stdin_pipe = [0i32; 2];

    unsafe {
        libc::pipe(stdout_pipe.as_mut_ptr());
        libc::pipe(stderr_pipe.as_mut_ptr());
        libc::pipe(stdin_pipe.as_mut_ptr());
    }

    // 保存原始的 stdout, stderr 和 stdin
    let stdout_fd = io::stdout().as_raw_fd();
    let stderr_fd = io::stderr().as_raw_fd();
    let stdin_fd = io::stdin().as_raw_fd();
    let stdout_save = unsafe { libc::dup(stdout_fd) };
    let stderr_save = unsafe { libc::dup(stderr_fd) };
    let stdin_save = unsafe { libc::dup(stdin_fd) };

    // 重定向 stdout 和 stderr 到管道，stdin 从空管道读取（立即 EOF）
    unsafe {
        libc::dup2(stdout_pipe[1], stdout_fd);
        libc::dup2(stderr_pipe[1], stderr_fd);
        libc::dup2(stdin_pipe[0], stdin_fd);
        libc::close(stdin_pipe[1]); // 关闭写端，读端立即返回 EOF
    }

    // 执行 Interpreter（argv[0] 需要是程序名）
    let mut interpreter = Interpreter::new().unwrap();
    let mut args_vec: Vec<String> = vec!["lua".to_string()];
    args_vec.extend(args.iter().map(|s| s.to_string()));
    let success = interpreter.pmain(&args_vec);

    // 刷新并恢复原始的 stdout, stderr 和 stdin
    io::stdout().flush().ok();
    io::stderr().flush().ok();
    unsafe {
        libc::dup2(stdout_save, stdout_fd);
        libc::dup2(stderr_save, stderr_fd);
        libc::dup2(stdin_save, stdin_fd);
        libc::close(stdout_save);
        libc::close(stderr_save);
        libc::close(stdin_save);
        libc::close(stdout_pipe[1]);
        libc::close(stderr_pipe[1]);
    }

    // 读取管道内容
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    unsafe {
        let mut stdout_file = std::fs::File::from_raw_fd(stdout_pipe[0]);
        let mut stderr_file = std::fs::File::from_raw_fd(stderr_pipe[0]);
        stdout_file.read_to_end(&mut stdout_buf).ok();
        stderr_file.read_to_end(&mut stderr_buf).ok();
        // File::from_raw_fd 会获取 fd 的所有权，drop 时自动关闭，不需要手动 close
    }

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