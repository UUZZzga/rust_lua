use crate::objects::*;
use crate::objects::PF_VAHID;
use crate::opcodes::*;
use super::lexer::{LexState, Token};

use crate::objects::Instruction;

const NO_JUMP: i32 = -1;

const VDKREG: i32 = 0;
const RDKCONST: i32 = 1;
const RDKTOCLOSE: i32 = 3;
const RDKCTC: i32 = 4;
const GDKREG: i32 = 5;
const GDKCONST: i32 = 6;

// ============================================================================
// !! 禁止删除、修改 meta-language 注释 !!
// ============================================================================
// Meta-Language (ANTLR4): Lua 5.5 语法规则 → Rust 编译器函数映射
// ============================================================================
//
// grammar Lua5_5;
//
// chunk
//   : block                                        // → compile_chunk() → parse_chunk()
//   ;
//
// block
//   : statement*                                   // → parse_block()
//   ;
//
// statement
//   : ';'                                          // → parse_statement()
//   | 'if' expr 'then' block
//     ('elseif' expr 'then' block)*
//     ('else' block)? 'end'                        // → parse_if()
//   | 'while' expr 'do' block 'end'                // → parse_while()
//   | 'do' block 'end'                             // → parse_do()
//   | 'repeat' block 'until' expr                  // → parse_repeat()
//   | 'for' NAME '=' expr ',' expr (',' expr)?
//     'do' block 'end'                             // → parse_for() (numeric for)
//   | 'for' namelist 'in' explist
//     'do' block 'end'                             // → parse_for() (generic for)
//   | 'function' funcname funcbody                 // → parse_func_stat()
//   | 'local' 'function' NAME funcbody             // → parse_local() (local func)
//   | 'local' attnamelist ('=' explist)?           // → parse_local()
//   | 'return' explist? (';')?                     // → parse_return()
//   | varlist '=' explist                          // → parse_assign_or_call() (赋值)
//   | functioncall                                 // → parse_assign_or_call() (调用)
//   | expr                                         // → parse_statement() expression
//   ;
//
// varlist
//   : var (',' var)*                               // → parse_assign_or_call()
//   ;
//
// var
//   : NAME                                         // → parse_prefix_exp()
//   | prefixexp '[' expr ']'                       // → parse_prefix_exp() LBracket
//   | prefixexp '.' NAME                           // → parse_prefix_exp() Dot
//   ;
//
// functioncall
//   : prefixexp args                               // → load_func() → parse_func_args()
//   | prefixexp ':' NAME args                      // → parse_func_args() method call
//   ;
//
// args
//   : '(' explist? ')'                             // → parse_args()
//   | tableconstructor                             // → parse_func_args() (table arg)
//   | STRING                                       // → parse_func_args() (string arg)
//   ;
//
// funcname
//   : NAME ('.' NAME)* (':' NAME)?                 // → parse_func_stat()
//   ;
//
// namelist
//   : NAME (',' NAME)*                             // → parse_for() / parse_local()
//   ;
//
// attnamelist
//   : NAME attrib? (',' NAME attrib?)*             // → parse_local() + getvarattribute()
//   ;
//
// attrib
//   : '<' NAME '>'                                 // → getvarattribute()
//   ;
//
// explist
//   : expr (',' expr)*                             // → parse_expr_list()
//   ;
//
// expr
//   : simpleExp                                    // → parse_expr() → parse_subexpr()
//   | expr binop expr                              // → parse_subexpr() Pratt parser
//   | unop expr                                    // → parse_simple_exp()
//   ;
//
// simpleExp
//   : 'nil'                                        // → parse_simple_exp()
//   | 'false'                                      // → parse_simple_exp()
//   | 'true'                                       // → parse_simple_exp()
//   | NUMBER                                       // → parse_simple_exp()
//   | STRING                                       // → parse_simple_exp()
//   | '...'                                        // → parse_simple_exp()
//   | '{' fieldlist? '}'                           // → parse_constructor()
//   | 'function' funcbody                          // → parse_body()
//   | prefixexp                                    // → parse_prefix_exp()
//   ;
//
// prefixexp
//   : varOrExp                                     // → parse_prefix_exp()
//   | functioncall                                 // → parse_prefix_exp()
//   | '(' expr ')'                                 // → parse_prefix_exp()
//   ;
//
// binop
//   : '+' | '-' | '*' | '/' | '//' | '%'           // → parse_subexpr() PREC_ADD/PREC_MUL
//   | '^'                                          // → parse_subexpr() PREC_POW
//   | '..'                                         // → parse_subexpr() PREC_CONCAT
//   | '<' | '<=' | '>' | '>=' | '==' | '~='        // → parse_subexpr() PREC_COMP
//   | 'and' | 'or'                                 // → parse_subexpr() PREC_AND/PREC_OR
//   | '|' | '&' | '~' | '<<' | '>>'                // → parse_subexpr() PREC_BOR/BAND/BXOR/SHL
//   ;
//
// unop
//   : 'not' | '-' | '#' | '~'                      // → parse_simple_exp()
//   ;
//
// tableconstructor
//   : '{' fieldlist? '}'                           // → parse_constructor()
//   ;
//
// fieldlist
//   : field (fieldsep field)* fieldsep?            // → parse_constructor()
//   ;
//
// field
//   : '[' expr ']' '=' expr                        // → parse_constructor() SETTABLE
//   | NAME '=' expr                                // → parse_constructor() SETFIELD
//   | expr                                         // → parse_constructor() SETI (array)
//   ;
//
// fieldsep
//   : ',' | ';'
//   ;
//
// funcbody
//   : '(' parlist? ')' block 'end'                 // → parse_body() / parse_body_ex()
//   ;
//
// parlist
//   : namelist (',' '...')?                        // → parse_body_ex()
//   | '...'                                        // → parse_body_ex()
//   ;
//
// ---------------------------------------------------------------------------
// 寄存器管理与指令生成 (FuncState 辅助方法)
// ---------------------------------------------------------------------------
// FuncState::alloc_reg()           → 分配寄存器(freereg++, max_freereg追踪)
// FuncState::free_reg()            → 释放寄存器(freereg--)
// FuncState::expr_to_reg()         → 确保表达式值在寄存器中
// FuncState::add_local()           → 添加局部变量 (VDKREG)
// FuncState::add_local_kind()      → 添加带类型局部变量
// FuncState::add_local_kind_reg()  → 添加带指定寄存器局部变量
// FuncState::find_local()          → 查找局部变量
// FuncState::code_abc(op, a, b, c)    → 生成 IABC 模式指令
// FuncState::code_abc_k(op, a, b, c, k) → 生成 IABC+k 位指令
// FuncState::code_abx(op, a, bx)   → 生成 IABx 模式指令
// FuncState::code_asbx(op, a, sbx) → 生成 IAsBx 模式有符号偏移指令
// FuncState::code_sj(op, sj, k)    → 生成 IsJ 模式跳转指令
// FuncState::code_ax(op, ax)       → 生成扩展A字段指令
// FuncState::emit(ins)             → 发射指令到原型
// FuncState::fix_jump(pc, dest, back) → 修复跳转指令偏移量
// FuncState::set_c(pc, c)          → 设置指令C参数
// FuncState::jump() → JMP          → 生成无条件跳转
// FuncState::negate_condition()    → 反转条件跳转语义
// FuncState::get_jump()            → 获取跳转目标地址
// FuncState::concat_jump()         → 串联跳转链表
// FuncState::const_k(value)        → 查找/添加常量
// FuncState::string_k(s)           → 字符串常量
// FuncState::int_k(i)              → 整型常量
// FuncState::float_k(f)            → 浮点常量
// FuncState::nvarstack()           → 计算当前变量栈大小
// FuncState::return_stat_gen()     → 生成RETURN指令
// FuncState::patch_true_jumps()    → 修复真分支跳转链表
// FuncState::patch_false_jumps()   → 修复假分支跳转链表
// FuncState::discharge_to_any_reg() → 表达式值移到任意寄存器
//
// 辅助函数:
//   check(fs, tok)                 → 检查当前token匹配
//   test_next(fs, tok)             → 检查并消费token
//   expect(fs, tok)                → 期望token(否则报错)
//   block_follow(fs, withUntil)    → 判断是否代码块结束标记
//   get_name(fs)                   → 获取NAME标识符
//   check_compare(fs)              → 检查比较运算符token
//   fits_sc(desc)                  → 判断Int是否适合SC参数
//   fits_sc_neg(v)                 → 判断Int取负后是否适合SC参数
//   int_to_sc(v)                   → Int转SC参数编码
//   fits_sbx(v)                    → 判断是否适合AsBx模式偏移
//   exp_to_const_k(fs, e)          → 表达式转常量索引
//   exp_to_k(fs, e)                → 表达式转常量索引(≤255)
//   check_addop(fs)                → 检查加减运算符(+/-)
//   check_mulop(fs)                → 检查乘除运算符(*/- // %)
//   tvalue_eq(a, b)                → TValue比较(常量去重)
// ---------------------------------------------------------------------------

// ============================================================================
// Expression descriptor
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum ExpKind {
    Void, Nil, Boolean, Int, Float, Str,
    NonReloc, Relocable, Call, Vararg, VJMP,
}

#[derive(Debug, Clone)]
pub struct ExpDesc {
    pub kind: ExpKind,
    pub info: i64,
    pub info2: i32,
    pub t: i32,
    pub f: i32,
}

impl ExpDesc {
    pub fn new(kind: ExpKind, info: i64) -> Self {
        ExpDesc { kind, info, info2: -1, t: NO_JUMP, f: NO_JUMP }
    }

    pub fn new_reloc_with_pc(info: i64, pc: i32) -> Self {
        ExpDesc { kind: ExpKind::Relocable, info, info2: pc, t: NO_JUMP, f: NO_JUMP }
    }

    pub fn into_reloc_with_pc(self, info: i64, pc: i32) -> Self {
        ExpDesc { kind: ExpKind::Relocable, info, info2: pc, t: self.t, f: self.f }
    }

    fn has_jumps(&self) -> bool {
        self.t != self.f
    }
}

// ============================================================================
// Local variable tracking
// ============================================================================

#[derive(Clone)]
struct LocalVar {
    name: String,
    start_pc: i32,
    active: bool,
    reg: i32,
    kind: i32,
}

// ============================================================================
// FuncState
// ============================================================================

#[cfg(debug_assertions)]
struct RegAllocEntry {
    file: &'static str,
    line: u32,
    column: u32,
    idx: i32,
}

pub struct FuncState {
    pub proto: Proto,
    pub prev: Option<Box<FuncState>>,
    pub pc: i32,
    pub freereg: i32,
    pub max_freereg: i32,
    pub locals: Vec<LocalVar>,
    pub errors: Vec<String>,
    pub needclose: bool,
    pub parent_locals: Vec<(String, i32)>,
    pub break_list: i32,
    ls: *mut LexState,
    #[cfg(debug_assertions)]
    reg_alloc_stack: Vec<RegAllocEntry>,
    #[cfg(debug_assertions)]
    reg_alloc_counter: i32,
}

/// ANTLR4: `chunk: block ;` — 编译器入口，初始化 FuncState，解析整个脚本块并生成原型
pub fn compile_chunk(ls: &mut LexState) -> Result<Proto, String> {
    let mut fs = FuncState::new(ls);
    fs.proto.num_params = 0;
    fs.proto.flag = PF_VAHID;

    ls.next();
    fs.code_abc(OpCode::VARARGPREP, 0, 0, 0);
    parse_chunk(&mut fs);

    if !fs.errors.is_empty() {
        return Err(fs.errors.join("\n"));
    }

    let mut proto = fs.proto;
    proto.max_stack_size = (fs.max_freereg + 2) as u8;
    proto.size_code = proto.code.len() as i32;
    proto.size_k = proto.constants.len() as i32;
    proto.size_p = proto.protos.len() as i32;
    Ok(proto)
}

impl FuncState {
    fn new(ls: &mut LexState) -> Self {
        FuncState {
            proto: crate::func::new_proto(),
            prev: None,
            pc: 0,
            freereg: 0,
            max_freereg: 0,
            locals: Vec::new(),
            errors: Vec::new(),
            needclose: false,
            parent_locals: Vec::new(),
            break_list: NO_JUMP,
            ls: ls as *mut LexState,
            #[cfg(debug_assertions)]
            reg_alloc_stack: Vec::new(),
            #[cfg(debug_assertions)]
            reg_alloc_counter: 0,
        }
    }

    fn ls(&self) -> &LexState { unsafe { &*self.ls } }
    fn ls_mut(&mut self) -> &mut LexState { unsafe { &mut *self.ls } }

    fn error(&mut self, msg: &str) {
        self.errors.push(format!("{}:{}: {}", self.ls().chunk_name, self.ls().lastline, msg));
    }
}

// ============================================================================
// Instruction emission
// ============================================================================

impl FuncState {
    /// 发射指令到原型代码数组，返回当前 pc 并自增
    fn emit(&mut self, ins: Instruction) -> i32 {
        self.proto.code.push(ins);
        let cur = self.pc;
        self.pc += 1;
        cur
    }

    /// 生成 IABC 模式指令: `op a b c` (无 k 位)
    fn code_abc(&mut self, op: OpCode, a: i32, b: i32, c: i32) -> i32 {
        if get_opmode(op) == OpMode::IvABC {
            self.emit(create_vabck(op, a, b, c, 0))
        } else {
            self.emit(create_abck(op, a, b, c, 0))
        }
    }

    /// 生成 IABC+k 位模式指令: `op a b c k`
    fn code_abc_k(&mut self, op: OpCode, a: i32, b: i32, c: i32, k: bool) -> i32 {
        if get_opmode(op) == OpMode::IvABC {
            self.emit(create_vabck(op, a, b, c, if k { 1 } else { 0 }))
        } else {
            self.emit(create_abck(op, a, b, c, if k { 1 } else { 0 }))
        }
    }

    /// 生成 IABx 模式指令: `op a bx` (无符号偏移)
    fn code_abx(&mut self, op: OpCode, a: i32, bx: i32) -> i32 {
        let ins = ((op as u32) << POS_OP) | ((a as u32) << POS_A) | ((bx as u32) << POS_BX);
        self.emit(ins)
    }

    /// 生成 IAsBx 模式指令: `op a sbx` (有符号偏移, 加 OFFSET_SBX)
    fn code_asbx(&mut self, op: OpCode, a: i32, sbx: i32) -> i32 {
        let ins = ((op as u32) << POS_OP)
            | ((a as u32) << POS_A)
            | ((((sbx + OFFSET_SBX) as u32) & mask1(SIZE_BX, 0)) << POS_BX);
        self.emit(ins)
    }

    /// 生成 IsJ 模式跳转指令: `op sj k` (有符号跳转偏移 + k 位)
    fn code_sj(&mut self, op: OpCode, sj: i32, k: i32) -> i32 {
        let ins = ((op as u32) << POS_OP)
            | ((((sj + OFFSET_sJ) as u32) & mask1(SIZE_sJ, 0)) << POS_SJ)
            | (((k & 1) as u32) << POS_K);
        self.emit(ins)
    }

    /// 生成扩展 A 字段指令: `op ax` (A 占满剩余位)
    fn code_ax(&mut self, op: OpCode, ax: i32) -> i32 {
        let ins = ((op as u32) << POS_OP) | ((ax as u32) << POS_A);
        self.emit(ins)
    }

    /// 修复跳转指令偏移量: 根据 OpMode 计算 dest 与 pc 之差写入指令
    fn fix_jump(&mut self, pc: i32, dest: i32, back: bool) {
        let i = &mut self.proto.code[pc as usize];
        let op = get_opcode(*i);
        match get_opmode(op) {
            OpMode::IABC => {
                if testarg_k(*i) {
                    let sbx = dest - pc - 1;
                    let masked = ((getarg_sbx(*i) as i64) & !0x1FFFF) | ((sbx as i64) & 0x1FFFF);
                    setarg(i, masked as i32, POS_BX, SIZE_BX);
                } else {
                    let offset = (dest - pc - 1) as i32;
                    setarg(i, offset, POS_B, SIZE_B);
                }
            }
            OpMode::IABx => {
                let mut offset = dest - (pc + 1);
                if back {
                    offset = -offset;
                }
                setarg(i, offset, POS_BX, SIZE_BX);
            }
            OpMode::IAsBx => {
                let offset = if back {
                    pc + 1 - dest + OFFSET_SBX
                } else {
                    dest - pc - 1 + OFFSET_SBX
                };
                setarg(i, offset, POS_BX, SIZE_BX);
            }
            OpMode::IsJ => {
                setarg(i, (dest - pc - 1) + OFFSET_sJ, POS_SJ, SIZE_sJ);
            }
            _ => {}
        }
    }

    /// 设置指定指令的 C 参数
    fn set_c(&mut self, pc: i32, c: i32) {
        let i = &mut self.proto.code[pc as usize];
        setarg(i, c, POS_C, SIZE_C);
    }

    /// 设置指定指令的 A 参数 (用于延迟寄存器分配)
    fn set_a(&mut self, pc: i32, a: i32) {
        let i = &mut self.proto.code[pc as usize];
        setarg(i, a, POS_A, SIZE_A);
    }

    fn set_tablesize(&mut self, pc: i32, ra: i32, asize: i32, hsize: i32) {
        let max_vc = (1 << SIZE_VC) as i32;
        let extra = asize / max_vc;
        let rc = asize % max_vc;
        let k = extra > 0;
        let hsize = if hsize != 0 {
            crate::objects::ceil_log2(hsize as u32) as i32 + 1
        } else {
            0
        };
        let inst = create_vabck(OpCode::NEWTABLE, ra, hsize, rc, if k { 1 } else { 0 });
        self.proto.code[pc as usize] = inst;
        self.proto.code[(pc + 1) as usize] = ((OpCode::EXTRAARG as u32) << POS_OP) | ((extra as u32) << POS_A);
    }

    /// 生成 JMP 无条件跳转指令，返回指令 pc 位置
    fn jump(&mut self) -> i32 {
        self.code_sj(OpCode::JMP, NO_JUMP, 0)
    }

    /// 反转条件跳转语义: 翻转前一条指令的 k 位
    fn negate_condition(&mut self, jmp_pc: i32) {
        let ctrl_pc = jmp_pc - 1;
        if ctrl_pc >= 0 && ctrl_pc < self.proto.code.len() as i32 {
            let ctrl_inst = &mut self.proto.code[ctrl_pc as usize];
            let cur_k = testarg_k(*ctrl_inst);
            setarg(ctrl_inst, if cur_k { 0 } else { 1 }, POS_K, 1);
        }
    }

    /// 获取跳转目标地址: 从 JMP 指令解码偏移量
    fn get_jump(&self, pc: i32) -> i32 {
        if pc == NO_JUMP {
            return NO_JUMP;
        }
        let offset = getarg_sj(self.proto.code[pc as usize]);
        if offset == NO_JUMP {
            NO_JUMP
        } else {
            pc + 1 + offset
        }
    }

    /// 串联跳转链表: 将 list2 追加到 list1 链尾
    fn concat_jump(&mut self, list1: &mut i32, list2: i32) {
        if list2 == NO_JUMP {
            return;
        }
        if *list1 == NO_JUMP {
            *list1 = list2;
            return;
        }
        let mut list = *list1;
        loop {
            let next = self.get_jump(list);
            if next == NO_JUMP {
                break;
            }
            list = next;
        }
        let offset = list2 - list - 1;
        setarg(&mut self.proto.code[list as usize], offset + OFFSET_sJ, POS_SJ, SIZE_sJ);
    }

    fn new_break(&mut self) -> i32 {
        let pc = self.jump();
        self.code_abc(OpCode::CLOSE, 0, 1, 0);
        pc
    }

    fn patch_breaks(&mut self, target: i32) {
        let mut cur = self.break_list;
        while cur != NO_JUMP {
            let next = self.get_jump(cur);
            self.fix_jump(cur, target, false);
            cur = next;
        }
        self.break_list = NO_JUMP;
    }

    /// 查找或添加常量到常量表: 去重后返回常量索引
    fn const_k(&mut self, value: TValue) -> i32 {
        for (i, c) in self.proto.constants.iter().enumerate() {
            if tvalue_eq(c, &value) {
                return i as i32;
            }
        }
        let idx = self.proto.constants.len() as i32;
        self.proto.constants.push(value);
        idx
    }

    /// 创建字符串常量并添加到常量表
    fn string_k(&mut self, s: &str) -> i32 {
        let t = crate::strings::StringTable::new();
        let ls = crate::strings::new_lstr(&t, s);
        self.const_k(TValue::Str(ls))
    }

    /// 创建整型常量并添加到常量表
    fn int_k(&mut self, i: i64) -> i32 {
        self.const_k(TValue::Integer(i))
    }

    /// 创建浮点常量并添加到常量表
    fn float_k(&mut self, f: f64) -> i32 {
        self.const_k(TValue::Float(f))
    }

    /// 分配新寄存器: freereg++ 并追踪 max_freereg
    #[cfg_attr(debug_assertions, track_caller)]
    fn alloc_reg(&mut self) -> i32 {
        let r = self.freereg;
        self.freereg += 1;
        if self.freereg > self.max_freereg {
            self.max_freereg = self.freereg;
        }
        #[cfg(debug_assertions)]
        {
            let caller = std::panic::Location::caller();
            self.reg_alloc_counter += 1;
            self.reg_alloc_stack.push(RegAllocEntry {
                file: caller.file(),
                line: caller.line(),
                column: caller.column(),
                idx: self.reg_alloc_counter,
            });
        }
        r
    }

    /// 释放最后一个寄存器: freereg--
    fn free_reg(&mut self) {
        if self.freereg > 0 {
            self.freereg -= 1;
        }
        #[cfg(debug_assertions)]
        {
            self.reg_alloc_stack.pop();
        }
    }

    #[cfg(debug_assertions)]
    fn reg_alloc_entry_desc(entry: &RegAllocEntry) -> String {
        format!(
            "  #{}: register allocated at {}:{}:{}",
            entry.idx, entry.file, entry.line, entry.column
        )
    }

    #[cfg(debug_assertions)]
    fn assert_regs_at(&self, expected_nregs: i32, context: &str) {
        let leaked = self.freereg - expected_nregs;
        if leaked != 0 {
            let mut details = String::new();
            let start = self
                .reg_alloc_stack
                .len()
                .saturating_sub(leaked.abs() as usize);
            for entry in &self.reg_alloc_stack[start..] {
                details.push_str(&Self::reg_alloc_entry_desc(entry));
                details.push('\n');
            }
            panic!(
                "REGISTER LEAK DETECTED [{}]: expected {} registers, found {} (leaked {}):\n{}",
                context, expected_nregs, self.freereg, leaked, details
            );
        }
    }

    #[cfg(debug_assertions)]
    fn assert_regs_clean(&self, context: &str) {
        self.assert_regs_at(self.nvarstack(), context);
    }

    #[cfg(not(debug_assertions))]
    fn assert_regs_at(&self, _expected_nregs: i32, _context: &str) {}

    #[cfg(not(debug_assertions))]
    fn assert_regs_clean(&self, _context: &str) {}

    fn set_freereg(&mut self, new_val: i32) {
        #[cfg(debug_assertions)]
        {
            let diff = (self.freereg - new_val).max(0) as usize;
            for _ in 0..diff.min(self.reg_alloc_stack.len()) {
                self.reg_alloc_stack.pop();
            }
        }
        self.freereg = new_val;
    }

    /// 添加局部变量 (VDKREG)，分配寄存器并返回
    fn add_local(&mut self, name: &str, start_pc: i32) -> i32 {
        let reg = self.alloc_reg();
        self.locals.push(LocalVar {
            name: name.to_string(),
            start_pc,
            active: true,
            reg,
            kind: VDKREG,
        });
        reg
    }

    /// 添加带类型局部变量 (RDKCONST/RDKTOCLOSE 等)，自动分配寄存器
    fn add_local_kind(&mut self, name: &str, start_pc: i32, kind: i32) -> i32 {
        let in_reg = kind <= RDKTOCLOSE;
        let reg = if in_reg && kind != RDKCTC {
            self.alloc_reg()
        } else {
            0
        };
        self.locals.push(LocalVar {
            name: name.to_string(),
            start_pc,
            active: true,
            reg,
            kind,
        });
        reg
    }

    /// 添加指定寄存器的局部变量
    fn add_local_kind_reg(&mut self, name: &str, start_pc: i32, kind: i32, reg: i32) {
        self.locals.push(LocalVar {
            name: name.to_string(),
            start_pc,
            active: true,
            reg,
            kind,
        });
    }

    /// 在当前作用域中查找局部变量 (从后往前)
    fn find_local(&self, name: &str) -> Option<i32> {
        for lv in self.locals.iter().rev() {
            if lv.active && lv.name == name {
                return Some(lv.reg);
            }
        }
        None
    }

    /// 在父作用域中查找上值，若找到则创建 UpvalDesc 并返回上值索引
    fn find_upvalue(&mut self, name: &str) -> Option<i32> {
        for (i, uv) in self.proto.upvalues.iter().enumerate() {
            if let Some(ref n) = uv.name {
                if n.as_str() == name {
                    return Some(i as i32);
                }
            }
        }
        for (pname, preg) in &self.parent_locals {
            if pname == name {
                let idx = self.proto.upvalues.len() as i32;
                let t = crate::strings::StringTable::new();
                let ls = crate::strings::new_lstr(&t, name);
                self.proto.upvalues.push(crate::objects::UpvalDesc {
                    name: Some(ls),
                    in_stack: true,
                    idx: *preg as u8,
                });
                self.proto.size_upvalues = self.proto.upvalues.len() as i32;
                return Some(idx);
            }
        }
        None
    }

    /// 将表达式结果确保在寄存器中: 根据 ExpKind 生成相应 LOAD/MOVE 指令
    fn expr_to_reg(&mut self, e: &ExpDesc) -> i32 {
        match e.kind {
            ExpKind::Void | ExpKind::Nil => {
                let r = self.alloc_reg();
                self.code_abc(OpCode::LOADNIL, r, 0, 0);
                r
            }
            ExpKind::Boolean => {
                let r = self.alloc_reg();
                if e.info != 0 {
                    self.code_abc(OpCode::LOADTRUE, r, 0, 0);
                } else {
                    self.code_abc(OpCode::LOADFALSE, r, 0, 0);
                }
                r
            }
            ExpKind::Int => {
                let r = self.alloc_reg();
                let val = e.info;
                if val <= i16::MAX as i64 && val >= i16::MIN as i64 {
                    self.code_asbx(OpCode::LOADI, r, val as i32);
                } else {
                    let k = self.int_k(val);
                    self.code_abx(OpCode::LOADK, r, k);
                }
                r
            }
            ExpKind::Float => {
                let r = self.alloc_reg();
                let f = f64::from_bits(e.info as u64);
                let fi = f as i64;
                if (fi as f64) == f && fits_sbx(fi) {
                    self.code_asbx(OpCode::LOADF, r, fi as i32);
                } else {
                    let k = self.float_k(f);
                    self.code_abx(OpCode::LOADK, r, k);
                }
                r
            }
            ExpKind::Str => {
                let r = self.alloc_reg();
                self.code_abx(OpCode::LOADK, r, e.info as i32);
                r
            }
            ExpKind::NonReloc => {
                if (e.info as i32) < self.nvarstack() {
                    e.info as i32
                } else {
                    debug_assert!(e.info as i32 == self.freereg - 1);
                    e.info as i32
                }
            }
            ExpKind::Call => {
                if e.info as i32 >= self.nvarstack() && e.info as i32 == self.freereg - 1 {
                    e.info as i32
                } else {
                    let r = self.alloc_reg();
                    self.code_abc(OpCode::MOVE, r, e.info as i32, 0);
                    r
                }
            }
            ExpKind::Relocable | ExpKind::Vararg => {
                if e.info as i32 == self.freereg - 1 {
                    if e.info2 >= 0 {
                        self.set_a(e.info2, e.info as i32);
                    }
                    if e.info2 == -2 {
                        self.code_abc(OpCode::NOT, e.info as i32, e.info as i32, 0);
                    }
                    e.info as i32
                } else if e.info2 >= 0 {
                    let r = self.alloc_reg();
                    self.set_a(e.info2, r);
                    r
                } else {
                    let r = self.alloc_reg();
                    self.code_abc(OpCode::MOVE, r, e.info as i32, 0);
                    r
                }
            }
            ExpKind::VJMP => {
                let r = self.alloc_reg();
                let jmp_pc = e.info as i32;
                
                let mut my_true_list = e.t;
                let my_false_list = e.f;
                
                if jmp_pc != NO_JUMP {
                    if my_true_list == NO_JUMP {
                        my_true_list = jmp_pc;
                    } else {
                        self.concat_jump(&mut my_true_list, jmp_pc);
                    }
                }
                
                if my_true_list != NO_JUMP || my_false_list != NO_JUMP {
                    let p_f = self.code_abc(OpCode::LFALSESKIP, r, 0, 0);
                    let load_true_pc = self.code_abc(OpCode::LOADTRUE, r, 0, 0);
                    let final_pc = self.pc;
                    self.patch_list_aux(my_true_list, final_pc, r, load_true_pc);
                    self.patch_list_aux(my_false_list, final_pc, r, p_f);
                }
                r
            }
        }
    }

    fn exp_to_reg(&mut self, e: &ExpDesc) -> i32 {
        let r = self.expr_to_reg(e);
        if e.kind == ExpKind::NonReloc
            && (e.info as i32) < self.nvarstack()
            && (e.t != NO_JUMP || e.f != NO_JUMP)
        {
            let new_r = self.alloc_reg();
            self.code_abc(OpCode::MOVE, new_r, r, 0);
            self.resolve_jumps(e, new_r);
            new_r
        } else {
            self.resolve_jumps(e, r);
            r
        }
    }

    fn cond_to_reg(&mut self, e: &ExpDesc) -> i32 {
        if matches!(e.kind, ExpKind::Void | ExpKind::Nil) {
            let r = self.alloc_reg();
            self.code_abc(OpCode::LOADFALSE, r, 0, 0);
            r
        } else {
            self.expr_to_reg(e)
        }
    }

    fn resolve_jumps(&mut self, e: &ExpDesc, r: i32) {
        if e.kind != ExpKind::VJMP && (e.t != NO_JUMP || e.f != NO_JUMP) {
            let need_f = self.need_value(e.f);
            let need_t = self.need_value(e.t);
            let p_f;
            let p_t;
            if need_f || need_t {
                let fj = self.jump();
                p_f = self.code_abc(OpCode::LFALSESKIP, r, 0, 0);
                p_t = self.code_abc(OpCode::LOADTRUE, r, 0, 0);
                let fix_to = self.pc;
                self.fix_jump(fj, fix_to, false);
            } else {
                p_f = NO_JUMP;
                p_t = NO_JUMP;
            }
            let final_pc = self.pc;
            self.patch_list_aux(e.f, final_pc, r, p_f);
            self.patch_list_aux(e.t, final_pc, r, p_t);
        }
    }

    fn need_value(&self, list: i32) -> bool {
        let mut cur = list;
        while cur != NO_JUMP {
            if cur < 1 || cur as usize >= self.proto.code.len() {
                return true;
            }
            let ctrl_inst = self.proto.code[(cur - 1) as usize];
            let op = get_opcode(ctrl_inst);
            if op != OpCode::TESTSET {
                return true;
            }
            cur = self.get_jump(cur);
        }
        false
    }

    fn patch_true_jumps(&mut self, list: i32, target: i32) {
        let mut cur = list;
        while cur != NO_JUMP {
            let next = self.get_jump(cur);
            self.patch_test_reg(cur, NO_REG as i32);
            self.fix_jump(cur, target, false);
            cur = next;
        }
    }

    fn patch_false_jumps(&mut self, list: i32, target: i32) {
        let mut cur = list;
        while cur != NO_JUMP {
            let next = self.get_jump(cur);
            self.patch_test_reg(cur, NO_REG as i32);
            self.fix_jump(cur, target, false);
            cur = next;
        }
    }

    fn remove_values(&mut self, list: i32) {
        let mut cur = list;
        while cur != NO_JUMP {
            let next = self.get_jump(cur);
            self.patch_test_reg(cur, NO_REG as i32);
            cur = next;
        }
    }

    fn patch_test_reg(&mut self, node: i32, reg: i32) -> bool {
        if node < 1 || node as usize >= self.proto.code.len() {
            return false;
        }
        let i = self.proto.code[(node - 1) as usize];
        if get_opcode(i) != OpCode::TESTSET {
            return false;
        }
        let b = getarg_b(i);
        if reg != NO_REG as i32 && reg != b {
            setarg(&mut self.proto.code[(node - 1) as usize], reg, POS_A, SIZE_A);
        } else {
            let k = testarg_k(i);
            self.proto.code[(node - 1) as usize] =
                ((OpCode::TEST as u32) << POS_OP)
                | ((b as u32) << POS_A)
                | (if k { 1u32 << POS_K } else { 0 });
        }
        true
    }

    fn patch_list_aux(&mut self, list: i32, vtarget: i32, reg: i32, dtarget: i32) {
        let mut cur = list;
        while cur != NO_JUMP {
            let next = self.get_jump(cur);
            if self.patch_test_reg(cur, reg) {
                self.fix_jump(cur, vtarget, false);
            } else {
                self.fix_jump(cur, dtarget, false);
            }
            cur = next;
        }
    }

    /// 将表达式值移到任意寄存器 (未使用)
    fn discharge_to_any_reg(&mut self, e: &ExpDesc) -> (i32, ExpDesc) {
        let r = self.expr_to_reg(e);
        let mut ne = ExpDesc::new(ExpKind::NonReloc, r as i64);
        ne.info2 = e.info2;
        (r, ne)
    }

    /// 生成 RETURN 指令: 根据 vararg 标志和返回值数量选择 RETURN0/RETURN1/RETURN
    fn return_stat_gen(&mut self, first: i32, nret: i32) {
        let is_vararg = (self.proto.flag & PF_VAHID) != 0;
        let c = if is_vararg { self.proto.num_params as i32 + 1 } else { 0 };
        match nret {
            0 => {
                if is_vararg || self.needclose {
                    self.code_abc_k(OpCode::RETURN, first, 1, c, self.needclose);
                } else {
                    self.code_abc(OpCode::RETURN0, first, 1, 0);
                }
            }
            1 => {
                if is_vararg || self.needclose {
                    self.code_abc_k(OpCode::RETURN, first, 2, c, self.needclose);
                } else {
                    self.code_abc(OpCode::RETURN1, first, 2, 0);
                }
            }
            _ => {
                self.code_abc_k(OpCode::RETURN, first, nret + 1, c, self.needclose);
            }
        }
    }

    /// 计算当前作用域变量栈大小: 遍历 active locals 找出最大 reg+1
    fn nvarstack(&self) -> i32 {
        let mut reglevel = 0;
        for local in &self.locals {
            if local.active && local.kind != RDKCTC {
                reglevel = local.reg + 1;
            }
        }
        reglevel
    }
}

// ============================================================================
// Token utilities
// ============================================================================

/// ANTLR4: 终端匹配 — 检查当前 token 是否与给定 token 类型相同
fn check(fs: &FuncState, t: &Token) -> bool {
    std::mem::discriminant(&fs.ls().token) == std::mem::discriminant(t)
}

/// ANTLR4: 终端匹配+消费 — 检查并消费当前 token
fn test_next(fs: &mut FuncState, t: &Token) -> bool {
    let l = fs.ls_mut();
    if std::mem::discriminant(&l.token) == std::mem::discriminant(t) {
        l.next();
        true
    } else {
        false
    }
}

/// ANTLR4: 终端匹配断言 — 期望当前 token 匹配，否则报错并跳过
fn expect(fs: &mut FuncState, t: &Token) {
    if !check(fs, t) {
        fs.error(&format!("expected {:?}, got {:?}", t, fs.ls().token));
    } else {
        fs.ls_mut().next();
    }
}

/// ANTLR4: 判断是否代码块结束标记 — 'end' | 'else' | 'elseif' | 'until' | EOF
fn block_follow(fs: &FuncState, with_until: bool) -> bool {
    match &fs.ls().token {
        Token::Else | Token::Elseif | Token::End | Token::Eof => true,
        Token::Until if with_until => true,
        _ => false,
    }
}

/// ANTLR4: NAME — 获取标识符名称并消费当前 token
fn get_name(fs: &mut FuncState) -> String {
    match &fs.ls().token {
        Token::Name(s) => {
            let name = s.clone();
            fs.ls_mut().next();
            name
        }
        _ => {
            fs.error(&format!("expected identifier {:?}", fs.ls().token));
            String::new()
        }
    }
}

// ============================================================================
// Parser entry
// ============================================================================

/// ANTLR4: `chunk: block ;` — 解析顶层脚本块，末了生成 RETURN 指令
fn parse_chunk(fs: &mut FuncState) {
    let is_last = block_follow(fs, true);
    if !is_last {
        parse_block(fs);
    }
    let nvarstack = fs.nvarstack();
    fs.return_stat_gen(nvarstack, 0);
}

/// ANTLR4: `block: statement* ;` — 解析代码块语句序列，直到遇到块结束标记
fn parse_block(fs: &mut FuncState) {
    while !block_follow(fs, true) {
        if check(fs, &Token::Return) {
            parse_statement(fs);
            return;
        }
        parse_statement(fs);
    }
}

/// ANTLR4: `attrib: '<' NAME '>' ;` — 获取变量属性标志 (const/close)
fn getvarattribute(fs: &mut FuncState, df: i32) -> i32 {
    if test_next(fs, &Token::Lt) {
        let attr = get_name(fs);
        expect(fs, &Token::Gt);
        match attr.as_str() {
            "const" => RDKCONST,
            "close" => RDKTOCLOSE,
            _ => {
                fs.error(&format!("unknown attribute '{}'", attr));
                df
            }
        }
    } else {
        df
    }
}

/// ANTLR4: 全局变量属性获取 — to-be-closed 报错
fn getglobalattribute(fs: &mut FuncState, df: i32) -> i32 {
    let kind = getvarattribute(fs, df);
    match kind {
        RDKTOCLOSE => {
            fs.error("global variables cannot be to-be-closed");
            kind
        }
        RDKCONST => GDKCONST,
        _ => kind,
    }
}

/// 检查全局变量是否存在: GETTABUP + ERRNNIL
fn checkglobal(fs: &mut FuncState, varname: &str, _line: i32) {
    let r = fs.alloc_reg();
    let k = fs.string_k(varname);
    fs.code_abc(OpCode::GETTABUP, r, 0, k);
    let k_bx = if k >= 256 { 0 } else { k + 1 };
    fs.code_abx(OpCode::ERRNNIL, r, k_bx);
    fs.free_reg();
}

/// ANTLR4: 全局变量声明 — 解析带有 global 前缀的属性变量声明列表
fn globalnames(fs: &mut FuncState, defkind: i32) {
    let mut names: Vec<String> = Vec::new();
    let mut kinds: Vec<i32> = Vec::new();

    loop {
        let name = get_name(fs);
        let kind = getglobalattribute(fs, defkind);
        names.push(name);
        kinds.push(kind);
        if !check(fs, &Token::Comma) { break; }
        fs.ls_mut().next();
    }

    let nvars = names.len();
    let has_init = check(fs, &Token::Eq);

    let mut regs = Vec::new();
        for i in 0..nvars {
            let reg = fs.add_local_kind(&names[i], fs.pc, kinds[i]);
            regs.push(reg);
        }

        if has_init {
        fs.ls_mut().next();
        let mut exps: Vec<ExpDesc> = Vec::new();
        loop {
            let ei = parse_expr(fs);
            exps.push(ei.exp);
            if !check(fs, &Token::Comma) { break; }
            fs.ls_mut().next();
        }
        let nexps = exps.len();

        for i in (0..nvars).rev() {
            if i < nexps {
                let val = &exps[i];
                let k_name = fs.string_k(&names[i]);
                if let Some(k_val) = exp_to_k(fs, val) {
                    fs.code_abc_k(OpCode::SETTABUP, 0, k_name, k_val, true);
                } else {
                    let val_reg = fs.expr_to_reg(val);
                    fs.code_abc(OpCode::SETTABUP, 0, k_name, val_reg);
                    fs.free_reg();
                }
            }
        }
        for i in 0..nvars {
            checkglobal(fs, &names[i], 0);
        }
    }
}

/// ANTLR4: `'global' namelist ;` — 处理 global 变量声明
fn globalstat(fs: &mut FuncState) {
    let defkind = getglobalattribute(fs, GDKREG);
    if !test_next(fs, &Token::Star) {
        globalnames(fs, defkind);
    } else {
        fs.add_local_kind("(global *)", fs.pc, defkind);
    }
}

/// ANTLR4: `'global' 'function' NAME funcbody ;` — 处理 global function
fn globalfunc(fs: &mut FuncState, _line: i32) {
    let fname = get_name(fs);
    fs.add_local_kind(&fname, fs.pc, GDKREG);
    let r = parse_body(fs, None);
    let k = fs.string_k(&fname);
    fs.code_abc(OpCode::SETTABUP, 0, k, r);
    fs.free_reg();
    checkglobal(fs, &fname, _line);
}

/// ANTLR4: global 分发 — 判断 global function 或 global 变量声明
fn globalstatfunc(fs: &mut FuncState, line: i32) {
    fs.ls_mut().next();
    if test_next(fs, &Token::Function) {
        globalfunc(fs, line);
    } else {
        globalstat(fs);
    }
}

/// ANTLR4: `statement: ';' | 'if' ... | 'while' ... | 'do' ... | 'for' ... | 'repeat' ... | 'function' ... | 'local' ... | 'return' ... | functioncall | varlist '=' explist | expr ;`
fn parse_statement(fs: &mut FuncState) {
    fs.assert_regs_clean("parse_statement entry");
    let result = match &fs.ls().token {
        Token::If => { parse_if(fs); None },
        Token::While => { parse_while(fs); None },
        Token::Do => { parse_do(fs); None },
        Token::For => { parse_for(fs); None },
        Token::Repeat => { parse_repeat(fs); None },
        Token::Function => { parse_func_stat(fs); None },
        Token::Local => { parse_local(fs); None },
        Token::Return => { parse_return(fs); Some("return") },
        Token::Semi => { fs.ls_mut().next(); None },
        Token::Break => {
            let jmp_pc = fs.new_break();
            let mut break_list = fs.break_list;
            if break_list == NO_JUMP {
                break_list = jmp_pc;
            } else {
                let mut cur = break_list;
                loop {
                    let next_pc = fs.get_jump(cur);
                    if next_pc == NO_JUMP {
                        break;
                    }
                    cur = next_pc;
                }
                let offset = jmp_pc - cur - 1;
                setarg(&mut fs.proto.code[cur as usize], offset + OFFSET_sJ, POS_SJ, SIZE_sJ);
            }
            fs.break_list = break_list;
            fs.ls_mut().next();
            None
        },
        Token::Name(name) => {
            let line = fs.ls().lastline;
            let is_global = name == "global" && {
                let l = fs.ls_mut();
                let lk = &l.lookahead_next().0;
                matches!(lk, Token::Lt | Token::Name(_) | Token::Star | Token::Function)
            };
            if is_global {
                globalstatfunc(fs, line);
            } else {
                parse_assign_or_call(fs);
                if check(fs, &Token::Semi) { fs.ls_mut().next(); }
            }
            None
        }
        Token::LParen
        | Token::Nil
        | Token::False
        | Token::True
        | Token::Int(_)
        | Token::Float(_)
        | Token::String(_)
        | Token::LBrace
        | Token::Minus
        | Token::Not
        | Token::Hash
        | Token::Tilde => {
            let ei = parse_expr(fs);
            if ei.exp.kind == ExpKind::Call {
                fs.set_c(ei.exp.info2, 1);
            }
            let _r = fs.expr_to_reg(&ei.exp);
            fs.free_reg();
            if check(fs, &Token::Semi) { fs.ls_mut().next(); }
            None
        }
        _ => {
            fs.error(&format!("unexpected token: {:?}", fs.ls().token));
            fs.ls_mut().next();
            None
        }
    };
    if result.is_none() {
        fs.assert_regs_clean("parse_statement exit");
    }
}

// ============================================================================
// Assignments and function calls
// ============================================================================

/// ANTLR4: `varlist '=' explist | functioncall ;` — 解析赋值语句或函数调用
fn parse_assign_or_call(fs: &mut FuncState) {
    let mut first = parse_prefix_exp(fs);
    
    let mut has_call = false;
    let mut extra_free = false;
    let mut freg: i32 = 0;
    if check(fs, &Token::LParen) || check(fs, &Token::Colon) || check(fs, &Token::LBrace) || matches!(&fs.ls().token, Token::String(..)) {
        has_call = true;
        let (fr, ef, _) = load_func(fs, &first);
        freg = fr;
        extra_free = ef;
        let _start_pc = fs.pc;
        parse_func_args(fs, freg);
        loop {
            match &fs.ls().token {
                Token::LParen | Token::LBrace | Token::String(_) | Token::Colon => {
                    parse_func_args(fs, freg);
                }
                _ => break,
            }
        }
        if has_call {
            let last_pc = fs.pc - 1;
            fs.set_c(last_pc, 1);
        }
    }
    
    loop {
        match &fs.ls().token {
            Token::Dot => {
                fs.ls_mut().next();
                let field = get_name(fs);
                let k = fs.string_k(&field);
                let base_reg = if let Some(r) = first.reg { r } else {
                    let r = fs.alloc_reg();
                    if let Some(key) = first.key {
                        fs.code_abc(OpCode::GETTABUP, r, 0, key);
                    }
                    r
                };
                first = PrefixResult {
                    var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                    table_reg: Some(base_reg), table_key: Some(k), table_key_is_const: true,
                    key_allocated_reg: false,
                    allocated_reg: first.allocated_reg || first.reg.is_none(),
                    is_env_upvalue: first.is_env_upvalue,
                    upval_idx: first.upval_idx,
                };
            }
            Token::LBracket => {
                fs.ls_mut().next();
                let base_reg = if let Some(r) = first.reg { r } else {
                    let r = fs.alloc_reg();
                    if let Some(key) = first.key {
                        fs.code_abc(OpCode::GETTABUP, r, 0, key);
                    }
                    r
                };
                let saved_freereg_before = fs.freereg;
                let ei = parse_expr(fs);
                expect(fs, &Token::RBracket);
                let (kr, key_is_const) = if ei.exp.kind == ExpKind::Str {
                    (ei.exp.info as i32, true)
                } else {
                    (fs.expr_to_reg(&ei.exp), false)
                };
                let key_allocated = !key_is_const && fs.freereg > saved_freereg_before;
                first = PrefixResult {
                    var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                    table_reg: Some(base_reg), table_key: Some(kr), table_key_is_const: key_is_const,
                    key_allocated_reg: key_allocated,
                    allocated_reg: first.allocated_reg || first.reg.is_none(),
                    is_env_upvalue: first.is_env_upvalue,
                    upval_idx: first.upval_idx,
                };
            }
            _ => break,
        }
    }
    
    if has_call && !check(fs, &Token::Eq) && !check(fs, &Token::Comma) {
        fs.set_freereg(fs.nvarstack());
        return;
    }
    
    if check(fs, &Token::Eq) || check(fs, &Token::Comma) {
        let mut vars = vec![first];
        while check(fs, &Token::Comma) {
            fs.ls_mut().next();
            vars.push(parse_prefix_exp(fs));
        }
        expect(fs, &Token::Eq);
        
        let mut exps: Vec<ExpDesc> = Vec::new();
        loop {
            let ei = parse_expr(fs);
            let has_comma = check(fs, &Token::Comma);
            if has_comma {
                 let r = fs.expr_to_reg(&ei.exp);
                exps.push(ExpDesc::new(ExpKind::NonReloc, r as i64));
                fs.ls_mut().next();
            } else {
                exps.push(ei.exp);
                break;
            }
        }
        
        for i in (0..vars.len()).rev() {
            if i < exps.len() {
                let v = &vars[i];
                let val = &exps[i];
                if let (Some(table_reg), Some(table_key)) = (v.table_reg, v.table_key) {
                    let can_settabup = v.is_env_upvalue && v.table_key_is_const;
                    if can_settabup {
                        let gettabup_inst = fs.proto.code.pop().unwrap();
                        fs.pc -= 1;
                        let env_k = getarg_c(gettabup_inst);
                        fs.proto.constants.remove(env_k as usize);
                        let adjusted_key = if (env_k as i32) < table_key { table_key - 1 } else { table_key };
                        if let Some(k_val) = exp_to_k(fs, val) {
                            fs.code_abc_k(OpCode::SETTABUP, 0, adjusted_key, k_val, true);
                        } else {
                            let val_reg = fs.expr_to_reg(val);
                            fs.code_abc(OpCode::SETTABUP, 0, adjusted_key, val_reg);
                            if val_reg >= fs.nvarstack() {
                                fs.free_reg();
                            }
                        }
                        if v.allocated_reg {
                            fs.free_reg();
                        }
                    } else {
                        let set_op = if v.table_key_is_const { OpCode::SETFIELD } else { OpCode::SETTABLE };
                        if let Some(k_val) = exp_to_k(fs, val) {
                            fs.code_abc_k(set_op, table_reg, table_key, k_val, true);
                        } else {
                            let val_reg = fs.expr_to_reg(val);
                            fs.code_abc_k(set_op, table_reg, table_key, val_reg, false);
                            if val_reg >= fs.nvarstack() {
                                fs.free_reg();
                            }
                        }
                        if v.key_allocated_reg {
                            fs.free_reg();
                        }
                        if v.allocated_reg {
                            fs.free_reg();
                        }
                    }
                } else if let Some(upval_idx) = v.upval_idx {
                    let val_reg = fs.expr_to_reg(val);
                    fs.code_abc(OpCode::SETUPVAL, val_reg, upval_idx, 0);
                    fs.free_reg();
                    if v.allocated_reg {
                        fs.free_reg();
                    }
                } else if let Some(ref name) = v.var_name {
                    let k_name = fs.string_k(name);
                    if let Some(k_val) = exp_to_k(fs, val) {
                        fs.code_abc_k(OpCode::SETTABUP, 0, k_name, k_val, true);
                    } else {
                        let val_reg = fs.expr_to_reg(val);
                        fs.code_abc(OpCode::SETTABUP, 0, k_name, val_reg);
                        if val_reg >= fs.nvarstack() {
                            fs.free_reg();
                        }
                    }
                } else if let Some(idx) = v.local_idx {
                    if i == exps.len() - 1 {
                        store_expr_to_local(fs, val, idx);
                    } else {
                        let val_reg = val.info as i32;
                        if idx != val_reg {
                            fs.code_abc(OpCode::MOVE, idx, val_reg, 0);
                        }
                        fs.free_reg();
                    }
                }
            }
        }
        return;
    }
    
    let (_r, _, _) = load_func(fs, &first);
    fs.free_reg();
}

/// 将常量表达式转换为常量表索引 (≤255 则返回)
fn exp_to_k(fs: &mut FuncState, e: &ExpDesc) -> Option<i32> {
    let info = match e.kind {
        ExpKind::Int => fs.int_k(e.info),
        ExpKind::Float => {
            let f = f64::from_bits(e.info as u64);
            fs.float_k(f)
        }
        ExpKind::Str => e.info as i32,
        ExpKind::Boolean => {
            let tv = if e.info != 0 { TValue::Boolean(true) } else { TValue::Boolean(false) };
            fs.const_k(tv)
        }
        ExpKind::Nil => fs.const_k(TValue::Nil(NilKind::Strict)),
        _ => return None,
    };
    if info <= 255 { Some(info) } else { None }
}

fn store_expr_to_local(fs: &mut FuncState, e: &ExpDesc, dest: i32) {
    match e.kind {
        ExpKind::Void | ExpKind::Nil => {
            fs.code_abc(OpCode::LOADNIL, dest, 0, 0);
        }
        ExpKind::Boolean => {
            if e.info != 0 {
                fs.code_abc(OpCode::LOADTRUE, dest, 0, 0);
            } else {
                fs.code_abc(OpCode::LOADFALSE, dest, 0, 0);
            }
        }
        ExpKind::Int => {
            let v = e.info;
            if v <= i16::MAX as i64 && v >= i16::MIN as i64 {
                fs.code_asbx(OpCode::LOADI, dest, v as i32);
            } else {
                let k = fs.int_k(v);
                fs.code_abx(OpCode::LOADK, dest, k);
            }
        }
        ExpKind::Float => {
            let f = f64::from_bits(e.info as u64);
            let fi = f as i64;
            if (fi as f64) == f && fits_sbx(fi) {
                fs.code_asbx(OpCode::LOADF, dest, fi as i32);
            } else {
                let k = fs.float_k(f);
                fs.code_abx(OpCode::LOADK, dest, k);
            }
        }
        ExpKind::Str => {
            fs.code_abx(OpCode::LOADK, dest, e.info as i32);
        }
        ExpKind::VJMP => {
            let jmp_pc = e.info as i32;

            let mut my_true_list = e.t;
            let my_false_list = e.f;

            if jmp_pc != NO_JUMP {
                if my_true_list == NO_JUMP {
                    my_true_list = jmp_pc;
                } else {
                    fs.concat_jump(&mut my_true_list, jmp_pc);
                }
            }

            if my_true_list != NO_JUMP || my_false_list != NO_JUMP {
                let p_f = fs.code_abc(OpCode::LFALSESKIP, dest, 0, 0);
                let load_true_pc = fs.code_abc(OpCode::LOADTRUE, dest, 0, 0);
                let final_pc = fs.pc;
                fs.patch_list_aux(my_true_list, final_pc, dest, load_true_pc);
                fs.patch_list_aux(my_false_list, final_pc, dest, p_f);
            }
        }
        _ => {
            if e.info2 >= 0 {
                if e.kind == ExpKind::Call {
                    if e.info as i32 != dest {
                        fs.code_abc(OpCode::MOVE, dest, e.info as i32, 0);
                        fs.free_reg();
                    }
                } else {
                    let prev_dest = e.info as i32;
                    fs.set_a(e.info2, dest);
                    if prev_dest != dest && (prev_dest >= fs.nvarstack() || prev_dest == fs.freereg - 1) {
                        fs.free_reg();
                    }
                }
            } else {
                let saved_freereg = fs.freereg;
                let val_reg = fs.expr_to_reg(e);
                if dest != val_reg {
                    fs.code_abc(OpCode::MOVE, dest, val_reg, 0);
                }
                if fs.freereg > saved_freereg || dest != val_reg {
                    fs.free_reg();
                }
            }
        }
    }
}

/// ANTLR4: functioncall 帮助 — 将函数值加载到寄存器以便调用
/// 返回 (函数寄存器, 是否需要额外释放基寄存器)
fn load_func(fs: &mut FuncState, p: &PrefixResult) -> (i32, bool, bool) {
    if let (Some(table_reg), Some(table_key)) = (p.table_reg, p.table_key) {
        if p.table_key_is_const {
            fs.code_abc(OpCode::GETFIELD, table_reg, table_reg, table_key);
            (table_reg, false, false)
        } else {
            let r = fs.alloc_reg();
            fs.code_abc(OpCode::GETTABLE, r, table_reg, table_key);
            (r, true, true)
        }
    } else if let Some(reg) = p.local_idx {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::MOVE, r, reg, 0);
        (r, false, true)
    } else if let Some(key) = p.key {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::GETTABUP, r, 0, key);
        (r, false, true)
    } else if let Some(reg) = p.reg {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::MOVE, r, reg, 0);
        (r, false, true)
    } else {
        (fs.alloc_reg(), false, true)
    }
}

/// ANTLR4: `args: '(' explist? ')' | tableconstructor | STRING ;` 及 `':' NAME args ;` — 解析函数参数并生成 CALL 指令
fn parse_func_args(fs: &mut FuncState, freg: i32) -> i32 {
    if matches!(&fs.ls().token, Token::String(..)) {
        let str_s = match &fs.ls().token {
            Token::String(s) => s.clone(),
            _ => String::new(),
        };
        fs.ls_mut().next();
        let k = fs.string_k(&str_s);
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        let pc = fs.code_abc(OpCode::CALL, freg, 2, 2);
        fs.free_reg();
        return pc;
    }
    
    if check(fs, &Token::LBrace) {
        let (tr, _n) = parse_constructor(fs);
        if freg + 1 != tr {
            fs.code_abc(OpCode::MOVE, freg + 1, tr, 0);
            fs.free_reg();
        }
        let pc = fs.code_abc(OpCode::CALL, freg, 2, 2);
        return pc;
    }
    
    if check(fs, &Token::Colon) {
        fs.ls_mut().next();
        let method = get_name(fs);
        let k = fs.string_k(&method);
        fs.code_abc(OpCode::GETTABLE, freg + 1, freg, k);
        if check(fs, &Token::LParen) {
            fs.ls_mut().next();
            let (na, na_multret) = parse_args(fs);
            expect(fs, &Token::RParen);
            let na_adj = if na_multret { 0 } else { na + 1 };
            let pc = fs.code_abc(OpCode::CALL, freg, na_adj, 2);
            for _ in 0..na {
                fs.free_reg();
            }
            return pc;
        }
        return -1;
    }
    
    if check(fs, &Token::LParen) {
        fs.ls_mut().next();
        let (nparams, nparams_multret) = parse_args(fs);
        expect(fs, &Token::RParen);
        let nparams_adj = if nparams_multret { 0 } else { nparams + 1 };
        let pc = fs.code_abc(OpCode::CALL, freg, nparams_adj, 2);
        for _ in 0..nparams {
            fs.free_reg();
        }
        return pc;
    }
    -1
}

/// ANTLR4: `explist: expr (',' expr)* ;` — 解析函数调用参数列表
fn parse_args(fs: &mut FuncState) -> (i32, bool) {
    if check(fs, &Token::RParen) || check(fs, &Token::RBrace) {
        return (0, false);
    }
    let ei = parse_expr(fs);
    let mut last_is_call = ei.exp.kind == ExpKind::Call;
    let mut last_call_pc = if last_is_call { ei.exp.info2 } else { -1 };
    let _r = if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() {
        if ei.exp.info2 >= 0 {
            fs.set_a(ei.exp.info2, ei.exp.info as i32);
        }
        ei.exp.info as i32
    } else {
        fs.exp_to_reg(&ei.exp)
    };
    if matches!(ei.exp.kind, ExpKind::NonReloc) && (ei.exp.info as i32) < fs.nvarstack() {
        let target = fs.alloc_reg();
        if ei.exp.info as i32 != target {
            fs.code_abc(OpCode::MOVE, target, ei.exp.info as i32, 0);
        }
    }
    let mut n = 1;
    while check(fs, &Token::Comma) {
        fs.ls_mut().next();
        let ei2 = parse_expr(fs);
        last_is_call = ei2.exp.kind == ExpKind::Call;
        last_call_pc = if last_is_call { ei2.exp.info2 } else { -1 };
        let _r2 = if matches!(ei2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei2.exp.has_jumps() {
            if ei2.exp.info2 >= 0 {
                fs.set_a(ei2.exp.info2, ei2.exp.info as i32);
            }
            ei2.exp.info as i32
        } else {
            fs.exp_to_reg(&ei2.exp)
        };
        if matches!(ei2.exp.kind, ExpKind::NonReloc) && (ei2.exp.info as i32) < fs.nvarstack() {
            let target = fs.alloc_reg();
            if ei2.exp.info as i32 != target {
                fs.code_abc(OpCode::MOVE, target, ei2.exp.info as i32, 0);
            }
        }
        n += 1;
    }
    if last_is_call {
        fs.set_c(last_call_pc, 0);
    }
    (n, last_is_call)
}

#[derive(Debug, Clone)]
struct PrefixResult {
    var_name: Option<String>,
    local_idx: Option<i32>,
    key: Option<i32>,
    reg: Option<i32>,
    table_reg: Option<i32>,
    table_key: Option<i32>,
    table_key_is_const: bool,
    key_allocated_reg: bool,
    allocated_reg: bool,
    is_env_upvalue: bool,
    upval_idx: Option<i32>,
}

/// ANTLR4: `prefixexp: varOrExp | functioncall | '(' expr ')' ;` 以及 `var: NAME | prefixexp '[' expr ']' | prefixexp '.' NAME ;`
fn parse_prefix_exp(fs: &mut FuncState) -> PrefixResult {
    match &fs.ls().token {
        Token::Name(name) => {
            let name = name.clone();
            fs.ls_mut().next();
            let mut result = if let Some(reg) = fs.find_local(&name) {
                PrefixResult { var_name: None, local_idx: Some(reg), key: None, reg: Some(reg), table_reg: None, table_key: None, table_key_is_const: false, key_allocated_reg: false, allocated_reg: false, is_env_upvalue: false, upval_idx: None }
            } else if let Some(upval_idx) = fs.find_upvalue(&name) {
                let r = fs.alloc_reg();
                fs.code_abc(OpCode::GETUPVAL, r, upval_idx, 0);
                PrefixResult { var_name: Some(name), local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, key_allocated_reg: false, allocated_reg: false, is_env_upvalue: false, upval_idx: Some(upval_idx) }
            } else {
                let k = fs.string_k(&name);
                let is_env = name == "_ENV";
                PrefixResult { var_name: Some(name), local_idx: None, key: Some(k), reg: None, table_reg: None, table_key: None, table_key_is_const: false, key_allocated_reg: false, allocated_reg: false, is_env_upvalue: is_env, upval_idx: None }
            };

            loop {
                match &fs.ls().token {
                    Token::Dot => {
                        fs.ls_mut().next();
                        let field = get_name(fs);
                        let k = fs.string_k(&field);
                        let base_reg = if let Some(r) = result.reg {
                            r
                        } else {
                            let r = fs.alloc_reg();
                            let gk = result.key.unwrap_or(0);
                            fs.code_abc(OpCode::GETTABUP, r, 0, gk);
                            r
                        };
                        result = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                        table_reg: Some(base_reg), table_key: Some(k), table_key_is_const: true,
                        key_allocated_reg: false,
                        allocated_reg: result.allocated_reg || result.reg.is_none(),
                        is_env_upvalue: result.is_env_upvalue,
                        upval_idx: result.upval_idx,
                    };
                }
                Token::LBracket => {
                    fs.ls_mut().next();
                    let base_reg = if let Some(r) = result.reg {
                        r
                    } else {
                        let r = fs.alloc_reg();
                        let gk = result.key.unwrap_or(0);
                        fs.code_abc(OpCode::GETTABUP, r, 0, gk);
                        r
                    };
                    let saved_freereg_before = fs.freereg;
                    let ei = parse_expr(fs);
                    expect(fs, &Token::RBracket);
                    let (kr, key_is_const) = if ei.exp.kind == ExpKind::Str {
                        (ei.exp.info as i32, true)
                    } else if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() {
                        if ei.exp.info2 >= 0 {
                            fs.set_a(ei.exp.info2, ei.exp.info as i32);
                        }
                        (ei.exp.info as i32, false)
                    } else {
                        (fs.expr_to_reg(&ei.exp), false)
                    };
                    let key_allocated = !key_is_const && fs.freereg > saved_freereg_before;
                    result = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                        table_reg: Some(base_reg), table_key: Some(kr), table_key_is_const: key_is_const,
                        key_allocated_reg: key_allocated,
                        allocated_reg: result.allocated_reg || result.reg.is_none(),
                        is_env_upvalue: result.is_env_upvalue,
                        upval_idx: result.upval_idx,
                    };
                    }
                    _ => break,
                }
            }

            result
        }
        Token::LParen => {
            fs.ls_mut().next();
            let e = parse_expr(fs);
            expect(fs, &Token::RParen);
            let r = fs.expr_to_reg(&e.exp);
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, key_allocated_reg: false, allocated_reg: true, is_env_upvalue: false, upval_idx: None }
        }
        _ => {
            let e = parse_simple_exp(fs);
            let r = fs.expr_to_reg(&e.exp);
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, key_allocated_reg: false, allocated_reg: true, is_env_upvalue: false, upval_idx: None }
        }
    }
}

// ============================================================================
// Expressions (Pratt)
// ============================================================================

#[derive(Debug, Clone)]
struct ExprItem {
    exp: ExpDesc,
}

/// ANTLR4: `expr: simpleExp | expr binop expr | unop expr ;` — 表达式解析入口，调用 Pratt 解析器
fn parse_expr(fs: &mut FuncState) -> ExprItem {
    parse_subexpr(fs, 0)
}

const PREC_OR: i32 = 1;
const PREC_AND: i32 = 2;
const PREC_COMP: i32 = 3;
const PREC_BOR: i32 = 4;
const PREC_BXOR: i32 = 5;
const PREC_BAND: i32 = 6;
const PREC_SHL: i32 = 7;
const PREC_CONCAT: i32 = 9;
const PREC_ADD: i32 = 10;
const PREC_MUL: i32 = 11;
const PREC_UNARY: i32 = 12;
const PREC_POW: i32 = 14;

/// ANTLR4: `expr: expr binop expr ;` — Pratt 递归下降二元表达式解析器
fn parse_subexpr(fs: &mut FuncState, limit: i32) -> ExprItem {
    let mut e = parse_simple_exp(fs);
    
    loop {
        let mut matched = false;
        
        if limit <= PREC_AND && check(fs, &Token::And) {
            let mut e_left = e.exp.clone();
            fs.ls_mut().next();
            
            if e_left.kind == ExpKind::VJMP && e_left.info != NO_JUMP as i64 {
                let saved_jmp = e_left.info as i32;
                fs.negate_condition(saved_jmp);
                fs.concat_jump(&mut e_left.f, saved_jmp);
                let here = fs.pc;
                fs.patch_true_jumps(e_left.t, here);
                e_left.t = NO_JUMP;
            } else {
                let skip_test =
                    matches!(e_left.kind, ExpKind::Boolean if e_left.info != 0)
                    || matches!(e_left.kind, ExpKind::Int | ExpKind::Float | ExpKind::Str);

                if skip_test {
                    let here = fs.pc;
                    fs.patch_true_jumps(e_left.t, here);
                    e_left.t = NO_JUMP;
                } else {
                    let reg_alloc = !matches!(e_left.kind, ExpKind::NonReloc | ExpKind::Relocable);
                    let reg = if reg_alloc {
                        fs.expr_to_reg(&e_left)
                    } else {
                        e_left.info as i32
                    };
                    let k = e_left.info2 == -2;
                    if k {
                        fs.code_abc_k(OpCode::TEST, reg, 0, 0, true);
                    } else {
                        fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, reg, 0, false);
                    }
                    let jmp_pc = fs.jump();
                    fs.concat_jump(&mut e_left.f, jmp_pc);
                    let here = fs.pc;
                    fs.patch_true_jumps(e_left.t, here);
                    e_left.t = NO_JUMP;
                    if reg_alloc || matches!(e_left.kind, ExpKind::Relocable) { fs.free_reg(); }
                }
            }
            
            let e2 = parse_subexpr(fs, PREC_AND + 1);
            let mut e2_exp = e2.exp.clone();
            fs.concat_jump(&mut e2_exp.f, e_left.f);
            
            e = ExprItem { exp: e2_exp };
            matched = true;
        }
        
        if limit <= PREC_OR && check(fs, &Token::Or) {
            let mut e_left = e.exp.clone();
            fs.ls_mut().next();
            
            if e_left.kind == ExpKind::VJMP && e_left.info != NO_JUMP as i64 {
                let saved_jmp = e_left.info as i32;
                fs.concat_jump(&mut e_left.t, saved_jmp);
                let here = fs.pc;
                fs.patch_false_jumps(e_left.f, here);
                e_left.f = NO_JUMP;
            } else {
                let skip_test =
                    matches!(e_left.kind, ExpKind::Nil | ExpKind::Boolean if e_left.info == 0);

                if skip_test {
                    let here = fs.pc;
                    fs.patch_false_jumps(e_left.f, here);
                    e_left.f = NO_JUMP;
                } else {
                    let reg_alloc = !matches!(e_left.kind, ExpKind::NonReloc | ExpKind::Relocable);
                    let reg = if reg_alloc {
                        fs.expr_to_reg(&e_left)
                    } else {
                        e_left.info as i32
                    };
                    let k = e_left.info2 == -2;
                    if k {
                        fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, reg, 0, k);
                    } else {
                        fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, reg, 0, true);
                    }
                    let jmp_pc = fs.jump();
                    fs.concat_jump(&mut e_left.t, jmp_pc);
                    let here = fs.pc;
                    fs.patch_false_jumps(e_left.f, here);
                    e_left.f = NO_JUMP;
                    if reg_alloc || matches!(e_left.kind, ExpKind::Relocable) { fs.free_reg(); }
                }
            }
            
            let e2 = parse_subexpr(fs, PREC_AND);
            let mut e2_exp = e2.exp.clone();
            fs.concat_jump(&mut e2_exp.t, e_left.t);
            
            e = ExprItem { exp: e2_exp };
            matched = true;
        }
        
        if limit <= PREC_COMP && check_compare(fs) {
            let ec = e.exp.clone();
            let op_tok = fs.ls().token.clone();
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_COMP + 1);
            let is_gt = matches!(op_tok, Token::Gt | Token::GtEq);
            let is_eq = matches!(op_tok, Token::EqEq | Token::TildeEq);
            let k = if matches!(op_tok, Token::TildeEq) { 0 } else { 1 };

            let sc_imm = if is_eq || is_gt {
                is_sc_number(&ec)
            } else {
                is_sc_number(&e2.exp)
            };

            if let Some(sc_val) = sc_imm {
                let (reg, imm, reg_alloc) = if is_eq {
                    let alloc = !matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc);
                    let reg = if alloc {
                        fs.exp_to_reg(&e2.exp)
                    } else if e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        fs.resolve_jumps(&e2.exp, reg);
                        reg
                    } else {
                        e2.exp.info as i32
                    };
                    (reg, int_to_sc(sc_val), alloc)
                } else if is_gt {
                    let alloc = !matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc);
                    let reg = if alloc {
                        fs.exp_to_reg(&e2.exp)
                    } else if e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        fs.resolve_jumps(&e2.exp, reg);
                        reg
                    } else {
                        e2.exp.info as i32
                    };
                    (reg, int_to_sc(sc_val), alloc)
                } else {
                    let alloc = !matches!(ec.kind, ExpKind::Relocable | ExpKind::NonReloc);
                    let reg = if alloc {
                        fs.exp_to_reg(&ec)
                    } else if ec.has_jumps() {
                        let reg = ec.info as i32;
                        fs.resolve_jumps(&ec, reg);
                        reg
                    } else {
                        ec.info as i32
                    };
                    (reg, int_to_sc(sc_val), alloc)
                };
                let imm_op = match op_tok {
                    Token::EqEq | Token::TildeEq => OpCode::EQI,
                    Token::Lt | Token::Gt => OpCode::LTI,
                    Token::LtEq | Token::GtEq => OpCode::LEI,
                    _ => OpCode::EQI,
                };
                fs.code_abc_k(imm_op, reg, imm, 0, k != 0);
                let jmp_pc = fs.jump();
                if reg_alloc || matches!(ec.kind, ExpKind::Relocable) || matches!(e2.exp.kind, ExpKind::Relocable) {
                    fs.free_reg();
                }
                e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
            } else {
                let is_eq_op = matches!(op_tok, Token::EqEq);
                if is_eq {
                    let ec_const_k = if !ec.has_jumps() {
                        exp_to_const_k(fs, &ec)
                    } else {
                        None
                    };
                    if let Some(k_idx) = ec_const_k {
                        let r2_alloc = !matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc);
                        let r2 = if r2_alloc {
                            fs.exp_to_reg(&e2.exp)
                        } else {
                            e2.exp.info as i32
                        };
                        fs.code_abc_k(OpCode::EQK, r2, k_idx, 0, is_eq_op);
                        let jmp_pc = fs.jump();
                        if r2_alloc || matches!(e2.exp.kind, ExpKind::Relocable) { fs.free_reg(); }
                        e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                    } else {
                        let r_alloc = !matches!(ec.kind, ExpKind::Relocable | ExpKind::NonReloc);
                        let r = if r_alloc {
                            fs.exp_to_reg(&ec)
                        } else if ec.has_jumps() {
                            let reg = ec.info as i32;
                            fs.resolve_jumps(&ec, reg);
                            reg
                        } else {
                            ec.info as i32
                        };
                        if matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) {
                            let sc = int_to_sc(e2.exp.info);
                            fs.code_abc_k(OpCode::EQI, r, sc, 0, is_eq_op);
                            let jmp_pc = fs.jump();
                            if r_alloc || matches!(ec.kind, ExpKind::Relocable) { fs.free_reg(); }
                            e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                        } else {
                            let const_k = exp_to_const_k(fs, &e2.exp);
                            if let Some(k_idx) = const_k {
                                fs.code_abc_k(OpCode::EQK, r, k_idx, 0, is_eq_op);
                                let jmp_pc = fs.jump();
                                if r_alloc || matches!(ec.kind, ExpKind::Relocable) { fs.free_reg(); }
                                e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                fs.code_abc_k(OpCode::EQ, r, r2, 0, is_eq_op);
                                let jmp_pc = fs.jump();
                                if !(matches!(e2.exp.kind, ExpKind::NonReloc) && (e2.exp.info as i32) < fs.nvarstack()) {
                                    fs.free_reg();
                                }
                                if r_alloc || matches!(ec.kind, ExpKind::Relocable) { fs.free_reg(); }
                                e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                            }
                        }
                    }
                } else {
                    let r_alloc = !matches!(ec.kind, ExpKind::Relocable | ExpKind::NonReloc);
                    let r = if r_alloc {
                        fs.exp_to_reg(&ec)
                    } else if ec.has_jumps() {
                        let reg = ec.info as i32;
                        fs.resolve_jumps(&ec, reg);
                        reg
                    } else {
                        ec.info as i32
                    };
                    let r2_alloc = !matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc);
                    let r2 = if r2_alloc {
                        fs.exp_to_reg(&e2.exp)
                    } else if e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        fs.resolve_jumps(&e2.exp, reg);
                        reg
                    } else {
                        e2.exp.info as i32
                    };
                    let (b, c) = if is_gt { (r2, r) } else { (r, r2) };
                    let (op, k) = match op_tok {
                        Token::EqEq => (OpCode::EQ, 1),
                        Token::TildeEq => (OpCode::EQ, 0),
                        Token::Lt | Token::Gt => (OpCode::LT, 1),
                        Token::LtEq | Token::GtEq => (OpCode::LE, 1),
                        _ => (OpCode::EQ, 1),
                    };
                    fs.code_abc_k(op, b, c, 0, k != 0);
                    let jmp_pc = fs.jump();
                    if r2_alloc || matches!(e2.exp.kind, ExpKind::Relocable) { fs.free_reg(); }
                    if r_alloc || matches!(ec.kind, ExpKind::Relocable) { fs.free_reg(); }
                    e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                }
            }
            matched = true;
        }
        
        if limit <= PREC_BOR && check(fs, &Token::Pipe) {
            let mut ec = e.exp.clone();
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_BOR + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let val = ec.info | e2.exp.info;
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                }
                _ => {
                    let r = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, ec.info as i32);
                        }
                        ec.info as i32
                    } else {
                        fs.expr_to_reg(&ec)
                    };
                    let k_idx = match &e2.exp.kind {
                        ExpKind::Int => {
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 { Some(k) } else { None }
                        }
                        _ => None,
                    };
                    if let Some(k) = k_idx {
                        let dest = fs.alloc_reg();
                        fs.code_abc(OpCode::BORK, dest, r, k);
                        fs.code_abc(OpCode::MMBINK, r, k, 14);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else {
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        fs.code_abc(OpCode::BOR, r, r, r2);
                        let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                        if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                            if r2 == fs.freereg - 1 && r2 != r {
                                fs.free_reg();
                            }
                        }
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_BXOR && check(fs, &Token::Tilde) {
            let mut ec = e.exp.clone();
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_BXOR + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) => {
                    let val = ec.info ^ e2.exp.info;
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                }
                _ => {
                    let r = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, ec.info as i32);
                        }
                        ec.info as i32
                    } else {
                        fs.expr_to_reg(&ec)
                    };
                    let k_idx = match &e2.exp.kind {
                        ExpKind::Int => {
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 { Some(k) } else { None }
                        }
                        _ => None,
                    };
                    if let Some(k) = k_idx {
                        let dest = fs.alloc_reg();
                        fs.code_abc(OpCode::BXORK, dest, r, k);
                        fs.code_abc(OpCode::MMBINK, r, k, 15);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else {
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        fs.code_abc(OpCode::BXOR, r, r, r2);
                        let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                        if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                            if r2 == fs.freereg - 1 && r2 != r {
                                fs.free_reg();
                            }
                        }
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_BAND && check(fs, &Token::Ampersand) {
            let mut ec = e.exp.clone();
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_BAND + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) => {
                    let val = ec.info & e2.exp.info;
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                }
                _ => {
                    let r = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, ec.info as i32);
                        }
                        ec.info as i32
                    } else {
                        fs.expr_to_reg(&ec)
                    };
                    let k_idx = match &e2.exp.kind {
                        ExpKind::Int => {
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 { Some(k) } else { None }
                        }
                        _ => None,
                    };
                    if let Some(k) = k_idx {
                        let dest = fs.alloc_reg();
                        fs.code_abc(OpCode::BANDK, dest, r, k);
                        fs.code_abc(OpCode::MMBINK, r, k, 13);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else {
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        fs.code_abc(OpCode::BAND, r, r, r2);
                        let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                        if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                            if r2 == fs.freereg - 1 && r2 != r {
                                fs.free_reg();
                            }
                        }
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_SHL && check(fs, &Token::LtLt) {
            let mut ec = e.exp.clone();
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_SHL + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) => {
                    let val = ec.info.wrapping_shl(e2.exp.info as u32);
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                }
                _ => {
                    if matches!(ec.kind, ExpKind::Int) && fits_sc(&ec) {
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.expr_to_reg(&e2.exp)
                        };
                        let dest = fs.alloc_reg();
                        let sc = int_to_sc(ec.info);
                        fs.code_abc(OpCode::SHLI, dest, r2, sc);
                        fs.code_abc(OpCode::MMBINI, r2, sc, 16);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else if matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) && fits_sc_neg(e2.exp.info) {
                        let r = if ec.has_jumps() {
                            let r = fs.exp_to_reg(&ec);
                            ec.t = NO_JUMP;
                            ec.f = NO_JUMP;
                            r
                        } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                            if ec.info2 >= 0 {
                                fs.set_a(ec.info2, ec.info as i32);
                            }
                            ec.info as i32
                        } else {
                            fs.expr_to_reg(&ec)
                        };
                        let v = e2.exp.info;
                        let dest = fs.alloc_reg();
                        let sc_neg = int_to_sc(-v);
                        let sc_pos = int_to_sc(v);
                        fs.code_abc(OpCode::SHRI, dest, r, sc_neg);
                        fs.code_abc(OpCode::MMBINI, r, sc_pos, 16);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else {
                        let r = if ec.has_jumps() {
                            let r = fs.exp_to_reg(&ec);
                            ec.t = NO_JUMP;
                            ec.f = NO_JUMP;
                            r
                        } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                            if ec.info2 >= 0 {
                                fs.set_a(ec.info2, ec.info as i32);
                            }
                            ec.info as i32
                        } else {
                            fs.expr_to_reg(&ec)
                        };
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.expr_to_reg(&e2.exp)
                        };
                        fs.code_abc(OpCode::SHL, r, r, r2);
                        let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                        if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                            if r2 == fs.freereg - 1 && r2 != r {
                                fs.free_reg();
                            }
                        }
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_SHL && check(fs, &Token::GtGt) {
            let mut ec = e.exp.clone();
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_SHL + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) => {
                    let val = ec.info.wrapping_shr(e2.exp.info as u32);
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                }
                _ => {
                    let r = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, ec.info as i32);
                        }
                        ec.info as i32
                    } else {
                        fs.expr_to_reg(&ec)
                    };
                    if matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) {
                        let v = e2.exp.info;
                        let dest = fs.alloc_reg();
                        let sc = int_to_sc(v);
                        fs.code_abc(OpCode::SHRI, dest, r, sc);
                        fs.code_abc(OpCode::MMBINI, r, sc, 17);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else {
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.expr_to_reg(&e2.exp)
                        };
                        fs.code_abc(OpCode::SHR, r, r, r2);
                        let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                        if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                            if r2 == fs.freereg - 1 && r2 != r {
                                fs.free_reg();
                            }
                        }
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_CONCAT && check(fs, &Token::DotDot) {
            let mut ec = e.exp.clone();
            let mut r = if ec.has_jumps() {
                let r = fs.exp_to_reg(&ec);
                ec.t = NO_JUMP;
                ec.f = NO_JUMP;
                r
            } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                if ec.info2 >= 0 {
                    fs.set_a(ec.info2, ec.info as i32);
                }
                ec.info as i32
            } else {
                fs.expr_to_reg(&ec)
            };
            if r < fs.nvarstack() {
                let new_r = fs.alloc_reg();
                fs.code_abc(OpCode::MOVE, new_r, r, 0);
                r = new_r;
            }
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_CONCAT);
            let _r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                let reg = e2.exp.info as i32;
                if e2.exp.info2 >= 0 {
                    fs.set_a(e2.exp.info2, reg);
                }
                reg
            } else {
                fs.exp_to_reg(&e2.exp)
            };
            fs.free_reg();
            let merged = if !fs.proto.code.is_empty() {
                let last = fs.proto.code[fs.proto.code.len() - 1];
                get_opcode(last) == OpCode::CONCAT && getarg_a(last) as i32 == r + 1
            } else {
                false
            };
            if merged {
                let n = getarg_b(fs.proto.code[fs.proto.code.len() - 1]);
                fs.proto.code.pop();
                fs.pc -= 1;
                fs.code_abc(OpCode::CONCAT, r, n + 1, 0);
            } else {
                fs.code_abc(OpCode::CONCAT, r, 2, 0);
            }
            e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
            matched = true;
        }
        
        if limit <= PREC_ADD && check_addop(fs) {
            let mut ec = e.exp.clone();
            let is_add = check(fs, &Token::Plus);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_ADD + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let val = if is_add { ec.info + e2.exp.info } else { ec.info - e2.exp.info };
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                }
                (ExpKind::Float, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f = f64::from_bits(ec.info as u64);
                    let val = if is_add { f + (e2.exp.info as f64) } else { f - (e2.exp.info as f64) };
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                }
                (ExpKind::Int, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f = f64::from_bits(e2.exp.info as u64);
                    let val = if is_add { (ec.info as f64) + f } else { (ec.info as f64) - f };
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                }
                (ExpKind::Float, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f1 = f64::from_bits(ec.info as u64);
                    let f2 = f64::from_bits(e2.exp.info as u64);
                    let val = if is_add { f1 + f2 } else { f1 - f2 };
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                }
                (ExpKind::Int, _) => {
                    if is_add {
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        if fits_sc(&ec) {
                            let sc = int_to_sc(ec.info);
                            let pc = fs.code_abc(OpCode::ADDI, r2, r2, sc);
                            fs.code_abc_k(OpCode::MMBINI, r2, sc, 6, true);
                            e = ExprItem { exp: ec.into_reloc_with_pc(r2 as i64, pc) };
                        } else {
                            let k = fs.int_k(ec.info);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::ADDK, r2, r2, k);
                                fs.code_abc_k(OpCode::MMBINK, r2, k, 6, true);
                                e = ExprItem { exp: ec.into_reloc_with_pc(r2 as i64, pc) };
                            } else {
                                let r = fs.expr_to_reg(&ec);
                                let pc = fs.code_abc(OpCode::ADD, r2, r2, r);
                                e = ExprItem { exp: ec.into_reloc_with_pc(r2 as i64, pc) };
                            }
                        }
                    } else {
                        let r = fs.expr_to_reg(&ec);
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let pc = fs.code_abc(OpCode::SUBK, 0, r, r2);
                        let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                        if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                            if r2 == fs.freereg - 1 && r2 != r {
                                fs.free_reg();
                            }
                        }
                        e = ExprItem { exp: ec.into_reloc_with_pc(r as i64, pc) };
                    }
                }
                _ => {
                    let r_src = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, ec.info as i32);
                        }
                        ec.info as i32
                    } else {
                        fs.expr_to_reg(&ec)
                    };
                    let r = if matches!(ec.kind, ExpKind::NonReloc) && (ec.info as i32) < fs.nvarstack() {
                        fs.alloc_reg()
                    } else {
                        r_src
                    };
                    if !is_add && matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) && fits_sc_neg(e2.exp.info) {
                        let v = e2.exp.info;
                        let sc_neg = int_to_sc(-v);
                        let sc_pos = int_to_sc(v);
                        let pc = fs.code_abc(OpCode::ADDI, r, r_src, sc_neg);
                        fs.code_abc(OpCode::MMBINI, r_src, sc_pos, 7);
                        e = ExprItem { exp: ec.into_reloc_with_pc(r as i64, pc) };
                    } else if is_add && matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) {
                        let v = e2.exp.info;
                        let sc = int_to_sc(v);
                        let pc = fs.code_abc(OpCode::ADDI, r, r_src, sc);
                        fs.code_abc(OpCode::MMBINI, r_src, sc, 6);
                        e = ExprItem { exp: ec.into_reloc_with_pc(r as i64, pc) };
                    } else if is_add && matches!(e2.exp.kind, ExpKind::Float) {
                        let f = f64::from_bits(e2.exp.info as u64);
                        let k = fs.float_k(f);
                        if k <= 255 {
                            let pc = fs.code_abc(OpCode::ADDK, r, r_src, k);
                            fs.code_abc(OpCode::MMBINK, r_src, k, 6);
                            e = ExprItem { exp: ec.into_reloc_with_pc(r as i64, pc) };
                        } else {
                            let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                                let reg = e2.exp.info as i32;
                                if e2.exp.info2 >= 0 {
                                    fs.set_a(e2.exp.info2, reg);
                                }
                                reg
                            } else {
                                fs.exp_to_reg(&e2.exp)
                            };
                            let pc = fs.code_abc(OpCode::ADD, r, r_src, r2);
                            let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                            if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                                if r2 == fs.freereg - 1 && r2 != r {
                                    fs.free_reg();
                                }
                            }
                            e = ExprItem { exp: ec.into_reloc_with_pc(r as i64, pc) };
                        }
                    } else {
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let op = if is_add { OpCode::ADD } else { OpCode::SUB };
                        let pc = fs.code_abc(op, r, r_src, r2);
                        let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                        if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                            if r2 == fs.freereg - 1 && r2 != r {
                                fs.free_reg();
                            }
                        }
                        e = ExprItem { exp: ec.into_reloc_with_pc(r as i64, pc) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_MUL && check_mulop(fs) {
            let mut ec = e.exp.clone();
            let is_mul = check(fs, &Token::Star);
            let is_div = check(fs, &Token::Slash);
            let is_idiv = check(fs, &Token::SlashSlash);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_MUL + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    if is_idiv {
                        let denom = e2.exp.info;
                        if denom != 0 {
                            let q = ec.info / denom;
                            let val = if (ec.info ^ denom) < 0 && ec.info % denom != 0 { q - 1 } else { q };
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::IDIVK, r, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 12);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::IDIV, r, r, r2);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
                    } else if is_div {
                        let val = ec.info as f64 / e2.exp.info as f64;
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else if is_mul {
                        let val = ec.info * e2.exp.info;
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                    } else {
                        let m = ec.info;
                        let n = e2.exp.info;
                        if n != 0 {
                            let r = m % n;
                            let val = if r != 0 && (r ^ n) < 0 { r + n } else { r };
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::MODK, r, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 9);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::MOD, r, r, r2);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
                    }
                }
                (ExpKind::Float, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f = f64::from_bits(ec.info as u64);
                    if is_mul {
                        let val = f * (e2.exp.info as f64);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else if is_div {
                        let val = f / (e2.exp.info as f64);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else if is_idiv {
                        let denom = e2.exp.info as f64;
                        if denom != 0.0 {
                            let val = (f / denom).floor();
                            if val != 0.0 && !val.is_nan() {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                let r = fs.expr_to_reg(&ec);
                                let k = fs.int_k(e2.exp.info);
                                if k <= 255 {
                                    fs.code_abc(OpCode::IDIVK, r, r, k);
                                    fs.code_abc(OpCode::MMBINK, r, k, 12);
                                } else {
                                    let r2 = fs.expr_to_reg(&e2.exp);
                                    fs.code_abc(OpCode::IDIV, r, r, r2);
                                }
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                            }
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 {
                                fs.code_abc(OpCode::IDIVK, r, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 12);
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                fs.code_abc(OpCode::IDIV, r, r, r2);
                            }
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                        }
                    } else {
                        let denom = e2.exp.info as f64;
                        if denom != 0.0 {
                            let r = f % denom;
                            let val = if (r > 0.0) == (denom < 0.0) && r != 0.0 { r + denom } else { r };
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k_idx = {
                                let k = fs.int_k(e2.exp.info);
                                if k <= 255 { Some(k) } else { None }
                            };
                            if let Some(k) = k_idx {
                                let dest = fs.alloc_reg();
                                fs.code_abc(OpCode::MODK, dest, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 9);
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                fs.code_abc(OpCode::MOD, r, r, r2);
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                            }
                        }
                    }
                }
                (ExpKind::Int, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f = f64::from_bits(e2.exp.info as u64);
                    if is_mul {
                        let val = (ec.info as f64) * f;
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else if is_div {
                        let val = (ec.info as f64) / f;
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else if is_idiv {
                        if f != 0.0 {
                            let n = ec.info as f64;
                            let val = (n / f).floor();
                            if val != 0.0 && !val.is_nan() {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                let r = fs.expr_to_reg(&ec);
                                let k = fs.float_k(f);
                                if k <= 255 {
                                    fs.code_abc(OpCode::IDIVK, r, r, k);
                                    fs.code_abc(OpCode::MMBINK, r, k, 12);
                                } else {
                                    let r2 = fs.expr_to_reg(&e2.exp);
                                    fs.code_abc(OpCode::IDIV, r, r, r2);
                                }
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                            }
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k = fs.float_k(f);
                            if k <= 255 {
                                fs.code_abc(OpCode::IDIVK, r, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 12);
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                fs.code_abc(OpCode::IDIV, r, r, r2);
                            }
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                        }
                    } else {
                        if f != 0.0 {
                            let n = ec.info as f64;
                            let r = n % f;
                            let val = if (r > 0.0) == (f < 0.0) && r != 0.0 { r + f } else { r };
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k_idx = {
                                let k = fs.float_k(f);
                                if k <= 255 { Some(k) } else { None }
                            };
                            if let Some(k) = k_idx {
                                let dest = fs.alloc_reg();
                                fs.code_abc(OpCode::MODK, dest, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 9);
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                fs.code_abc(OpCode::MOD, r, r, r2);
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                            }
                        }
                    }
                }
                (ExpKind::Float, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f1 = f64::from_bits(ec.info as u64);
                    let f2 = f64::from_bits(e2.exp.info as u64);
                    if is_mul {
                        let val = f1 * f2;
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else if is_div {
                        let val = f1 / f2;
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else if is_idiv {
                        if f2 != 0.0 {
                            let val = (f1 / f2).floor();
                            if val != 0.0 && !val.is_nan() {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                let r = fs.expr_to_reg(&ec);
                                let k = fs.float_k(f2);
                                if k <= 255 {
                                    fs.code_abc(OpCode::IDIVK, r, r, k);
                                    fs.code_abc(OpCode::MMBINK, r, k, 12);
                                } else {
                                    let r2 = fs.expr_to_reg(&e2.exp);
                                    fs.code_abc(OpCode::IDIV, r, r, r2);
                                }
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                            }
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k = fs.float_k(f2);
                            if k <= 255 {
                                fs.code_abc(OpCode::IDIVK, r, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 12);
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                fs.code_abc(OpCode::IDIV, r, r, r2);
                            }
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                        }
                    } else {
                        if f2 != 0.0 {
                            let r = f1 % f2;
                            let val = if (r > 0.0) == (f2 < 0.0) && r != 0.0 { r + f2 } else { r };
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k_idx = {
                                let k = fs.float_k(f2);
                                if k <= 255 { Some(k) } else { None }
                            };
                            if let Some(k) = k_idx {
                                let dest = fs.alloc_reg();
                                fs.code_abc(OpCode::MODK, dest, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 9);
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                fs.code_abc(OpCode::MOD, r, r, r2);
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                            }
                        }
                    }
                }
                _ => {
                    let r = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, ec.info as i32);
                        }
                        ec.info as i32
                    } else {
                        fs.expr_to_reg(&ec)
                    };
                    let k_idx = match &e2.exp.kind {
                        ExpKind::Int => {
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 { Some(k) } else { None }
                        }
                        ExpKind::Float => {
                            let f = f64::from_bits(e2.exp.info as u64);
                            let k = fs.float_k(f);
                            if k <= 255 { Some(k) } else { None }
                        }
                        _ => None,
                    };
                    if is_idiv {
                        if let Some(k) = k_idx {
                            fs.code_abc(OpCode::IDIVK, r, r, k);
                            fs.code_abc(OpCode::MMBINK, r, k, 12);
                        } else {
                            let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                                let reg = e2.exp.info as i32;
                                if e2.exp.info2 >= 0 {
                                    fs.set_a(e2.exp.info2, reg);
                                }
                                reg
                            } else {
                                fs.exp_to_reg(&e2.exp)
                            };
                            fs.code_abc(OpCode::IDIV, r, r, r2);
                            let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                            if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                                if r2 == fs.freereg - 1 && r2 != r {
                                    fs.free_reg();
                                }
                            }
                        }
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                    } else if let Some(k) = k_idx {
                        let op = if is_mul { OpCode::MULK } else if is_div { OpCode::DIVK } else { OpCode::MODK };
                        let pc = fs.code_abc(op, r, r, k);
                        let tm = if is_mul { 8 } else if is_div { 11 } else { 9 };
                        fs.code_abc(OpCode::MMBINK, r, k, tm);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                    } else {
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let r_dest = if matches!(ec.kind, ExpKind::NonReloc) && (ec.info as i32) < fs.nvarstack() {
                            r2
                        } else {
                            r
                        };
                        let op = if is_mul { OpCode::MUL } else if is_div { OpCode::DIV } else { OpCode::MOD };
                        let pc = fs.code_abc(op, r_dest, r, r2);
                        let tm = if is_mul { 8 } else if is_div { 11 } else { 9 };
                        fs.code_abc(OpCode::MMBIN, r, r2, tm);
                        if r_dest != r2 {
                            let e2_reloc = matches!(e2.exp.kind, ExpKind::Relocable);
                            if e2_reloc || (!matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps()) {
                                if r2 == fs.freereg - 1 && r2 != r {
                                    fs.free_reg();
                                }
                            }
                        }
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_POW && check(fs, &Token::Caret) {
            let mut ec = e.exp.clone();
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_POW);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let base = ec.info as f64;
                    let exp = e2.exp.info;
                    let result = base.powi(exp as i32);
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Float, result.to_bits() as i64) };
                }
                (ExpKind::Float, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let base = f64::from_bits(ec.info as u64);
                    let exp = e2.exp.info;
                    let result = base.powi(exp as i32);
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Float, result.to_bits() as i64) };
                }
                (ExpKind::Int, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let base = ec.info as f64;
                    let exp = f64::from_bits(e2.exp.info as u64);
                    let result = base.powf(exp);
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Float, result.to_bits() as i64) };
                }
                (ExpKind::Float, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let base = f64::from_bits(ec.info as u64);
                    let exp = f64::from_bits(e2.exp.info as u64);
                    let result = base.powf(exp);
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Float, result.to_bits() as i64) };
                }
                _ => {
                    let r = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc | ExpKind::Relocable) {
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, ec.info as i32);
                        }
                        ec.info as i32
                    } else {
                        fs.expr_to_reg(&ec)
                    };
                    let k_idx = match &e2.exp.kind {
                        ExpKind::Int => {
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 { Some(k) } else { None }
                        }
                        ExpKind::Float => {
                            let f = f64::from_bits(e2.exp.info as u64);
                            let k = fs.float_k(f);
                            if k <= 255 { Some(k) } else { None }
                        }
                        _ => None,
                    };
                    if let Some(k) = k_idx {
                        let dest = fs.alloc_reg();
                        fs.code_abc(OpCode::POWK, dest, r, k);
                        fs.code_abc(OpCode::MMBINK, r, k, 10);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else {
                        let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let dest = fs.alloc_reg();
                        fs.code_abc(OpCode::POW, dest, r, r2);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    }
                }
            }
            matched = true;
        }
        
        if !matched {
            break;
        }
    }
    
    e
}

/// ANTLR4: 检查比较运算符 token: `==` | `~=` | `<` | `<=` | `>` | `>=`
fn check_compare(fs: &FuncState) -> bool {
    matches!(fs.ls().token, Token::EqEq | Token::TildeEq | Token::Lt | Token::LtEq | Token::Gt | Token::GtEq)
}

const OFFSET_SC: i64 = 127;

/// 判断整型常量是否适合 SC 参数编码 (i8 范围内)
fn fits_sc(desc: &ExpDesc) -> bool {
    if let ExpKind::Int = desc.kind {
        let v = desc.info;
        (v as i8 as i64) == v
    } else {
        false
    }
}

/// 判断整型常量取负后是否适合 SC 编码
fn fits_sc_neg(v: i64) -> bool {
    (v as i8 as i64) == v && ((-v) as i8 as i64) == -v
}

/// 将整型常量转换为 SC 参数编码 (加 OFFSET_SC 偏移)
fn int_to_sc(v: i64) -> i32 {
    ((v as u64).wrapping_add(OFFSET_SC as u64)) as i32
}

/// 获取表达式的 SC 整数值 (C 的 isSCnumber 对应)
/// 如果 Int 常量适合 SC，返回 Some(整数值)；
/// 如果 Float 常量可精确转为 Int 且适合 SC，返回 Some(整数值)；
/// 否则返回 None。
fn is_sc_number(desc: &ExpDesc) -> Option<i64> {
    if desc.has_jumps() {
        return None;
    }
    match desc.kind {
        ExpKind::Int => {
            let v = desc.info;
            if (v as i8 as i64) == v {
                Some(v)
            } else {
                None
            }
        }
        ExpKind::Float => {
            let f = f64::from_bits(desc.info as u64);
            let i = f as i64;
            if (i as f64 - f).abs() < f64::EPSILON && (i as i8 as i64) == i {
                Some(i)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 判断整型值是否适合 AsBx 模式的有符号偏移量
fn fits_sbx(v: i64) -> bool {
    v >= -(OFFSET_SBX as i64) && v <= (OFFSET_SBX as i64) + 1
}

/// 将表达式转换为常量表索引 (用于 EQK 等比较指令)
fn exp_to_const_k(fs: &mut FuncState, e: &ExpDesc) -> Option<i32> {
    let k = match e.kind {
        ExpKind::Str => e.info as i32,
        ExpKind::Boolean => {
            let tv = if e.info != 0 { TValue::Boolean(true) } else { TValue::Boolean(false) };
            fs.const_k(tv)
        }
        ExpKind::Nil => fs.const_k(TValue::Nil(NilKind::Strict)),
        ExpKind::Float => {
            let f = f64::from_bits(e.info as u64);
            fs.float_k(f)
        }
        ExpKind::Int => {
            if fits_sc(e) {
                return None;
            }
            fs.int_k(e.info)
        }
        _ => return None,
    };
    if k <= 255 { Some(k) } else { None }
}

fn exp2rk(fs: &mut FuncState, e: &ExpDesc) -> (i32, bool) {
    if e.t == NO_JUMP && e.f == NO_JUMP {
        let info = match e.kind {
            ExpKind::Boolean => {
                fs.const_k(if e.info != 0 { TValue::Boolean(true) } else { TValue::Boolean(false) })
            }
            ExpKind::Nil => fs.const_k(TValue::Nil(NilKind::Strict)),
            ExpKind::Int => fs.int_k(e.info),
            ExpKind::Float => {
                let f = f64::from_bits(e.info as u64);
                fs.float_k(f)
            }
            ExpKind::Str => e.info as i32,
            _ => {
                let r = fs.exp_to_reg(e);
                return (r, false);
            }
        };
        if info <= 255 {
            return (info, true);
        }
    }
    let r = fs.exp_to_reg(e);
    (r, false)
}

/// ANTLR4: 检查加减运算符 token: `+` | `-`
fn check_addop(fs: &FuncState) -> bool {
    matches!(fs.ls().token, Token::Plus | Token::Minus)
}

/// ANTLR4: 检查乘除运算符 token: `*` | `/` | `%` | `//`
fn check_mulop(fs: &FuncState) -> bool {
    matches!(fs.ls().token, Token::Star | Token::Slash | Token::Percent | Token::SlashSlash)
}

/// ANTLR4: `simpleExp: 'nil' | 'false' | 'true' | NUMBER | STRING | '...' | tableconstructor | 'function' funcbody | prefixexp ;` 以及 `unop expr`
fn parse_simple_exp(fs: &mut FuncState) -> ExprItem {
    let mut e = match &fs.ls().token {
        Token::Nil => {
            fs.ls_mut().next();
            ExpDesc::new(ExpKind::Nil, 0)
        }
        Token::True => {
            fs.ls_mut().next();
            ExpDesc::new(ExpKind::Boolean, 1)
        }
        Token::False => {
            fs.ls_mut().next();
            ExpDesc::new(ExpKind::Boolean, 0)
        }
        Token::Int(v) => {
            let val = *v;
            fs.ls_mut().next();
            ExpDesc::new(ExpKind::Int, val)
        }
        Token::Float(v) => {
            let val = *v;
            fs.ls_mut().next();
            ExpDesc::new(ExpKind::Float, val.to_bits() as i64)
        }
        Token::String(s) => {
            let s = s.clone();
            fs.ls_mut().next();
            let k = fs.string_k(&s);
            ExpDesc::new(ExpKind::Str, k as i64)
        }
        Token::DotDotDot => {
            fs.ls_mut().next();
            let r = fs.alloc_reg();
            fs.code_abc(OpCode::VARARG, r, 0, 0);
            ExpDesc::new(ExpKind::Vararg, r as i64)
        }
        Token::LBrace => {
            let (r, _n) = parse_constructor(fs);
            ExpDesc::new(ExpKind::Relocable, r as i64)
        }
        Token::Name(name) => {
            let name = name.clone();
            fs.ls_mut().next();
            if let Some(reg) = fs.find_local(&name) {
                ExpDesc::new(ExpKind::NonReloc, reg as i64)
            } else if let Some(upval_idx) = fs.find_upvalue(&name) {
                let r = fs.alloc_reg();
                fs.code_abc(OpCode::GETUPVAL, r, upval_idx, 0);
                ExpDesc::new(ExpKind::Relocable, r as i64)
            } else {
                let r = fs.alloc_reg();
                let k = fs.string_k(&name);
                fs.code_abc(OpCode::GETTABUP, r, 0, k);
                ExpDesc::new(ExpKind::Relocable, r as i64)
            }
        }
        Token::LParen => {
            fs.ls_mut().next();
            let ei = parse_expr(fs);
            expect(fs, &Token::RParen);
            ei.exp
        }
        Token::Not | Token::Minus | Token::Hash | Token::Tilde => {
            let op_tok = fs.ls().token.clone();
            fs.ls_mut().next();
            let ei = parse_subexpr(fs, PREC_UNARY);
            match op_tok {
                Token::Not => {
                    match ei.exp.kind {
                        ExpKind::Nil | ExpKind::Boolean if ei.exp.info == 0 => {
                            let mut e = ExpDesc::new(ExpKind::Boolean, 1);
                            e.t = ei.exp.f;
                            e.f = ei.exp.t;
                            fs.remove_values(e.t);
                            fs.remove_values(e.f);
                            e
                        }
                        ExpKind::Int | ExpKind::Float | ExpKind::Str
                            | ExpKind::Boolean => {
                            let mut e = ExpDesc::new(ExpKind::Boolean, 0);
                            e.t = ei.exp.f;
                            e.f = ei.exp.t;
                            fs.remove_values(e.t);
                            fs.remove_values(e.f);
                            e
                        }
                        ExpKind::VJMP => {
                            let mut e = ei.exp.clone();
                            fs.negate_condition(e.info as i32);
                            std::mem::swap(&mut e.t, &mut e.f);
                            fs.remove_values(e.t);
                            fs.remove_values(e.f);
                            e
                        }
                        _ => {
                            let r = if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() {
                                let r = ei.exp.info as i32;
                                if ei.exp.info2 >= 0 {
                                    fs.set_a(ei.exp.info2, r);
                                }
                                r
                            } else {
                                fs.expr_to_reg(&ei.exp)
                            };
                            let mut e = ExpDesc::new(ExpKind::Relocable, r as i64);
                            e.info2 = -2;
                            e
                        }
                    }
                }
                Token::Minus => {
                    if ei.exp.has_jumps() {
                        let r = fs.exp_to_reg(&ei.exp);
                        fs.code_abc(OpCode::UNM, r, r, 0);
                        ExpDesc::new(ExpKind::Relocable, r as i64)
                    } else {
                        match ei.exp.kind {
                            ExpKind::Int => {
                                ExpDesc::new(ExpKind::Int, -(ei.exp.info))
                            }
                            ExpKind::Float => {
                                let f = f64::from_bits(ei.exp.info as u64);
                                let result = -f;
                                if result.is_nan() || result == 0.0 {
                                    let r = fs.expr_to_reg(&ei.exp);
                                    fs.code_abc(OpCode::UNM, r, r, 0);
                                    ExpDesc::new(ExpKind::Relocable, r as i64)
                                } else {
                                    ExpDesc::new(ExpKind::Float, result.to_bits() as i64)
                                }
                            }
                            _ => {
                                let r = fs.expr_to_reg(&ei.exp);
                                fs.code_abc(OpCode::UNM, r, r, 0);
                                ExpDesc::new(ExpKind::Relocable, r as i64)
                            }
                        }
                    }
                }
                Token::Hash => {
                    if ei.exp.kind == ExpKind::NonReloc && (ei.exp.info as i32) < fs.nvarstack() {
                        let r = fs.alloc_reg();
                        fs.code_abc(OpCode::LEN, r, ei.exp.info as i32, 0);
                        ExpDesc::new(ExpKind::Relocable, r as i64)
                    } else {
                        let r = fs.expr_to_reg(&ei.exp);
                        fs.code_abc(OpCode::LEN, r, r, 0);
                        ExpDesc::new(ExpKind::Relocable, r as i64)
                    }
                }
                Token::Tilde => {
                    if ei.exp.has_jumps() {
                        let r = fs.exp_to_reg(&ei.exp);
                        fs.code_abc(OpCode::BNOT, r, r, 0);
                        ExpDesc::new(ExpKind::Relocable, r as i64)
                    } else {
                        match ei.exp.kind {
                            ExpKind::Int => {
                                ExpDesc::new(ExpKind::Int, !(ei.exp.info))
                            }
                            _ => {
                                let r = fs.expr_to_reg(&ei.exp);
                                fs.code_abc(OpCode::BNOT, r, r, 0);
                                ExpDesc::new(ExpKind::Relocable, r as i64)
                            }
                        }
                    }
                }
                _ => {
                    let r = fs.expr_to_reg(&ei.exp);
                    ExpDesc::new(ExpKind::Relocable, r as i64)
                }
            }
        }
        Token::Function => {
            fs.ls_mut().next();
            let r = parse_body(fs, None);
            ExpDesc::new(ExpKind::Relocable, r as i64)
        }
        _ => {
            fs.error(&format!("unexpected token in expression: {:?}", fs.ls().token));
            fs.ls_mut().next();
            ExpDesc::new(ExpKind::Nil, 0)
        }
    };

    loop {
        match &fs.ls().token {
            Token::LParen | Token::LBrace | Token::String(..) | Token::Colon => {
                let mut freg = fs.expr_to_reg(&e);
                if matches!(e.kind, ExpKind::NonReloc) && freg < fs.nvarstack() {
                    let new_freg = fs.alloc_reg();
                    if new_freg != freg {
                        fs.code_abc(OpCode::MOVE, new_freg, freg, 0);
                    }
                    freg = new_freg;
                }
                let call_pc = parse_func_args(fs, freg);
                e = if call_pc >= 0 {
                    ExpDesc { kind: ExpKind::Call, info: freg as i64, info2: call_pc, t: NO_JUMP, f: NO_JUMP }
                } else {
                    ExpDesc::new(ExpKind::Relocable, freg as i64)
                };
            }
            Token::Dot => {
                fs.ls_mut().next();
                let field = get_name(fs);
                let k = fs.string_k(&field);
                let base_reg = fs.expr_to_reg(&e);
                let result_reg = if matches!(e.kind, ExpKind::NonReloc) && (e.info as i32) < fs.nvarstack() {
                    fs.alloc_reg()
                } else {
                    base_reg
                };
                fs.code_abc(OpCode::GETFIELD, result_reg, base_reg, k);
                e = ExpDesc::new(ExpKind::Relocable, result_reg as i64);
            }
            Token::LBracket => {
                fs.ls_mut().next();
                let base_reg = if matches!(e.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e.has_jumps() {
                    if e.info2 >= 0 {
                        fs.set_a(e.info2, e.info as i32);
                    }
                    e.info as i32
                } else {
                    fs.expr_to_reg(&e)
                };
                let base_is_nonreloc_local = matches!(e.kind, ExpKind::NonReloc) && (e.info as i32) < fs.nvarstack();
                let ei = parse_expr(fs);
                expect(fs, &Token::RBracket);
                let result_reg;
                if ei.exp.kind == ExpKind::Int
                    && ei.exp.info >= 0
                    && ei.exp.info <= ((1u32 << SIZE_C) - 1) as i64
                {
                    result_reg = if base_is_nonreloc_local {
                        fs.alloc_reg()
                    } else {
                        base_reg
                    };
                    fs.code_abc(OpCode::GETI, result_reg, base_reg, ei.exp.info as i32);
                } else {
                    let key_reg = if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() {
                        if ei.exp.info2 >= 0 {
                            fs.set_a(ei.exp.info2, ei.exp.info as i32);
                        }
                        ei.exp.info as i32
                    } else {
                        fs.expr_to_reg(&ei.exp)
                    };
                    if base_is_nonreloc_local && key_reg >= fs.nvarstack() && key_reg == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    result_reg = if base_is_nonreloc_local {
                        fs.alloc_reg()
                    } else {
                        base_reg
                    };
                    fs.code_abc(OpCode::GETTABLE, result_reg, base_reg, key_reg);
                    if result_reg != key_reg && key_reg == fs.freereg - 1 {
                        fs.free_reg();
                    }
                }
                e = ExpDesc::new(ExpKind::Relocable, result_reg as i64);
            }
            _ => break,
        }
    }

    ExprItem { exp: e }
}

// ============================================================================
// Statements
// ============================================================================

/// ANTLR4: `'if' expr 'then' block ('elseif' expr 'then' block)* ('else' block)? 'end' ;`
fn parse_if(fs: &mut FuncState) {
    fs.ls_mut().next();
    let ei = parse_expr(fs);

    let is_const_true = matches!(ei.exp.kind, ExpKind::Boolean) && ei.exp.info != 0;

    let mut if_jmp = NO_JUMP;

    if !is_const_true {
        let pre_freereg = fs.freereg;
        let cond_reg = fs.cond_to_reg(&ei.exp);
        if matches!(ei.exp.kind, ExpKind::Relocable)
            && !fs.proto.code.is_empty()
            && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT
        {
            fs.proto.code.pop();
            fs.pc -= 1;
            fs.code_abc_k(OpCode::TEST, cond_reg, 0, 0, true);
        } else {
            fs.code_abc(OpCode::TEST, cond_reg, 0, 0);
        }
        if_jmp = fs.jump();
        if fs.freereg > pre_freereg {
            fs.free_reg();
        }
    }

    expect(fs, &Token::Then);
    let block_freereg = fs.freereg;
    let saved_nlocals = fs.locals.len();
    parse_block(fs);
    for local in &mut fs.locals[saved_nlocals..] {
        local.active = false;
    }
    fs.set_freereg(block_freereg);
    let mut exit_jumps = Vec::new();

    while check(fs, &Token::Elseif) {
        let j = fs.jump();
        exit_jumps.push(j);
        if if_jmp != NO_JUMP {
            fs.fix_jump(if_jmp, fs.pc, false);
        }
        fs.ls_mut().next();
        let ei2 = parse_expr(fs);
        let is_const_true2 = matches!(ei2.exp.kind, ExpKind::Boolean) && ei2.exp.info != 0;
        if !is_const_true2 {
            let pre_freereg2 = fs.freereg;
            let cr2 = fs.cond_to_reg(&ei2.exp);
            if matches!(ei2.exp.kind, ExpKind::Relocable)
                && !fs.proto.code.is_empty()
                && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT
            {
                fs.proto.code.pop();
                fs.pc -= 1;
                fs.code_abc_k(OpCode::TEST, cr2, 0, 0, true);
            } else {
                fs.code_abc(OpCode::TEST, cr2, 0, 0);
            }
            if_jmp = fs.jump();
            if fs.freereg > pre_freereg2 {
                fs.free_reg();
            }
        } else {
            if_jmp = NO_JUMP;
        }
        expect(fs, &Token::Then);
        fs.set_freereg(block_freereg);
        let saved_nlocals = fs.locals.len();
        parse_block(fs);
        for local in &mut fs.locals[saved_nlocals..] {
            local.active = false;
        }
        fs.set_freereg(block_freereg);
    }

    if check(fs, &Token::Else) {
        let j = fs.jump();
        exit_jumps.push(j);
        if if_jmp != NO_JUMP {
            fs.fix_jump(if_jmp, fs.pc, false);
        }
        fs.ls_mut().next();
        fs.set_freereg(block_freereg);
        let saved_nlocals = fs.locals.len();
        parse_block(fs);
        for local in &mut fs.locals[saved_nlocals..] {
            local.active = false;
        }
        fs.set_freereg(block_freereg);
    } else {
        if if_jmp != NO_JUMP {
            fs.fix_jump(if_jmp, fs.pc, false);
        }
    }
    expect(fs, &Token::End);

    for j in exit_jumps {
        fs.fix_jump(j, fs.pc, false);
    }
}

/// ANTLR4: `'while' expr 'do' block 'end' ;`
fn parse_while(fs: &mut FuncState) {
    fs.ls_mut().next();
    let loop_start = fs.pc;
    let ei = parse_expr(fs);
    let pre_freereg = fs.freereg;
    let r = fs.cond_to_reg(&ei.exp);
    if matches!(ei.exp.kind, ExpKind::Relocable)
        && !fs.proto.code.is_empty()
        && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT
    {
        fs.proto.code.pop();
        fs.pc -= 1;
        fs.code_abc_k(OpCode::TEST, r, 0, 0, true);
    } else {
        fs.code_abc(OpCode::TEST, r, 0, 0);
    }
    let jmp = fs.jump();
    if fs.freereg > pre_freereg {
        fs.free_reg();
    }
    expect(fs, &Token::Do);

    let saved_breaklist = fs.break_list;
    fs.break_list = NO_JUMP;

    parse_block(fs);

    fs.code_sj(OpCode::JMP, loop_start - fs.pc - 1, 0);
    fs.fix_jump(jmp, fs.pc, false);

    fs.patch_breaks(fs.pc);
    fs.break_list = saved_breaklist;

    expect(fs, &Token::End);
}

/// ANTLR4: `'do' block 'end' ;`
fn parse_do(fs: &mut FuncState) {
    fs.ls_mut().next();
    let saved_nlocals = fs.locals.len();
    let saved_freereg = fs.freereg;
    let saved_needclose = fs.needclose;
    parse_block(fs);
    if fs.needclose && !saved_needclose {
        fs.code_abc(OpCode::CLOSE, saved_freereg, 0, 0);
    }
    for local in &mut fs.locals[saved_nlocals..] {
        local.active = false;
    }
    fs.set_freereg(saved_freereg);
    expect(fs, &Token::End);
}

/// ANTLR4: `'repeat' block 'until' expr ;`
fn parse_repeat(fs: &mut FuncState) {
    fs.ls_mut().next();
    let loop_start = fs.pc;

    let saved_breaklist = fs.break_list;
    fs.break_list = NO_JUMP;

    parse_block(fs);
    expect(fs, &Token::Until);
    let ei = parse_expr(fs);

    let is_const_true = matches!(ei.exp.kind, ExpKind::Boolean if ei.exp.info != 0)
        || matches!(ei.exp.kind, ExpKind::Int | ExpKind::Float | ExpKind::Str);

    if !is_const_true {
        let pre_freereg = fs.freereg;
        let r = fs.cond_to_reg(&ei.exp);
        if matches!(ei.exp.kind, ExpKind::Relocable)
            && !fs.proto.code.is_empty()
            && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT
        {
            fs.proto.code.pop();
            fs.pc -= 1;
            fs.code_abc_k(OpCode::EQ, r, 0, 0, false);
        } else {
            fs.code_abc_k(OpCode::EQ, r, 0, 0, true);
        }
        let jmp = fs.jump();
        if fs.freereg > pre_freereg {
            fs.free_reg();
        }

        fs.patch_breaks(fs.pc);
        fs.break_list = saved_breaklist;

        fs.fix_jump(jmp, loop_start, true);
    } else {
        fs.patch_breaks(fs.pc);
        fs.break_list = saved_breaklist;
    }
}

/// ANTLR4: `'for' NAME '=' expr ',' expr (',' expr)? 'do' block 'end' ;` (numeric for) 以及 `'for' namelist 'in' explist 'do' block 'end' ;` (generic for)
fn parse_for(fs: &mut FuncState) {
    fs.ls_mut().next();
    let name = get_name(fs);
    
    if check(fs, &Token::Eq) {
        fs.ls_mut().next();
        let saved_nlocals = fs.locals.len();
        let base = fs.freereg;
        let saved_freereg = fs.freereg;

        fs.set_freereg(base);
        let ei = parse_expr(fs);
        let init_r = fs.expr_to_reg(&ei.exp);
        if init_r != base {
            fs.code_abc(OpCode::MOVE, base, init_r, 0);
        }
        expect(fs, &Token::Comma);

        fs.set_freereg(base + 1);
        let ei2 = parse_expr(fs);
        let limit_r = fs.expr_to_reg(&ei2.exp);
        if limit_r != base + 1 {
            fs.code_abc(OpCode::MOVE, base + 1, limit_r, 0);
        }

        if check(fs, &Token::Comma) {
            fs.ls_mut().next();
            fs.set_freereg(base + 2);
            let ei3 = parse_expr(fs);
            let step_r = fs.expr_to_reg(&ei3.exp);
            if step_r != base + 2 {
                fs.code_abc(OpCode::MOVE, base + 2, step_r, 0);
            }
        } else {
            fs.set_freereg(base + 2);
            fs.code_asbx(OpCode::LOADI, base + 2, 1);
        }

        fs.set_freereg(base + 3);
        
        expect(fs, &Token::Do);
        
        fs.add_local_kind_reg("(for state)", fs.pc, RDKCONST, base);
        fs.add_local_kind_reg("(for state)", fs.pc, RDKCONST, base + 1);
        fs.add_local_kind_reg(&name, fs.pc, RDKCONST, base + 2);
        
        let prep = fs.code_abx(OpCode::FORPREP, base, 0);

        let saved_breaklist = fs.break_list;
        fs.break_list = NO_JUMP;

        parse_block(fs);

        fs.fix_jump(prep, fs.pc, false);
        let loop_pc = fs.code_abx(OpCode::FORLOOP, base, 0);
        fs.fix_jump(loop_pc, prep + 1, true);

        fs.patch_breaks(fs.pc);
        fs.break_list = saved_breaklist;

        for local in &mut fs.locals[saved_nlocals..] {
            local.active = false;
        }
        fs.set_freereg(saved_freereg);
        expect(fs, &Token::End);
    } else {
        let mut vars = vec![name];
        while check(fs, &Token::Comma) {
            fs.ls_mut().next();
            let var = get_name(fs);
            vars.push(var);
        }
        expect(fs, &Token::In);
        
        let saved_nlocals = fs.locals.len();
        let saved_freereg = fs.freereg;
        let base = fs.freereg;
        
        fs.add_local_kind("(for state)", fs.pc, RDKCONST);
        fs.add_local_kind("(for state)", fs.pc, RDKCONST);
        fs.add_local_kind("(for state)", fs.pc, RDKTOCLOSE);
        let ncontrol = vars.len() as i32;
        let var_locals_start = fs.locals.len();
        for var_name in &vars {
            fs.add_local_kind(var_name, fs.pc, RDKCONST);
        }
        for lv in &mut fs.locals[var_locals_start..] {
            lv.active = false;
        }
        
        fs.set_freereg(base);
        let pc_before = fs.pc;
        let mut nexps = 0;
        loop {
            parse_expr(fs);
            nexps += 1;
            if !check(fs, &Token::Comma) { break; }
            fs.ls_mut().next();
        }
        for lv in &mut fs.locals[var_locals_start..] {
            lv.active = true;
        }
        
        let mut last_is_call = false;
        if fs.pc > pc_before {
            for i in (pc_before..fs.pc).rev() {
                if get_opcode(fs.proto.code[i as usize]) == OpCode::CALL {
                    let needed = (6 - nexps).max(1).min(255);
                    setarg(&mut fs.proto.code[i as usize], needed, POS_C, SIZE_C);
                    last_is_call = true;
                    break;
                }
            }
        }
        
        if !last_is_call && nexps < 4 {
            fs.code_abc(OpCode::LOADNIL, base + nexps, 3 - nexps, 0);
        }
        fs.set_freereg(base + 3 + ncontrol);
        fs.needclose = true;
        if fs.freereg > fs.max_freereg {
            fs.max_freereg = fs.freereg;
        }
        
        expect(fs, &Token::Do);

        let saved_breaklist = fs.break_list;
        fs.break_list = NO_JUMP;

        let prep = fs.code_abx(OpCode::TFORPREP, base, 0);
        parse_block(fs);
        
        fs.fix_jump(prep, fs.pc, false);
        fs.code_abc(OpCode::TFORCALL, base, 0, ncontrol);
        let loop_pc = fs.code_abx(OpCode::TFORLOOP, base, 0);
        fs.fix_jump(loop_pc, prep + 1, true);

        fs.patch_breaks(fs.pc);
        fs.break_list = saved_breaklist;

        expect(fs, &Token::End);
        
        fs.code_abc(OpCode::CLOSE, base, 0, 0);
        fs.set_freereg(saved_freereg);
        fs.locals.truncate(saved_nlocals);
    }
}

/// ANTLR4: `'function' funcname funcbody ;`
fn parse_func_stat(fs: &mut FuncState) {
    fs.ls_mut().next();
    let name = get_name(fs);

    let mut chain: Vec<(bool, String)> = vec![(false, name.clone())];
    while check(fs, &Token::Dot) || check(fs, &Token::Colon) {
        let is_colon = check(fs, &Token::Colon);
        fs.ls_mut().next();
        let field = get_name(fs);
        chain.push((is_colon, field));
    }

    if chain.len() == 1 {
        let name = &chain[0].1;
        let r = parse_body_ex(fs, false, None);
        if let Some(reg) = fs.find_local(name) {
            fs.code_abc(OpCode::MOVE, reg, r, 0);
        } else {
            let k = fs.string_k(name);
            fs.code_abc(OpCode::SETTABUP, 0, k, r);
        }
        fs.free_reg();
        return;
    }

    let first_name = &chain[0].1;
    let mut base_reg = if let Some(reg) = fs.find_local(first_name) {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::MOVE, r, reg, 0);
        r
    } else {
        let r = fs.alloc_reg();
        let k = fs.string_k(first_name);
        fs.code_abc(OpCode::GETTABUP, r, 0, k);
        r
    };

    let last_idx = chain.len() - 1;
    for i in 1..last_idx {
        let (_col, fname) = &chain[i];
        let k = fs.string_k(fname);
        fs.code_abc(OpCode::GETTABLE, base_reg, base_reg, k);
    }

    let (is_colon, last_name) = &chain[last_idx];
    let freg = parse_body_ex(fs, *is_colon, None);
    let fk = fs.string_k(last_name);
    fs.code_abc(OpCode::SETTABLE, base_reg, fk, freg);
    fs.free_reg();
    fs.free_reg();
}

/// ANTLR4: `'local' 'function' NAME funcbody | 'local' attnamelist ('=' explist)? ;`
fn parse_local(fs: &mut FuncState) {
    fs.ls_mut().next();
    
    if check(fs, &Token::Function) {
        fs.ls_mut().next();
        let name = get_name(fs);
        let reg = fs.add_local(&name, fs.pc);
        parse_body(fs, Some(reg));
    } else {
        let defkind = getvarattribute(fs, VDKREG);
        let mut names: Vec<String> = Vec::new();
        let mut kinds: Vec<i32> = Vec::new();

        loop {
            let name = get_name(fs);
            let kind = getvarattribute(fs, defkind);
            if kind == RDKTOCLOSE {
                if kinds.iter().any(|&k| k == RDKTOCLOSE) {
                    fs.error("multiple to-be-closed variables in local list");
                }
            }
            names.push(name);
            kinds.push(kind);
            if !check(fs, &Token::Comma) { break; }
            fs.ls_mut().next();
        }

        let nvars = names.len();

        let has_init = check(fs, &Token::Eq);

        if has_init {
            fs.ls_mut().next();
            let saved_freereg = fs.freereg;
            let mut last_exp: Option<ExpDesc> = None;
            let mut n_vals = 0;

            loop {
                let ei = parse_expr(fs);
                let target = saved_freereg + n_vals as i32;
                match ei.exp.kind {
                    ExpKind::NonReloc => {
                        let r = ei.exp.info as i32;
                        if r != target {
                            fs.code_abc(OpCode::MOVE, target, r, 0);
                        }
                        last_exp = Some(ExpDesc::new(ExpKind::NonReloc, target as i64));
                    }
                    ExpKind::Relocable => {
                        if ei.exp.info2 >= 0 {
                            fs.set_a(ei.exp.info2, target);
                            last_exp = Some(ExpDesc::new(ExpKind::NonReloc, target as i64));
                        } else {
                            let r = ei.exp.info as i32;
                            if r != target {
                                fs.code_abc(OpCode::MOVE, target, r, 0);
                            }
                            last_exp = Some(ExpDesc::new(ExpKind::Relocable, target as i64));
                        }
                    }
                    _ => {
                        let r = ei.exp.info as i32;
                        if matches!(ei.exp.kind, ExpKind::Call) && r == target {
                            fs.freereg = target + 1;
                        } else {
                            fs.set_freereg(target);
                            let _ = fs.expr_to_reg(&ei.exp);
                        }
                        last_exp = Some(ei.exp.clone());
                    }
                }
                n_vals += 1;
                if !check(fs, &Token::Comma) { break; }
                fs.ls_mut().next();
            }

            let last_is_ctc = n_vals == nvars
                && nvars > 0
                && kinds[nvars - 1] == RDKCONST
                && last_exp.as_ref().map(|e| matches!(e.kind,
                    ExpKind::Int | ExpKind::Float | ExpKind::Str | ExpKind::Boolean | ExpKind::Nil
                )).unwrap_or(false);

            let n_reg = if last_is_ctc { nvars - 1 } else { nvars };

            let last_is_call = last_exp.as_ref().map(|e| matches!(e.kind, ExpKind::Call)).unwrap_or(false);
            if last_is_call {
                if let Some(ref last_e) = last_exp {
                    let call_pc = last_e.info2;
                    if call_pc >= 0 {
                        let needed = ((n_reg - n_vals + 2) as i32).min(255);
                        setarg(&mut fs.proto.code[call_pc as usize], needed, POS_C, SIZE_C);
                    }
                }
            }

            for i in 0..n_reg {
                fs.add_local_kind_reg(&names[i], fs.pc, kinds[i], saved_freereg + i as i32);
            }
            if last_is_ctc {
                fs.add_local_kind(&names[nvars - 1], fs.pc, RDKCTC);
            }

            fs.set_freereg(saved_freereg + n_reg as i32);

            if !last_is_call && n_vals < n_reg {
                let remaining = n_reg - n_vals;
                fs.code_abc(OpCode::LOADNIL, saved_freereg + n_vals as i32, remaining as i32 - 1, 0);
            }
        } else {
            let start_reg = fs.add_local_kind(&names[0], fs.pc, kinds[0]);
            for i in 1..nvars {
                fs.add_local_kind(&names[i], fs.pc, kinds[i]);
            }
            fs.code_abc(OpCode::LOADNIL, start_reg, nvars as i32 - 1, 0);
        }

        for (i, &kind) in kinds.iter().enumerate() {
            if kind == RDKTOCLOSE {
                if let Some(reg) = fs.find_local(&names[i]) {
                    fs.code_abc(OpCode::TBC, reg, 0, 0);
                    break;
                }
            }
        }
    }
}

/// ANTLR4: `'return' explist? (';')? ;`
fn parse_return(fs: &mut FuncState) {
    fs.ls_mut().next();
    if block_follow(fs, true) || check(fs, &Token::Semi) {
        fs.return_stat_gen(fs.nvarstack(), 0);
        if check(fs, &Token::Semi) { fs.ls_mut().next(); }
        return;
    }
    
    let ei = parse_expr(fs);
    let r = fs.exp_to_reg(&ei.exp);
    
    if check(fs, &Token::Comma) {
        fs.ls_mut().next();
        let nret = 1 + parse_expr_list(fs);
        fs.return_stat_gen(r, nret);
    } else {
        fs.return_stat_gen(r, 1);
    }
    if check(fs, &Token::Semi) { fs.ls_mut().next(); }
}

/// ANTLR4: `explist: expr (',' expr)* ;` — 解析逗号分隔的表达式列表
fn parse_expr_list(fs: &mut FuncState) -> i32 {
    let ei = parse_expr(fs);
    let _r = fs.expr_to_reg(&ei.exp);
    let mut n = 1;
    while check(fs, &Token::Comma) {
        fs.ls_mut().next();
        let ei2 = parse_expr(fs);
        let _r2 = fs.expr_to_reg(&ei2.exp);
        n += 1;
    }
    n
}

/// ANTLR4: `tableconstructor: '{' fieldlist? '}' ;`
fn parse_constructor(fs: &mut FuncState) -> (i32, i32) {
    fs.ls_mut().next();
    let table_r = fs.alloc_reg();
    let pc = fs.code_abc_k(OpCode::NEWTABLE, 0, 0, 0, false);
    fs.code_ax(OpCode::EXTRAARG, 0);
    let mut need_array: i32 = 0;
    let mut tostore: i32 = 0;
    let mut need_hash: i32 = 0;
    let mut last_list_exp: Option<ExpDesc> = None;

    if !check(fs, &Token::RBrace) {
        loop {
            if check(fs, &Token::LBracket) {
                if let Some(prev) = last_list_exp.take() {
                    fs.exp_to_reg(&prev);
                    tostore += 1;
                }
                if tostore > 0 {
                    fs.code_abc(OpCode::SETLIST, table_r, tostore, need_array);
                    need_array += tostore;
                    tostore = 0;
                    fs.set_freereg(table_r + 1);
                }
                fs.ls_mut().next();
                let ek = parse_expr(fs);
                let k_r = fs.exp_to_reg(&ek.exp);
                expect(fs, &Token::RBracket);
                expect(fs, &Token::Eq);
                let ev = parse_expr(fs);
                let (v_rk, is_k) = exp2rk(fs, &ev.exp);
                fs.code_abc_k(OpCode::SETTABLE, table_r, k_r, v_rk, is_k);
                if !is_k {
                    fs.free_reg();
                }
                fs.free_reg();
                need_hash += 1;
            } else if let Token::Name(s) = &fs.ls().token {
                let name = s.clone();
                let next_is_eq = fs.ls_mut().lookahead_next().0 == Token::Eq;
                if next_is_eq {
                    if let Some(prev) = last_list_exp.take() {
                        fs.exp_to_reg(&prev);
                        tostore += 1;
                    }
                    if tostore > 0 {
                        fs.code_abc(OpCode::SETLIST, table_r, tostore, need_array);
                        need_array += tostore;
                        tostore = 0;
                        fs.set_freereg(table_r + 1);
                    }
                    fs.ls_mut().next();
                    fs.ls_mut().next();
                    let ev = parse_expr(fs);
                    let k = fs.string_k(&name);
                    let (v_rk, is_k) = exp2rk(fs, &ev.exp);
                    fs.code_abc_k(OpCode::SETFIELD, table_r, k, v_rk, is_k);
                    if !is_k {
                        fs.free_reg();
                    }
                    need_hash += 1;
                } else {
                    if let Some(prev) = last_list_exp.take() {
                        fs.exp_to_reg(&prev);
                        tostore += 1;
                    }
                    last_list_exp = Some(parse_expr(fs).exp);
                }
            } else {
                if let Some(prev) = last_list_exp.take() {
                    fs.exp_to_reg(&prev);
                    tostore += 1;
                }
                last_list_exp = Some(parse_expr(fs).exp);
            }

            if !check(fs, &Token::Comma) && !check(fs, &Token::Semi) { break; }
            fs.ls_mut().next();
            if check(fs, &Token::RBrace) { break; }
        }
    }

    if let Some(last) = last_list_exp {
        if last.kind == ExpKind::Call {
            fs.set_c(last.info2, 0);
            fs.code_abc(OpCode::SETLIST, table_r, 0, need_array);
            need_array += tostore;
            fs.set_freereg(table_r + 1);
        } else if last.kind == ExpKind::Vararg {
            fs.code_abc(OpCode::SETLIST, table_r, 0, need_array);
            need_array += tostore;
            fs.set_freereg(table_r + 1);
        } else {
            fs.exp_to_reg(&last);
            tostore += 1;
            if tostore > 0 {
                fs.code_abc(OpCode::SETLIST, table_r, tostore, need_array);
                need_array += tostore;
                fs.set_freereg(table_r + 1);
            }
        }
    } else if tostore > 0 {
        fs.code_abc(OpCode::SETLIST, table_r, tostore, need_array);
        need_array += tostore;
        fs.set_freereg(table_r + 1);
    }

    expect(fs, &Token::RBrace);

    fs.set_tablesize(pc, table_r, need_array, need_hash);

    (table_r, need_array)
}

/// ANTLR4: `funcbody: '(' parlist? ')' block 'end' ;` — 解析函数体 (非 method)
fn parse_body(fs: &mut FuncState, target: Option<i32>) -> i32 {
    parse_body_ex(fs, false, target)
}

/// ANTLR4: `funcbody: '(' parlist? ')' block 'end' ;` — 解析函数体 (可指定 method 添加 self 参数)
fn parse_body_ex(fs: &mut FuncState, ismethod: bool, target: Option<i32>) -> i32 {
    expect(fs, &Token::LParen);
    let has_params = !check(fs, &Token::RParen);
    let mut is_vararg = false;
    let mut n_params: u8 = 0;
    
    let mut param_names = Vec::new();
    if ismethod {
        param_names.push("self".to_string());
        n_params = 1;
    }
    if has_params {
        loop {
            if check(fs, &Token::DotDotDot) {
                is_vararg = true;
                fs.ls_mut().next();
                break;
            }
            if let Token::Name(name) = &fs.ls().token {
                let name = name.clone();
                fs.ls_mut().next();
                n_params += 1;
                param_names.push(name);
            }
            if !check(fs, &Token::Comma) { break; }
            fs.ls_mut().next();
        }
    }
    expect(fs, &Token::RParen);
    
    let mut new_fs = FuncState::new(fs.ls_mut());
    new_fs.proto.num_params = n_params;
    if is_vararg { new_fs.proto.flag = PF_VAHID; }

    for name in &param_names {
        new_fs.add_local(name, 2);
    }

    for local in &fs.locals {
        if local.active && local.kind != RDKCTC {
            new_fs.parent_locals.push((local.name.clone(), local.reg));
        }
    }
    new_fs.parent_locals.extend(fs.parent_locals.iter().cloned());

    parse_chunk(&mut new_fs);
    expect(&mut new_fs, &Token::End);

    if !new_fs.proto.upvalues.is_empty() {
        fs.needclose = true;
    }

    let proto = new_fs.proto;
    let p_idx = fs.proto.protos.len() as i32;
    fs.proto.protos.push(proto);
    let r = target.unwrap_or_else(|| fs.alloc_reg());
    fs.code_abx(OpCode::CLOSURE, r, p_idx);
    r
}

// ============================================================================
// Value comparison for constant dedup
// ============================================================================

/// TValue 比较: 用于常量去重，支持 nil/bool/int/float/string 类型
fn tvalue_eq(a: &TValue, b: &TValue) -> bool {
    match (a, b) {
        (TValue::Nil(_), TValue::Nil(_)) => true,
        (TValue::Boolean(a), TValue::Boolean(b)) => a == b,
        (TValue::Integer(a), TValue::Integer(b)) => a == b,
        (TValue::Float(a), TValue::Float(b)) => a.to_bits() == b.to_bits(),
        (TValue::Integer(a), TValue::Float(b)) => (*a as f64).to_bits() == b.to_bits(),
        (TValue::Float(a), TValue::Integer(b)) => a.to_bits() == (*b as f64).to_bits(),
        (TValue::Str(a), TValue::Str(b)) => a.as_str() == b.as_str(),
        _ => false,
    }
}