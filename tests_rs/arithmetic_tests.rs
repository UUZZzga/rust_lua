//! 算术运算正确性测试
//!
//! 验证项目:
//! 1. 整数算术 — 加减乘除取模整除
//! 2. 浮点算术 — 加减乘除取模整除
//! 3. 混合算术 — 整数与浮点混合运算
//! 4. 幂运算 — ^ 操作符
//! 5. 按位运算 — & | ~ << >>
//! 6. 一元运算 — 负号、按位取反
//! 7. 除零处理 — 整数除零、浮点除零
//! 8. 数值转换 — 整数↔浮点自动转换
//!
//! 对应 C 源码: lvm.cpp 中的 OP_ADD/SUB/MUL/MUL/MOD/IDIV/POW/
//!              BAND/BOR/BXOR/SHL/SHR/UNM/BNOT
//! 对应 Rust: execute.rs 中的 op_add/op_sub/op_mul/op_div/op_mod/
//!            op_idiv/op_pow/op_band/op_bor/op_bxor/op_shl/op_shr/
//!            op_unm/op_bnot

use lua_rs::cli::Interpreter;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::sync::{Arc, Mutex};

struct SharedWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                buffer: buffer.clone(),
            },
            buffer,
        )
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
    let (stdout_writer, stdout_buffer) = SharedWriter::new();
    let (stderr_writer, stderr_buffer) = SharedWriter::new();
    interpreter.set_stdout(Box::new(stdout_writer));
    interpreter.set_stderr(Box::new(stderr_writer));

    let mut args_vec: Vec<String> = vec!["lua".to_string()];
    args_vec.extend(args.iter().map(|s| s.to_string()));
    let success = interpreter.pmain(&args_vec);

    let stdout_buf = stdout_buffer.lock().unwrap().clone();
    let stderr_buf = stderr_buffer.lock().unwrap().clone();

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

// ============================================================================
// 1. 整数算术
// ============================================================================

#[test]
fn test_int_add() {
    let output = run_lua(&["-e", "print(2 + 3)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("5"));
}

#[test]
fn test_int_sub() {
    let output = run_lua(&["-e", "print(10 - 4)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("6"));
}

#[test]
fn test_int_mul() {
    let output = run_lua(&["-e", "print(6 * 7)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("42"));
}

#[test]
fn test_int_div() {
    let output = run_lua(&["-e", "print(10 / 2)"]);
    assert!(output.status.success());
    // 整数/整数在 Lua 5.5 中如果整除则返回整数，否则返回浮点
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("5.0") || stdout.contains("5"),
        "10/2 应为 5, got: {}",
        stdout
    );
}

#[test]
fn test_int_mod() {
    let output = run_lua(&["-e", "print(10 % 3)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("1"));
}

#[test]
fn test_int_idiv() {
    let output = run_lua(&["-e", "print(10 // 3)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("3"));
}

#[test]
fn test_int_mod_negative() {
    let output = run_lua(&["-e", "print(-10 % 3)"]);
    assert!(output.status.success());
    // Lua 风格取模: 结果与除数同号
    assert!(String::from_utf8_lossy(&output.stdout).contains("2"));
}

#[test]
fn test_int_idiv_negative() {
    let output = run_lua(&["-e", "print(-10 // 3)"]);
    assert!(output.status.success());
    // Lua 风格整除: floor(-10/3) = floor(-3.33) = -4
    assert!(String::from_utf8_lossy(&output.stdout).contains("-4"));
}

// ============================================================================
// 2. 浮点算术
// ============================================================================

#[test]
fn test_float_add() {
    let output = run_lua(&["-e", "print(2.5 + 3.5)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("6"));
}

#[test]
fn test_float_sub() {
    let output = run_lua(&["-e", "print(10.5 - 4.5)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("6"));
}

#[test]
fn test_float_mul() {
    let output = run_lua(&["-e", "print(2.5 * 4.0)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("10"));
}

#[test]
fn test_float_div() {
    let output = run_lua(&["-e", "print(10.0 / 4.0)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("2.5"));
}

#[test]
fn test_float_mod() {
    let output = run_lua(&["-e", "print(10.5 % 3.0)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("1.5"));
}

#[test]
fn test_float_idiv() {
    let output = run_lua(&["-e", "print(10.5 // 3.0)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("3"));
}

// ============================================================================
// 3. 混合算术 (整数与浮点)
// ============================================================================

#[test]
fn test_mixed_add() {
    let output = run_lua(&["-e", "print(2 + 3.5)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("5.5"));
}

#[test]
fn test_mixed_mul() {
    let output = run_lua(&["-e", "print(3 * 2.0)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("6"));
}

#[test]
fn test_mixed_div() {
    let output = run_lua(&["-e", "print(7 / 2)"]);
    assert!(output.status.success());
    // 整数/整数如果不能整除，返回浮点
    assert!(String::from_utf8_lossy(&output.stdout).contains("3.5"));
}

#[test]
fn test_mixed_idiv() {
    let output = run_lua(&["-e", "print(7.5 // 2)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("3"));
}

// ============================================================================
// 4. 幂运算
// ============================================================================

#[test]
fn test_pow_int() {
    let output = run_lua(&["-e", "print(2 ^ 10)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("1024"));
}

#[test]
fn test_pow_float() {
    let output = run_lua(&["-e", "print(2.0 ^ 0.5)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // sqrt(2) ≈ 1.414...
    assert!(
        stdout.contains("1.414"),
        "2^0.5 应约为 1.414, got: {}",
        stdout
    );
}

#[test]
fn test_pow_negative() {
    let output = run_lua(&["-e", "print(2 ^ -1)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("0.5"));
}

// ============================================================================
// 5. 按位运算
// ============================================================================

#[test]
fn test_band() {
    let output = run_lua(&["-e", "print(0xFF & 0x0F)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("15"));
}

#[test]
fn test_bor() {
    let output = run_lua(&["-e", "print(0xF0 | 0x0F)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("255"));
}

#[test]
fn test_bxor() {
    let output = run_lua(&["-e", "print(0xFF ~ 0x0F)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("240"));
}

#[test]
fn test_shl() {
    let output = run_lua(&["-e", "print(1 << 4)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("16"));
}

#[test]
fn test_shr() {
    let output = run_lua(&["-e", "print(256 >> 4)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("16"));
}

// ============================================================================
// 6. 一元运算
// ============================================================================

#[test]
fn test_unm_int() {
    let output = run_lua(&["-e", "print(-42)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("-42"));
}

#[test]
fn test_unm_float() {
    let output = run_lua(&["-e", "print(-3.14)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("-3.14"));
}

#[test]
fn test_bnot_zero() {
    let output = run_lua(&["-e", "print(~0)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("-1"));
}

#[test]
fn test_bnot_one() {
    let output = run_lua(&["-e", "print(~1)"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("-2"));
}

// ============================================================================
// 7. 除零处理
// ============================================================================

#[test]
fn test_int_div_zero_error() {
    let output = run_lua(&[
        "-e",
        "local ok, err = pcall(function() return 1 // 0 end)\n\
         print(ok)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("false"),
        "整数整除零应报错, got: {}",
        stdout
    );
}

#[test]
fn test_int_mod_zero_error() {
    let output = run_lua(&[
        "-e",
        "local ok, err = pcall(function() return 1 % 0 end)\n\
         print(ok)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("false"),
        "整数取模零应报错, got: {}",
        stdout
    );
}

#[test]
fn test_float_div_zero_inf() {
    let output = run_lua(&[
        "-e",
        "local r = 1.0 / 0.0\n\
         print(r)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 浮点除零返回 inf
    assert!(stdout.contains("inf"), "浮点除零应为 inf, got: {}", stdout);
}

// ============================================================================
// 8. 数值转换
// ============================================================================

#[test]
fn test_int_to_float_div() {
    let output = run_lua(&["-e", "print(4 / 2)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 4/2 = 2.0 (浮点除法)
    assert!(stdout.contains("2.0"), "4/2 应为 2.0, got: {}", stdout);
}

#[test]
fn test_float_to_int_idiv() {
    let output = run_lua(&["-e", "print(9.9 // 1)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("9"), "9.9//1 应为 9, got: {}", stdout);
}

#[test]
fn test_mixed_arithmetic_chain() {
    let output = run_lua(&["-e", "print(1 + 2 * 3 - 4 // 2)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 1 + 6 - 2 = 5
    assert!(stdout.contains("5"), "1+2*3-4//2 应为 5, got: {}", stdout);
}
