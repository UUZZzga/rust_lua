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
    fn test_assign_local() {
        assert_inst_match("local a = 1 + 2\nlocal b = a * 3\nlocal c = a - 1", None);
    }

    #[test]
    fn test_assign_local2() {
        assert_inst_match("local a = 1 + 2\nlocal d = a + 5\nlocal e = a & 2", None);
    }

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
    fn test_return_expr() {
        assert_inst_match("return 1 + 2", None);
    }

    #[test]
    fn test_return_expr_complex1() {
        assert_inst_match("return 2^3^2 == 2^(3^2)", None);
    }

    #[test]
    fn test_return_expr_complex2() {
        assert_inst_match("return 2^3*4 == (2^3)*4", None);
    }

    #[test]
    fn test_return_expr_complex3() {
        assert_inst_match("return 2.0^-2 == 1/4 and -2^- -2 == - - -4", None);
    }

    #[test]
    fn test_return_expr_complex4() {
        assert_inst_match("return not nil and 2 and not(2>3 or 3<2)", None);
    }

    #[test]
    fn test_return_expr_complex5() {
        assert_inst_match("return -3-1-5 == 0+0-9", None);
    }

    #[test]
    fn test_return_expr_complex6() {
        assert_inst_match("return -2^2 == -4 and (-2)^2 == 4 and 2*2-3-1 == 0", None);
    }

    #[test]
    fn test_return_expr_complex7() {
        assert_inst_match("return -3%5 == 2 and -3+5 == 2", None);
    }

    #[test]
    fn test_return_expr_complex8() {
        assert_inst_match("return 2*1+3/3 == 3 and 1+2 .. 3*1 == \"33\"", None);
    }

    #[test]
    fn test_return_expr_complex9() {
        assert_inst_match("return not(2+1 > 3*1) and \"a\"..\"b\" > \"a\"", None);
    }

    #[test]
    fn test_return_expr_complex10() {
        assert_inst_match("return 0xF0 | 0xCC ~ 0xAA & 0xFD == 0xF4", None);
    }

    #[test]
    fn test_return_expr_complex11() {
        assert_inst_match("return 0xFD & 0xAA ~ 0xCC | 0xF0 == 0xF4", None);
    }

    #[test]
    fn test_return_expr_complex12() {
        assert_inst_match("return 0xF0 & 0x0F + 1 == 0x10", None);
    }

    #[test]
    fn test_return_expr_complex13() {
        assert_inst_match("return 3^4//2^3//5 == 2", None);
    }

    #[test]
    fn test_return_expr_complex14() {
        assert_inst_match("return -3+4*5//2^3^2//9+4%10/3 == (-3)+(((4*5)//(2^(3^2)))//9)+((4%10)/3)", None);
    }

    #[test]
    fn test_return_expr_complex15() {
        assert_inst_match("return not ((true or false) and nil)", None);
    }

    #[test]
    fn test_return_expr_complex16() {
        assert_inst_match("return true or false  and nil", None);
    }

    #[test]
    fn test_return_expr_complex17() {
        assert_inst_match("return (((1 or false) and true) or false) == true", None);
    }

    #[test]
    fn test_return_expr_complex18() {
        assert_inst_match("return (((nil and true) or false) and true) == false", None);
    }

    #[test]
    fn test_return_expr_complex19() {
        assert_inst_match("return -(1 or 2) == -1 and (1 and 2)+(-1.25 or -4) == 0.75", None);
    }

    #[test]
    fn test_return_expr_complex20() {
        assert_inst_match("local x, y = 1, 2; return (x>y) and x or y == 2", None);
    }

    #[test]
    fn test_return_expr_complex21() {
        assert_inst_match("local x, y = 1, 2; x,y=2,1; return (x>y) and x or y == 2", None);
    }

    #[test]
    fn test_return_expr_complex22() {
        assert_inst_match("return 1234567890 == tonumber('1234567890') and 1234567890+1 == 1234567891", None);
    }

    #[test]
    fn test_return_expr_complex23() {
        assert_inst_match("local x = ((b or a)+1 == 2 and (10 or a)+1 == 11); return x", None);
    }

    #[test]
    fn test_return_expr_len() {
        assert_inst_match("return #a", None);
    }

    #[test]
    fn test_return_table1() {
        assert_inst_match("return {123456789}", None);
    }

    #[test]
    fn test_return_table2() {
        assert_inst_match("return {'123456789'}", None);
    }

    #[test]
    fn test_return_table3() {
        assert_inst_match("return {3, 100, 5.0, -10}", None);
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
    fn test_if_false_raise_error() {
        assert_inst_match(
            "local a; if false then a = 3 // 0; a = 0 % 0 end",
            None,
        );
    }

    #[test]
    fn test_for() {
        assert_inst_match("for i = 1, 5 do return 1 end", None);
    }

    #[test]
    fn test_for_len() {
        assert_inst_match("for i = 1, #a do end", None);
    }

    #[test]
    fn test_for_in() {
        assert_inst_match("for k,v,w in a do end", None);
    }

    #[test]
    fn test_for_generic1() {
        assert_inst_match("local a = {} for _, __ in ipairs(a) do end", None);
    }

    #[test]
    fn test_for_generic2() {
        assert_inst_match("local a = {} for _, __ in ipairs(a) do local b end", None);
    }

    #[test]
    fn test_function_upvalue() {
        assert_inst_match("local a; local function f(x) x={a=1}; x={x=1}; x={G=1} end", None);
    }

    #[test]
    fn test_function_2() {
        assert_inst_match("function checkload (s, msg) assert(string.find(select(2, load(s)), msg)) end", None);
    }

    #[test]
    fn test_assert_expr1() {
        assert_inst_match("assert(-3+4*5//2^3^2//9+4%10/3 == (-3)+(((4*5)//(2^(3^2)))//9)+((4%10)/3))", None);
    }

    #[test]
    fn test_assert_var1() {
        assert_inst_match("local x = ((b or a)+1 == 2 and (10 or a)+1 == 11); assert(x);", None);
    }

    #[test]
    fn test_assert_var2() {
        assert_inst_match("local x\nx = (((2<3) or 1) == true and (2<3 and 4) == 4); assert(x);", None);
    }

    #[test]
    fn test_table_field_assign1() {
        assert_inst_match("local A = {};A.a = nil;A.b = false;A.c = 123", None);
    }

    #[test]
    fn test_table_field_assign2() {
        assert_inst_match("local A = {};A['a'] = nil;A['b'] = false;A['c'] = 123", None);
    }

    #[test]
    fn test_table_field_assign3() {
        assert_inst_match("A.aa = nil", None);
    }

    #[test]
    fn test_table_field_call() {
        assert_inst_match("string.format('%s', op)", None);
    }

    #[test]
    fn test_load_string() {
        assert_inst_match("load(string.format('', 123))", None);
    }

    #[test]
    fn test_do_local() {
        assert_inst_match("do local a = {} end", None);
    }

    // #[test]
    // fn test_big_lua() {
    //     assert_inst_match_file("big.lua");
    // }

    // #[test]
    // fn test_constructs_lua() {
    //     assert_inst_match_file("constructs.lua");
    // }

    #[test]
    fn test_focus_lua() {
        assert_inst_match_file("test_focus.lua");
    }

    fn assert_inst_match_file(name: &str) {
        assert_inst_match(get_lua_script(name).as_str(), Some(name));
    }

    fn get_lua_script(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests_lua/");
        path.push(name);
        std::fs::read_to_string(path.as_path()).unwrap()
    }

    fn assert_compile_ok(source: &str, name: Option<&str>) {
        let result = crate::compiler::compile(source, name.unwrap_or("=test_assert"));
        assert!(result.is_ok(), "Compile failed: {:?}", result.err());
    }

    #[test]
    fn test_reg_no_leak_basic_expr() {
        assert_compile_ok("return 1 + 2", None);
    }

    #[test]
    fn test_reg_no_leak_complex_expr() {
        assert_compile_ok("return (1 + 2) * (3 + 4) / (5 - 6)", None);
    }

    #[test]
    fn test_reg_no_leak_if_else() {
        assert_compile_ok("if true then return 1 else return 2 end", None);
        assert_compile_ok("if false then return 1 else return 2 end", None);
    }

    #[test]
    fn test_reg_no_leak_if_elseif() {
        assert_compile_ok(
            "if true then return 1 elseif true then return 2 else return 3 end",
            None,
        );
        assert_compile_ok(
            "if false then a=1 elseif true then a=2 else a=3 end",
            None,
        );
    }

    #[test]
    fn test_reg_no_leak_nested_if() {
        assert_compile_ok(
            "if true then if true then return 1 else return 2 end else return 3 end",
            None,
        );
        assert_compile_ok(
            "if true then if false then a=1 elseif true then a=2 else a=3 end else a=4 end",
            None,
        );
    }

    #[test]
    fn test_reg_no_leak_function_call() {
        assert_compile_ok("local function f(x, y) return x + y end; local a = f(1, 2)", None);
    }

    #[test]
    fn test_reg_no_leak_method_call() {
        assert_compile_ok("local t = {}; local function m(t, x) return x end; local a = m(t, 1)", None);
    }

    #[test]
    fn test_reg_no_leak_string_concat() {
        assert_compile_ok("return 'hello' .. ' ' .. 'world'", None);
        assert_compile_ok("local a = 'x'; local b = 'y'; local c = a .. b", None);
    }

    #[test]
    fn test_reg_no_leak_table_constructor() {
        assert_compile_ok("return {1, 2, 3, 4, 5}", None);
        assert_compile_ok("return {a=1, b=2, c=3}", None);
        assert_compile_ok("return {[1]=1, [2]=2, [3]=3, 4, 5, 6}", None);
    }

    #[test]
    fn test_reg_no_leak_for_numeric() {
        assert_compile_ok("for i=1,10 do local x = i end", None);
        assert_compile_ok("for i=1,10,2 do if i==5 then break end end", None);
    }

    #[test]
    fn test_reg_no_leak_for_generic() {
        assert_compile_ok("for i=1,3 do local x = i end", None);
    }

    #[test]
    fn test_reg_no_leak_local_decls() {
        assert_compile_ok("local a, b, c = 1, 2, 3", None);
        assert_compile_ok("local a <const> = 42", None);
        assert_compile_ok("local a = 1; local b = a + 2; local c = b * 3", None);
    }

    #[test]
    fn test_reg_no_leak_function_def() {
        assert_compile_ok("local function f(a, b, c) return a + b + c end", None);
        assert_compile_ok("local f = function(...) return ... end", None);
    }

    #[test]
    fn test_reg_no_leak_global_assign() {
        assert_compile_ok("local a = 1; local b = 2; local c = a + b", None);
    }

    #[test]
    fn test_reg_no_leak_unary_ops() {
        assert_compile_ok("return -1; return not true; return #'abc'; return ~0", None);
    }

    #[test]
    fn test_reg_no_leak_bitwise_ops() {
        assert_compile_ok("return 1 & 2 | 3 ~ 4", None);
        assert_compile_ok("local a = 1 << 2; local b = a >> 1", None);
    }

    #[test]
    fn test_reg_no_leak_multiple_return() {
        assert_compile_ok("return 1, 2, 3", None);
        assert_compile_ok("local function f() return 1, 2 end; local a, b = f()", None);
    }
}
