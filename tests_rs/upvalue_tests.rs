//! 上值 (Upvalue) 与闭包 (Closure) 正确性测试
//!
//! 验证项目:
//! 1. 闭包共享上值 — 多个闭包共享同一个上值，一个修改另一个可见
//! 2. 上值生命周期 — 外层函数返回后，内层闭包仍能正确访问上值
//! 3. 计数器模式 — 经典 counter 闭包，验证 Open→Closed 上值转换
//! 4. 嵌套闭包 — 多层嵌套的闭包正确捕获上值
//! 5. SETUPVAL/GETUPVAL — 上值读写正确性

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
// 1. 计数器模式 — 验证 Open→Closed 上值转换
// ============================================================================

/// 经典 counter 闭包: n=n+1 验证上值读写正确
/// 对应 C 的 luaF_findupval + close 流程
#[test]
fn test_counter_basic() {
    let output = run_lua(&[
        "-e",
        "function counter() local n=0; return function() n=n+1; return n end end\n\
         c=counter()\n\
         print(c(), c(), c())",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1"), "第一次调用应返回 1, got: {}", stdout);
    assert!(stdout.contains("2"), "第二次调用应返回 2, got: {}", stdout);
    assert!(stdout.contains("3"), "第三次调用应返回 3, got: {}", stdout);
}

/// 多个计数器独立 — 每个闭包有自己独立的上值
#[test]
fn test_counter_independent() {
    let output = run_lua(&[
        "-e",
        "function counter() local n=0; return function() n=n+1; return n end end\n\
         a=counter()\n\
         b=counter()\n\
         print(a(), a(), b(), a(), b())",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // a: 1, 2, 3; b: 1, 2
    assert!(stdout.contains("1\t2\t1\t3\t2"), "期望 1 2 1 3 2, got: {}", stdout);
}

// ============================================================================
// 2. 闭包共享上值 — 多个闭包共享同一个上值
// ============================================================================

/// 两个闭包共享同一个上值: 一个写，一个读
#[test]
fn test_shared_upvalue() {
    let output = run_lua(&[
        "-e",
        "function make_pair()\n\
         \x20 local shared = 0\n\
         \x20 return function() shared = shared + 10; return shared end,\n\
         \x20        function() return shared end\n\
         end\n\
         inc, get = make_pair()\n\
         print(get())\n\
         print(inc())\n\
         print(get())\n\
         print(inc())\n\
         print(get())",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // get()=0, inc()=10, get()=10, inc()=20, get()=20
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 5, "期望至少 5 行输出, got: {}", stdout);
    assert!(lines[0].contains("0"), "第一次 get 应为 0, got: {}", lines[0]);
    assert!(lines[1].contains("10"), "第一次 inc 应为 10, got: {}", lines[1]);
    assert!(lines[2].contains("10"), "第二次 get 应为 10, got: {}", lines[2]);
    assert!(lines[3].contains("20"), "第二次 inc 应为 20, got: {}", lines[3]);
    assert!(lines[4].contains("20"), "第三次 get 应为 20, got: {}", lines[4]);
}

// ============================================================================
// 3. 嵌套闭包 — 多层嵌套的闭包正确捕获上值
// ============================================================================

/// 两层嵌套闭包: 外层函数返回后，中层和内层仍能正确访问上值
#[test]
fn test_nested_closure() {
    let output = run_lua(&[
        "-e",
        "function outer()\n\
         \x20 local x = 1\n\
         \x20 local function middle()\n\
         \x20   local y = 2\n\
         \x20   return function() return x + y end\n\
         \x20 end\n\
         \x20 return middle\n\
         end\n\
         m = outer()\n\
         inner = m()\n\
         print(inner())",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3"), "期望 3 (1+2), got: {}", stdout);
}

/// 三层嵌套闭包: 每层都修改上值
#[test]
fn test_triple_nested_closure() {
    let output = run_lua(&[
        "-e",
        "function f1()\n\
         \x20 local a = 1\n\
         \x20 local function f2()\n\
         \x20   local b = 2\n\
         \x20   local function f3()\n\
         \x20     local c = 3\n\
         \x20     return function() a=a+1; b=b+1; c=c+1; return a*100+b*10+c end\n\
         \x20   end\n\
         \x20   return f3\n\
         \x20 end\n\
         \x20 return f2\n\
         end\n\
         f2 = f1()\n\
         f3 = f2()\n\
         g = f3()\n\
         print(g())\n\
         print(g())\n\
         print(g())",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 第一次: a=2,b=3,c=4 → 234
    // 第二次: a=3,b=4,c=5 → 345
    // 第三次: a=4,b=5,c=6 → 456
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 3, "期望至少 3 行输出, got: {}", stdout);
    assert!(lines[0].trim() == "234", "第一次应为 234, got: {}", lines[0]);
    assert!(lines[1].trim() == "345", "第二次应为 345, got: {}", lines[1]);
    assert!(lines[2].trim() == "456", "第三次应为 456, got: {}", lines[2]);
}

// ============================================================================
// 4. 上值生命周期 — 外层函数返回后上值仍有效
// ============================================================================

/// 外层函数返回后，上值从 Open 变为 Closed，内层闭包仍能访问
#[test]
fn test_upvalue_closed_after_return() {
    let output = run_lua(&[
        "-e",
        "function make_adder(n)\n\
         \x20 return function(x) return x + n end\n\
         end\n\
         add5 = make_adder(5)\n\
         add10 = make_adder(10)\n\
         print(add5(3))\n\
         print(add10(3))\n\
         print(add5(7))",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 3, "期望至少 3 行输出, got: {}", stdout);
    assert!(lines[0].trim() == "8", "add5(3) 应为 8, got: {}", lines[0]);
    assert!(lines[1].trim() == "13", "add10(3) 应为 13, got: {}", lines[1]);
    assert!(lines[2].trim() == "12", "add5(7) 应为 12, got: {}", lines[2]);
}

/// 上值为表时，闭包修改表内容，其他闭包可见
#[test]
fn test_upvalue_table_shared() {
    let output = run_lua(&[
        "-e",
        "function make_table_ops()\n\
         \x20 local t = {}\n\
         \x20 return function(k, v) t[k] = v end,\n\
         \x20        function(k) return t[k] end\n\
         end\n\
         set, get = make_table_ops()\n\
         set('x', 100)\n\
         set('y', 200)\n\
         print(get('x'))\n\
         print(get('y'))\n\
         print(get('z'))",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 3, "期望至少 3 行输出, got: {}", stdout);
    assert!(lines[0].trim() == "100", "get('x') 应为 100, got: {}", lines[0]);
    assert!(lines[1].trim() == "200", "get('y') 应为 200, got: {}", lines[1]);
    assert!(lines[2].trim() == "nil", "get('z') 应为 nil, got: {}", lines[2]);
}

// ============================================================================
// 5. SETUPVAL/GETUPVAL — 上值读写正确性
// ============================================================================

/// SETUPVAL 修改上值后，再次调用闭包时应看到修改后的值
#[test]
fn test_setupval_persists() {
    let output = run_lua(&[
        "-e",
        "function make_getter_setter()\n\
         \x20 local val = 0\n\
         \x20 local function get() return val end\n\
         \x20 local function set(v) val = v end\n\
         \x20 return get, set\n\
         end\n\
         get, set = make_getter_setter()\n\
         print(get())\n\
         set(42)\n\
         print(get())\n\
         set(99)\n\
         print(get())",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 3, "期望至少 3 行输出, got: {}", stdout);
    assert!(lines[0].trim() == "0", "初始值应为 0, got: {}", lines[0]);
    assert!(lines[1].trim() == "42", "set(42) 后应为 42, got: {}", lines[1]);
    assert!(lines[2].trim() == "99", "set(99) 后应为 99, got: {}", lines[2]);
}

/// 上值为字符串，修改后正确反映
#[test]
fn test_upvalue_string() {
    let output = run_lua(&[
        "-e",
        "function make_greeter()\n\
         \x20 local name = 'world'\n\
         \x20 return function() return 'hello, ' .. name end,\n\
         \x20        function(n) name = n end\n\
         end\n\
         greet, setname = make_greeter()\n\
         print(greet())\n\
         setname('lua')\n\
         print(greet())",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.len() >= 2, "期望至少 2 行输出, got: {}", stdout);
    assert!(lines[0].contains("hello, world"), "期望 'hello, world', got: {}", lines[0]);
    assert!(lines[1].contains("hello, lua"), "期望 'hello, lua', got: {}", lines[1]);
}
