use crate::objects::*;
use crate::objects::PF_VAHID;
use crate::opcodes::*;
use super::lexer::{LexState, Token};

use crate::objects::Instruction;

const NO_JUMP: i32 = -1;
const LUA_MULTRET: i32 = -1;

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
//   | NAME '=' expr                                // → parse_constructor() SETI
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
        ExpDesc { kind, info, info2: 0, t: NO_JUMP, f: NO_JUMP }
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

pub struct FuncState {
    pub proto: Proto,
    pub prev: Option<Box<FuncState>>,
    pub pc: i32,
    pub freereg: i32,
    pub max_freereg: i32,
    pub locals: Vec<LocalVar>,
    pub errors: Vec<String>,
    pub needclose: bool,
    ls: *mut LexState,
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
            ls: ls as *mut LexState,
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
        self.emit(create_abck(op, a, b, c, 0))
    }

    /// 生成 IABC+k 位模式指令: `op a b c k`
    fn code_abc_k(&mut self, op: OpCode, a: i32, b: i32, c: i32, k: bool) -> i32 {
        self.emit(create_abck(op, a, b, c, if k { 1 } else { 0 }))
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
    fn alloc_reg(&mut self) -> i32 {
        let r = self.freereg;
        self.freereg += 1;
        if self.freereg > self.max_freereg {
            self.max_freereg = self.freereg;
        }
        r
    }

    /// 释放最后一个寄存器: freereg--
    fn free_reg(&mut self) {
        if self.freereg > 0 {
            self.freereg -= 1;
        }
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
                if e.info as i32 == self.freereg - 1 {
                    e.info as i32
                } else {
                    let r = self.alloc_reg();
                    self.code_abc(OpCode::MOVE, r, e.info as i32, 0);
                    r
                }
            }
            ExpKind::Relocable | ExpKind::Call | ExpKind::Vararg => {
                if e.info as i32 == self.freereg - 1 {
                    e.info as i32
                } else {
                    let r = self.alloc_reg();
                    self.code_abc(OpCode::MOVE, r, e.info as i32, 0);
                    r
                }
            }
            ExpKind::VJMP => {
                let r = self.alloc_reg();
                let jmp_pc = e.info as i32;
                
                // Move VJMP's own JMP into true-list, then handle t/f lists
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
                    self.code_abc(OpCode::LFALSESKIP, r, 0, 0);
                    let load_true_pc = self.code_abc(OpCode::LOADTRUE, r, 0, 0);
                    let final_pc = self.pc;
                    self.patch_true_jumps(my_true_list, load_true_pc);
                    self.patch_false_jumps(my_false_list, final_pc);
                }
                r
            }
        }
    }

    /// 修复真分支跳转链表: 遍历链表将所有 JMP 指向 target
    fn patch_true_jumps(&mut self, list: i32, target: i32) {
        let mut cur = list;
        while cur != NO_JUMP {
            let next = self.get_jump(cur);
            self.fix_jump(cur, target, false);
            cur = next;
        }
    }

    /// 修复假分支跳转链表: 遍历链表将所有 JMP 指向 target
    fn patch_false_jumps(&mut self, list: i32, target: i32) {
        let mut cur = list;
        while cur != NO_JUMP {
            let next = self.get_jump(cur);
            self.fix_jump(cur, target, false);
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
                    let k = self.needclose;
                    self.code_abc_k(OpCode::RETURN, first, 1, c, k);
                } else {
                    self.code_abc(OpCode::RETURN0, first, 1, 0);
                }
            }
            1 => {
                if is_vararg {
                    self.code_abc(OpCode::RETURN, first, 2, self.proto.num_params as i32 + 1);
                } else {
                    self.code_abc(OpCode::RETURN1, first, 2, 0);
                }
            }
            _ => {
                self.code_abc(OpCode::RETURN, first, nret + 1, c);
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
    let r = parse_body(fs);
    let k = fs.string_k(&fname);
    fs.code_abc(OpCode::SETTABUP, 0, k, r);
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
    match &fs.ls().token {
        Token::If => parse_if(fs),
        Token::While => parse_while(fs),
        Token::Do => parse_do(fs),
        Token::For => parse_for(fs),
        Token::Repeat => parse_repeat(fs),
        Token::Function => parse_func_stat(fs),
        Token::Local => parse_local(fs),
        Token::Return => parse_return(fs),
        Token::Semi => { fs.ls_mut().next(); }
        Token::Break => { fs.ls_mut().next(); }
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
            let _r = fs.expr_to_reg(&ei.exp);
            fs.free_reg();
            if check(fs, &Token::Semi) { fs.ls_mut().next(); }
        }
        _ => {
            fs.error(&format!("unexpected token: {:?}", fs.ls().token));
            fs.ls_mut().next();
        }
    }
}

// ============================================================================
// Assignments and function calls
// ============================================================================

/// ANTLR4: `varlist '=' explist | functioncall ;` — 解析赋值语句或函数调用
fn parse_assign_or_call(fs: &mut FuncState) {
    let mut first = parse_prefix_exp(fs);
    
    let mut has_call = false;
    if check(fs, &Token::LParen) || check(fs, &Token::Colon) || check(fs, &Token::LBrace) || matches!(&fs.ls().token, Token::String(..)) {
        has_call = true;
        let freg = load_func(fs, &first);
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
                    table_reg: Some(base_reg), table_key: Some(k),
                };
            }
            Token::LBracket => {
                fs.ls_mut().next();
                let ei = parse_expr(fs);
                expect(fs, &Token::RBracket);
                let base_reg = if let Some(r) = first.reg { r } else {
                    let r = fs.alloc_reg();
                    if let Some(key) = first.key {
                        fs.code_abc(OpCode::GETTABUP, r, 0, key);
                    }
                    r
                };
                let kr = fs.expr_to_reg(&ei.exp);
                first = PrefixResult {
                    var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                    table_reg: Some(base_reg), table_key: Some(kr),
                };
            }
            _ => break,
        }
    }
    
    if has_call && !check(fs, &Token::Eq) && !check(fs, &Token::Comma) {
        fs.free_reg();
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
            exps.push(ei.exp);
            if !check(fs, &Token::Comma) { break; }
            fs.ls_mut().next();
        }
        
        for (i, v) in vars.iter().enumerate() {
            if i < exps.len() {
                let val = &exps[i];
                if let (Some(table_reg), Some(table_key)) = (v.table_reg, v.table_key) {
                    if let Some(k_val) = exp_to_k(fs, val) {
                        fs.code_abc_k(OpCode::SETTABLE, table_reg, table_key, k_val, true);
                    } else {
                        let val_reg = fs.expr_to_reg(val);
                        fs.code_abc(OpCode::SETTABLE, table_reg, table_key, val_reg);
                        fs.free_reg();
                    }
                } else if let Some(ref name) = v.var_name {
                    let k_name = fs.string_k(name);
                    if let Some(k_val) = exp_to_k(fs, val) {
                        fs.code_abc_k(OpCode::SETTABUP, 0, k_name, k_val, true);
                    } else {
                        let val_reg = fs.expr_to_reg(val);
                        fs.code_abc(OpCode::SETTABUP, 0, k_name, val_reg);
                        fs.free_reg();
                    }
                } else if let Some(idx) = v.local_idx {
                    let val_reg = fs.expr_to_reg(val);
                    if idx != val_reg {
                        fs.code_abc(OpCode::MOVE, idx, val_reg, 0);
                    }
                    fs.free_reg();
                }
            }
        }
        return;
    }
    
    let _r = load_func(fs, &first);
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

/// ANTLR4: functioncall 帮助 — 将函数值加载到寄存器以便调用
fn load_func(fs: &mut FuncState, p: &PrefixResult) -> i32 {
    if let (Some(table_reg), Some(table_key)) = (p.table_reg, p.table_key) {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::GETTABLE, r, table_reg, table_key);
        r
    } else if let Some(reg) = p.local_idx {
        if reg == fs.freereg - 1 {
            reg
        } else {
            let r = fs.alloc_reg();
            fs.code_abc(OpCode::MOVE, r, reg, 0);
            r
        }
    } else if let Some(key) = p.key {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::GETTABUP, r, 0, key);
        r
    } else if let Some(reg) = p.reg {
        if reg == fs.freereg - 1 {
            reg
        } else {
            let r = fs.alloc_reg();
            fs.code_abc(OpCode::MOVE, r, reg, 0);
            r
        }
    } else {
        fs.alloc_reg()
    }
}

/// ANTLR4: `args: '(' explist? ')' | tableconstructor | STRING ;` 及 `':' NAME args ;` — 解析函数参数并生成 CALL 指令
fn parse_func_args(fs: &mut FuncState, freg: i32) {
    if matches!(&fs.ls().token, Token::String(..)) {
        let str_s = match &fs.ls().token {
            Token::String(s) => s.clone(),
            _ => String::new(),
        };
        fs.ls_mut().next();
        let k = fs.string_k(&str_s);
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        fs.code_abc(OpCode::CALL, freg, 2, 2);
        fs.free_reg();
        return;
    }
    
    if check(fs, &Token::LBrace) {
        let (tr, _n) = parse_constructor(fs);
        fs.code_abc(OpCode::MOVE, freg + 1, tr, 0);
        fs.code_abc(OpCode::CALL, freg, 2, 2);
        fs.free_reg();
        return;
    }
    
    if check(fs, &Token::Colon) {
        fs.ls_mut().next();
        let method = get_name(fs);
        let k = fs.string_k(&method);
        fs.code_abc(OpCode::GETTABLE, freg + 1, freg, k);
        if check(fs, &Token::LParen) {
            fs.ls_mut().next();
            let na = parse_args(fs);
            expect(fs, &Token::RParen);
            fs.code_abc(OpCode::CALL, freg, na + 1, 2);
            for _ in 0..na {
                fs.free_reg();
            }
        }
        return;
    }
    
    if check(fs, &Token::LParen) {
        fs.ls_mut().next();
        let nparams = parse_args(fs);
        expect(fs, &Token::RParen);
        fs.code_abc(OpCode::CALL, freg, nparams + 1, 2);
        for _ in 0..nparams {
            fs.free_reg();
        }
        return;
    }
}

/// ANTLR4: `explist: expr (',' expr)* ;` — 解析函数调用参数列表
fn parse_args(fs: &mut FuncState) -> i32 {
    if check(fs, &Token::RParen) || check(fs, &Token::RBrace) {
        return 0;
    }
    let ei = parse_expr(fs);
    let r = fs.expr_to_reg(&ei.exp);
    let mut n = 1;
    while check(fs, &Token::Comma) {
        fs.ls_mut().next();
        let ei2 = parse_expr(fs);
        let _r2 = fs.expr_to_reg(&ei2.exp);
        n += 1;
    }
    n
}

#[derive(Debug, Clone)]
struct PrefixResult {
    var_name: Option<String>,
    local_idx: Option<i32>,
    key: Option<i32>,
    reg: Option<i32>,
    table_reg: Option<i32>,
    table_key: Option<i32>,
}

/// ANTLR4: `prefixexp: varOrExp | functioncall | '(' expr ')' ;` 以及 `var: NAME | prefixexp '[' expr ']' | prefixexp '.' NAME ;`
fn parse_prefix_exp(fs: &mut FuncState) -> PrefixResult {
    match &fs.ls().token {
        Token::Name(name) => {
            let name = name.clone();
            fs.ls_mut().next();

            let mut result = if let Some(reg) = fs.find_local(&name) {
                PrefixResult { var_name: None, local_idx: Some(reg), key: None, reg: Some(reg), table_reg: None, table_key: None }
            } else {
                let k = fs.string_k(&name);
                PrefixResult { var_name: Some(name), local_idx: None, key: Some(k), reg: None, table_reg: None, table_key: None }
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
                            table_reg: Some(base_reg), table_key: Some(k),
                        };
                    }
                    Token::LBracket => {
                        fs.ls_mut().next();
                        let ei = parse_expr(fs);
                        expect(fs, &Token::RBracket);
                        let base_reg = if let Some(r) = result.reg {
                            r
                        } else {
                            let r = fs.alloc_reg();
                            let gk = result.key.unwrap_or(0);
                            fs.code_abc(OpCode::GETTABUP, r, 0, gk);
                            r
                        };
                        let kr = fs.expr_to_reg(&ei.exp);
                        result = PrefixResult {
                            var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                            table_reg: Some(base_reg), table_key: Some(kr),
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
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None }
        }
        _ => {
            let e = parse_simple_exp(fs);
            let r = fs.expr_to_reg(&e.exp);
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None }
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
        
        if limit <= PREC_OR && (check(fs, &Token::Or) || check(fs, &Token::And)) {
            let mut e_left = e.exp.clone();
            let is_and = check(fs, &Token::And);
            fs.ls_mut().next();
            
            if e_left.kind == ExpKind::VJMP {
                let saved_jmp = e_left.info as i32;
                fs.negate_condition(saved_jmp);
                if is_and {
                    fs.concat_jump(&mut e_left.f, saved_jmp);
                } else {
                    fs.concat_jump(&mut e_left.t, saved_jmp);
                }
            } else {
                let reg = fs.expr_to_reg(&e_left);
                let k = if is_and { 0 } else { 1 };
                fs.code_abc_k(OpCode::TESTSET, reg, reg, 0, k != 0);
                let jmp_pc = fs.jump();
                let mut vexp = ExpDesc::new(ExpKind::VJMP, NO_JUMP as i64);
                if is_and {
                    fs.concat_jump(&mut vexp.f, jmp_pc);
                } else {
                    fs.concat_jump(&mut vexp.t, jmp_pc);
                }
                e_left = vexp;
            }
            
            let e2 = parse_subexpr(fs, if is_and { PREC_AND + 1 } else { PREC_OR + 1 });
            let mut e2_exp = e2.exp.clone();
            
            if is_and {
                fs.concat_jump(&mut e2_exp.f, e_left.f);
                e2_exp.t = e_left.t;
            } else {
                fs.concat_jump(&mut e2_exp.t, e_left.t);
                e2_exp.f = e_left.f;
            }
            
            if e2_exp.kind != ExpKind::VJMP {
                let reg = fs.expr_to_reg(&e2_exp);
                let mut vexp = ExpDesc::new(ExpKind::VJMP, NO_JUMP as i64);
                vexp.f = e2_exp.f;
                vexp.t = e2_exp.t;
                e2_exp = vexp;
            }
            
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

            if fits_sc(&ec) && fits_sc(&e2.exp) {
                let (reg, imm) = if is_eq {
                    let reg = fs.expr_to_reg(&e2.exp);
                    (reg, int_to_sc(ec.info))
                } else if is_gt {
                    let reg = fs.expr_to_reg(&e2.exp);
                    (reg, int_to_sc(ec.info))
                } else {
                    let reg = fs.expr_to_reg(&ec);
                    (reg, int_to_sc(e2.exp.info))
                };
                let imm_op = match op_tok {
                    Token::EqEq | Token::TildeEq => OpCode::EQI,
                    Token::Lt | Token::Gt => OpCode::LTI,
                    Token::LtEq | Token::GtEq => OpCode::LEI,
                    _ => OpCode::EQI,
                };
                fs.code_abc_k(imm_op, reg, imm, 0, k != 0);
                let jmp_pc = fs.jump();
                fs.free_reg();
                e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
            } else {
                let r = fs.expr_to_reg(&ec);
                let const_k = if is_eq {
                    exp_to_const_k(fs, &e2.exp)
                } else {
                    None
                };
                if let Some(k_idx) = const_k {
                    let is_eq_op = matches!(op_tok, Token::EqEq);
                    fs.code_abc_k(OpCode::EQK, r, k_idx, 0, is_eq_op);
                    let jmp_pc = fs.jump();
                    fs.free_reg();
                    e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                } else {
                    let r2 = fs.expr_to_reg(&e2.exp);
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
                    fs.free_reg();
                    fs.free_reg();
                    e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                }
            }
            matched = true;
        }
        
        if limit <= PREC_BOR && check(fs, &Token::Pipe) {
            let ec = e.exp.clone();
            let r = fs.expr_to_reg(&ec);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_BOR + 1);
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
                let r2 = fs.expr_to_reg(&e2.exp);
                fs.code_abc(OpCode::BOR, r, r, r2);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
            }
            matched = true;
        }
        
        if limit <= PREC_BXOR && check(fs, &Token::Tilde) {
            let ec = e.exp.clone();
            let r = fs.expr_to_reg(&ec);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_BXOR + 1);
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
                let r2 = fs.expr_to_reg(&e2.exp);
                fs.code_abc(OpCode::BXOR, r, r, r2);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
            }
            matched = true;
        }
        
        if limit <= PREC_BAND && check(fs, &Token::Ampersand) {
            let ec = e.exp.clone();
            let r = fs.expr_to_reg(&ec);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_BAND + 1);
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
                let r2 = fs.expr_to_reg(&e2.exp);
                fs.code_abc(OpCode::BAND, r, r, r2);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
            }
            matched = true;
        }
        
        if limit <= PREC_SHL && check(fs, &Token::LtLt) {
            let ec = e.exp.clone();
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_SHL + 1);
            if matches!(ec.kind, ExpKind::Int) && fits_sc(&ec) {
                let r2 = fs.expr_to_reg(&e2.exp);
                let dest = fs.alloc_reg();
                let sc = int_to_sc(ec.info);
                fs.code_abc(OpCode::SHLI, dest, r2, sc);
                fs.code_abc(OpCode::MMBINI, r2, sc, 16);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
            } else if matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) && fits_sc_neg(e2.exp.info) {
                let r = fs.expr_to_reg(&ec);
                let v = e2.exp.info;
                let dest = fs.alloc_reg();
                let sc_neg = int_to_sc(-v);
                let sc_pos = int_to_sc(v);
                fs.code_abc(OpCode::SHRI, dest, r, sc_neg);
                fs.code_abc(OpCode::MMBINI, r, sc_pos, 16);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
            } else {
                let r = fs.expr_to_reg(&ec);
                let r2 = fs.expr_to_reg(&e2.exp);
                fs.code_abc(OpCode::SHL, r, r, r2);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
            }
            matched = true;
        }
        
        if limit <= PREC_SHL && check(fs, &Token::GtGt) {
            let ec = e.exp.clone();
            let r = fs.expr_to_reg(&ec);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_SHL + 1);
            if matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) {
                let v = e2.exp.info;
                let dest = fs.alloc_reg();
                let sc = int_to_sc(v);
                fs.code_abc(OpCode::SHRI, dest, r, sc);
                fs.code_abc(OpCode::MMBINI, r, sc, 17);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
            } else {
                let r2 = fs.expr_to_reg(&e2.exp);
                fs.code_abc(OpCode::SHR, r, r, r2);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
            }
            matched = true;
        }
        
        if limit <= PREC_CONCAT && check(fs, &Token::DotDot) {
            let ec = e.exp.clone();
            let r = fs.expr_to_reg(&ec);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_CONCAT);
            let _r2 = fs.expr_to_reg(&e2.exp);
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
            let ec = e.exp.clone();
            let is_add = check(fs, &Token::Plus);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_ADD + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) => {
                    let val = if is_add { ec.info + e2.exp.info } else { ec.info - e2.exp.info };
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                }
                (ExpKind::Int, _) => {
                    let r = fs.expr_to_reg(&ec);
                    let r2 = fs.expr_to_reg(&e2.exp);
                    if is_add && fits_sc(&ec) {
                        fs.code_abc(OpCode::ADDI, r, r2, int_to_sc(ec.info));
                        fs.code_abc(OpCode::MMBINI, r2, int_to_sc(ec.info), 6);
                    } else {
                        fs.code_abc(if is_add { OpCode::ADDI } else { OpCode::SUBK }, r, r2, ec.info as i32);
                    }
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                }
                _ => {
                    let r = fs.expr_to_reg(&ec);
                    if !is_add && matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) && fits_sc_neg(e2.exp.info) {
                        let v = e2.exp.info;
                        let dest = fs.alloc_reg();
                        let sc_neg = int_to_sc(-v);
                        let sc_pos = int_to_sc(v);
                        fs.code_abc(OpCode::ADDI, dest, r, sc_neg);
                        fs.code_abc(OpCode::MMBINI, r, sc_pos, 7);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else if is_add && matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) {
                        let v = e2.exp.info;
                        let dest = fs.alloc_reg();
                        let sc = int_to_sc(v);
                        fs.code_abc(OpCode::ADDI, dest, r, sc);
                        fs.code_abc(OpCode::MMBINI, r, sc, 6);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else {
                        let r2 = fs.expr_to_reg(&e2.exp);
                        let op = if is_add { OpCode::ADD } else { OpCode::SUB };
                        fs.code_abc(op, r, r, r2);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_MUL && check_mulop(fs) {
            let ec = e.exp.clone();
            let is_mul = check(fs, &Token::Star);
            let is_div = check(fs, &Token::Slash);
            let is_idiv = check(fs, &Token::SlashSlash);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_MUL + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) => {
                    if is_idiv {
                        let denom = e2.exp.info;
                        if denom != 0 {
                            let val = ec.info.wrapping_div_euclid(denom);
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let r2 = fs.expr_to_reg(&e2.exp);
                            fs.code_abc(OpCode::IDIV, r, r, r2);
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                        }
                    } else if is_div {
                        let val = ec.info as f64 / e2.exp.info as f64;
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else {
                        let val = if is_mul { ec.info * e2.exp.info } else { ec.info % e2.exp.info };
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                    }
                }
                _ => {
                    let r = fs.expr_to_reg(&ec);
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
                        let op = if is_mul { OpCode::MULK } else if is_div { OpCode::DIVK } else if is_idiv { OpCode::IDIVK } else { OpCode::MODK };
                        fs.code_abc(op, dest, r, k);
                        let tm = if is_mul { 8 } else if is_div { 11 } else if is_idiv { 12 } else { 9 };
                        fs.code_abc(OpCode::MMBINK, r, k, tm);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    } else {
                        let r2 = fs.expr_to_reg(&e2.exp);
                        let op = if is_mul { OpCode::MUL } else if is_div { OpCode::DIV } else if is_idiv { OpCode::IDIV } else { OpCode::MOD };
                        fs.code_abc(op, r, r, r2);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_POW && check(fs, &Token::Caret) {
            let ec = e.exp.clone();
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_POW);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) => {
                    let base = ec.info;
                    let exp = e2.exp.info;
                    if exp >= 0 && exp <= 63 {
                        let mut result: i64 = 1;
                        for _ in 0..exp { result *= base; }
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Int, result) };
                    } else {
                        let r = fs.expr_to_reg(&ec);
                        let r2 = fs.expr_to_reg(&e2.exp);
                        let dest = fs.alloc_reg();
                        fs.code_abc(OpCode::POW, dest, r, r2);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, dest as i64) };
                    }
                }
                _ => {
                    let r = fs.expr_to_reg(&ec);
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
                        let r2 = fs.expr_to_reg(&e2.exp);
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
            let r = fs.expr_to_reg(&ei.exp);
            match op_tok {
                Token::Not => { fs.code_abc(OpCode::NOT, r, r, 0); }
                Token::Minus => { fs.code_abc(OpCode::UNM, r, r, 0); }
                Token::Hash => { fs.code_abc(OpCode::LEN, r, r, 0); }
                Token::Tilde => { fs.code_abc(OpCode::BNOT, r, r, 0); }
                _ => {}
            }
            ExpDesc::new(ExpKind::Relocable, r as i64)
        }
        Token::Function => {
            fs.ls_mut().next();
            let r = parse_body(fs);
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
                let freg = fs.expr_to_reg(&e);
                parse_func_args(fs, freg);
                e = ExpDesc::new(ExpKind::Relocable, freg as i64);
            }
            Token::Dot => {
                fs.ls_mut().next();
                let field = get_name(fs);
                let k = fs.string_k(&field);
                let base_reg = fs.expr_to_reg(&e);
                let result_reg = fs.alloc_reg();
                fs.code_abc(OpCode::GETTABLE, result_reg, base_reg, k);
                e = ExpDesc::new(ExpKind::Relocable, result_reg as i64);
            }
            Token::LBracket => {
                fs.ls_mut().next();
                let ei = parse_expr(fs);
                expect(fs, &Token::RBracket);
                let base_reg = fs.expr_to_reg(&e);
                let key_reg = fs.expr_to_reg(&ei.exp);
                let result_reg = fs.alloc_reg();
                fs.code_abc(OpCode::GETTABLE, result_reg, base_reg, key_reg);
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
        let cond_reg = fs.expr_to_reg(&ei.exp);
        fs.code_abc(OpCode::TEST, cond_reg, 0, 0);
        if_jmp = fs.jump();
        fs.free_reg();
    }

    expect(fs, &Token::Then);
    let block_freereg = fs.freereg;
    parse_block(fs);
    fs.freereg = block_freereg;
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
            let cr2 = fs.expr_to_reg(&ei2.exp);
            fs.code_abc(OpCode::TEST, cr2, 0, 0);
            if_jmp = fs.jump();
            fs.free_reg();
        } else {
            if_jmp = NO_JUMP;
        }
        expect(fs, &Token::Then);
        fs.freereg = block_freereg;
        parse_block(fs);
        fs.freereg = block_freereg;
    }

    if check(fs, &Token::Else) {
        let j = fs.jump();
        exit_jumps.push(j);
        if if_jmp != NO_JUMP {
            fs.fix_jump(if_jmp, fs.pc, false);
        }
        fs.ls_mut().next();
        fs.freereg = block_freereg;
        parse_block(fs);
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
    let r = fs.expr_to_reg(&ei.exp);
    fs.code_abc(OpCode::TEST, r, 0, 0);
    let jmp = fs.jump();
    fs.free_reg();
    expect(fs, &Token::Do);
    parse_block(fs);
    fs.code_asbx(OpCode::JMP, 0, loop_start - fs.pc - 1);
    fs.fix_jump(jmp, fs.pc, false);
    expect(fs, &Token::End);
}

/// ANTLR4: `'do' block 'end' ;`
fn parse_do(fs: &mut FuncState) {
    fs.ls_mut().next();
    parse_block(fs);
    expect(fs, &Token::End);
}

/// ANTLR4: `'repeat' block 'until' expr ;`
fn parse_repeat(fs: &mut FuncState) {
    fs.ls_mut().next();
    let loop_start = fs.pc;
    parse_block(fs);
    expect(fs, &Token::Until);
    let ei = parse_expr(fs);
    let r = fs.expr_to_reg(&ei.exp);
    fs.code_abc(OpCode::EQ, r, 0, 0);
    fs.fix_jump(fs.pc - 1, loop_start, true);
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
        let ei = parse_expr(fs);
        let init_r = fs.expr_to_reg(&ei.exp);
        expect(fs, &Token::Comma);
        let ei2 = parse_expr(fs);
        let limit_r = fs.expr_to_reg(&ei2.exp);
        
        let step_r = if check(fs, &Token::Comma) {
            fs.ls_mut().next();
            let ei3 = parse_expr(fs);
            fs.expr_to_reg(&ei3.exp)
        } else {
            let r = fs.alloc_reg();
            fs.code_asbx(OpCode::LOADI, r, 1);
            r
        };
        
        if init_r != base {
            fs.code_abc(OpCode::MOVE, base, init_r, 0);
        }
        if limit_r != base + 1 {
            fs.code_abc(OpCode::MOVE, base + 1, limit_r, 0);
        }
        if step_r != base + 2 {
            fs.code_abc(OpCode::MOVE, base + 2, step_r, 0);
        }
        fs.freereg = base + 3;
        
        expect(fs, &Token::Do);
        
        fs.add_local_kind_reg("(for state)", fs.pc, RDKCONST, base);
        fs.add_local_kind_reg("(for state)", fs.pc, RDKCONST, base + 1);
        fs.add_local_kind_reg(&name, fs.pc, RDKCONST, base + 2);
        
        let prep = fs.code_abx(OpCode::FORPREP, base, 0);
        parse_block(fs);
        fs.fix_jump(prep, fs.pc, false);
        let loop_pc = fs.code_abx(OpCode::FORLOOP, base, 0);
        fs.fix_jump(loop_pc, prep + 1, true);
        
        for local in &mut fs.locals[saved_nlocals..] {
            local.active = false;
        }
        fs.freereg = saved_freereg;
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
        for var_name in &vars {
            fs.add_local_kind(var_name, fs.pc, RDKCONST);
        }
        
        fs.freereg = base;
        let pc_before = fs.pc;
        loop {
            parse_expr(fs);
            if !check(fs, &Token::Comma) { break; }
            fs.ls_mut().next();
        }
        
        if fs.pc > pc_before {
            for i in (pc_before..fs.pc).rev() {
                if get_opcode(fs.proto.code[i as usize]) == OpCode::CALL {
                    let needed = (3.max(ncontrol) + 1).min(255);
                    setarg(&mut fs.proto.code[i as usize], needed, POS_C, SIZE_C);
                    break;
                }
            }
        }
        
        fs.code_abc(OpCode::LOADNIL, base + 1, base + 2, 0);
        fs.freereg = base + 3 + ncontrol;
        fs.needclose = true;
        if fs.freereg > fs.max_freereg {
            fs.max_freereg = fs.freereg;
        }
        
        expect(fs, &Token::Do);
        
        let prep = fs.code_abx(OpCode::TFORPREP, base, 0);
        fs.freereg -= 1;
        
        parse_block(fs);
        
        fs.fix_jump(prep, fs.pc, false);
        fs.code_abc(OpCode::TFORCALL, base, 0, ncontrol);
        let loop_pc = fs.code_abx(OpCode::TFORLOOP, base, 0);
        fs.fix_jump(loop_pc, prep + 1, true);
        
        expect(fs, &Token::End);
        
        fs.code_abc(OpCode::CLOSE, base, 0, 0);
        fs.freereg = saved_freereg;
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
        let r = parse_body_ex(fs, false);
        if let Some(reg) = fs.find_local(name) {
            fs.code_abc(OpCode::MOVE, reg, r, 0);
        } else {
            let k = fs.string_k(name);
            fs.code_abc(OpCode::SETTABUP, 0, k, r);
        }
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
        let next_reg = fs.alloc_reg();
        fs.code_abc(OpCode::GETTABLE, next_reg, base_reg, k);
        base_reg = next_reg;
    }

    let (is_colon, last_name) = &chain[last_idx];
    let freg = parse_body_ex(fs, *is_colon);
    let fk = fs.string_k(last_name);
    fs.code_abc(OpCode::SETTABLE, base_reg, fk, freg);
}

/// ANTLR4: `'local' 'function' NAME funcbody | 'local' attnamelist ('=' explist)? ;`
fn parse_local(fs: &mut FuncState) {
    fs.ls_mut().next();
    
    if check(fs, &Token::Function) {
        fs.ls_mut().next();
        let name = get_name(fs);
        let reg = fs.add_local(&name, fs.pc);
        let r = parse_body(fs);
        fs.code_abc(OpCode::MOVE, reg, r, 0);
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
                        let r = ei.exp.info as i32;
                        if r != target {
                            fs.code_abc(OpCode::MOVE, target, r, 0);
                        }
                        last_exp = Some(ExpDesc::new(ExpKind::Relocable, target as i64));
                    }
                    _ => {
                        fs.freereg = target;
                        let _ = fs.expr_to_reg(&ei.exp);
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

            for i in 0..n_reg {
                fs.add_local_kind_reg(&names[i], fs.pc, kinds[i], saved_freereg + i as i32);
            }
            if last_is_ctc {
                fs.add_local_kind(&names[nvars - 1], fs.pc, RDKCTC);
            }

            fs.freereg = saved_freereg + n_reg as i32;

            for _ in n_vals..n_reg {
                fs.code_abc(OpCode::LOADNIL, saved_freereg + n_vals as i32, 0, 0);
                n_vals += 1;
            }
        } else {
            for i in 0..nvars {
                let reg = fs.add_local_kind(&names[i], fs.pc, kinds[i]);
                fs.code_abc(OpCode::LOADNIL, reg, 0, 0);
            }
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
    let r = fs.expr_to_reg(&ei.exp);
    
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
    fs.code_ax(OpCode::NEWTABLE, 0);
    let mut n_arr: i32 = 0;
    
    if !check(fs, &Token::RBrace) {
        loop {
            if check(fs, &Token::LBracket) {
                fs.ls_mut().next();
                let ek = parse_expr(fs);
                let k_r = fs.expr_to_reg(&ek.exp);
                expect(fs, &Token::RBracket);
                expect(fs, &Token::Eq);
                let ev = parse_expr(fs);
                let v_r = fs.expr_to_reg(&ev.exp);
                fs.code_abc(OpCode::SETTABLE, table_r, k_r, v_r);
                fs.free_reg();
                fs.free_reg();
            } else if let Token::Name(s) = &fs.ls().token {
                let name = s.clone();
                let next_is_eq = fs.ls_mut().lookahead_next().0 == Token::Eq;
                if next_is_eq {
                    fs.ls_mut().next();
                    fs.ls_mut().next();
                    let ev = parse_expr(fs);
                    let v_r = fs.expr_to_reg(&ev.exp);
                    let k = fs.string_k(&name);
                    fs.code_abc(OpCode::SETI, table_r, k, v_r);
                    fs.free_reg();
                } else {
                    n_arr += 1;
                    let ev = parse_expr(fs);
                    let v_r = fs.expr_to_reg(&ev.exp);
                    fs.code_abc(OpCode::SETI, table_r, n_arr, v_r);
                    fs.free_reg();
                }
            } else {
                n_arr += 1;
                let ev = parse_expr(fs);
                let v_r = fs.expr_to_reg(&ev.exp);
                fs.code_abc(OpCode::SETI, table_r, n_arr, v_r);
                fs.free_reg();
            }
            
            if !check(fs, &Token::Comma) && !check(fs, &Token::Semi) { break; }
            fs.ls_mut().next();
            if check(fs, &Token::RBrace) { break; }
        }
    }
    expect(fs, &Token::RBrace);
    
    (table_r, n_arr)
}

/// ANTLR4: `funcbody: '(' parlist? ')' block 'end' ;` — 解析函数体 (非 method)
fn parse_body(fs: &mut FuncState) -> i32 {
    parse_body_ex(fs, false)
}

/// ANTLR4: `funcbody: '(' parlist? ')' block 'end' ;` — 解析函数体 (可指定 method 添加 self 参数)
fn parse_body_ex(fs: &mut FuncState, ismethod: bool) -> i32 {
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
    
    parse_chunk(&mut new_fs);
    expect(&mut new_fs, &Token::End);
    
    let proto = new_fs.proto;
    let p_idx = fs.proto.protos.len() as i32;
    fs.proto.protos.push(proto);
    let r = fs.alloc_reg();
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