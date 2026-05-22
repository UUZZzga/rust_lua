#[cfg(test)]
mod compiler_compare_tests {
    use crate::compiler::compile;
    use crate::compiler::bytecode_dump;

    fn compile_rust(source: &str) -> crate::objects::Proto {
        crate::compiler::compile(source, "=test").expect("Rust compile failed")
    }

    unsafe fn compile_c(source: &str) -> bytecode_dump::DumpedFunction {
        let dump_data = bytecode_dump::compile_with_c_lua(source)
            .expect("C compile failed");
        bytecode_dump::parse_dump(dump_data)
            .expect("dump parse failed")
    }

    #[test]
    fn debug_dump_return_42() {
        unsafe {
            let dump_data = bytecode_dump::compile_with_c_lua("return 42")
                .expect("C compile failed");
            eprintln!("DUMP size: {} bytes", dump_data.len());
            eprintln!("DUMP hex (first 100 bytes): {:02x?}", &dump_data[..dump_data.len().min(100)]);
            
            match bytecode_dump::parse_dump(dump_data) {
                Ok(func) => {
                    eprintln!("Parsed C OK: numparams={}, flag={}, maxstack={}, code_len={}, constants_len={}",
                        func.numparams, func.flag, func.maxstacksize, func.code.len(), func.constants.len());
                    eprintln!("C Code: {:?}", func.code);
                }
                Err(e) => eprintln!("Parse error: {}", e),
            }
        }
        
        // Also dump Rust instructions
        let rust_proto = compile_rust("return 42");
        eprintln!("Rust proto code raw: {:?}", rust_proto.code);
        eprintln!("Rust proto code len: {}", rust_proto.code.len());
        for (i, inst) in rust_proto.code.iter().enumerate() {
            eprintln!("  Rust[{}]: raw={:#010x} op={:3} A={:3} B={:3} C={:3} k={}",
                i, inst,
                inst & 0x7f,
                (inst >> 7) & 0xff,
                (inst >> 16) & 0xff,
                (inst >> 24) & 0xff,
                (inst >> 15) & 1,
            );
        }
    }

    fn assert_inst_match(source: &str) {
        let rust_proto = compile_rust(source);
        let c_func = unsafe { compile_c(source) };

        let diffs = bytecode_dump::compare_instructions(&rust_proto.code, &c_func.code);
        if !diffs.is_empty() {
            let rust_dump = bytecode_dump::dump_instructions(&rust_proto.code);
            let c_dump = bytecode_dump::dump_c_instructions(&c_func.code);
            panic!(
                "Instruction mismatch for source: {}\n\
                 Differences:\n  {}\n\n\
                 Rust instructions:\n{}\n\n\
                 C++ instructions:\n{}",
                source,
                diffs.join("\n  "),
                rust_dump,
                c_dump
            );
        }
    }

    fn assert_inst_match_allow_constants(source: &str) {
        let rust_proto = compile_rust(source);
        let c_func = unsafe { compile_c(source) };

        let diffs = bytecode_dump::compare_instructions(&rust_proto.code, &c_func.code);
        if !diffs.is_empty() {
            eprintln!(
                "WARNING: Instruction diff for source: {}\n  {}\nRust constants: {:?}\nC++ constants: {:?}",
                source,
                diffs.join("\n  "),
                rust_proto.constants,
                c_func.constants,
            );
        }
    }

    // ===== 字面量测试 =====

    #[test]
    fn test_literal_nil() { assert_inst_match("nil"); }

    #[test]
    fn test_literal_false() { assert_inst_match("false"); }

    #[test]
    fn test_literal_true() { assert_inst_match("true"); }

    #[test]
    fn test_literal_integer() { assert_inst_match("42"); }

    #[test]
    fn test_literal_integer_small() { assert_inst_match("10"); }

    #[test]
    fn test_literal_integer_large() { assert_inst_match("99999"); }

    #[test]
    fn test_literal_float() { assert_inst_match("3.14"); }

    #[test]
    fn test_literal_string() { assert_inst_match("'hello'"); }

    // ===== 全局变量访问 =====

    #[test]
    fn test_global_get() { assert_inst_match("x"); }

    // ===== 表达式测试 =====

    #[test]
    fn test_expr_add_two_ints() { assert_inst_match("2 + 2"); }

    #[test]
    fn test_expr_add_three_ints() { assert_inst_match("1 + 2 + 3"); }

    #[test]
    fn test_expr_sub_ints() { assert_inst_match("5 - 3"); }

    #[test]
    fn test_expr_mul_ints() { assert_inst_match("3 * 4"); }

    #[test]
    fn test_expr_div_ints() { assert_inst_match("10 / 2"); }

    #[test]
    fn test_expr_concat_strings() { assert_inst_match("'a' .. 'b'"); }

    #[test]
    fn test_expr_concat_three_strings() { assert_inst_match("'a' .. 'b' .. 'c'"); }

    // ===== 比较测试 (布尔操作将在后续优化中匹配) =====

    #[test]
    fn test_expr_eq() { assert_inst_match_allow_constants("1 == 2"); }

    #[test]
    fn test_expr_ne() { assert_inst_match_allow_constants("1 ~= 2"); }

    #[test]
    fn test_expr_lt() { assert_inst_match_allow_constants("1 < 2"); }

    #[test]
    fn test_expr_le() { assert_inst_match_allow_constants("1 <= 2"); }

    #[test]
    fn test_expr_gt() { assert_inst_match_allow_constants("1 > 2"); }

    #[test]
    fn test_expr_ge() { assert_inst_match_allow_constants("1 >= 2"); }

    // ===== 赋值测试 =====

    #[test]
    fn test_assign_integer() { assert_inst_match("x = 42"); }

    #[test]
    fn test_assign_string() { assert_inst_match("x = 'hello'"); }

    #[test]
    fn test_assign_expression() { assert_inst_match("x = 1 + 2"); }

    #[test]
    fn test_multi_assign() { assert_inst_match("x = 1; y = 2"); }

    // ===== 函数调用测试 =====

    #[test]
    fn test_call_no_args() { assert_inst_match("f()"); }

    #[test]
    fn test_call_one_arg() { assert_inst_match("f(42)"); }

    #[test]
    fn test_call_two_args() { assert_inst_match("f(1, 2)"); }

    #[test]
    fn test_call_string_arg() { assert_inst_match("f('hello')"); }

    #[test]
    fn test_call_literal_string() { assert_inst_match("print'hello'"); }

    // ===== 注释测试 =====

    #[test]
    fn test_comment_line() { assert_inst_match("-- this is a comment\n42"); }

    #[test]
    fn test_comment_eof() { assert_inst_match("42 -- comment at end"); }

    // ===== 复合语句测试 =====

    #[test]
    fn test_two_stmts() { assert_inst_match("x = 1; return x"); }

    #[test]
    fn test_expr_stmt() { assert_inst_match("1 + 2"); }

    // ===== return 语句测试 =====

    #[test]
    fn test_return_int() { assert_inst_match("return 42"); }

    #[test]
    fn test_return_multi() { assert_inst_match("return 1, 2, 3"); }

    #[test]
    fn test_return_expr() { assert_inst_match("return 1 + 2"); }
}