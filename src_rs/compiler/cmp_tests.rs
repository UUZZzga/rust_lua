#[cfg(test)]
mod compiler_compare_tests {
    use crate::compiler::bytecode_dump;

    fn compile_rust(source: &str, name: Option<&str>) -> crate::objects::Proto {
        crate::compiler::compile(source, name.unwrap_or("=test")).expect("Rust compile failed")
    }

    unsafe fn compile_c(source: &str) -> bytecode_dump::DumpedFunction {
        let dump_data =
            bytecode_dump::compile_with_c_lua(source.as_bytes()).expect("C compile failed");
        bytecode_dump::parse_dump(dump_data).expect("dump parse failed")
    }

    fn assert_inst_match(source: &str, name: Option<&str>) {
        let rust_proto = compile_rust(source, name);
        let c_func = unsafe { compile_c(source) };

        let diffs = bytecode_dump::compare_instructions(&rust_proto.code, &c_func.code);
        if !diffs.is_empty() {
            let rust_dump = bytecode_dump::dump_instructions(&rust_proto.code);
            let c_dump = bytecode_dump::dump_c_instructions(&c_func.code, &c_func.constants);
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
        let rust_proto = compile_rust(source, None);
        let c_func = unsafe { compile_c(source) };

        let diffs = bytecode_dump::compare_instructions(&rust_proto.code, &c_func.code);
        let filtered: Vec<String> = diffs
            .into_iter()
            .filter(|d| {
                !d.contains("constant index mismatch") && !d.contains("constant type mismatch")
            })
            .collect();
        if !filtered.is_empty() {
            let rust_dump = bytecode_dump::dump_instructions(&rust_proto.code);
            let c_dump = bytecode_dump::dump_c_instructions(&c_func.code, &c_func.constants);
            panic!(
                "Instruction mismatch for source: {}\n\
                 Differences:\n  {}\n\n\
                 Rust instructions:\n{}\n\n\
                 C++ instructions:\n{}",
                source,
                filtered.join("\n  "),
                rust_dump,
                c_dump
            );
        }
    }

    #[test]
    fn debug_dump_return_42() {
        unsafe {
            let dump_data =
                bytecode_dump::compile_with_c_lua(b"return 42").expect("C compile failed");
            eprintln!("DUMP size: {} bytes", dump_data.len());
            eprintln!(
                "DUMP hex (first 100 bytes): {:02x?}",
                &dump_data[..dump_data.len().min(100)]
            );

            match bytecode_dump::parse_dump(dump_data) {
                Ok(func) => {
                    eprintln!("Parsed C OK: numparams={}, flag={}, maxstack={}, code_len={}, constants_len={}",
                        func.numparams, func.flag, func.maxstacksize, func.code.len(), func.constants.len());
                    eprintln!("C Code: {:?}", func.code);
                }
                Err(e) => eprintln!("Parse error: {}", e),
            }
        }

        let rust_proto = compile_rust("return 42", None);
        eprintln!("Rust proto code raw: {:?}", rust_proto.code);
        eprintln!("Rust proto code len: {}", rust_proto.code.len());
        for (i, inst) in rust_proto.code.iter().enumerate() {
            eprintln!(
                "  Rust[{}]: raw={:#010x} op={:3} A={:3} B={:3} C={:3} k={}",
                i,
                inst,
                inst & 0x7f,
                (inst >> 7) & 0xff,
                (inst >> 16) & 0xff,
                (inst >> 24) & 0xff,
                (inst >> 15) & 1,
            );
        }
    }

    // ===== 字面量 return 测试 (C++ Lua 不支持裸表达式语句) =====

    #[test]
    fn test_literal_nil() {
        assert_inst_match("return nil", None);
    }

    #[test]
    fn test_literal_false() {
        assert_inst_match("return false", None);
    }

    #[test]
    fn test_literal_true() {
        assert_inst_match("return true", None);
    }

    #[test]
    fn test_literal_integer() {
        assert_inst_match("return 42", None);
    }

    #[test]
    fn test_literal_integer_small() {
        assert_inst_match("return 10", None);
    }

    #[test]
    fn test_literal_integer_large() {
        assert_inst_match("return 99999", None);
    }

    #[test]
    fn test_literal_float() {
        assert_inst_match("return 3.14", None);
    }

    #[test]
    fn test_literal_string() {
        assert_inst_match("return 'hello'", None);
    }

    // ===== 全局变量 return 测试 =====

    #[test]
    fn test_global_get() {
        assert_inst_match("return x", None);
    }

    // ===== 表达式 return 测试 =====

    #[test]
    fn test_expr_add_two_ints() {
        assert_inst_match("return 2 + 2", None);
    }

    #[test]
    fn test_expr_add_three_ints() {
        assert_inst_match("return 1 + 2 + 3", None);
    }

    #[test]
    fn test_expr_sub_ints() {
        assert_inst_match("return 5 - 3", None);
    }

    #[test]
    fn test_expr_mul_ints() {
        assert_inst_match("return 3 * 4", None);
    }

    #[test]
    fn test_expr_div_ints() {
        assert_inst_match("return 10 / 2", None);
    }

    #[test]
    fn test_expr_concat_strings() {
        assert_inst_match("return 'a' .. 'b'", None);
    }

    #[test]
    fn test_expr_concat_three_strings() {
        assert_inst_match("return 'a' .. 'b' .. 'c'", None);
    }

    // ===== 比较测试 =====

    #[test]
    fn test_expr_eq() {
        assert_inst_match_allow_constants("return 1 == 2");
    }

    #[test]
    fn test_expr_ne() {
        assert_inst_match_allow_constants("return 1 ~= 2");
    }

    #[test]
    fn test_expr_lt() {
        assert_inst_match_allow_constants("return 1 < 2");
    }

    #[test]
    fn test_expr_le() {
        assert_inst_match_allow_constants("return 1 <= 2");
    }

    #[test]
    fn test_expr_gt() {
        assert_inst_match_allow_constants("return 1 > 2");
    }

    #[test]
    fn test_expr_ge() {
        assert_inst_match_allow_constants("return 1 >= 2");
    }

    // ===== 赋值测试 (Rust 编译器暂不支持，编译时需先支持) =====

    #[test]
    fn test_assign_integer() {
        assert_inst_match("x = 42", None);
    }

    #[test]
    fn test_assign_string() {
        assert_inst_match("x = 'hello'", None);
    }

    #[test]
    fn test_assign_expression() {
        assert_inst_match("x = 1 + 2", None);
    }

    #[test]
    fn test_multi_assign() {
        assert_inst_match("x = 1; y = 2", None);
    }

    // ===== 函数调用测试 (C++ Lua 允许函数调用作为表达式语句) =====

    #[test]
    fn test_call_no_args() {
        assert_inst_match("f()", None);
    }

    #[test]
    fn test_call_one_arg() {
        assert_inst_match("f(42)", None);
    }

    #[test]
    fn test_call_two_args() {
        assert_inst_match("f(1, 2)", None);
    }

    #[test]
    fn test_call_string_arg() {
        assert_inst_match("f('hello')", None);
    }

    #[test]
    fn test_call_literal_string() {
        assert_inst_match("print'hello'", None);
    }

    // ===== 注释测试 =====

    #[test]
    fn test_comment_line() {
        assert_inst_match("return 42 -- comment", None);
    }

    #[test]
    fn test_comment_eof() {
        assert_inst_match("-- comment\nreturn 42", None);
    }

    // ===== 复合语句测试 =====

    #[test]
    fn test_two_stmts() {
        assert_inst_match("x = 1; return x", None);
    }

    #[test]
    fn test_expr_stmt() {
        assert_inst_match("return 1 + 2", None);
    }

    // ===== return 语句测试 =====

    #[test]
    fn test_return_int() {
        assert_inst_match("return 42", None);
    }

    #[test]
    fn test_return_multi() {
        assert_inst_match("return 1, 2, 3", None);
    }

    #[test]
    fn test_if_true() {
        assert_inst_match("if true then return 1 end", None);
    }

    #[test]
    fn test_if_false() {
        assert_inst_match("if false then return 1 end", None);
    }

    #[test]
    fn test_if_true_else() {
        assert_inst_match("if true then return 1 else return 2 end", None);
    }

    #[test]
    fn test_if_true_elseif_else() {
        assert_inst_match(
            "if true then return 1 elseif true then return 2 else return 3 end",
            None,
        );
    }

    #[test]
    fn test_for() {
        assert_inst_match("for i = 1, 5 do return 1 end", None);
    }

    #[test]
    fn test_for_in() {
        assert_inst_match("for k,v,w in a do end", None);
    }

    #[test]
    fn test_return_expr() {
        assert_inst_match("return 1 + 2", None);
    }

    // #[test]
    // fn test_big_lua() {
    //     assert_inst_match_file("big.lua");
    // }

    // #[test]
    // fn test_focus_lua() {
    //     assert_inst_match_file("test_focus.lua");
    // }

    fn assert_inst_match_file(name: &str) {
        assert_inst_match(get_lua_script(name).as_str(), Some(name));
    }

    fn get_lua_script(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests_lua/");
        path.push(name);
        std::fs::read_to_string(path.as_path()).unwrap()
    }
}
