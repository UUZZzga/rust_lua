#[cfg(test)]
mod compiler_compare_tests {
    use crate::compiler::bytecode_dump;
    use crate::opcodes;

    fn compile_rust(source: &str, name: Option<&str>) -> crate::objects::Proto {
        crate::compiler::compile(&mut crate::state::LuaState::new(), source, name.unwrap_or("=test")).expect("Rust compile failed")
    }

    unsafe fn compile_c(source: &str) -> bytecode_dump::DumpedFunction {
        let dump_data =
            bytecode_dump::compile_with_c_lua(source.as_bytes()).expect("C compile failed");
        bytecode_dump::parse_dump(dump_data).expect("dump parse failed")
    }

    fn compare_proto_recursive(
        rust_proto: &crate::objects::Proto,
        c_func: &bytecode_dump::DumpedFunction,
        source: &str,
        path: &str,
    ) {
        // 对比 Proto 元数据字段
        let mut meta_diffs = Vec::new();

        if rust_proto.num_params != c_func.numparams {
            meta_diffs.push(format!("num_params: Rust={}, C={}", rust_proto.num_params, c_func.numparams));
        }
        if rust_proto.flag != c_func.flag {
            meta_diffs.push(format!("flag: Rust={}, C={}", rust_proto.flag, c_func.flag));
        }
        if rust_proto.max_stack_size != c_func.maxstacksize {
            meta_diffs.push(format!("max_stack_size: Rust={}, C={}", rust_proto.max_stack_size, c_func.maxstacksize));
        }
        if rust_proto.line_defined != c_func.linedefined {
            meta_diffs.push(format!("line_defined: Rust={}, C={}", rust_proto.line_defined, c_func.linedefined));
        }
        if rust_proto.last_line_defined != c_func.lastlinedefined {
            meta_diffs.push(format!("last_line_defined: Rust={}, C={}", rust_proto.last_line_defined, c_func.lastlinedefined));
        }
        if rust_proto.size_upvalues as usize != c_func.upvalues.len() {
            meta_diffs.push(format!("size_upvalues: Rust={}, C={}", rust_proto.size_upvalues, c_func.upvalues.len()));
        }
        if rust_proto.size_k as usize != c_func.constants.len() {
            meta_diffs.push(format!("size_k: Rust={}, C={}", rust_proto.size_k, c_func.constants.len()));
        }
        if rust_proto.size_code as usize != c_func.code.len() {
            meta_diffs.push(format!("size_code: Rust={}, C={}", rust_proto.size_code, c_func.code.len()));
        }
        if rust_proto.size_line_info != c_func.size_line_info {
            meta_diffs.push(format!("size_line_info: Rust={}, C={}", rust_proto.size_line_info, c_func.size_line_info));
        }
        if rust_proto.size_abs_line_info != c_func.size_abs_line_info {
            meta_diffs.push(format!("size_abs_line_info: Rust={}, C={}", rust_proto.size_abs_line_info, c_func.size_abs_line_info));
        }
        if rust_proto.size_loc_vars != c_func.size_loc_vars {
            meta_diffs.push(format!("size_loc_vars: Rust={}, C={}", rust_proto.size_loc_vars, c_func.size_loc_vars));
        }
        // size_p 通过 protos.len() 对比
        if rust_proto.size_p as usize != c_func.protos.len() {
            meta_diffs.push(format!("size_p: Rust={}, C={}", rust_proto.size_p, c_func.protos.len()));
        }

        if !meta_diffs.is_empty() {
            panic!(
                "Proto metadata mismatch for source: {}\n\
                 Function: {}\n\
                 Differences:\n  {}\n",
                source,
                path,
                meta_diffs.join("\n  ")
            );
        }

        let diffs = bytecode_dump::compare_instructions(&rust_proto.code, &c_func.code);
        if !diffs.is_empty() {
            let rust_dump = bytecode_dump::dump_instructions(&rust_proto.code);
            let c_dump = bytecode_dump::dump_c_instructions(&c_func.code, &c_func.constants);
            // Print upvalue info for debugging
            let rust_uv: Vec<String> = rust_proto.upvalues.iter().map(|uv| {
                let name_str = uv.name.as_ref().map(|s| s.to_string()).unwrap_or_else(|| "?".to_string());
                format!("{}(instack={},idx={})", name_str, uv.in_stack, uv.idx)
            }).collect();
            let c_uv: Vec<String> = c_func.upvalues.iter().map(|(instack, idx, _kind)| {
                format!("(instack={},idx={})", instack, idx)
            }).collect();
            panic!(
                "Instruction mismatch for source: {}\n\
                 Function: {}\n\
                 Differences:\n  {}\n\n\
                 Rust upvalues: {}\n\
                 C upvalues: {}\n\n\
                 Rust instructions:\n{}\n\n\
                 C++ instructions:\n{}",
                source,
                path,
                diffs.join("\n  "),
                rust_uv.join(", "),
                c_uv.join(", "),
                rust_dump,
                c_dump
            );
        }
        // 递归比较子函数（闭包对应的函数）
        let rust_protos = &rust_proto.protos;
        let c_protos = &c_func.protos;
        if rust_protos.len() != c_protos.len() {
            panic!(
                "Sub-function count mismatch for source: {}\n\
                 Function: {}\n\
                 Rust has {} sub-functions, C has {}",
                source,
                path,
                rust_protos.len(),
                c_protos.len()
            );
        }
        for (i, (rp, cp)) in rust_protos.iter().zip(c_protos.iter()).enumerate() {
            let sub_path = format!("{}/proto[{}]", path, i);
            compare_proto_recursive(rp, cp, source, &sub_path);
        }
    }

    fn assert_inst_match(source: &str, name: Option<&str>) {
        let rust_proto = compile_rust(source, name);
        let c_func = unsafe { compile_c(source) };
        compare_proto_recursive(&rust_proto, &c_func, source, "main");
    }

    fn assert_inst_match_allow_constants(source: &str) {
        let rust_proto = compile_rust(source, None);
        let c_func = unsafe { compile_c(source) };
        compare_proto_recursive(&rust_proto, &c_func, source, "main");
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
        assert_inst_match("local a\nlocal b\nlocal c", None);
        assert_inst_match("local a = 1 + 2\nlocal b = a * 3\nlocal c = a - 1", None);
        assert_inst_match("local a = 1 + 2\nlocal d = a + 5\nlocal e = a & 2", None);
        assert_inst_match("local a <const> = 123; return a", None);
        assert_inst_match("local a <const> = '123'; return a", None);
        assert_inst_match("local a <const> = 123; assert(a)", None);
        assert_inst_match("local a <const> = '123'; assert(a)", None);
        assert_inst_match("local f; f, X = nil", None);
        assert_inst_match("local a, b; assert(a * b > 2.0^32)", None);
        assert_inst_match("local max, s, err; assert(not s and string.find(err, string.rep('', 10)) and #string.gsub(err, '', '') >= max)", None);
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
    fn test_call() {
        assert_inst_match("f()", None);
        assert_inst_match("f(42)", None);
        assert_inst_match("f(1, 2)", None);
        assert_inst_match("f('hello')", None);
        assert_inst_match("print'hello'", None);
        assert_inst_match("x = {f{}}", None);
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

    #[test]
    fn test_expr_concat() {
        assert_inst_match("local a; local b  = a .. '123'", None);
    }

    // ===== return 语句测试 =====

    #[test]
    fn test_return() {
        assert_inst_match("return 42", None);
        assert_inst_match("return 1, 2, 3", None);
        assert_inst_match("return 1 + 2", None);
        assert_inst_match("return 2^3^2 == 2^(3^2)", None);
        assert_inst_match("return 2^3*4 == (2^3)*4", None);
        assert_inst_match("return 2.0^-2 == 1/4 and -2^- -2 == - - -4", None);
        assert_inst_match("return not nil and 2 and not(2>3 or 3<2)", None);
        assert_inst_match("return -3-1-5 == 0+0-9", None);
        assert_inst_match("return -2^2 == -4 and (-2)^2 == 4 and 2*2-3-1 == 0", None);
        assert_inst_match("return -3%5 == 2 and -3+5 == 2", None);
        assert_inst_match("return 2*1+3/3 == 3 and 1+2 .. 3*1 == \"33\"", None);
        assert_inst_match("return not(2+1 > 3*1) and \"a\"..\"b\" > \"a\"", None);
        assert_inst_match("return 0xF0 | 0xCC ~ 0xAA & 0xFD == 0xF4", None);
        assert_inst_match("return 0xFD & 0xAA ~ 0xCC | 0xF0 == 0xF4", None);
        assert_inst_match("return 0xF0 & 0x0F + 1 == 0x10", None);
        assert_inst_match("return 3^4//2^3//5 == 2", None);
        assert_inst_match("return -3+4*5//2^3^2//9+4%10/3 == (-3)+(((4*5)//(2^(3^2)))//9)+((4%10)/3)", None);
        assert_inst_match("return not ((true or false) and nil)", None);
        assert_inst_match("return true or false  and nil", None);
        assert_inst_match("return (((1 or false) and true) or false) == true", None);
        assert_inst_match("return (((nil and true) or false) and true) == false", None);
        assert_inst_match("return -(1 or 2) == -1 and (1 and 2)+(-1.25 or -4) == 0.75", None);
        assert_inst_match("local x, y = 1, 2; return (x>y) and x or y == 2", None);
        assert_inst_match("local x, y = 1, 2; x,y=2,1; return (x>y) and x or y == 2", None);
        assert_inst_match("return 1234567890 == tonumber('1234567890') and 1234567890+1 == 1234567891", None);
        assert_inst_match("local x = ((b or a)+1 == 2 and (10 or a)+1 == 11); return x", None);
        assert_inst_match("local a,b = 1,nil; local x = ((b or a)+1 == 2 and (10 or a)+1 == 11); return x", None);
        assert_inst_match("return #a", None);
        assert_inst_match("return {123456789}", None);
        assert_inst_match("return {'123456789'}", None);
        assert_inst_match("return {3, 100, 5.0, -10}", None);
        assert_inst_match("if a then return 'a' end; local b = {}; for _ in pairs(b) do end", None);
    }

    #[test]
    fn test_if() {
        assert_inst_match("if true then return 1 end", None);
        assert_inst_match("if false then return 1 end", None);
        assert_inst_match("if true then return 1 else return 2 end", None);
        assert_inst_match("if true then return 1 elseif true then return 2 else return 3 end",None);
        assert_inst_match("local a; if false then a = 3 // 0; a = 0 % 0 end",None);
        assert_inst_match("if a.b == 0 then end",None);
        assert_inst_match("if a.b ~= 0 then end",None);
        assert_inst_match("if _ENV.b == 0 then end",None);
        assert_inst_match("if _ENV.b ~= 0 then end",None);
        assert_inst_match("local i = 0; if i % 60000 == 0 then end",None);
        assert_inst_match("if a then return 'a' end",None);
        assert_inst_match("if a then else return 'a' end",None);
        assert_inst_match("if not a then --[\n local b = {} end --",None);
        assert_inst_match("if not a then --[\n local b = {}\n local c = [[]]\n end --",None);
        assert_inst_match("if not a then local d = b.c() local e = '' .. d end",None);
        assert_inst_match("local a, b, c; if not a then end",None);
    }

    #[test]
    fn test_while() {
        assert_inst_match("while false do end", None);
        assert_inst_match("while nil do end;", None);
        assert_inst_match("local a=nil; while not a do end", None);
        assert_inst_match("local a; while a ~= (a + 0.0) or (a - 1) ~= (a - 1.0) do a = a // 2 end", None);
        assert_inst_match("local i = 1, a; while a[i] ~= 0 do i = a[i] end", None);
    }

    #[test]
    fn test_for() {
        assert_inst_match("for i = 1, 5 do return 1 end", None);
        assert_inst_match("for i = 1, 1000 do break; end", None);
        assert_inst_match("local a = nil; for i = i, 1, -1 do a = a + 1 end", None);
        assert_inst_match("for i = 1, n do for i = i, 1, -1 do end end", None);
        assert_inst_match("if not a then b = 0 end; local c = {}; for i=3000,-3000,-1 do c[i + 0.0] = i; end", None);
        assert_inst_match("local a, lim; for i = 1,lim do a[#a + 1] = '' .. -(2*(lim - i + 1) + 1) end", None);
        assert_inst_match("local T; local b = T.a(0, 10); for i = 1, 10 do local v, p = b.c(b, i) assert(v == nil and p) end", None);
    }

    #[test]
    fn test_for_len() {
        assert_inst_match("for i = 1, #a do end", None);
    }

    #[test]
    fn test_for_in() {
        assert_inst_match("for k,v,w in a do end", None);
        assert_inst_match("for _ in a.b('1'..';'..'2', '2') do end", None);
        assert_inst_match("for _ in a:b('1'..';'..'2', '2') do end", None);
        assert_inst_match("for _, _ in ipairs({}) do for _, _ in ipairs({}) do end end", None);
    }

    #[test]
    fn test_for_generic() {
        assert_inst_match("local a = {} for _, __ in ipairs(a) do end", None);
        assert_inst_match("local a = {} for _, __ in ipairs(a) do local b end", None);
        assert_inst_match("do local a = {} for _, __ in ipairs(a) do local b end end", None);
        assert_inst_match("for _, _ in _ do local a, b assert(a == b) end", None);
    }

    #[test]
    fn test_function_upvalue() {
        assert_inst_match("local a; local function f(x) x={a=1}; x={x=1}; x={G=1} end", None);
        assert_inst_match("local a; local function f(x) local b=a .. '' end", None);
    }

    #[test]
    fn test_function_2() {
        assert_inst_match("function checkload (s, msg) assert(string.find(select(2, load(s)), msg)) end", None);
        assert_inst_match("function f(i) if i < 10 then end end", None);
        assert_inst_match("function f(i) if i < 10 then local i = 0 end end", None);
        assert_inst_match("function f () return 1,2,3; end; local a, b, c = f()", None);
        assert_inst_match("function f () return 1,2,3; end; local a, b, c = (f())", None);
        assert_inst_match("function f () return 1,2,3; end; local a, b, c; a, b, c = f()", None);
        assert_inst_match("function f () return 1,2,3; end; local a, b, c; a, b, c = (f())", None);
        assert_inst_match("local a, b = 3 and f()", None);
        assert_inst_match("local function h(a,b,c,d,e) while (a>=b or c or (d and e) or nil) do return 1; end; return 0; end", None);
        assert_inst_match("assert(not a(b, 'c'))", None);
        assert_inst_match("local a; a.b.c = function (...) end", None);
        assert_inst_match("local a,i,j,b; local function foo() i, a[i], a, j, a[j], a[i+j] = j, i, i, b, j, i end", None);
        assert_inst_match("local t = {}; (function (a) t[a], a = 10, 20  end)(1)", None);
        assert_inst_match("local t = {} (function (a) t[a], a = 10, 20  end)(1)", None);
        assert_inst_match("local T local a = {T.f[[]]} assert(T.f('', 2, 0) == 10.0/0) a = T.f('')", None);
        assert_inst_match("local t = setmetatable({x = 20}, {__len = function (t) return t.x end})", None);
        assert_inst_match("local a, t; f(t, {n=1,a})", None);
    }

    #[test]
    fn test_function_close() {
        assert_inst_match("do local a = {} local function f () local b = a end f() end", None);
        assert_inst_match("do local a, b = {}, {} local function f () local c = a end f() end", None);
        assert_inst_match("do local a, b = {}, {} local function f () local c = b end f() end", None);
        assert_inst_match("if not a then local b local function f(x) local x = t.f(x) return b .. x end end", None);
        assert_inst_match("for _, _ in _ do local b local function f(x) local x = t.f(x) return b .. x end end", None);
    }

    #[test]
    fn test_assert_expr1() {
        assert_inst_match("assert(-3+4*5//2^3^2//9+4%10/3 == (-3)+(((4*5)//(2^(3^2)))//9)+((4%10)/3))", None);
        assert_inst_match("assert(a == n*(n+1)/2 and i==3)", None);
        assert_inst_match("assert(t[1] and t[n] and not t[0] and not t[n+1])", None);
        assert_inst_match("local t = {}; assert(t[1] and t[n] and not t[0] and not t[n+1])", None);
        assert_inst_match("local t, n = {}, 100; assert(t[1] and t[n] and not t[0] and not t[n+1])", None);
        assert_inst_match("local n = 100; assert(a == n*(n+1)/2 and i==3)", None);
        assert_inst_match("local f, g, h; assert(f(1,2,nil,nil,'x') == nil and g(1,2,nil,nil,'x') == 0 and h(1,2,nil,nil,'x') == 0)", None);
        assert_inst_match("x = 2<3 and not 3; assert(x == false)", None);
        assert_inst_match("local x = 2<3 and not 3; assert(x == false)", None);
        assert_inst_match("x = 2<1 or (2>1 and 'a'); assert(x == false)", None);
        assert_inst_match("local x = 2<1 or (2>1 and 'a'); assert(x == false)", None);
        assert_inst_match("a,b = F(1)~=nil", None);
        assert_inst_match("local a,b = F(1)~=nil", None);
        assert_inst_match("assert(a() == b[2] and c == not not b[2])", None);
        assert_inst_match("local x; x = 2<3 and not 3", None);
        assert_inst_match("local x; x = 2<1 or (2>1 and 'a')", None);
        assert_inst_match("if not a then assert(b.c(d'') == d'') end", None);
        assert_inst_match("if not a then b, c, d, e = nil end", None);
        assert_inst_match("_ENV = _G", None);
        assert_inst_match("local a; _ENV = a", None);
        assert_inst_match("(Message or print)('123456')", None);
        assert_inst_match("_ENV.x, _ENV.y = nil", None);
        assert_inst_match("assert((10 or assert(nil)) == 10)", None);
        assert_inst_match("assert(not (nil and assert(nil)))", None);
        assert_inst_match("assert(not not a == true)", None);
        assert_inst_match("local a; assert(not not a == true)", None);
        assert_inst_match("assert(not 'x' == false)", None);
        assert_inst_match("local a, b; assert(a[b] == 10 and a[b - 1] == 11 and a[-b] == 12 and a[-b + 1] == 13)", None);
        assert_inst_match("local a; assert(a == 3.0 and math.type(a) == 'float')", None);
        assert_inst_match("local a, x, y; assert(x == a..a and y == 5)", None);
        assert_inst_match("local lim = 12000; local a = {}; a[#a + 1] = '' .. -(2*lim + 2)", None);
        assert_inst_match("local T; local f = T.a[[]] T.b(f, 2, '')", None);
        assert_inst_match("local T, i; assert(T.ref{} == i)", None);
    }

    #[test]
    fn test_assert_var() {
        assert_inst_match("local x = ((b or a)+1 == 2 and (10 or a)+1 == 11); assert(x);", None);
        assert_inst_match("local x\nx = (((2<3) or 1) == true and (2<3 and 4) == 4); assert(x);", None);
        assert_inst_match("assert(x[1] == 3 and x[2] == 5 and x[3] == 10 and x[4] == 9 and x[12] == 1)", None);
        assert_inst_match("local a,i,j,b; i, a[i], a, j, a[j], a[i+j] = j, i, i, b, j, i", None);
    }

    #[test]
    fn test_table_field_assign() {
        assert_inst_match("local A = {};A.a = nil;A.b = false;A.c = 123", None);
        assert_inst_match("local A = {};A['a'] = nil;A['b'] = false;A['c'] = 123", None);
        assert_inst_match("local a = {1}; a[#a + 1] = 2", None);
        assert_inst_match("local a = {1}; a[#a + 1] = 2 .. ''", None);
        assert_inst_match("local a = {1}; a[#a + 1] = {1}", None);
        assert_inst_match("local a = {1}; a[#a + 1] = function (b) return {b + 1 + ''} end", None);
        assert_inst_match("a[#a + 1] = 2 .. ''", None);
        assert_inst_match("local a, b = nil, 23; local x = {a or b+2}", None);
        assert_inst_match("local a = nil; local x = {f(100)*2+3 or a}", None);
        assert_inst_match("local a, b = nil, 23; local x = {f=2+3 or a, a = b+2}", None);
        assert_inst_match("local a; a={y=1}", None);
        assert_inst_match("local abc = {{'(0==_ENV.a)', 0 == _ENV.a}}", None);
        assert_inst_match("local a = {{'a', 1}}; a[1][2] = 2", None);
        assert_inst_match("a[1][2] = 2", None);
        assert_inst_match("A.aa = nil", None);
        assert_inst_match("_ENV.aa = nil", None);
        assert_inst_match("local aa = 1; _ENV.aa = aa", None);
        assert_inst_match("local aa = 1; _ENV.aa = aa; _ENV.aa = nil", None);
        assert_inst_match("_ENV.a = b.c(0, 1)", None);
        assert_inst_match("local a = {}; getmetatable(a).__index = function () end", None);
        assert_inst_match("local e, m; assert(not e and m:find(\"'newindex'\"))", None);
        assert_inst_match("local a = {['a'] = '', ['b'] = '', ['c'] = '', ['d'] = '', ['e'] = '', ['f'] = '', ['g'] = ''}", None);
        assert_inst_match("a.b.c = nil; a.b.c.d = nil; a.b.c.d.e = nil; a.b.c.d.e.f = nil; a.b.c.d.e.f.g = nil; a.b.c.d.e.f.g.h = nil; a.b.c.d.e.f.g.h.i = nil; a.b.c.d.e.f.g.h.i.g = nil; a.b.c.d.e.f.g.h.i.g.k = nil", None);
        assert_inst_match("a.b.c = 1; a.b.c.d = 1; a.b.c.d.e = 1; a.b.c.d.e.f = 1; a.b.c.d.e.f.g = 1; a.b.c.d.e.f.g.h = 1; a.b.c.d.e.f.g.h.i = 1; a.b.c.d.e.f.g.h.i.g = 1; a.b.c.d.e.f.g.h.i.g.k = 1", None);
        assert_inst_match("a.b.c = '1'; a.b.c.d = '1'; a.b.c.d.e = '1'; a.b.c.d.e.f = '1'; a.b.c.d.e.f.g = '1'; a.b.c.d.e.f.g.h = '1'; a.b.c.d.e.f.g.h.i = '1'; a.b.c.d.e.f.g.h.i.g = '1'; a.b.c.d.e.f.g.h.i.g.k = '1'", None);
        assert_inst_match("assert(not a or a.b(c) == a.b(d))", None);
        assert_inst_match("a[f()], b, a[f()+3] = f(), a, 'x'", None);
        assert_inst_match("local a; a[f()], b, a[f()+3] = f(), a, 'x'", None);
        assert_inst_match("local a, b, f; a[f()], b, a[f()+3] = f(), a, 'x'", None);
        assert_inst_match("a,b = f(), 1, 2, 3, f()", None);
        assert_inst_match("local a, b, f; a,b = f(), 1, 2, 3, f()", None);
        assert_inst_match("local a = {}; a[print](a[a[f]] == a[print])", None);
        assert_inst_match("local a, b, c; a = {10,9,8,7,6,5,4,3,2; [-3]='a', [f]=print, a='a', b='ab'}", None);
        assert_inst_match("local a, b, c; a[1], f(a)[2], b, c = {['alo']=assert}, 10, a[1], a[f], 6, 10, 23, f(a), 2", None);
        assert_inst_match("local a; a.aVeryLongName012345678901234567890123456789012345678901234567890123456789 = 10", None);
        assert_inst_match("local a; local function foo () end; assert(foo() == 10 and a.aVeryLongName012345678901234567890123456789012345678901234567890123456789 == 10)", None);
        assert_inst_match("local a, b; a[-b] = 12; a[-b + 1.0] = 13", None);
    }

    #[test]
    fn test_table_field_call() {
        assert_inst_match("string.format('%s', op)", None);
        assert_inst_match("local t = {}; t[#t + 1] = (''):format(1, 2)", None);
    }

    #[test]
    fn test_load_string() {
        assert_inst_match("load(string.format('', 123))", None);
    }

    #[test]
    fn test_load_call() {
        assert_inst_match("load()()", None);
        assert_inst_match("(load())()", None);
        assert_inst_match("local result = (load())()", None);
    }

    #[test]
    fn test_do_local() {
        assert_inst_match("do local a = {} end", None);
    }

    #[test]
    fn test_do_local_ref() {
        // Test: do with local var reference - C should NOT generate CLOSE
        assert_inst_match("do local a = {}; print(a) end", None);
    }

    #[test]
    fn test_do_local_ref2() {
        assert_inst_match("do local a = {}; local b = a end", None);
    }

    #[test]
    fn test_do_nested_func() {
        // Test: do with nested function that captures local var - C should generate CLOSE
        assert_inst_match("do local a = {}; local function f() a.x = true end end", None);
    }

    #[test]
    fn test_for_with_var_ref() {
        // Test: for loop where control variable is referenced in body
        assert_inst_match("local p = 2; for i=1,10 do local x = p + i end", None);
    }

    #[test]
    fn test_call_function() {
        assert_inst_match("local f, a, b; local c = f(a, b)", None);
        assert_inst_match("local a = b(); a = b()", None);
        assert_inst_match("local a = b('', 1 ~ 2)", None);
        assert_inst_match("local a = b('', 1 | 2)", None);
        assert_inst_match("local a = b('', 1 + 2)", None);
        assert_inst_match("local a = b('', 1 << 2)", None);
        assert_inst_match("a = b('', 1 ~ 2)", None);
        assert_inst_match("a = b('', 1 | 2)", None);
        assert_inst_match("a = b('', 1 + 2)", None);
        assert_inst_match("a = b('', 1 << 2)", None);
        assert_inst_match("local function a() end\na()", None);
        assert_inst_match("x = {f(1), f(2), f(3);};", None);
        assert_inst_match("local a,b;a,b = F(1)~=nil", None);
        assert_inst_match("local a,b;a,b = F(nil)==nil", None);
    }

    #[test]
    fn test_repeat() {
        assert_inst_match("repeat until 1; repeat until true;", None);
        assert_inst_match("local x = 1; repeat local a until x>=12", None);
    }

    /// Test that repeat-until with `or` expression and upvalues generates
    /// correct JMP targets. Before the fix, the `or` short-circuit jump
    /// skipped the CLOSE instruction (JMP 8 → past CLOSE), but after the
    /// fix it correctly goes through CLOSE (JMP 4 → CLOSE).
    #[test]
    fn test_repeat_until_or_with_upvalue() {
        assert_inst_match(r#"
local a = {}
do
  local x = 1
  a[1] = function() return x end
end
local i = 0
repeat
  local x
  i = i + 1
until i > 3 or a[1]() ~= 1
"#, None);
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
    fn test_for_break_closure() {
        // Numeric for loop with break and closure capturing loop variable
        assert_inst_match("for i = 1, 3 do local f = function() return i end break end", None);
    }

    #[test]
    fn test_env_closure_close() {
        // do local _ENV with closure capturing _ENV - should generate CLOSE on block exit
        assert_inst_match(r#"
do
  local _ENV = {}
  function foo() return A end
end
"#, None);
    }

    #[test]
    fn test_env_nested_closure_close() {
        // Nested _ENV blocks with closures - tests multiple CLOSE instructions
        assert_inst_match(r#"
do
  local mt = {}
  do
    local _ENV = mt
    function foo(x)
      A = x
      do local _ENV = _G; A = 1000 end
      return function(y) return A .. y end
    end
  end
end
"#, None);
    }

    #[test]
    fn debug_goto_simple() {
        // Minimal test: goto jumping out of a block with a local variable
        // This matches the pattern in goto.lua that causes PC 54 difference
        assert_inst_match(r#"local x; do local y = 12; goto l1; ::l2:: x = x + 1; goto l3; ::l1:: x = y; goto l2; end; ::l3:: return x"#, None);
    }

    #[test]
    fn debug_goto_with_y() {
        // Same but with local y inside the block (like goto.lua)
        assert_inst_match(r#"
local x
do
  local y = 12
  goto l1
  ::l2:: x = x + 1; goto l3
  ::l1:: x = y; goto l2
end
::l3:: return x
"#, None);
    }

    #[test]
    fn debug_goto_while() {
        // goto.lua lines 89-96: goto out of while loop with local variable
        assert_inst_match(r#"
local x = 13
while true do
  goto l4
  goto l1
  goto l1
  local x = 45
  ::l1:: ;;;
end
::l4:: assert(x == 13)
"#, None);
    }

    #[test]
    fn debug_goto_if() {
        // goto.lua lines 98-104: goto inside if block with local variable
        assert_inst_match(r#"
if print then
  goto l1
  error("should not be here")
  goto l2
  local x
  ::l1:: ; ::l2:: ;;
else end
"#, None);
    }

    #[test]
    fn debug_goto_with_globals() {
        // Like goto.lua: global declarations + goto jumping out of block with upvalue
        assert_inst_match(r#"
global<const> print, assert
local x
do
  local y = 12
  goto l1
  ::l2:: x = x + 1; goto l3
  ::l1:: x = y; goto l2
end
::l3:: assert(x == 13)
"#, None);
    }

    #[test]
    fn debug_global_func() {
        // global function foo - foo should be resolved as global variable
        assert_inst_match(r#"
global<const> print, assert
local foo = 20
do
  global function foo (x)
    if x == 0 then return 1 else return 2 * foo(x - 1) end
  end
  assert(foo == _ENV.foo and foo(4) == 16)
end
"#, None);
    }

    #[test]
    fn debug_global_init() {
        // global X; X = 20 - test that SETTABUP uses constant for value
        assert_inst_match(r#"
global<const> print
do
  local X = 10
  do global X; X = 20 end
end
"#, None);
    }

    /// Regression test: <const> variables referenced by child functions should NOT
    /// be captured as upvalues, and should NOT cause extra CLOSE instructions.
    /// Before the fix, RDKCTC variables in parent_locals were incorrectly treated
    /// as regular locals, causing find_upvalue to create upvalues for them and
    /// mark_block_upval to set has_upval=true, which generated extra CLOSE instructions.
    #[test]
    fn test_ctc_no_extra_close() {
        // This is a minimal reproduction of the code.lua string constants test:
        // local k0 <const> is referenced inside f1, but since it's a <const>,
        // it should be inlined as a constant, not captured as an upvalue.
        // The do...end block should NOT generate a CLOSE instruction.
        assert_inst_match(r#"
do
  local k0 <const> = "hello"
  local function f1 ()
    local k <const> = k0
    return function ()
             return function () return k end
           end
  end
  local f2 = f1()
  local f3 = f2()
end
"#, None);
    }

    /// Test that <const> integer variables in parent scope are also
    /// correctly inlined as constants in child functions, not captured as upvalues.
    #[test]
    fn test_ctc_int_no_extra_close() {
        assert_inst_match(r#"
do
  local k0 <const> = 42
  local function f1 ()
    return function () return k0 end
  end
  local f2 = f1()
end
"#, None);
    }

    #[test]
    fn test_local_func_in_func() {
        assert_inst_match(r#"
local Z = function (le)
      local function a (f)
        return le(function (x) return f(f)(x) end)
      end
      return a(a)
    end

local F = function (f)
      return function (n)
               if n == 0 then return 1
               else return n*f(n-1) end
             end
    end

assert(5*Z(F)(4)==5 and Z(F)(5)==5*Z(F)(4))
"#, None);
    }

    #[test]
    fn test_local_shadow_assign() {
        assert_inst_match(r#"
local a = {i = 10}
do
  local a = {}
  a = 1
end
a = nil
(function (x) a=x end)(23)
assert(a == 23)
"#, None);
    }

    #[test]
    fn test_nil_not_callable() {
        // Constants (nil, true, false, numbers, strings) should NOT be treated
        // as function calls. Before the fix, `nil(...)` was incorrectly parsed
        // as a function call chain, producing extra CALL+MOVE instructions.
        assert_inst_match(r#"
local a = 1
a = nil
(function(x) a = x end)(23)
assert(a == 23)
"#, None);
    }

    #[test]
    fn test_paren_const_method_call() {
        // Parenthesized constants CAN have call suffixes.
        // (''):format(...) is valid and must use LOADK+GETTABLE (not SELF)
        // for long method names.
        assert_inst_match("local x = (''):format('%d', 1)", None);
    }

    #[test]
    fn test_long_method_name_func_stat() {
        // Long method names (> LUAI_MAXSHORTLEN) in function definitions
        // must use LOADK+SETTABLE instead of SETFIELD.
        // Before the fix, SETFIELD was incorrectly used for long string keys.
        assert_inst_match(r#"
local t = {x = 1}
function t:_012345678901234567890123456789012345678901234567890123456789 ()
  return self.x
end
assert(t:_012345678901234567890123456789012345678901234567890123456789() == 1)
"#, None);
    }

    #[test]
    fn test_not_in_if_condition() {
        // NOT operator in if condition should produce TEST with NOT's B operand
        // as the register, not VRELOC's info (which is 0).
        // Before fix: TEST 0 k (wrong register 0)
        // After fix: TEST 1 k (correct register from NOT's B operand)
        assert_inst_match(r#"
local x = true
if not x then
  x = false
end
"#, None);
    }

    #[test]
    fn test_not_in_while_condition() {
        // NOT operator in while condition should produce TEST with NOT's B operand.
        // Before fix: TEST 0 k (wrong register)
        assert_inst_match(r#"
local x = true
while not x do
  x = false
end
"#, None);
    }

    #[test]
    fn test_not_in_elseif_condition() {
        // NOT operator in elseif condition should produce TEST with NOT's B operand.
        assert_inst_match(r#"
local x = true
if x then
  x = false
elseif not x then
  x = true
end
"#, None);
    }

    #[test]
    fn test_not_in_repeat_until() {
        // NOT operator in repeat-until condition should produce EQ with correct register.
        assert_inst_match(r#"
local x = false
repeat
  x = true
until not x
"#, None);
    }

    #[test]
    fn test_and_with_comparison() {
        // 'and' with comparison operators produces VJMP for each condition.
        // The false list must contain all JMPs and be patched correctly.
        // Before fix: second JMP was not patched (JMP -1 / infinite loop).
        assert_inst_match(r#"
if 1 < 2 and 3 < 4 then
  local x = 1
end
"#, None);
    }

    #[test]
    fn test_or_with_not() {
        // 'or' with NOT should remove NOT and use TEST with NOT's B operand.
        assert_inst_match(r#"
local x = false
if x or not x then
  x = true
end
"#, None);
    }

    #[test]
    fn test_and_or_combined() {
        // Combined and/or with comparisons tests jump list patching.
        assert_inst_match(r#"
local a = 1
if a < 2 and a > 0 or a == 3 then
  a = 2
end
"#, None);
    }

    #[test]
    fn test_not_preserves_jumps() {
        // NOT should preserve and swap t/f jump lists from the operand.
        // Before fix: NOT discarded jump lists, causing missing JMP+LFALSESKIP+LOADTRUE.
        assert_inst_match(r#"
local x = (not true)
assert(x == false)
"#, None);
    }

    #[test]
    fn test_bandk_reg_reuse() {
        // BANDK should reuse the left operand's register when it's a temporary.
        // Before the fix, the left operand's register was not freed, causing
        // a register offset of 1 in subsequent instructions.
        assert_inst_match(r#"
local a = (1 + 1) & 255
assert(a == 2)
"#, None);
    }

    #[test]
    fn test_not_table_constructor() {
        // Bug: constructor expression was incorrectly marked as Relocable instead of NonReloc,
        // causing an extra MOVE instruction before NOT when applying 'not' to a table constructor.
        assert_inst_match("return not {}", None);
    }

    #[test]
    fn test_not_table_constructor_in_expr() {
        // More complex case: not applied to table constructor in a comparison
        assert_inst_match("return not {} == false", None);
    }

    #[test]
    fn test_api_lua() {
        assert_inst_match_file("api.lua");
    }
  
    #[test]
    fn test_attrib_lua() {
        assert_inst_match_file("attrib.lua");
    }

    #[test]
    fn test_big_lua() {
        assert_inst_match_file("big.lua");
    }

    #[test]
    fn test_bitwise_lua() {
        assert_inst_match_file("bitwise.lua");
    }

    #[test]
    fn test_bwcoercion_lua() {
        assert_inst_match_file("bwcoercion.lua");
    }

    #[test]
    fn test_calls_lua() {
        assert_inst_match_file("calls.lua");
    }

    #[test]
    fn test_closure_lua() {
        assert_inst_match_file("closure.lua");
    }

    #[test]
    fn test_code_lua() {
        assert_inst_match_file("code.lua");
    }

    #[test]
    fn test_constructs_lua() {
        assert_inst_match_file("constructs.lua");
    }

    #[test]
    fn test_coroutine_lua() {
        assert_inst_match_file("coroutine.lua");
    }

    #[test]
    fn test_cstack_lua() {
        assert_inst_match_file("cstack.lua");
    }

    #[test]
    fn test_db_lua() {
        assert_inst_match_file("db.lua");
    }

    #[test]
    fn test_errors_lua() {
        assert_inst_match_file("errors.lua");
    }

    #[test]
    fn test_events_lua() {
        assert_inst_match_file("events.lua");
    }

    #[test]
    fn test_files_lua() {
        assert_inst_match_file("files.lua");
    }

    #[test]
    fn test_gc_lua() {
        assert_inst_match_file("gc.lua");
    }

    #[test]
    fn test_gengc_lua() {
        assert_inst_match_file("gengc.lua");
    }

    #[test]
    fn test_heavy_lua() {
        // Heavy.lua requires more stack space due to deep recursion in the compiler
        let child = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                assert_inst_match_file("heavy.lua");
            })
            .expect("thread spawn failed");
        child.join().expect("test thread panicked");
    }
 
    #[test]
    fn test_literals_lua() {
        assert_inst_match_file("literals.lua");
    }
 
    #[test]
    fn test_locals_lua() {
        assert_inst_match_file("locals.lua");
    }
 
    #[test]
    fn test_math_lua() {
        assert_inst_match_file("math.lua");
    }

    #[test]
    fn test_memerr_lua() {
        assert_inst_match_file("memerr.lua");
    }

    #[test]
    fn test_nextvar_lua() {
        assert_inst_match_file("nextvar.lua");
    }

    #[test]
    fn test_pm_lua() {
        assert_inst_match_file("pm.lua");
    }

    #[test]
    fn test_sort_lua() {
        assert_inst_match_file("sort.lua");
    }

    #[test]
    fn test_strings_lua() {
        assert_inst_match_file("strings.lua");
    }

    #[test]
    fn test_tpack_lua() {
        assert_inst_match_file("tpack.lua");
    }

    #[test]
    fn test_tracegc_lua() {
        assert_inst_match_file("tracegc.lua");
    }

    #[test]
    fn test_utf8_lua() {
        assert_inst_match_file("utf8.lua");
    }

    #[test]
    fn test_vararg_lua() {
        assert_inst_match_file("vararg.lua");
    }

    #[test]
    fn test_verybig_lua() {
        assert_inst_match_file("verybig.lua");
    }

    // ===== Int 取负溢出（wrapping_neg）测试 =====
    // -0x8000000000000000 即 -i64::MIN 会溢出，应使用 wrapping_neg 折叠为常量
    // 而非 panic（debug 模式）或生成 UNM 指令

    #[test]
    fn test_negate_mininteger_hex() {
        // -0x8000000000000000 应折叠为常量 LOADK，而非 LOADK + UNM
        // 0x8000000000000000 被词法分析器解析为 i64::MIN，取负应使用 wrapping_neg
        let source = "local min = -0x8000000000000000; return min";
        assert_inst_match(source, None);
    }

    #[test]
    fn test_negate_mininteger_hex_in_expr() {
        // 在表达式中使用 -0x8000000000000000
        let source = "local max, min = 0x7fffffffffffffff, -0x8000000000000000; return max";
        assert_inst_match(source, None);
    }

    // ===== ADDK 交换律优化测试 =====
    // Float 常量在加法左操作数时，应交换操作数使用 ADDK+MMBINK 而非 LOADK+ADD+MMBIN

    #[test]
    fn test_addk_float_commutative() {
        // 5.2 + b => ADDK + MMBINK (flip=true)
        // 如果缺少交换律优化，会生成 LOADK + ADD + MMBIN
        let source = "local b = 1; return 5.2 + b";
        assert_inst_match(source, None);
    }

    #[test]
    fn test_addk_float_commutative_global() {
        // 全局变量的 Float 加法交换律优化
        let source = "return 3.14 + x";
        assert_inst_match(source, None);
    }

    #[test]
    fn test_addk_float_commutative_chain() {
        // 链式 Float 加法：1.5 + a + b
        let source = "local a = 1; local b = 2; return 1.5 + a + b";
        assert_inst_match(source, None);
    }

    // ===== GETFIELD 短字符串索引优化测试 =====
    // table["shortkey"] 中 key 为短字符串时，应使用 GETFIELD 而非 LOADK+GETTABLE

    #[test]
    fn test_getfield_bracket_short_string() {
        // (10)["3"] => GETFIELD (key "3" 是短字符串)
        // 如果缺少 GETFIELD 优化，会生成 LOADK + GETTABLE
        let source = "return (10)['3']";
        assert_inst_match(source, None);
    }

    #[test]
    fn test_getfield_bracket_short_string_local() {
        // local t = {}; t["key"] => GETFIELD
        let source = "local t = {}; return t['key']";
        assert_inst_match(source, None);
    }

    #[test]
    fn test_getfield_bracket_short_string_global() {
        // 全局表索引短字符串 key => GETFIELD
        let source = "return x['name']";
        assert_inst_match(source, None);
    }

    #[test]
    fn test_goto_lua() {
        assert_inst_match_file_allow_constants("goto.lua");
    }

    #[test]
    fn test_focus_lua() {
        assert_inst_match_file("test_focus.lua");
    }

    #[test]
    fn test_setfield_overflow_lua() {
        assert_inst_match_file("test_setfield_overflow.lua");
    }

    #[test]
    fn test_const_index_overflow_gettabup() {
        // When constant pool index exceeds MAXINDEXRK (255),
        // GETTABUP must fall back to GETUPVAL+LOADK+GETTABLE,
        // and SETTABUP must fall back to GETUPVAL+LOADK+SETTABLE,
        // and SETFIELD must fall back to LOADK+SETTABLE.
        let mut source = String::new();
        // Generate 256 short string constants to push subsequent constants past MAXINDEXRK
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // Now "getmetatable" and "__index" will have constant indices > 255
        source.push_str("local b; getmetatable(b).__index = function (t, i) return t.p[i] end");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_const_index_overflow_getfield() {
        // When constant pool index exceeds MAXINDEXRK (255),
        // GETFIELD must fall back to LOADK+GETTABLE,
        // and SETFIELD must fall back to LOADK+SETTABLE.
        let mut source = String::new();
        // Generate 256 short string constants to push subsequent constants past MAXINDEXRK
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // Now "testKey" will have a constant index > 255, so GETFIELD must fall back
        source.push_str("local t; local x = t.testKey; t.testKey = 42");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_const_index_overflow_getfield_chain() {
        // Test GETFIELD overflow in chained field access (a.b.c where b's index > MAXINDEXRK)
        let mut source = String::new();
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // "fieldA" and "fieldB" will have constant indices > 255
        source.push_str("local a; local x = a.fieldA.fieldB");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_const_index_overflow_getfield_call() {
        // Test GETFIELD overflow in function call context (a.method() where method's index > MAXINDEXRK)
        let mut source = String::new();
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // "myMethod" will have a constant index > 255
        source.push_str("local a; a.myMethod()");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_const_index_overflow_settabup_assign() {
        // When a global variable's key constant index exceeds MAXINDEXRK (255),
        // the assignment must emit GETUPVAL+LOADK before evaluating the right side,
        // matching C compiler's evaluation order (left side first).
        // e.g., "L1 = T.newstate()" where "L1" has constant index > 255
        let mut source = String::new();
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // "T" and "newstate" will have constant indices > 255
        // The assignment target "L1" also has constant index > 255
        source.push_str("L1 = T.newstate()");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_const_index_overflow_setfield_dot() {
        // When _ENV.xxx = value and xxx's constant index exceeds MAXINDEXRK,
        // the Dot branch must emit GETUPVAL before evaluating the value,
        // matching C compiler's evaluation order.
        let mut source = String::new();
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // "longFieldName" will have a constant index > 255
        source.push_str("_ENV.longFieldName = true");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_const_index_overflow_settabup_bracket() {
        // When _ENV[xxx] = value and xxx's constant index exceeds MAXINDEXRK,
        // the LBracket branch must emit GETUPVAL before evaluating the value,
        // matching C compiler's evaluation order.
        let mut source = String::new();
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // "longKeyName" will have a constant index > 255
        source.push_str("_ENV['longKeyName'] = true");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_const_index_overflow_gettabup_bracket_read() {
        // When reading _ENV["key"] and key's constant index exceeds MAXINDEXRK,
        // the compiler must keep the GETUPVAL instruction with the correct register
        // (not register 0) and emit LOADK+GETTABLE, matching C compiler output.
        let mut source = String::new();
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // "readKey" will have a constant index > 255
        source.push_str("return _ENV['readKey']");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_const_index_overflow_gettabup_dot_read() {
        // When reading _ENV.key and key's constant index exceeds MAXINDEXRK,
        // the compiler must keep the GETUPVAL instruction with the correct register
        // and emit LOADK+GETTABLE, matching C compiler output.
        let mut source = String::new();
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // "readField" will have a constant index > 255
        source.push_str("return _ENV.readField");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_or_with_relocable_in_local() {
        // Test that TESTSET is converted to TEST when `or` expression with
        // a Relocable right operand (e.g., NEWTABLE) is used in local declaration.
        // Previously, parse_local did not call resolve_jumps for Relocable expressions,
        // leaving TESTSET unconverted and JMP offset wrong.
        assert_inst_match("local a; local mt = a or {}", None);
    }

    #[test]
    fn test_or_with_relocable_nonrelloc_in_local() {
        // Test that TESTSET is converted to TEST when `or` expression with
        // a NonReloc right operand is used in local declaration.
        assert_inst_match("local a,b; local c = a or b", None);
    }

    #[test]
    fn test_and_with_relocable_in_local() {
        // Test that TESTSET is converted to TEST when `and` expression with
        // a Relocable right operand is used in local declaration.
        assert_inst_match("local a; local b = a and a", None);
    }

    #[test]
    fn test_func_stat_long_name_closure_order() {
        // Test that for function declarations with non-short-string names
        // (constant index > MAXINDEXRK), the compiler evaluates table and key
        // BEFORE CLOSURE, matching C compiler's evaluation order.
        // Previously, CLOSURE was emitted before GETUPVAL+LOADK.
        let mut source = String::new();
        for i in 0..256 {
            if i % 5 == 0 { source.push('\n'); }
            source.push_str(&format!("_ = \"s{:03}\"; ", i));
        }
        // "longFuncName" will have a constant index > 255
        source.push_str("function longFuncName() end");
        assert_inst_match(&source, None);
    }

    #[test]
    fn test_and_or_testset_to_test() {
        // Test that TESTSET is converted to TEST in and/or expressions
        // When the result register is NO_REG or equals B, TESTSET should become TEST
        assert_inst_match("local a; local b = a and 1", None);
    }

    #[test]
    fn test_and_in_assign() {
        // Test that TESTSET is converted to TEST when and expression is used in assignment
        // like f(g).x = f(2) and f(10)+f(9)
        assert_inst_match("local f,g; f(g).x = f(2) and f(10)+f(9)", None);
    }

    #[test]
    fn test_and_or_in_if() {
        // Test that TESTSET is converted to TEST in if conditions
        assert_inst_match("local a; if a then return 1 end", None);
    }

    #[test]
    fn test_or_in_for_limit() {
        // Test that TESTSET is converted to TEST in for loop limit
        assert_inst_match("local s; for i = 1, (s and 100 or 200) do end", None);
    }

    #[test]
    fn test_while_nil_testset_to_test() {
        // Test that Nil is converted to Boolean(false) in while condition,
        // generating LOADFALSE (not LOADNIL) and TEST (not TESTSET)
        assert_inst_match("while nil do end", None);
    }

    #[test]
    fn test_while_false_testset_to_test() {
        // Test that TESTSET is converted to TEST in while condition
        assert_inst_match("while false do end", None);
    }

    #[test]
    fn test_repeat_until_nil() {
        // Test that Nil is converted to Boolean(false) in repeat-until condition
        assert_inst_match("repeat local a = {} until nil", None);
    }

    #[test]
    fn test_not_or_testset_to_test() {
        // Test that TESTSET is converted to TEST in "if not (a and b or c)"
        // This tests the full chain: or → not → if condition
        assert_inst_match("local n; if not (n and n or n == \"x\") then end", None);
    }

    #[test]
    fn test_for_in_no_extra_move() {
        // Test that generic for loop doesn't generate extra MOVE instruction
        // for the iterator expression
        assert_inst_match("for k,v,w in a do end", None);
    }

    fn assert_inst_match_file(name: &str) {
        assert_inst_match(get_lua_script(name).as_str(), Some(name));
    }

    fn assert_inst_match_file_allow_constants(name: &str) {
        assert_inst_match_allow_constants(get_lua_script(name).as_str());
    }

    fn get_lua_script(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests_lua/");
        path.push(name);
        let bytes = std::fs::read(path.as_path()).expect(&format!("Failed to read file: {:?}", path));
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn assert_compile_ok(source: &str, name: Option<&str>) {
        let result = crate::compiler::compile(&mut crate::state::LuaState::new(), source, name.unwrap_or("=test_assert"));
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
    fn test_and_reg_leak() {
        // This tests the register leak in 'and' expressions with comparisons
        // Before fix: Rust generates ADDI 8 0 -1 (freereg=8), C generates ADDI 6 0 -1 (freereg=6)
        assert_inst_match(r#"
local a = 0
assert(a == 0 and a == 0)
assert(a == 0)
"#, None);
        // More complex: with bitwise operations and comparisons in 'and'
        assert_inst_match(r#"
local a, b, c, d
a = 0xFF
assert(a >> 4 == 0xF and a == 0xFF)
assert(a == 0xFF)
"#, None);
        // Even more complex: matching the actual bitwise.lua pattern
        assert_inst_match(r#"
local a = 0
assert(-1 >> 1 == (1 << 63) - 1 and 1 << 31 == 0x80000000)
assert(a == 0)
"#, None);
    }

    #[test]
    fn test_and_chain_jmp_debug() {
        // Debug test for and chain JMP targets
        assert_inst_match("local a,b,c,d; local x = a and b and c and d", None);
        // With comparison (VJMP) as first operand
        assert_inst_match("local a,b,c,d; local x = a <= b and c and d", None);
        // Return with and chain
        assert_inst_match("local a,b,c,d; local function f() return a and b and c and d end", None);
        // Return with and chain and comparison
        assert_inst_match("local a,b,c,d,e; local function f() return a <= b and c and d and e end", None);
        // Return with and/or chain
        assert_inst_match("local a,b,c,d,e; local function f() return a <= b and c and d and e or 1 end", None);
        // More complex: and chain ending with false, then or true
        assert_inst_match("local a,b,c,d,e; local function f() return a <= b and c and d and e and false or true end", None);
        // With parameters (matching proto[12] pattern: LE + TEST + TEST + TEST)
        assert_inst_match("local function f(a,b,c,d,e) return a <= b and c and d and e and false or true end", None);
        // Matching constructs.lua proto[12]: not (a>=b or c or d and e or nil) with return 0/1
        assert_inst_match("function g(a,b,c,d,e) if not (a>=b or c or d and e or nil) then return 0; else return 1; end end", None);
    }

    #[test]
    fn test_or_band_debug() {
        // Simple or + band: y or -1, then & 0xFFFFFFFF
        assert_inst_match("local function f(x,y) return (x or -1) & 0xFFFFFFFF end", None);
        assert_inst_match("local function f(x,y) return (y or -1) & 0xFFFFFFFF end", None);
        // Both x or -1 and y or -1
        assert_inst_match("local function f(x,y) return ((x or -1) & (y or -1)) & 0xFFFFFFFF end", None);
    }

    #[test]
    fn test_shr_reg_order() {
        // This tests the register allocation order in SHR expressions
        // Before fix: Rust generates ADDI 7 0 -1 (freereg=7), C generates ADDI 6 0 -1 (freereg=6)
        // Root cause: Rust compiles left operand before right operand in SHR general case,
        // but C's codebinexpval compiles right operand first (e2), then left (e1).
        // This causes different register allocation when both operands need temp registers.
        // Minimal reproduction: -1 >> (numbits - 1) where both sides need a register.
        assert_inst_match(r#"
local numbits = 64
assert(-1 >> (numbits - 1) == 1)
"#, None);
    }

    #[test]
    fn test_reg_no_leak_multiple_return() {
        assert_compile_ok("return 1, 2, 3", None);
        assert_compile_ok("local function f() return 1, 2 end; local a, b = f()", None);
    }

    #[test]
    fn test_unm_as_func_arg() {
        // UNM expression as a function argument must allocate a new register
        // for the result, not reuse the operand register. This matches C's
        // luaK_exp2nextreg behavior for VRELOC expressions.
        // Before fix: Rust generated UNM r r (overwriting operand),
        // C generated UNM new_r r (preserving operand).
        assert_inst_match("local a,b,c; a(b,c,-a,c)", None);
        assert_inst_match("local x; print(-x, x)", None);
        assert_inst_match("local a,b,c,d; a(b,c,-d,d)", None);
    }

    #[test]
    fn test_close_no_extra_for_outer_upvalue() {
        // When a do-block contains a local function that captures upvalues
        // from the OUTER scope (not from the block itself), no CLOSE instruction
        // should be generated at the end of the block. Only when a variable
        // defined IN the block is captured as an upvalue should CLOSE be emitted.
        // Before fix: Rust generated extra CLOSE instruction because it checked
        // whether sub-prototypes have ANY upvalues, not whether they reference
        // variables defined in the current block.
        assert_inst_match(
            r#"do
  local function f(n)
    local s = string.format("x", n)
    local r = assert(load(s))
  end
  f(1)
  f(2)
end"#,
            None,
        );
    }

    #[test]
    fn test_close_needed_for_block_upvalue() {
        // When a variable defined in a do-block IS captured as an upvalue
        // by a sub-function, a CLOSE instruction SHOULD be generated.
        assert_inst_match(
            r#"do
  local a = 10
  local function f() return a end
  f()
end"#,
            None,
        );
    }

    #[test]
    fn test_relocable_as_func_arg() {
        // Other Relocable expressions (NEWTABLE, CLOSURE) as function
        // arguments should also use exp_to_reg for proper register allocation.
        assert_inst_match("local f; f({}, 1)", None);
        assert_inst_match("local f; f(function() end)", None);
    }

    // ===== goto / label 测试 =====

    #[test]
    fn test_goto_simple() {
        assert_inst_match("goto done; ::done::", None);
    }

    #[test]
    fn test_goto_in_do_block() {
        assert_inst_match("do goto done end; ::done::", None);
    }

    #[test]
    fn test_goto_in_for_loop() {
        assert_inst_match("for i = 1, 5 do if i > 3 then goto endloop end end; ::endloop::", None);
    }

    #[test]
    fn test_goto_in_nested_for() {
        assert_inst_match("local s = 0; for i = 1, 5 do for j = 1, 5 do if i + j < 5 then goto endloop end; s = s + i end end; ::endloop::", None);
    }

    #[test]
    fn test_label_simple() {
        assert_inst_match("::start::", None);
    }

    #[test]
    fn test_label_with_goto() {
        assert_inst_match("::loop::; goto loop", None);
    }

    // ===== ...t 命名 vararg 参数测试 =====

    #[test]
    fn test_named_vararg_simple() {
        assert_inst_match("local function f(...t) return t end", None);
    }

    #[test]
    fn test_named_vararg_with_params() {
        assert_inst_match("local function f(a, b, ...t) return t end", None);
    }

    #[test]
    fn test_named_vararg_index() {
        assert_inst_match("local function f(...t) return t[1] end", None);
    }

    #[test]
    fn test_named_vararg_field() {
        assert_inst_match("local function f(...t) return t.n end", None);
    }

    #[test]
    fn test_named_vararg_len() {
        assert_inst_match("local function f(...t) return #t end", None);
    }

    // ===== 命名 vararg 参数扩展测试 =====
    // 以下测试覆盖 VVARGVAR 在各编译路径中的正确处理：
    // - LOADK+GETVARG（Dot/LBracket 分支，不设 PF_VATAB）
    // - LEN 对 VVARGVAR 分配新寄存器并设置 PF_VATAB
    // - num_params 不包含 vararg 参数（RETURN C 操作数正确）
    // - VVARGVAR 在赋值目标中的处理

    #[test]
    fn test_named_vararg_field_assign() {
        // VVARGVAR as assignment target: t.x = 1
        // This tests parse_assign_or_call Dot branch with VVARGVAR
        assert_inst_match("local function f(...t) t.x = 1 end", None);
    }

    #[test]
    fn test_named_vararg_index_assign() {
        // VVARGVAR as assignment target: t[1] = 2
        // This tests parse_assign_or_call LBracket branch with VVARGVAR
        assert_inst_match("local function f(...t) t[1] = 2 end", None);
    }

    #[test]
    fn test_named_vararg_field_in_expr() {
        // VVARGVAR field access in expression: t.n + 1
        assert_inst_match("local function f(...t) return t.n + 1 end", None);
    }

    #[test]
    fn test_named_vararg_index_in_expr() {
        // VVARGVAR index access in expression: t[1] + t[2]
        assert_inst_match("local function f(...t) return t[1] + t[2] end", None);
    }

    #[test]
    fn test_named_vararg_len_in_if() {
        // VVARGVAR LEN in if condition: if #t > 0
        assert_inst_match("local function f(...t) if #t > 0 then return 1 end end", None);
    }

    #[test]
    fn test_named_vararg_with_regular_params() {
        // VVARGVAR with regular parameters: f(a, b, ...t) - num_params should be 2 (not 3)
        assert_inst_match("local function f(a, b, ...t) return t[a] end", None);
    }

    #[test]
    fn test_named_vararg_return_c_operand() {
        // This specifically tests that num_params does NOT include the vararg parameter.
        // Before fix: num_params=1 (wrong), RETURN C=2
        // After fix: num_params=0 (correct), RETURN C=1
        assert_inst_match("local function f(...t) return t end", None);
    }

    // ===== ..._ENV 命名 vararg 参数作为 _ENV 的测试 =====
    // 以下测试覆盖 _ENV 是 VVARGVAR 时全局变量访问的正确处理：
    // - code_global_via_env 的 VVARGVAR 分支应复用 kr 寄存器（GETVARG A=0 + VRELOC）
    // - code_global_via_env_prefix 的 VVARGVAR 分支应返回 is_vvargvar: true（设置 PF_VATAB）
    // - globalnames 应在 _ENV 是 VVARGVAR 时使用 GETTABLE/SETTABLE + PF_VATAB
    // - luaK_finish 将 GETVARG 转为 GETTABLE，RETURN1 不被转为 RETURN

    #[test]
    fn test_named_vararg_env_global_assign_and_read() {
        // _ENV 作为命名 vararg 参数，全局变量赋值后读取
        // 对应 vararg.lua proto[16]:
        //   local function aux (..._ENV)
        //     global a; a = 10
        //     return a
        //   end
        // 修复前：Rust 生成 GETTABLE 2 0 1（多分配寄存器），C 生成 GETTABLE 1 0 1
        // 修复后：Rust 复用 kr 寄存器，生成 GETTABLE 1 0 1
        let source = "local function aux (..._ENV) global a; a = 10 return a end";
        assert_inst_match(source, None);
    }

    #[test]
    fn test_named_vararg_env_global_init() {
        // _ENV 作为命名 vararg 参数，global 声明带初始化
        // 对应 vararg.lua proto[17]:
        //   local function aux (... _ENV)
        //     global a = 10
        //     return a
        //   end
        // 修复前：Rust 使用 GETTABUP/SETTABUP（错误），C 使用 GETTABLE/SETTABLE
        // 修复后：Rust 使用 GETTABLE/SETTABLE + PF_VATAB
        let source = "local function aux (... _ENV) global a = 10 return a end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_global_const_star() {
        // Test: local _ENV should make _ENV a local variable, causing GETFIELD instead of GETTABUP
        let source = "do local _ENV = _G; assert(true) end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_upvalue_close_in_block() {
        // Test: when a local function captures a local variable from a block,
        // the block should generate CLOSE instruction
        let source = "do local a = {}; local function f() a.x = true end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_upvalue_close_in_if() {
        // Test: when a local function inside an if block captures a local variable,
        // the if block should generate CLOSE instruction
        let source = "if true then local a = {}; local function f() a.x = true end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_upvalue_close_shadowed_var() {
        // Test: when a local variable is shadowed, the inner function should capture the inner variable
        let source = "do local a = 1; local a = {}; local function f() a.x = true end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_upvalue_close_with_global_const_star() {
        // Test: upvalue close with global <const> * at top
        let source = "global <const> *; do local a = {}; local function f() a.x = true end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_upvalue_close_in_if_with_for_loop() {
        // Test: when a local function inside an if block captures a local variable,
        // and there's a for loop inside the if block, the if block should still
        // generate CLOSE instruction
        let source = "if true then local a = {}; local function f() a.x = true end; for i=1,1 do end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_upvalue_close_in_if_with_for_loop_global_const() {
        // Same as above but with global <const> * at top
        let source = "global <const> *; if true then local a = {}; local function f() a.x = true end; for i=1,1 do end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_upvalue_close_in_if_with_for_and_break() {
        // Test: if block with upvalue capture, for loop, and break statement
        let source = "global <const> *; if true then local a = {}; local function f() a.x = true end; for i=1,math.huge do if i>1 then break end end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_upvalue_close_shadowed_var_in_if() {
        // Test: two local a in the same if block, second one captured by nested function
        let source = "global <const> *; if true then local a = 1; local a = {}; local function f() a.x = true end; for i=1,1 do end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_upvalue_close_with_for_loops_before() {
        // Test: if block with upvalue capture, but with for loops before the function definition
        let source = "global <const> *; if true then local a = 1; for i=1,1 do end; local a = {}; local function f() a.x = true end; for i=1,1 do end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_forstat_close_with_upvalue() {
        // Test: for loop where the loop variable is captured by a closure inside the loop
        // The forstat block should generate CLOSE
        let source = "for i=1,10 do local function f() return i end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_forstat_close_with_upvalue_global_const() {
        // Same but with global <const> *
        let source = "global <const> *; for i=1,10 do local function f() return i end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_forstat_close_with_body_upvalue() {
        // Test: for loop where a body variable is captured by a closure
        let source = "for i=1,10 do local a = {}; local function f() a.x = true end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_forstat_close_with_body_upvalue_global_const() {
        // Same but with global <const> *
        let source = "global <const> *; for i=1,10 do local a = {}; local function f() a.x = true end end";
        assert_inst_match(source, None);
    }

    #[test]
    fn test_numeric_for_close() {
        // Numeric for loop with closure capturing outer variable - forstat block needs CLOSE
        assert_inst_match(r#"
local a = {}
local function additems()
  a.x = true
end
for i = 1, 10 do
  local st = pcall(additems)
  local count = 0
  for k, v in pairs(a) do
    count = count + 1
  end
  if st then break end
end
"#, None);
    }

    #[test]
    fn test_numeric_for_inner_generic_for_close() {
        // Numeric for loop containing generic for loop with to-be-closed state
        // The generic for's forstat block has marktobeclosed, which should propagate CLOSE
        assert_inst_match(r#"
local a = {}
local function additems()
  a.x = true
end
for i = 1, math.huge do
  pcall(additems)
  local count = 0
  for k, v in pairs(a) do
    count = count + 1
  end
  if count == 5 then break end
end
"#, None);
    }

    #[test]
    fn test_numeric_for_inner_generic_for_close2() {
        assert_inst_match(r#"
do
  local a = {}
  local function additems ()
    a.x = true; a.y = true; a.z = true
    a[1] = true
    a[2] = true
  end
  for i = 1, math.huge do
    pcall(additems)
    local count = 0
    for k, v in pairs(a) do
      assert(a[k] == v)
      count = count + 1
    end
    if count == 5 then break end
  end
end
"#, None);
    }

    #[test]
    fn test_goto_out_of_nested_generic_for_with_tbc() {
        // goto jumping out of nested generic for loops with to-be-closed variables.
        // The inner for loop's closing variable must be properly closed.
        // Previous bug: reglevel didn't skip inactive variables from exited blocks,
        // causing CLOSE operand to be too large (e.g., CLOSE 29 instead of CLOSE 26).
        let source = r#"
local func2close = function(f)
  return setmetatable({}, {__close = f})
end
local numopen = 0
local function open(x)
  numopen = numopen + 1
  return function() x = x - 1; if x > 0 then return x end end,
         nil, nil,
         func2close(function() numopen = numopen - 1 end)
end
do
  local s = 0
  for i in open(10) do
    for j in open(10) do
       if i + j < 5 then goto endloop end
       s = s + i
    end
  end
  ::endloop::
end
"#;
        assert_inst_match(source, None);
    }

    #[test]
    fn debug_while_true_goto() {
        let source = r#"while true do
  goto l4
  goto l1
  goto l1
  local x = 45
  ::l1:: ;;;
end
::l4:: assert(x == 13)"#;
        let rust_proto = compile_rust(source, None);
        let c_func = unsafe { compile_c(source) };

        let diffs = bytecode_dump::compare_instructions(&rust_proto.code, &c_func.code);
        let rust_dump = bytecode_dump::dump_instructions(&rust_proto.code);
        let c_dump = bytecode_dump::dump_c_instructions(&c_func.code, &c_func.constants);

        eprintln!("=== debug_while_true_goto ===");
        eprintln!("Differences:\n  {}", diffs.join("\n  "));
        eprintln!("\nRust instructions:\n{}", rust_dump);
        eprintln!("\nC instructions:\n{}", c_dump);
        eprintln!("\nC constants:");
        for (i, c) in c_func.constants.iter().enumerate() {
            eprintln!("  {}: {:?}", i, c);
        }
        if !diffs.is_empty() {
            panic!("Instruction mismatch found!");
        }
    }

    #[test]
    fn debug_while_true_goto_simple() {
        // Simplified variant to isolate the while-true-goto issue
        let source = r#"while true do
  goto l1
  local x = 45
  ::l1:: ;;;
end"#;
        let rust_proto = compile_rust(source, None);
        let c_func = unsafe { compile_c(source) };

        let diffs = bytecode_dump::compare_instructions(&rust_proto.code, &c_func.code);
        let rust_dump = bytecode_dump::dump_instructions(&rust_proto.code);
        let c_dump = bytecode_dump::dump_c_instructions(&c_func.code, &c_func.constants);

        eprintln!("=== debug_while_true_goto_simple ===");
        eprintln!("Differences:\n  {}", diffs.join("\n  "));
        eprintln!("\nRust instructions:\n{}", rust_dump);
        eprintln!("\nC instructions:\n{}", c_dump);
        eprintln!("\nC constants:");
        for (i, c) in c_func.constants.iter().enumerate() {
            eprintln!("  {}: {:?}", i, c);
        }
        if !diffs.is_empty() {
            panic!("Instruction mismatch found!");
        }
    }

    #[test]
    fn debug_while_simple() {
        let source = r#"while true do
  goto l1
  local x = 45
  ::l1:: ;;;
end"#;
        let rust_proto = compile_rust(source, None);
        let c_func = unsafe { compile_c(source) };

        eprintln!("=== Rust instructions ===");
        for (i, inst) in rust_proto.code.iter().enumerate() {
            let opcode = opcodes::get_opcode(*inst);
            if opcode == opcodes::OpCode::JMP {
                let sj_raw = opcodes::getarg(*inst, opcodes::POS_SJ, opcodes::SIZE_BX + opcodes::SIZE_A);
                let sj = sj_raw - opcodes::OFFSET_sJ;
                let target = i as i32 + sj + 1;
                eprintln!("  PC {:3}: raw={:#010x} JMP sj_raw={} sj={} target={}", i, inst, sj_raw, sj, target);
            } else {
                eprintln!("  PC {:3}: raw={:#010x} {}", i, inst, bytecode_dump::format_instruction(*inst));
            }
        }

        eprintln!("=== C instructions ===");
        for (i, inst) in c_func.code.iter().enumerate() {
            let raw = bytecode_dump::dump_inst_to_raw(inst);
            let opcode = opcodes::get_opcode(raw);
            if opcode == opcodes::OpCode::JMP {
                let sj_raw = opcodes::getarg(raw, opcodes::POS_SJ, opcodes::SIZE_BX + opcodes::SIZE_A);
                let sj = sj_raw - opcodes::OFFSET_sJ;
                let target = i as i32 + sj + 1;
                eprintln!("  PC {:3}: raw={:#010x} JMP sj_raw={} sj={} target={}", i, raw, sj_raw, sj, target);
            } else {
                eprintln!("  PC {:3}: raw={:#010x} {}", i, raw, bytecode_dump::format_c_instruction(inst, &c_func.constants));
            }
        }

        assert_inst_match(source, None);
    }

    #[test]
    fn debug_goto_if_print() {
        // "if print then" section from goto.lua (lines 98-104)
        let source = r#"if print then
  goto l1
  error("should not be here")
  goto l2
  local x
  ::l1:: ; ::l2:: ;;
else end"#;
        let rust_proto = compile_rust(source, None);
        let c_func = unsafe { compile_c(source) };

        let diffs = bytecode_dump::compare_instructions(&rust_proto.code, &c_func.code);
        let rust_dump = bytecode_dump::dump_instructions(&rust_proto.code);
        let c_dump = bytecode_dump::dump_c_instructions(&c_func.code, &c_func.constants);

        eprintln!("=== debug_goto_if_print ===");
        eprintln!("Differences:\n  {}", diffs.join("\n  "));
        eprintln!("\nRust instructions:\n{}", rust_dump);
        eprintln!("\nC instructions:\n{}", c_dump);
        eprintln!("\nC constants:");
        for (i, c) in c_func.constants.iter().enumerate() {
            eprintln!("  {}: {:?}", i, c);
        }
        if !diffs.is_empty() {
            panic!("Instruction mismatch found!");
        }
    }

    #[test]
    fn debug_goto_closing_upvalues() {
        // "closing upvalues" section from goto.lua (lines 168-190)
        let source = r#"local function foo ()
  local t = {}
  do
  local i = 1
  local a, b, c, d
  t[1] = function () return a, b, c, d end
  ::l1::
  local b
  do
    local c
    t[#t + 1] = function () return a, b, c, d end
    if i > 2 then goto l2 end
    do
      local d
      t[#t + 1] = function () return a, b, c, d end
      i = i + 1
      local a
      goto l1
    end
  end
  end
  ::l2:: return t
end"#;
        let rust_proto = compile_rust(source, None);
        let c_func = unsafe { compile_c(source) };

        let diffs = bytecode_dump::compare_instructions(&rust_proto.code, &c_func.code);
        let rust_dump = bytecode_dump::dump_instructions(&rust_proto.code);
        let c_dump = bytecode_dump::dump_c_instructions(&c_func.code, &c_func.constants);

        eprintln!("=== debug_goto_closing_upvalues ===");
        eprintln!("Differences:\n  {}", diffs.join("\n  "));
        eprintln!("\nRust instructions:\n{}", rust_dump);
        eprintln!("\nC instructions:\n{}", c_dump);
        eprintln!("\nC constants:");
        for (i, c) in c_func.constants.iter().enumerate() {
            eprintln!("  {}: {:?}", i, c);
        }
        if !diffs.is_empty() {
            panic!("Instruction mismatch found!");
        }
    }

    #[test]
    fn test_goto_out_of_nested_generic_for_with_prior_block() {
        // Variant: a prior do-block creates inactive locals in the Vec,
        // which reglevel must skip when computing CLOSE operand for goto.
        let source = r#"
local func2close = function(f)
  return setmetatable({}, {__close = f})
end
do
  local x = 1
  do local y = 2 end
  local function open(n)
    return function() n = n - 1; if n > 0 then return n end end,
           nil, nil,
           func2close(function() end)
  end
  local s = 0
  for i in open(5) do
    for j in open(5) do
      if i + j < 3 then goto endloop end
      s = s + i
    end
  end
  ::endloop::
end
"#;
        assert_inst_match(source, None);
    }

    // ===== 修复回归测试：确保之前修复的 bug 不会复发 =====

    #[test]
    fn test_upvalue_search_past_non_matching_local() {
        // Bug: find_upvalue stopped searching when encountering a non-global,
        // non-matching variable. This caused 'a' to not be found as an upvalue
        // because the search stopped at 'f' (a local function name).
        // C's searchvar continues searching past non-matching variables.
        let source = r#"
local a = {}
local function f()
  a.x = true
end
"#;
        assert_inst_match(source, None);
    }

    #[test]
    fn test_for_loop_close_with_upvalue() {
        // Bug: for loop missing CLOSE instruction because find_upvalue's
        // break on non-matching non-global variable prevented mark_block_upval.
        let source = r#"
for i=1,10 do
  local a = {}
  local function f()
    a.x = true
  end
end
"#;
        assert_inst_match(source, None);
    }

    #[test]
    fn test_global_init_scope_ordering() {
        // Bug: globalnames evaluated expressions before declaring variables,
        // causing right-hand side to reference the newly declared global
        // instead of the outer local. C declares first but doesn't activate
        // (increase nactvar) until after expression evaluation.
        let source = r#"
global<const> print
do
  local a = 10
  local b = 20
  do global a, b; a, b = a, b end
end
"#;
        assert_inst_match_allow_constants(source);
    }

    #[test]
    fn test_global_function_upvalue() {
        // Bug: global function's body couldn't reference the function itself
        // as an upvalue because parent_locals excluded GDKREG variables.
        let source = r#"
global<const> print
global function foo()
  print(foo)
end
"#;
        assert_inst_match_allow_constants(source);
    }

    #[test]
    fn test_global_init_nil_fill() {
        // Bug: global a, b, c = 10 didn't fill b and c with nil.
        // C's initglobal uses adjust_assign which generates LOADNIL for
        // missing initializers.
        let source = r#"
global<const> print
do global a, b, c = 10 end
"#;
        assert_inst_match_allow_constants(source);
    }

    // ===== local function 遮蔽 global <const> * 回归测试 =====
    // Bug: parse_func_stat 中 find_global_decl 优先于 find_local，
    // 当 global <const> * 生效时，`function f() end` 会将 "f" 加入常量池，
    // 导致后续常量索引偏移，使 setmetatable 的索引从 255 变为 256，
    // 超过 MAXINDEXRK，触发 GETTABUP 回退为 GETUPVAL+LOADK+GETTABLE。
    // 修复：find_local_ex 优先查找，local 变量正确遮蔽 global 声明，
    // 不再将函数名加入常量池。

    #[test]
    fn test_local_func_shadows_global_const_star() {
        // `local function f` should shadow `global <const> *` for variable `f`,
        // generating MOVE instead of SETTABUP, and not adding "f" to constant pool.
        let source = r#"
global <const> *
local function f(s, p)
  return s
end
function f(a, b)
  return string.gsub(a, '.', b)
end
"#;
        assert_inst_match(source, None);
    }

    #[test]
    fn test_local_func_shadows_global_const_star_simple() {
        // Minimal case: local function f shadows global <const> *
        let source = r#"
global <const> *
local function f() end
function f() return 1 end
"#;
        assert_inst_match(source, None);
    }

    // ===== 冒号调用+字符串参数测试 =====
    // obj:method"string" 应生成 SELF + LOADK + CALL B=3（而非 B=2）

    #[test]
    fn test_colon_call_string_arg() {
        // Bug: colon call with string argument generated CALL B=2 instead of B=3.
        // SELF puts method at freg and self at freg+1, string arg at freg+2,
        // so CALL should have B=3 (method + self + 1 arg).
        let source = r#"
global<const> print
local obj = {}
function obj:write(s) end
obj:write"hello"
"#;
        assert_inst_match(source, None);
    }

    // ===== 冒号调用+表构造器参数测试 =====
    // obj:method{...} 应生成 SELF + table + CALL B=3（而非 B=2）

    #[test]
    fn test_colon_call_table_arg() {
        // Bug: colon call with table constructor argument generated CALL B=2
        // instead of B=3. Same issue as string argument case.
        let source = r#"
global<const> print
local obj = {}
function obj:configure(t) end
obj:configure{x=1}
"#;
        assert_inst_match(source, None);
    }

    // ===== GEI 指令生成测试 =====
    // 0 <= x 应生成 GEI x 0，而非 LOADI + LE

    #[test]
    fn test_le_const_left_gei() {
        // Bug: "0 <= x" generated LOADI 0 + LE instead of GEI x 0.
        // When the left operand of <= is a small integer constant and
        // the right operand is not, the comparison should be flipped:
        // (0 <= x) is equivalent to (x >= 0), generating GEI.
        let source = r#"
global<const> print
local x = 1
if 0 <= x then print("yes") end
"#;
        assert_inst_match(source, None);
    }

    #[test]
    fn test_lt_const_left_gti() {
        // Similar: "0 < x" should generate GTI x 0 (x > 0).
        let source = r#"
global<const> print
local x = 1
if 0 < x then print("yes") end
"#;
        assert_inst_match(source, None);
    }

    #[test]
    fn test_le_neg_const_left_gei() {
        // "-1 <= x" should generate GEI x -1.
        let source = r#"
global<const> print
local x = 0
if -1 <= x then print("yes") end
"#;
        assert_inst_match(source, None);
    }

    #[test]
    fn test_const_div_compare() {
        // local inf = math.huge * 2 + 1
        // local mz <const> = -1/inf
        // local z <const> = 1/inf
        // assert(mz == z)
        assert_inst_match(
            r#"
local inf = math.huge * 2 + 1
local mz <const> = -1/inf
local z <const> = 1/inf
assert(mz == z)
"#,
            Some("test_const_div_compare")
        );
    }

    // ===== CTC 变量遮蔽测试 =====
    // 当 <const> 变量被同名非 CTC 变量遮蔽时，find_local_ctc 不应找到旧的 CTC 变量
    // 修复前：find_local_ctc 跳过非 CTC 的同名变量，找到被遮蔽的旧 CTC 变量，
    // 导致 -1/inf 被错误识别为 -0.0，生成 EQI 而非 EQ

    #[test]
    fn test_ctc_shadowing() {
        // mz 先声明为 CTC(-0.0)，再重新声明为非 CTC(-1/inf)
        // z 先声明为 CTC(0.0)，再重新声明为非 CTC(1/inf)
        // 比较时应使用运行时值，生成 EQ 而非 EQI
        assert_inst_match(
            r#"
local inf = math.huge * 2 + 1
local mz <const> = -0.0
local z <const> = 0.0
local mz <const> = -1/inf
local z <const> = 1/inf
assert(mz == z)
"#,
            Some("test_ctc_shadowing")
        );
    }

    // ===== Int/Int DIV 常量折叠 0.0 检查测试 =====
    // 0/(-1) 结果为 -0.0，不应被常量折叠（C 的 constfolding 拒绝 0.0 结果）
    // 修复前：0/(-1) 被折叠为 Float(-0.0)，is_sc_number 将其识别为 SC number 0，
    // 生成 EQI 而非 EQ

    #[test]
    fn test_int_div_zero_result() {
        // 0/x 其中 x=-1，结果为 -0.0，不应折叠
        assert_inst_match(
            r#"
local x = -1
local mz = 0/x
assert(mz == 0)
"#,
            Some("test_int_div_zero_result")
        );
    }

    // ===== BAND flip 操作数顺序测试 =====
    // 当 BAND 的左操作数是整型常量时，C 的 codebitwise 会交换操作数并设 flip=1
    // 在非 K 路径中，C 的 codebinNoK 会交换回原始顺序再调用 codebinexpval
    // 修复前：Rust 未交换回原始顺序，导致常量先获得寄存器，非常量后获得
    // 例如 BNOT 结果 & large_int 应生成 BAND 21 22 21 而非 BAND 21 21 21

    #[test]
    fn test_band_flip_operand_order() {
        // ~(4 << -1) & 8822622750169614806
        // BNOT 结果在 R21，常量应在 R22（而非 R21 覆盖 BNOT 结果）
        assert_inst_match(
            r#"
local x = 4 << -1
x = ~x
x = x & 8822622750169614806
return x
"#,
            Some("test_band_flip_operand_order")
        );
    }

    /// Regression test: upvalue used as function call should not generate
    /// duplicate GETUPVAL instructions.
    /// Before the fix, parse_prefix_exp eagerly loaded upvalues into registers
    /// (generating GETUPVAL), then load_func matched the upval_idx branch first
    /// and generated another GETUPVAL, causing all subsequent instructions to
    /// use registers offset by 1.
    /// The fix delays GETUPVAL generation until load_func, matching C's behavior.
    #[test]
    fn test_upvalue_call_no_duplicate_getupval() {
        // Minimal case: outer local `f` is used as a function call inside inner function.
        // Before fix: Rust generates GETUPVAL twice (one from parse_prefix_exp, one from load_func),
        // causing an extra register to be consumed and CONCAT operands shifted.
        // After fix: Rust generates GETUPVAL once, matching C compiler output.
        assert_inst_match(r#"
local f = error
local function g(x)
  f("attempt to '" .. x .. "' a value", 4)
end
"#, None);
    }

    /// Variant: upvalue call with string concatenation in arguments,
    /// matching the pattern from bwcoercion.lua's trymt function.
    #[test]
    fn test_upvalue_call_concat_args() {
        assert_inst_match(r#"
local err, tp = error, type
local function trymt(x, y, name)
  err("attempt to '" .. name .. "' a " .. tp(x) .. " with a " .. tp(y), 4)
end
"#, None);
    }

    /// Regression test: upvalue indexed by another upvalue must emit
    /// GETUPVAL for the key BEFORE GETUPVAL for the table, matching C's
    /// order (yindex's luaK_exp2val emits key code first, then luaK_indexed
    /// emits table GETUPVAL).
    /// Before the fix, Rust emitted table GETUPVAL first, then key GETUPVAL,
    /// causing instruction order mismatch.
    /// Pattern from closure.lua: `local dummy = function () return a[A] end`
    /// where both `a` and `A` are upvalues.
    #[test]
    fn test_upvalue_indexed_by_upvalue() {
        // Minimal case: both table and key are upvalues.
        // C order: GETUPVAL key (relocatable) → GETUPVAL table (relocatable)
        //          → GETTABLE (relocatable)
        // Before fix: GETUPVAL table → GETUPVAL key → GETTABLE (wrong order)
        assert_inst_match(r#"
local A = 1
local a = {}
local function f()
  return a[A]
end
"#, None);
    }

    /// Variant: upvalue indexed by upvalue with the key declared first.
    #[test]
    fn test_upvalue_indexed_by_upvalue_key_first() {
        // Key upvalue declared before table upvalue
        assert_inst_match(r#"
local a = {}
local A = 1
local function f()
  return a[A]
end
"#, None);
    }

    /// Variant: upvalue indexed by upvalue used in a more complex expression.
    #[test]
    fn test_upvalue_indexed_by_upvalue_in_expr() {
        // From closure.lua line 265: local dummy = function () return a[A] end
        assert_inst_match(r#"
local A, B = 0, {}
local function f(x)
  local a = {}
  local dummy = function () return a[A] end
  return dummy()
end
"#, None);
    }

    #[test]
    fn debug_events_proto37() {
        let source = std::fs::read_to_string("tests_lua/events.lua").unwrap();
        let rust_proto = compile_rust(&source, None);
        let c_func = unsafe { compile_c(&source) };
        fn find_proto<'a>(p: &'a crate::objects::Proto, path: &str, target: &str) -> Option<&'a crate::objects::Proto> {
            if path == target { return Some(p); }
            for (i, sp) in p.protos.iter().enumerate() {
                let sub_path = format!("{}/proto[{}]", path, i);
                if let Some(found) = find_proto(sp, &sub_path, target) {
                    return Some(found);
                }
            }
            None
        }
        let rust_p37 = find_proto(&rust_proto, "main", "main/proto[37]").expect("proto[37] not found");
        let c_p37 = &c_func.protos[37];
        let mut out = String::new();
        out.push_str("=== proto[37] ===\n");
        out.push_str(&format!("Rust upvalues: {:?}\n", rust_p37.upvalues));
        out.push_str(&format!("C upvalues: {:?}\n", c_p37.upvalues));
        out.push_str(&format!("Rust num_params: {}, line: {}\n", rust_p37.num_params, rust_p37.line_defined));
        out.push_str(&format!("C numparams: {}, flag: {}, line: {}\n", c_p37.numparams, c_p37.flag, c_p37.linedefined));
        out.push_str(&format!("Rust instructions:\n{}\n", bytecode_dump::dump_instructions(&rust_p37.code)));
        out.push_str(&format!("C instructions:\n{}\n", bytecode_dump::dump_c_instructions(&c_p37.code, &c_p37.constants)));
        std::fs::write("/tmp/proto37_debug.txt", out).unwrap();
    }

    #[test]
    fn debug_or_add_simple() {
        // Simplified version of sort.lua proto[9]:
        // function check(a, f)
        //   f = f or function(x,y) return x<y end;
        //   ...
        // end
        let source = "function perm(s, n) n = n or #s end";
        let rust_proto = compile_rust(source, None);
        let c_func = unsafe { compile_c(source) };
        let mut out = String::new();
        out.push_str("=== sort proto[10] simplified ===\n");
        out.push_str(&format!("Rust has {} sub-protos\n", rust_proto.protos.len()));
        out.push_str(&format!("Rust main instructions:\n{}\n", bytecode_dump::dump_instructions(&rust_proto.code)));
        for (i, p) in rust_proto.protos.iter().enumerate() {
            out.push_str(&format!("Rust proto[{}] instructions:\n{}\n", i, bytecode_dump::dump_instructions(&p.code)));
        }
        out.push_str(&format!("C main instructions:\n{}\n", bytecode_dump::dump_c_instructions(&c_func.code, &c_func.constants)));
        for (i, p) in c_func.protos.iter().enumerate() {
            out.push_str(&format!("C proto[{}] instructions:\n{}\n", i, bytecode_dump::dump_c_instructions(&p.code, &p.constants)));
        }
        std::fs::write("/tmp/sort_debug.txt", out).unwrap();
    }

    /// Simplified reproduction of db.lua proto[17] upvalue ordering issue:
    /// _ENV must be upvalue #0, debug must be upvalue #1 (created after _ENV).
    #[test]
    fn test_env_upvalue_order_simple() {
        // Child function accesses a global first (creating _ENV upvalue),
        // then accesses parent's local (creating debug upvalue).
        // _ENV must be upvalue #0, debug must be upvalue #1.
        let source = "local debug = 1\nfunction f() collectgarbage() return debug end\n";
        assert_inst_match(source, None);
    }

    /// Reproduction with global declaration (like db.lua proto[17]).
    /// The global declaration must not prevent _ENV from being upvalue #0.
    #[test]
    fn test_env_upvalue_order_with_global_decl() {
        let source = "local debug = 1\nfunction f() global collectgarbage; collectgarbage(); return debug end\n";
        assert_inst_match(source, None);
    }

}
