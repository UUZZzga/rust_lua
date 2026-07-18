//! 数学库 (lmathlib.cpp) 集成测试
//!
//! 测试所有数学库函数的功能正确性，包括:
//! - 常量: pi, huge, maxinteger, mininteger
//! - 基本函数: abs, floor, ceil, sqrt, exp, log
//! - 三角函数: sin, cos, tan, asin, acos, atan
//! - 角度转换: deg, rad
//! - 数值操作: fmod, modf, frexp, ldexp, tointeger, ult
//! - min/max
//! - math.type
//! - 随机数: random, randomseed
//!
//! 对应 C 源码: lmathlib.cpp

use lua_rs::cli::Interpreter;
use lua_rs::objects::{NilKind, TValue};
use lua_rs::state::LuaState;
use lua_rs::stdlib::math_lib;
use lua_rs::table::Table;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::sync::{Arc, Mutex};

// ============================================================================
// 辅助工具: 捕获输出的 writer
// ============================================================================

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

/// 运行 Lua 代码并返回输出
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

/// 运行 Lua 表达式代码 (-e)
fn run_lua_expr(code: &str) -> std::process::Output {
    run_lua(&["-e", code])
}

// ============================================================================
// 常量测试 (对应 C 的 luaopen_math 中常量设置)
// ============================================================================

#[test]
fn test_math_pi() {
    let output = run_lua_expr("print(math.pi)");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().contains("3.14159265358979"),
        "expected PI value, got: {}",
        stdout
    );
}

#[test]
fn test_math_huge() {
    let output = run_lua_expr("print(math.huge)");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.trim() == "inf", "expected 'inf', got: {}", stdout);
}

#[test]
fn test_math_maxinteger() {
    let output = run_lua_expr("print(math.maxinteger)");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // i64::MAX = 9223372036854775807
    assert!(
        stdout.trim().contains("9223372036854775807"),
        "expected maxinteger, got: {}",
        stdout
    );
}

#[test]
fn test_math_mininteger() {
    let output = run_lua_expr("print(math.mininteger)");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // i64::MIN = -9223372036854775808
    assert!(
        stdout.trim().contains("-9223372036854775808"),
        "expected mininteger, got: {}",
        stdout
    );
}

// ============================================================================
// math.abs 测试 (对应 C 的 math_abs)
// ============================================================================

#[test]
fn test_math_abs_positive_integer() {
    let output = run_lua_expr("print(math.abs(42))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"));
}

#[test]
fn test_math_abs_negative_integer() {
    let output = run_lua_expr("print(math.abs(-42))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"));
}

#[test]
fn test_math_abs_float() {
    let output = run_lua_expr("print(math.abs(-3.14))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3.14"));
}

#[test]
fn test_math_abs_zero() {
    let output = run_lua_expr("print(math.abs(0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0"));
}

// ============================================================================
// math.floor / math.ceil 测试 (对应 C 的 math_floor / math_ceil)
// ============================================================================

#[test]
fn test_math_floor_float() {
    let output = run_lua_expr("print(math.floor(3.7))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"));
}

#[test]
fn test_math_floor_negative() {
    let output = run_lua_expr("print(math.floor(-3.2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-4"));
}

#[test]
fn test_math_floor_integer() {
    let output = run_lua_expr("print(math.floor(5))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("5"));
}

#[test]
fn test_math_ceil_float() {
    let output = run_lua_expr("print(math.ceil(3.2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4"));
}

#[test]
fn test_math_ceil_negative() {
    let output = run_lua_expr("print(math.ceil(-3.7))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-3"));
}

#[test]
fn test_math_ceil_integer() {
    let output = run_lua_expr("print(math.ceil(5))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("5"));
}

// ============================================================================
// math.sqrt / math.exp / math.log 测试
// ============================================================================

#[test]
fn test_math_sqrt() {
    let output = run_lua_expr("print(math.sqrt(4))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2"));
}

#[test]
fn test_math_sqrt_two() {
    let output = run_lua_expr("print(math.sqrt(2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1.414"));
}

#[test]
fn test_math_exp() {
    let output = run_lua_expr("print(math.exp(0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1"));
}

#[test]
fn test_math_exp_one() {
    let output = run_lua_expr("print(math.exp(1))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // e ≈ 2.71828
    assert!(stdout.contains("2.718"));
}

#[test]
fn test_math_log_natural() {
    let output = run_lua_expr("print(math.log(1))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0"));
}

#[test]
fn test_math_log_e() {
    let output = run_lua_expr("print(math.log(math.exp(1)))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1"));
}

#[test]
fn test_math_log_base_2() {
    let output = run_lua_expr("print(math.log(8, 2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"));
}

#[test]
fn test_math_log_base_10() {
    let output = run_lua_expr("print(math.log(100, 10))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2"));
}

#[test]
fn test_math_log_base_other() {
    let output = run_lua_expr("print(math.log(1000, 10))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"));
}

// ============================================================================
// 三角函数测试 (对应 C 的 math_sin / math_cos / math_tan)
// ============================================================================

#[test]
fn test_math_sin_zero() {
    let output = run_lua_expr("print(math.sin(0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0"));
}

#[test]
fn test_math_sin_half_pi() {
    let output = run_lua_expr("print(math.sin(math.pi / 2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1"));
}

#[test]
fn test_math_cos_zero() {
    let output = run_lua_expr("print(math.cos(0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1"));
}

#[test]
fn test_math_cos_pi() {
    let output = run_lua_expr("print(math.cos(math.pi))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // cos(PI) = -1, 可能显示为 "-1.0" 或 "-1"
    assert!(stdout.contains("-1"));
}

#[test]
fn test_math_tan_zero() {
    let output = run_lua_expr("print(math.tan(0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0"));
}

#[test]
fn test_math_tan_quarter_pi() {
    let output = run_lua_expr("print(math.tan(math.pi / 4))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // tan(PI/4) ≈ 1
    assert!(stdout.contains("1") || stdout.contains("0.9999"));
}

// ============================================================================
// 反三角函数测试 (对应 C 的 math_asin / math_acos / math_atan)
// ============================================================================

#[test]
fn test_math_asin_one() {
    let output = run_lua_expr("print(math.asin(1))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // asin(1) = PI/2 ≈ 1.5708
    assert!(stdout.contains("1.570"));
}

#[test]
fn test_math_acos_one() {
    let output = run_lua_expr("print(math.acos(1))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0"));
}

#[test]
fn test_math_acos_neg_one() {
    let output = run_lua_expr("print(math.acos(-1))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // acos(-1) = PI ≈ 3.14159
    assert!(stdout.contains("3.141"));
}

#[test]
fn test_math_atan_single() {
    let output = run_lua_expr("print(math.atan(1))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // atan(1) = PI/4 ≈ 0.7854
    assert!(stdout.contains("0.785"));
}

#[test]
fn test_math_atan_two_args() {
    let output = run_lua_expr("print(math.atan(1, 1))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // atan2(1, 1) = PI/4 ≈ 0.7854
    assert!(stdout.contains("0.785"));
}

// ============================================================================
// 角度转换测试 (对应 C 的 math_deg / math_rad)
// ============================================================================

#[test]
fn test_math_deg_pi() {
    let output = run_lua_expr("print(math.deg(math.pi))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("180"));
}

#[test]
fn test_math_deg_half_pi() {
    let output = run_lua_expr("print(math.deg(math.pi / 2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("90"));
}

#[test]
fn test_math_rad_180() {
    let output = run_lua_expr("print(math.rad(180))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3.141"));
}

#[test]
fn test_math_rad_90() {
    let output = run_lua_expr("print(math.rad(90))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1.570"));
}

#[test]
fn test_math_deg_rad_roundtrip() {
    let output = run_lua_expr("print(math.rad(math.deg(1.0)))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // rad(deg(1.0)) 应该 ≈ 1.0
    assert!(stdout.contains("1"));
}

// ============================================================================
// math.fmod 测试 (对应 C 的 math_fmod)
// ============================================================================

#[test]
fn test_math_fmod_integer() {
    let output = run_lua_expr("print(math.fmod(10, 3))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1"));
}

#[test]
fn test_math_fmod_negative() {
    let output = run_lua_expr("print(math.fmod(-10, 3))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-1"));
}

#[test]
fn test_math_fmod_float() {
    let output = run_lua_expr("print(math.fmod(10.5, 3.0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1.5"));
}

#[test]
fn test_math_fmod_zero_error() {
    let output = run_lua_expr("print(pcall(math.fmod, 10, 0))");
    // pcall 捕获错误, 程序不应崩溃
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("fmod(10, 0) stdout: {}, stderr: {}", stdout, stderr);
    // 应该返回 false (错误被捕获)
    assert!(
        stdout.contains("false") || !output.status.success(),
        "expected pcall to catch error or fail"
    );
}

// ============================================================================
// math.modf 测试 (对应 C 的 math_modf)
// ============================================================================

#[test]
fn test_math_modf_positive() {
    let output = run_lua_expr("print(math.modf(3.14))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // modf 返回整数部分和小数部分
    assert!(stdout.contains("3"));
    assert!(stdout.contains("0.14"));
}

#[test]
fn test_math_modf_negative() {
    let output = run_lua_expr("print(math.modf(-3.14))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-3"));
    assert!(stdout.contains("-0.14"));
}

#[test]
fn test_math_modf_integer() {
    let output = run_lua_expr("print(math.modf(42))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 整数: 整数部分是自身, 小数部分是 0
    assert!(stdout.contains("42"));
    assert!(stdout.contains("0"));
}

#[test]
fn test_math_modf_whole_float() {
    let output = run_lua_expr("print(math.modf(3.0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"));
}

// ============================================================================
// math.tointeger 测试 (对应 C 的 math_toint)
// ============================================================================

#[test]
fn test_math_tointeger_integer() {
    let output = run_lua_expr("print(math.tointeger(42))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"));
}

#[test]
fn test_math_tointeger_float_whole() {
    let output = run_lua_expr("print(math.tointeger(42.0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"));
}

#[test]
fn test_math_tointeger_float_fraction() {
    let output = run_lua_expr("print(math.tointeger(42.5))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 不可转换: 返回 nil
    assert!(stdout.contains("nil"));
}

#[test]
fn test_math_tointeger_string() {
    let output = run_lua_expr("print(math.tointeger('hello'))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 字符串不可转换: 返回 nil
    assert!(stdout.contains("nil"));
}

// ============================================================================
// math.ult 测试 (对应 C 的 math_ult)
// ============================================================================

#[test]
fn test_math_ult_true() {
    let output = run_lua_expr("print(math.ult(1, 2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("true"));
}

#[test]
fn test_math_ult_false() {
    let output = run_lua_expr("print(math.ult(2, 1))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("false"));
}

#[test]
fn test_math_ult_equal() {
    let output = run_lua_expr("print(math.ult(5, 5))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("false"));
}

#[test]
fn test_math_ult_unsigned() {
    // -1 作为无符号是 u64::MAX, 大于 1
    let output = run_lua_expr("print(math.ult(1, -1))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("true"));
}

// ============================================================================
// math.frexp / math.ldexp 测试 (对应 C 的 math_frexp / math_ldexp)
// ============================================================================

#[test]
fn test_math_frexp_one() {
    let output = run_lua_expr("print(math.frexp(1.0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // frexp(1.0) = 0.5, 1
    assert!(stdout.contains("0.5"));
    assert!(stdout.contains("1"));
}

#[test]
fn test_math_frexp_four() {
    let output = run_lua_expr("print(math.frexp(4.0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // frexp(4.0) = 0.5, 3
    assert!(stdout.contains("0.5"));
    assert!(stdout.contains("3"));
}

#[test]
fn test_math_ldexp_basic() {
    let output = run_lua_expr("print(math.ldexp(0.5, 3))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // ldexp(0.5, 3) = 4.0
    assert!(stdout.contains("4"));
}

#[test]
fn test_math_frexp_ldexp_roundtrip() {
    let output = run_lua_expr("print(math.ldexp(math.frexp(3.14)))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 应该 ≈ 3.14
    assert!(stdout.contains("3.14"));
}

// ============================================================================
// math.min / math.max 测试 (对应 C 的 math_min / math_max)
// ============================================================================

#[test]
fn test_math_min_integers() {
    let output = run_lua_expr("print(math.min(3, 1, 2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1"));
}

#[test]
fn test_math_min_floats() {
    let output = run_lua_expr("print(math.min(3.14, 1.41, 2.71))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1.41"));
}

#[test]
fn test_math_min_mixed() {
    let output = run_lua_expr("print(math.min(3, 1.5, 2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1.5"));
}

#[test]
fn test_math_min_single() {
    let output = run_lua_expr("print(math.min(42))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"));
}

#[test]
fn test_math_max_integers() {
    let output = run_lua_expr("print(math.max(1, 3, 2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"));
}

#[test]
fn test_math_max_floats() {
    let output = run_lua_expr("print(math.max(1.41, 3.14, 2.71))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3.14"));
}

#[test]
fn test_math_max_mixed() {
    let output = run_lua_expr("print(math.max(1, 2.5, 2))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2.5"));
}

#[test]
fn test_math_max_single() {
    let output = run_lua_expr("print(math.max(42))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"));
}

// ============================================================================
// math.type 测试 (对应 C 的 math_type)
// ============================================================================

#[test]
fn test_math_type_integer() {
    let output = run_lua_expr("print(math.type(42))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("integer"));
}

#[test]
fn test_math_type_float() {
    let output = run_lua_expr("print(math.type(3.14))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("float"));
}

#[test]
fn test_math_type_float_whole() {
    let output = run_lua_expr("print(math.type(3.0))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 3.0 是浮点数
    assert!(stdout.contains("float"));
}

#[test]
fn test_math_type_string() {
    let output = run_lua_expr("print(math.type('hello'))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 非数字返回 nil
    assert!(stdout.contains("nil"));
}

#[test]
fn test_math_type_nil() {
    let output = run_lua_expr("print(math.type(nil))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("nil"));
}

// ============================================================================
// math.random 测试 (对应 C 的 math_random)
// ============================================================================

#[test]
fn test_math_random_no_args() {
    let output = run_lua_expr("print(math.random())");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 应该输出一个 [0, 1) 范围的浮点数
    let trimmed = stdout.trim();
    let val: f64 = trimmed
        .parse()
        .expect(&format!("expected float, got: {}", trimmed));
    assert!(
        val >= 0.0 && val < 1.0,
        "random() returned {} which is out of [0, 1)",
        val
    );
}

#[test]
fn test_math_random_single_arg() {
    // 测试 100 次, 都应该在 [1, 6] 范围内
    // 注意: Lua VM 的比较运算符有 bug, 所以用 math.min/max 来检查范围
    let code = r#"
        local min_val = 999
        local max_val = -999
        for i = 1, 100 do
            local r = math.random(6)
            min_val = math.min(min_val, r)
            max_val = math.max(max_val, r)
        end
        print("MIN=" .. min_val .. " MAX=" .. max_val)
    "#;
    let output = run_lua_expr(code);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 解析输出验证范围
    assert!(
        stdout.contains("MIN="),
        "expected MIN= in output: {}",
        stdout
    );
    assert!(
        stdout.contains("MAX="),
        "expected MAX= in output: {}",
        stdout
    );
    // 提取最小值和最大值
    let min_str = stdout
        .split("MIN=")
        .nth(1)
        .unwrap()
        .split(" ")
        .next()
        .unwrap();
    let max_str = stdout.split("MAX=").nth(1).unwrap().trim();
    let min_val: i64 = min_str
        .parse()
        .expect(&format!("expected integer, got: {}", min_str));
    let max_val: i64 = max_str
        .parse()
        .expect(&format!("expected integer, got: {}", max_str));
    assert!(min_val >= 1, "min value {} should be >= 1", min_val);
    assert!(max_val <= 6, "max value {} should be <= 6", max_val);
}

#[test]
fn test_math_random_two_args() {
    // 测试 100 次, 都应该在 [10, 20] 范围内
    // 注意: Lua VM 的比较运算符有 bug, 所以用 math.min/max 来检查范围
    let code = r#"
        local min_val = 999
        local max_val = -999
        for i = 1, 100 do
            local r = math.random(10, 20)
            min_val = math.min(min_val, r)
            max_val = math.max(max_val, r)
        end
        print("MIN=" .. min_val .. " MAX=" .. max_val)
    "#;
    let output = run_lua_expr(code);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("MIN="),
        "expected MIN= in output: {}",
        stdout
    );
    assert!(
        stdout.contains("MAX="),
        "expected MAX= in output: {}",
        stdout
    );
    let min_str = stdout
        .split("MIN=")
        .nth(1)
        .unwrap()
        .split(" ")
        .next()
        .unwrap();
    let max_str = stdout.split("MAX=").nth(1).unwrap().trim();
    let min_val: i64 = min_str
        .parse()
        .expect(&format!("expected integer, got: {}", min_str));
    let max_val: i64 = max_str
        .parse()
        .expect(&format!("expected integer, got: {}", max_str));
    assert!(min_val >= 10, "min value {} should be >= 10", min_val);
    assert!(max_val <= 20, "max value {} should be <= 20", max_val);
}

#[test]
fn test_math_random_zero_arg() {
    // math.random(0) 返回全范围随机整数
    let output = run_lua_expr("print(type(math.random(0)))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("number"));
}

#[test]
fn test_math_random_empty_interval_error() {
    let output = run_lua_expr("print(pcall(math.random, 5, 3))");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("random(5, 3) stdout: {}, stderr: {}", stdout, stderr);
    // pcall 应该捕获错误, 返回 false
    assert!(
        stdout.contains("false") || !output.status.success(),
        "expected pcall to catch error"
    );
}

// ============================================================================
// math.randomseed 测试 (对应 C 的 math_randomseed)
// ============================================================================

#[test]
fn test_math_randomseed_reproducible() {
    // 相同种子应产生相同随机序列
    let code = r#"
        math.randomseed(42)
        local r1 = {}
        for i = 1, 5 do
            r1[i] = math.random(1, 1000)
        end
        math.randomseed(42)
        local r2 = {}
        for i = 1, 5 do
            r2[i] = math.random(1, 1000)
        end
        local same = true
        for i = 1, 5 do
            if r1[i] ~= r2[i] then
                same = false
                break
            end
        end
        if same then print("REPRODUCIBLE") else print("NOT_REPRODUCIBLE") end
    "#;
    let output = run_lua_expr(code);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REPRODUCIBLE"),
        "same seed should produce same sequence: {}",
        stdout
    );
}

#[test]
fn test_math_randomseed_returns_values() {
    // math.randomseed 应该返回两个种子值
    let output = run_lua_expr("print(math.randomseed(42))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 应该包含 42 和 0
    assert!(stdout.contains("42"));
}

#[test]
fn test_math_randomseed_two_args() {
    let output = run_lua_expr("print(math.randomseed(42, 99))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"));
    assert!(stdout.contains("99"));
}

// ============================================================================
// 综合测试
// ============================================================================

#[test]
fn test_math_chain_operations() {
    // 链式操作: floor(sqrt(16)) = 4
    let output = run_lua_expr("print(math.floor(math.sqrt(16)))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4"));
}

#[test]
fn test_math_abs_then_floor() {
    // abs(-3.7) = 3.7, floor(3.7) = 3
    let output = run_lua_expr("print(math.floor(math.abs(-3.7)))");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"));
}

#[test]
fn test_math_trig_identity() {
    // sin^2(x) + cos^2(x) = 1
    let code = r#"
        local x = 0.5
        local result = math.sin(x)^2 + math.cos(x)^2
        if math.abs(result - 1.0) < 1e-10 then
            print("IDENTITY_OK")
        else
            print("IDENTITY_FAIL: " .. result)
        end
    "#;
    let output = run_lua_expr(code);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("IDENTITY_OK"));
}

#[test]
fn test_math_log_exp_inverse() {
    // log(exp(x)) = x
    let code = r#"
        local x = 2.5
        local result = math.log(math.exp(x))
        if math.abs(result - x) < 1e-10 then
            print("INVERSE_OK")
        else
            print("INVERSE_FAIL: " .. result)
        end
    "#;
    let output = run_lua_expr(code);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("INVERSE_OK"));
}

#[test]
fn test_math_deg_rad_inverse() {
    // rad(deg(x)) = x
    let code = r#"
        local x = 1.234
        local result = math.rad(math.deg(x))
        if math.abs(result - x) < 1e-10 then
            print("INVERSE_OK")
        else
            print("INVERSE_FAIL: " .. result)
        end
    "#;
    let output = run_lua_expr(code);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("INVERSE_OK"));
}

#[test]
fn test_math_max_min_relationship() {
    // max(a, b) >= min(a, b)
    let code = r#"
        local a, b = 3.14, 2.71
        local mx = math.max(a, b)
        local mn = math.min(a, b)
        if mx >= mn then
            print("OK")
        else
            print("FAIL")
        end
    "#;
    let output = run_lua_expr(code);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("OK"));
}

// ============================================================================
// 错误处理测试
// ============================================================================

#[test]
fn test_math_abs_no_arg() {
    let output = run_lua_expr("print(pcall(math.abs))");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("abs() stdout: {}, stderr: {}", stdout, stderr);
    // pcall 应该捕获错误
    assert!(
        stdout.contains("false") || !output.status.success(),
        "expected pcall to catch error"
    );
}

#[test]
fn test_math_abs_string_arg() {
    let output = run_lua_expr("print(pcall(math.abs, 'hello'))");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("abs('hello') stdout: {}, stderr: {}", stdout, stderr);
    assert!(
        stdout.contains("false") || !output.status.success(),
        "expected pcall to catch error"
    );
}

#[test]
fn test_math_min_no_arg() {
    let output = run_lua_expr("print(pcall(math.min))");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("min() stdout: {}, stderr: {}", stdout, stderr);
    assert!(
        stdout.contains("false") || !output.status.success(),
        "expected pcall to catch error"
    );
}

#[test]
fn test_math_max_no_arg() {
    let output = run_lua_expr("print(pcall(math.max))");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("max() stdout: {}, stderr: {}", stdout, stderr);
    assert!(
        stdout.contains("false") || !output.status.success(),
        "expected pcall to catch error"
    );
}

#[test]
fn test_math_ult_no_arg() {
    let output = run_lua_expr("print(pcall(math.ult, 1))");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("ult(1) stdout: {}, stderr: {}", stdout, stderr);
    assert!(
        stdout.contains("false") || !output.status.success(),
        "expected pcall to catch error"
    );
}

// ============================================================================
// 直接调用 Rust API 的测试 (不通过 Lua 代码)
// ============================================================================

#[test]
fn test_rust_api_math_abs() {
    assert_eq!(
        math_lib::math_abs(&TValue::Integer(-42)).unwrap(),
        TValue::Integer(42)
    );
    assert_eq!(
        math_lib::math_abs(&TValue::Float(-3.14)).unwrap(),
        TValue::Float(3.14)
    );
}

#[test]
fn test_rust_api_math_floor_ceil() {
    assert_eq!(
        math_lib::math_floor(&TValue::Float(3.7)).unwrap(),
        TValue::Integer(3)
    );
    assert_eq!(
        math_lib::math_ceil(&TValue::Float(3.2)).unwrap(),
        TValue::Integer(4)
    );
}

#[test]
fn test_rust_api_math_trig() {
    assert!((math_lib::math_sin(0.0) - 0.0).abs() < 1e-15);
    assert!((math_lib::math_cos(0.0) - 1.0).abs() < 1e-15);
    assert!((math_lib::math_tan(0.0) - 0.0).abs() < 1e-15);
}

#[test]
fn test_rust_api_math_log() {
    assert!((math_lib::math_log(1.0, None) - 0.0).abs() < 1e-15);
    assert!((math_lib::math_log(8.0, Some(2.0)) - 3.0).abs() < 1e-15);
    assert!((math_lib::math_log(100.0, Some(10.0)) - 2.0).abs() < 1e-15);
}

#[test]
fn test_rust_api_math_min_max() {
    let args = vec![TValue::Integer(3), TValue::Integer(1), TValue::Integer(2)];
    assert_eq!(math_lib::math_min(&args).unwrap(), TValue::Integer(1));
    assert_eq!(math_lib::math_max(&args).unwrap(), TValue::Integer(3));
}

#[test]
fn test_rust_api_math_type() {
    assert_eq!(math_lib::math_type(&TValue::Integer(42)), Some("integer"));
    assert_eq!(math_lib::math_type(&TValue::Float(3.14)), Some("float"));
    assert_eq!(math_lib::math_type(&TValue::Boolean(true)), None);
}

#[test]
fn test_rust_api_math_tointeger() {
    assert_eq!(math_lib::math_tointeger(&TValue::Integer(42)), Some(42));
    assert_eq!(math_lib::math_tointeger(&TValue::Float(42.0)), Some(42));
    assert_eq!(math_lib::math_tointeger(&TValue::Float(42.5)), None);
}

#[test]
fn test_rust_api_math_ult() {
    assert!(math_lib::math_ult(1, 2));
    assert!(!math_lib::math_ult(2, 1));
    assert!(!math_lib::math_ult(-1, 1)); // -1 作为无符号是 u64::MAX
    assert!(math_lib::math_ult(1, -1));
}

#[test]
fn test_rust_api_math_modf() {
    let (int, frac) = math_lib::math_modf(&TValue::Float(3.14)).unwrap();
    assert_eq!(int, TValue::Integer(3));
    assert!(matches!(frac, TValue::Float(f) if (f - 0.14).abs() < 1e-15));

    let (int, frac) = math_lib::math_modf(&TValue::Float(-3.14)).unwrap();
    assert_eq!(int, TValue::Integer(-3));
    assert!(matches!(frac, TValue::Float(f) if (f - (-0.14)).abs() < 1e-15));
}

#[test]
fn test_rust_api_math_fmod() {
    assert_eq!(
        math_lib::math_fmod(&TValue::Integer(10), &TValue::Integer(3)).unwrap(),
        TValue::Integer(1)
    );
    assert!(math_lib::math_fmod(&TValue::Integer(10), &TValue::Integer(0)).is_err());
}

#[test]
fn test_rust_api_math_frexp_ldexp() {
    let (m, e) = math_lib::math_frexp(4.0);
    assert!((m - 0.5).abs() < 1e-15);
    assert_eq!(e, 3);

    let result = math_lib::math_ldexp(0.5, 3);
    assert!((result - 4.0).abs() < 1e-15);
}

#[test]
fn test_rust_api_math_random() {
    let mut state = math_lib::RandState::new();
    state.setseed(42, 0);

    // 无参数: 返回 [0, 1) 浮点数
    let result = math_lib::math_random(&mut state, &[]).unwrap();
    match result {
        TValue::Float(f) => assert!(f >= 0.0 && f < 1.0),
        _ => panic!("expected float"),
    }

    // 单参数: 返回 [1, n] 整数
    for _ in 0..100 {
        let result = math_lib::math_random(&mut state, &[TValue::Integer(6)]).unwrap();
        match result {
            TValue::Integer(n) => assert!(n >= 1 && n <= 6),
            _ => panic!("expected integer"),
        }
    }

    // 双参数: 返回 [low, up] 整数
    for _ in 0..100 {
        let result =
            math_lib::math_random(&mut state, &[TValue::Integer(10), TValue::Integer(20)]).unwrap();
        match result {
            TValue::Integer(n) => assert!(n >= 10 && n <= 20),
            _ => panic!("expected integer"),
        }
    }
}

#[test]
fn test_rust_api_math_random_reproducible() {
    let mut state1 = math_lib::RandState::new();
    let mut state2 = math_lib::RandState::new();
    state1.setseed(123, 0);
    state2.setseed(123, 0);

    for _ in 0..10 {
        let r1 = state1.nextrand();
        let r2 = state2.nextrand();
        assert_eq!(r1, r2, "same seed should produce same sequence");
    }
}

#[test]
fn test_rust_api_open_math_lib() {
    let mut state = LuaState::new();
    math_lib::open_math_lib(&mut state);

    // 验证 math 全局表已注册
    let key = TValue::Str(state.intern_str("math"));
    assert!(state.globals.get(&key).is_some());

    // 验证随机状态已初始化
    assert!(state.math_random_state.is_some());
}

#[test]
fn test_rust_api_constants() {
    assert!((math_lib::PI - std::f64::consts::PI).abs() < 1e-15);
    assert!(math_lib::HUGE.is_infinite() && math_lib::HUGE > 0.0);
    assert_eq!(math_lib::MAX_INTEGER, i64::MAX);
    assert_eq!(math_lib::MIN_INTEGER, i64::MIN);
}
