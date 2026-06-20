//! 元方法 (Metamethod) 正确性测试
//!
//! 验证项目:
//! 1. op_mmbin — 算术元方法 (ADD/SUB/MUL/DIV/MOD/IDIV/POW)
//! 2. op_unm — 一元负号元方法 (__unm)
//! 3. op_bnot — 按位取反元方法 (__bnot)
//! 4. op_mmbini — 整数算术元方法 (与立即数运算)
//! 5. op_mmbink — 常量算术元方法 (与常量运算)
//! 6. try_concat_tm — 字符串拼接元方法 (__concat)
//! 7. 比较元方法 (__eq/__lt/__le)
//! 8. __len 元方法
//!
//! 对应 C 源码: ltm.cpp 中的 luaT_trybinTM, luaT_trybiniTM, luaT_trybinassocTM,
//!              luaT_tryconcatTM, luaT_callorderTM
//! 对应 Rust: tm.rs 中的 try_bin_tm, try_bini_tm, try_bin_assoc_tm,
//!            try_concat_tm, call_order_tm

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
// 1. op_mmbin — 算术元方法 (__add/__sub/__mul/__div/__mod/__idiv/__pow)
// 对应 C: luaT_trybinTM(L, s2v(ra), rb, result, tm)
// 对应 Rust: try_bin_tm(state, &p1, &p2, result, tm)
// ============================================================================

/// __add 元方法: 表 + 数值
#[test]
fn test_mmbin_add() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__add = function(a, b) return 100 end})\n\
         print(t + 5)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("100"), "__add 应返回 100, got: {}", stdout);
}

/// __sub 元方法: 表 - 数值
#[test]
fn test_mmbin_sub() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__sub = function(a, b) return 42 end})\n\
         print(t - 5)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"), "__sub 应返回 42, got: {}", stdout);
}

/// __mul 元方法: 表 * 数值
#[test]
fn test_mmbin_mul() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__mul = function(a, b) return 7 end})\n\
         print(t * 3)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("7"), "__mul 应返回 7, got: {}", stdout);
}

/// __div 元方法: 表 / 数值
#[test]
fn test_mmbin_div() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__div = function(a, b) return 10 end})\n\
         print(t / 2)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("10"), "__div 应返回 10, got: {}", stdout);
}

/// __mod 元方法: 表 % 数值
#[test]
fn test_mmbin_mod() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__mod = function(a, b) return 3 end})\n\
         print(t % 5)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"), "__mod 应返回 3, got: {}", stdout);
}

/// __idiv 元方法: 表 // 数值
#[test]
fn test_mmbin_idiv() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__idiv = function(a, b) return 9 end})\n\
         print(t // 2)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("9"), "__idiv 应返回 9, got: {}", stdout);
}

/// __pow 元方法: 表 ^ 数值
#[test]
fn test_mmbin_pow() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__pow = function(a, b) return 8 end})\n\
         print(t ^ 2)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("8"), "__pow 应返回 8, got: {}", stdout);
}

/// __add 元方法: 数值 + 表 (翻转参数)
#[test]
fn test_mmbin_add_flipped() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__add = function(a, b) return 200 end})\n\
         print(5 + t)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("200"), "5 + t 应返回 200, got: {}", stdout);
}

// ============================================================================
// 2. op_unm — 一元负号元方法 (__unm)
// 对应 C: luaT_trybinTM(L, rb, rb, ra, TM_UNM)
// 对应 Rust: try_bin_tm(state, &v, &v, a, TagMethod::Unm)
// ============================================================================

/// __unm 元方法: -表
#[test]
fn test_unm_metamethod() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__unm = function(a) return -999 end})\n\
         print(-t)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-999"), "__unm 应返回 -999, got: {}", stdout);
}

/// 一元负号: 整数直接取反 (不调用元方法)
#[test]
fn test_unm_integer() {
    let output = run_lua(&["-e", "print(-42)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-42"), "-42 应为 -42, got: {}", stdout);
}

/// 一元负号: 浮点数直接取反 (不调用元方法)
#[test]
fn test_unm_float() {
    let output = run_lua(&["-e", "print(-3.14)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-3.14"), "-3.14 应为 -3.14, got: {}", stdout);
}

// ============================================================================
// 3. op_bnot — 按位取反元方法 (__bnot)
// 对应 C: luaT_trybinTM(L, rb, rb, ra, TM_BNOT)
// 对应 Rust: try_bin_tm(state, &v, &v, a, TagMethod::BNot)
// ============================================================================

/// __bnot 元方法: ~表
#[test]
fn test_bnot_metamethod() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__bnot = function(a) return 123 end})\n\
         print(~t)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("123"), "__bnot 应返回 123, got: {}", stdout);
}

/// 按位取反: 整数直接计算 (不调用元方法)
#[test]
fn test_bnot_integer() {
    let output = run_lua(&["-e", "print(~0)"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // ~0 = -1 in two's complement
    assert!(stdout.contains("-1"), "~0 应为 -1, got: {}", stdout);
}

// ============================================================================
// 4. 按位运算元方法 (__band/__bor/__bxor/__shl/__shr)
// ============================================================================

/// __band 元方法
#[test]
fn test_mmbin_band() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__band = function(a, b) return 1 end})\n\
         print(t & 3)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1"), "__band 应返回 1, got: {}", stdout);
}

/// __bor 元方法
#[test]
fn test_mmbin_bor() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__bor = function(a, b) return 2 end})\n\
         print(t | 3)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2"), "__bor 应返回 2, got: {}", stdout);
}

/// __bxor 元方法
#[test]
fn test_mmbin_bxor() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__bxor = function(a, b) return 4 end})\n\
         print(t ~ 3)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4"), "__bxor 应返回 4, got: {}", stdout);
}

/// __shl 元方法
#[test]
fn test_mmbin_shl() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__shl = function(a, b) return 16 end})\n\
         print(t << 2)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("16"), "__shl 应返回 16, got: {}", stdout);
}

/// __shr 元方法
#[test]
fn test_mmbin_shr() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__shr = function(a, b) return 8 end})\n\
         print(t >> 2)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("8"), "__shr 应返回 8, got: {}", stdout);
}

// ============================================================================
// 5. try_concat_tm — 字符串拼接元方法 (__concat)
// 对应 C: luaT_tryconcatTM
// 对应 Rust: try_concat_tm
// ============================================================================

/// __concat 元方法: 表 .. 字符串
#[test]
fn test_concat_metamethod() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__concat = function(a, b) return 'concat_result' end})\n\
         print(t .. 'world')",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("concat_result"), "__concat 应返回 concat_result, got: {}", stdout);
}

/// __concat 元方法: 字符串 .. 表
#[test]
fn test_concat_metamethod_flipped() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__concat = function(a, b) return 'flipped' end})\n\
         print('hello' .. t)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("flipped"), "__concat 翻转应返回 flipped, got: {}", stdout);
}

/// 字符串拼接: 不调用元方法
#[test]
fn test_concat_strings() {
    let output = run_lua(&["-e", "print('hello' .. ' ' .. 'world')"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello world"), "拼接应为 'hello world', got: {}", stdout);
}

// ============================================================================
// 6. 比较元方法 (__eq/__lt/__le)
// 对应 C: luaT_callorderTM
// 对应 Rust: call_order_tm
// ============================================================================

/// __eq 元方法: 表 == 表
#[test]
fn test_eq_metamethod() {
    let output = run_lua(&[
        "-e",
        "local mt = {__eq = function(a, b) return true end}\n\
         local t1 = setmetatable({}, mt)\n\
         local t2 = setmetatable({}, mt)\n\
         print(t1 == t2)\n\
         print(t1 == t1)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 2, "期望至少 2 行输出, got: {}", stdout);
    assert!(lines[0].contains("true"), "t1 == t2 应为 true, got: {}", lines[0]);
    assert!(lines[1].contains("true"), "t1 == t1 应为 true, got: {}", lines[1]);
}

/// __lt 元方法: 表 < 表
#[test]
fn test_lt_metamethod() {
    let output = run_lua(&[
        "-e",
        "local mt = {__lt = function(a, b) return true end}\n\
         local t1 = setmetatable({}, mt)\n\
         local t2 = setmetatable({}, mt)\n\
         print(t1 < t2)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("true"), "__lt 应返回 true, got: {}", stdout);
}

/// __le 元方法: 表 <= 表
#[test]
fn test_le_metamethod() {
    let output = run_lua(&[
        "-e",
        "local mt = {__le = function(a, b) return false end}\n\
         local t1 = setmetatable({}, mt)\n\
         local t2 = setmetatable({}, mt)\n\
         print(t1 <= t2)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("false"), "__le 应返回 false, got: {}", stdout);
}

// ============================================================================
// 7. __len 元方法
// 对应 C: luaV_objlen
// 对应 Rust: obj_len
// ============================================================================

/// __len 元方法: #表
#[test]
fn test_len_metamethod() {
    let output = run_lua(&[
        "-e",
        "local t = setmetatable({}, {__len = function(a) return 42 end})\n\
         print(#t)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"), "__len 应返回 42, got: {}", stdout);
}

/// #字符串: 不调用元方法，返回字符串长度
#[test]
fn test_len_string() {
    let output = run_lua(&["-e", "print(#'hello')"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("5"), "#'hello' 应为 5, got: {}", stdout);
}

/// #表: 不调用元方法，返回表数组部分长度
#[test]
fn test_len_table() {
    let output = run_lua(&["-e", "print(#{10, 20, 30})"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"), "#表 应为 3, got: {}", stdout);
}

// ============================================================================
// 8. 错误处理 — 无元方法时的错误
// 对应 C: luaT_trybinTM 失败时调用 luaG_opinterror
// ============================================================================

/// 无元方法时: 表 + 表 应报错
#[test]
fn test_mmbin_no_metamethod_error() {
    let output = run_lua(&[
        "-e",
        "local t = {}\n\
         local ok, err = pcall(function() return t + t end)\n\
         print(ok)\n\
         print(err)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 2, "期望至少 2 行输出, got: {}", stdout);
    assert!(lines[0].contains("false"), "pcall 应返回 false, got: {}", lines[0]);
    assert!(lines[1].to_lowercase().contains("arithmetic") || lines[1].contains("perform"),
           "错误消息应包含 arithmetic/perform, got: {}", lines[1]);
}

/// 无元方法时: -表 应报错
#[test]
fn test_unm_no_metamethod_error() {
    let output = run_lua(&[
        "-e",
        "local t = {}\n\
         local ok, err = pcall(function() return -t end)\n\
         print(ok)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 1, "期望至少 1 行输出, got: {}", stdout);
    assert!(lines[0].contains("false"), "pcall 应返回 false, got: {}", lines[0]);
}

/// 无元方法时: ~表 应报错
#[test]
fn test_bnot_no_metamethod_error() {
    let output = run_lua(&[
        "-e",
        "local t = {}\n\
         local ok, err = pcall(function() return ~t end)\n\
         print(ok)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 1, "期望至少 1 行输出, got: {}", stdout);
    assert!(lines[0].contains("false"), "pcall 应返回 false, got: {}", lines[0]);
}

/// 无元方法时: 表 .. 表 应报错
#[test]
fn test_concat_no_metamethod_error() {
    let output = run_lua(&[
        "-e",
        "local t = {}\n\
         local ok, err = pcall(function() return t .. t end)\n\
         print(ok)",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 1, "期望至少 1 行输出, got: {}", stdout);
    assert!(lines[0].contains("false"), "pcall 应返回 false, got: {}", lines[0]);
}
