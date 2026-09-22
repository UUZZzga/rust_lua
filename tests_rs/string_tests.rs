use lua_rs::cli::Interpreter;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;

/// 运行 Lua 代码并返回输出
fn run_lua(args: &[&str]) -> std::process::Output {
    let mut buff = lua_rs::mock::io_mock::BufferIo::new("");
    let success = {
        let mut interpreter = Interpreter::new(&mut buff);
        let mut args_vec: Vec<String> = vec!["lua".to_string()];
        args_vec.extend(args.iter().map(|s| s.to_string()));
        interpreter.pmain(&args_vec)
    };
    let stdout_buf = buff.stdout();
    let stderr_buf = buff.stderr();

    let stdout_str = String::from_utf8_lossy(&stdout_buf);
    let stderr_str = String::from_utf8_lossy(&stderr_buf);
    if !stdout_str.is_empty() {
        println!("STDOUT:\n{}", stdout_str);
    }
    if !stderr_str.is_empty() {
        eprintln!("STDERR:\n{}", stderr_str);
    }

    std::process::Output {
        status: std::process::ExitStatus::from_raw(if success { 0 } else { 1 } as _),
        stdout: stdout_buf.to_vec(),
        stderr: stderr_buf.to_vec(),
    }
}

// ============================================================================
// 字符串库集成测试
// ============================================================================

#[test]
fn test_string_lower() {
    let output = run_lua(&["-e", "print(string.lower('HELLO WORLD'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello world"));
}

#[test]
fn test_string_len() {
    let output = run_lua(&["-e", "print(string.len('hello'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("5"));
}

#[test]
fn test_string_sub() {
    let output = run_lua(&["-e", "print(string.sub('hello world', 1, 5))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello"));
}

#[test]
fn test_string_sub_negative() {
    let output = run_lua(&["-e", "print(string.sub('hello', -3))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("llo"));
}

#[test]
fn test_string_reverse() {
    let output = run_lua(&["-e", "print(string.reverse('hello'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("olleh"));
}

#[test]
fn test_string_byte() {
    let output = run_lua(&["-e", "print(string.byte('A'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("65"));
}

#[test]
fn test_string_char() {
    let output = run_lua(&["-e", "print(string.char(72, 73))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("HI"));
}

#[test]
fn test_string_rep() {
    let output = run_lua(&["-e", "print(string.rep('ab', 3))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ababab"));
}

#[test]
fn test_string_rep_with_sep() {
    let output = run_lua(&["-e", "print(string.rep('x', 3, '-'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("x-x-x"));
}

#[test]
fn test_string_find_plain() {
    let output = run_lua(&["-e", "print(string.find('hello world', 'world'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("7"));
    assert!(stdout.contains("11"));
}

#[test]
fn test_string_find_not_found() {
    let output = run_lua(&["-e", "print(string.find('hello', 'xyz'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("nil"));
}

#[test]
fn test_string_find_pattern() {
    let output = run_lua(&["-e", "print(string.find('hello123', '%d+'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("6"));
}

#[test]
fn test_string_format_string() {
    let output = run_lua(&["-e", "print(string.format('Hello, %s!', 'World'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Hello, World!"));
}

#[test]
fn test_string_format_integer() {
    let output = run_lua(&["-e", "print(string.format('Value: %d', 42))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Value: 42"));
}

#[test]
fn test_string_format_hex() {
    let output = run_lua(&["-e", "print(string.format('%x', 255))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ff"));
}

#[test]
fn test_string_format_multiple() {
    let output = run_lua(&["-e", "print(string.format('%s=%d', 'x', 10))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("x=10"));
}

#[test]
fn test_string_match_basic() {
    let output = run_lua(&["-e", "print(string.match('hello123world', '%d+'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("123"));
}

#[test]
fn test_string_match_capture() {
    let output = run_lua(&["-e", "print(string.match('key=value', '(%w+)=(%w+)'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("key"));
    assert!(stdout.contains("value"));
}

#[test]
fn test_string_gsub_basic() {
    let output = run_lua(&["-e", "print(string.gsub('hello world', 'world', 'Lua'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello Lua"));
}

#[test]
fn test_string_gsub_count() {
    let output = run_lua(&["-e", "print(string.gsub('aaa', 'a', 'b'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("bbb"));
    assert!(stdout.contains("3"));
}

#[test]
fn test_string_gsub_pattern() {
    let output = run_lua(&["-e", "print(string.gsub('hello 123 world', '%d+', 'NUM'))"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello NUM world"));
}

#[test]
fn test_string_method_syntax() {
    // 测试方法调用语法 s:upper()
    let output = run_lua(&["-e", "print(('hello'):upper())"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("HELLO"));
}

#[test]
fn test_string_concat_with_lib() {
    let output = run_lua(&["-e", "print(string.upper('hello') .. '!')"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("HELLO!"));
}
