use crate::objects::*;
use crate::objects::PF_VAHID;
use crate::opcodes::*;
use super::lexer::{LexState, Token};

use crate::objects::Instruction;

const NO_JUMP: i32 = -1;

const VDKREG: i32 = 0;
const RDKCONST: i32 = 1;
const RDKVAVAR: i32 = 2;
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
    VVARGVAR, VVARGIND,
}

#[derive(Debug, Clone)]
pub struct ExpDesc {
    pub kind: ExpKind,
    pub info: i64,
    pub info2: i32,
    pub t: i32,
    pub f: i32,
    /// For ExpKind::Str: stores the string value before it's added to the constant table.
    /// When Some(s), the string hasn't been added yet (info is unused).
    /// When None, the string has been added and info stores the constant index.
    pub str_val: Option<String>,
}

impl ExpDesc {
    pub fn new(kind: ExpKind, info: i64) -> Self {
        ExpDesc { kind, info, info2: -1, t: NO_JUMP, f: NO_JUMP, str_val: None }
    }

    pub fn new_str(s: String) -> Self {
        ExpDesc { kind: ExpKind::Str, info: -1, info2: -1, t: NO_JUMP, f: NO_JUMP, str_val: Some(s) }
    }

    pub fn new_reloc_with_pc(info: i64, pc: i32) -> Self {
        ExpDesc { kind: ExpKind::Relocable, info, info2: pc, t: NO_JUMP, f: NO_JUMP, str_val: None }
    }

    pub fn into_reloc_with_pc(self, info: i64, pc: i32) -> Self {
        ExpDesc { kind: ExpKind::Relocable, info, info2: pc, t: self.t, f: self.f, str_val: None }
    }

    fn has_jumps(&self) -> bool {
        self.t != self.f
    }
}

// ============================================================================
// Local variable tracking
// ============================================================================

/// Variable visible from a parent/grandparent function.
/// Used by find_upvalue to create upvalue references.
#[derive(Clone)]
struct ParentVar {
    name: String,
    is_local: bool,       // true = direct parent's local, false = inherited from ancestor
    // is_local=true:
    reg: i32,             // register in direct parent
    local_idx: usize,     // index in direct parent's locals array
    // is_local=false:
    upval_idx: usize,     // index in direct parent's upvalues array (0 = not yet created)
}

#[derive(Clone)]
struct LocalVar {
    name: String,
    start_pc: i32,
    active: bool,
    reg: i32,
    kind: i32,
    ctc_kind: Option<ExpKind>,
    ctc_info: Option<i64>,
    ctc_str: Option<String>,
    vidx: i32,  // variable index at declaration time (like C's vidx = nactvar at declaration)
}

struct LabelDesc {
    name: String,
    pc: i32,       // label 位置（跳转目标）
    nactvar: i32,  // label 处的 locals 索引（对应 C 的 bl->nactvar，在紧凑数组中索引=计数）
    line: i32,
}

struct GotoDesc {
    name: String,
    pc: i32,       // JMP 指令的 pc
    line: i32,
    nactvar: i32,  // goto 处的 locals 索引（对应 C 的 fs->nactvar）
    close: bool,   // 是否需要 CLOSE
}

/// Corresponds to C's BlockCnt - tracks block nesting and upvalue flags
#[derive(Clone, Copy)]
struct BlockEntry {
    saved_nlocals: usize,   // locals index at block entry (like C's bl->nactvar in compact array)
    has_upval: bool,        // like C's bl->upval
    is_function_body: bool, // true for the function body block (C's bl->previous==NULL)
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
    pub prev: *mut FuncState,  // raw pointer to parent FuncState (like C's fs->prev)
    pub pc: i32,
    pub freereg: i32,
    pub max_freereg: i32,
    pub locals: Vec<LocalVar>,
    pub errors: Vec<String>,
    pub needclose: bool,
    pub parent_locals: Vec<ParentVar>,  // variables visible from parent/grandparent functions
    pub break_list: i32,
    pub labels: Vec<LabelDesc>,
    pub gotos: Vec<GotoDesc>,
    lasttarget: i32,
    block_stack: Vec<BlockEntry>,  // 每个块的信息，栈顶是当前块
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
            prev: std::ptr::null_mut(),
            pc: 0,
            freereg: 0,
            max_freereg: 0,
            locals: Vec::new(),
            errors: Vec::new(),
            needclose: false,
            parent_locals: Vec::new(),
            break_list: NO_JUMP,
            labels: Vec::new(),
            gotos: Vec::new(),
            lasttarget: 0,
            block_stack: Vec::new(),
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

    fn code_nil(&mut self, from: i32, n: i32) {
        let l = from + n - 1;
        if self.pc > self.lasttarget {
            let previous = self.proto.code[self.pc as usize - 1];
            if get_opcode(previous) == OpCode::LOADNIL {
                let pfrom = getarg_a(previous);
                let pl = pfrom + getarg_b(previous);
                if (pfrom <= from && from <= pl + 1) || (from <= pfrom && pfrom <= l + 1) {
                    let new_from = if pfrom < from { pfrom } else { from };
                    let new_l = if pl > l { pl } else { l };
                    self.set_a(self.pc - 1, new_from);
                    self.set_b(self.pc - 1, new_l - new_from);
                    return;
                }
            }
        }
        self.code_abc(OpCode::LOADNIL, from, n - 1, 0);
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

    /// 设置指定指令的 B 参数
    fn set_b(&mut self, pc: i32, b: i32) {
        let i = &mut self.proto.code[pc as usize];
        setarg(i, b, POS_B, SIZE_B);
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

    /// 将 ExpKind::Str 表达式的字符串添加到常量表（如果尚未添加），
    /// 返回常量索引。这实现了延迟添加，与 C++ 编译器的 VKSTR 行为一致。
    fn discharge_str(&mut self, e: &mut ExpDesc) -> i32 {
        if let Some(ref s) = e.str_val {
            let k = self.string_k(s);
            e.info = k as i64;
            e.str_val = None;
            k
        } else {
            e.info as i32
        }
    }

    /// 获取 ExpKind::Str 表达式的常量索引（不修改 ExpDesc）。
    /// 如果字符串尚未添加到常量表，则添加之。
    fn get_str_k(&mut self, e: &ExpDesc) -> i32 {
        if let Some(ref s) = e.str_val {
            self.string_k(s)
        } else {
            e.info as i32
        }
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

    fn free_exp_reg(&mut self, e: &ExpDesc) {
        if matches!(e.kind, ExpKind::NonReloc | ExpKind::Relocable) && (e.info as i32) >= self.nvarstack() && (e.info as i32) == self.freereg - 1 {
            self.free_reg();
        }
    }

    fn free_exps(&mut self, e1: &ExpDesc, e2: &ExpDesc) {
        let r1 = if matches!(e1.kind, ExpKind::NonReloc | ExpKind::Relocable) && (e1.info as i32) >= self.nvarstack() {
            e1.info as i32
        } else {
            -1
        };
        let r2 = if matches!(e2.kind, ExpKind::NonReloc | ExpKind::Relocable) && (e2.info as i32) >= self.nvarstack() {
            e2.info as i32
        } else {
            -1
        };
        if r1 > r2 {
            if r1 >= 0 && r1 == self.freereg - 1 { self.free_reg(); }
            if r2 >= 0 && r2 == self.freereg - 1 { self.free_reg(); }
        } else {
            if r2 >= 0 && r2 == self.freereg - 1 { self.free_reg(); }
            if r1 >= 0 && r1 == self.freereg - 1 { self.free_reg(); }
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
        let vidx = self.locals.len() as i32;
        self.locals.push(LocalVar {
            name: name.to_string(),
            start_pc,
            active: true,
            reg,
            kind: VDKREG,
            ctc_kind: None,
            ctc_info: None,
            ctc_str: None,
            vidx,
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
        let vidx = self.locals.len() as i32;
        self.locals.push(LocalVar {
            name: name.to_string(),
            start_pc,
            active: true,
            reg,
            kind,
            ctc_kind: None,
            ctc_info: None,
            ctc_str: None,
            vidx,
        });
        // Like C's marktobeclosed: mark current block as needing CLOSE
        if kind == RDKTOCLOSE {
            if let Some(blk) = self.block_stack.last_mut() {
                blk.has_upval = true;
            }
            self.needclose = true;
        }
        reg
    }

    /// 添加指定寄存器的局部变量
    fn add_local_kind_reg(&mut self, name: &str, start_pc: i32, kind: i32, reg: i32) {
        let vidx = self.locals.len() as i32;
        self.locals.push(LocalVar {
            name: name.to_string(),
            start_pc,
            active: true,
            reg,
            kind,
            ctc_kind: None,
            ctc_info: None,
            ctc_str: None,
            vidx,
        });
    }

    /// 在当前作用域中查找局部变量 (从后往前)
    fn find_local(&self, name: &str) -> Option<i32> {
        for lv in self.locals.iter().rev() {
            if lv.active && lv.name == name && lv.kind <= RDKTOCLOSE {
                return Some(lv.reg);
            }
        }
        None
    }

    fn find_local_ctc(&mut self, name: &str) -> Option<ExpDesc> {
        let ctc_str = {
            let mut found = None;
            for lv in self.locals.iter().rev() {
                if lv.active && lv.name == name && lv.kind == RDKCTC {
                    let kind = lv.ctc_kind.clone().unwrap();
                    if kind == ExpKind::Str {
                        if let Some(ref s) = lv.ctc_str {
                            found = Some(s.clone());
                            break;
                        }
                    }
                    break;
                }
            }
            found
        };
        if let Some(s) = ctc_str {
            return Some(ExpDesc::new_str(s));
        }
        for lv in self.locals.iter().rev() {
            if lv.active && lv.name == name && lv.kind == RDKCTC {
                return Some(ExpDesc::new(lv.ctc_kind.clone().unwrap(), lv.ctc_info.unwrap()));
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
        // Search from the end (like C's searchvar which searches from nactvar-1 down)
        // to find the innermost variable with the given name.
        for (j, pvar) in self.parent_locals.iter().enumerate().rev() {
            if pvar.name == name {
                let idx = self.proto.upvalues.len() as i32;
                let t = crate::strings::StringTable::new();
                let ls = crate::strings::new_lstr(&t, name);
                if pvar.is_local {
                    // Variable is a local in the direct parent function
                    self.proto.upvalues.push(crate::objects::UpvalDesc {
                        name: Some(ls),
                        in_stack: true,
                        idx: pvar.reg as u8,
                        parent_local_idx: pvar.local_idx,
                    });
                } else {
                    // Variable is inherited from a grandparent function.
                    // We need to find or create the corresponding upvalue in the
                    // direct parent first, then reference it as in_stack=false.
                    // Since we can't access the parent's FuncState here (it's in self.prev),
                    // we search the parent's existing upvalues by name.
                    // The parent's upvalues are accessible through self.prev.
                    let parent_upval_idx = self.find_or_create_parent_upvalue(name);
                    self.proto.upvalues.push(crate::objects::UpvalDesc {
                        name: Some(ls),
                        in_stack: false,
                        idx: parent_upval_idx as u8,
                        parent_local_idx: 0, // not applicable for in_stack=false
                    });
                }
                self.proto.size_upvalues = self.proto.upvalues.len() as i32;
                let _ = j; // index for potential future use
                return Some(idx);
            }
        }
        None
    }

    /// Find or create an upvalue in the direct parent function for a variable
    /// inherited from a grandparent. Returns the upvalue index in the parent.
    fn find_or_create_parent_upvalue(&mut self, name: &str) -> usize {
        let prev = self.prev;
        if prev.is_null() {
            return 0;
        }
        // SAFETY: prev is set in parse_body to point to the parent FuncState,
        // which is guaranteed to be alive for the duration of the child's compilation.
        let prev = unsafe { &mut *prev };
        // First, check if the parent already has an upvalue for this name
        for (i, uv) in prev.proto.upvalues.iter().enumerate() {
            if let Some(ref n) = uv.name {
                if n.as_str() == name {
                    return i;
                }
            }
        }
        // Search parent_locals for the variable
        for (j, pvar) in prev.parent_locals.iter().enumerate().rev() {
            if pvar.name == name {
                let t = crate::strings::StringTable::new();
                let ls = crate::strings::new_lstr(&t, name);
                let idx = prev.proto.upvalues.len();
                if pvar.is_local {
                    prev.proto.upvalues.push(crate::objects::UpvalDesc {
                        name: Some(ls),
                        in_stack: true,
                        idx: pvar.reg as u8,
                        parent_local_idx: pvar.local_idx,
                    });
                } else {
                    // The parent also inherited this from its grandparent.
                    // We need to recursively create upvalues up the chain.
                    let grandparent_upval_idx = prev.find_or_create_parent_upvalue(name);
                    prev.proto.upvalues.push(crate::objects::UpvalDesc {
                        name: Some(ls),
                        in_stack: false,
                        idx: grandparent_upval_idx as u8,
                        parent_local_idx: 0,
                    });
                }
                prev.proto.size_upvalues = prev.proto.upvalues.len() as i32;
                return idx;
            }
        }
        // Should not happen if the variable exists somewhere in the chain
        0
    }

    /// 查找局部变量并返回 (寄存器号, 种类)
    /// 跳过 GDKREG/GDKCONST 类型的变量（它们应作为全局变量处理）
    fn find_local_ex(&self, name: &str) -> Option<(i32, i32)> {
        for lv in self.locals.iter().rev() {
            if lv.active && lv.name == name && lv.kind <= RDKTOCLOSE {
                return Some((lv.reg, lv.kind));
            }
        }
        None
    }

    /// 获取当前标签位置（即下一个指令的 pc）
    fn get_label(&self) -> i32 {
        self.pc
    }

    /// 将表达式结果确保在寄存器中: 根据 ExpKind 生成相应 LOAD/MOVE 指令
    fn expr_to_reg(&mut self, e: &ExpDesc) -> i32 {
        match e.kind {
            ExpKind::VVARGVAR => {
                self.proto.flag |= PF_VATAB;
                // Convert to NonReloc - the vararg table is in the register
                let r = e.info as i32;
                if r < self.nvarstack() {
                    r
                } else if r == self.freereg - 1 {
                    r
                } else {
                    let dst = self.alloc_reg();
                    self.code_abc(OpCode::MOVE, dst, r, 0);
                    dst
                }
            }
            ExpKind::VVARGIND => {
                // VVARGIND should not reach here in current implementation;
                // it's handled directly in parse_prefix_exp/parse_assign_or_call
                self.proto.flag |= PF_VATAB;
                let r = e.info as i32;
                r
            }
            ExpKind::Void | ExpKind::Nil => {
                let r = self.alloc_reg();
                self.code_nil(r, 1);
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
                if fits_sbx(val) {
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
                let k = self.get_str_k(e);
                self.code_abx(OpCode::LOADK, r, k);
                r
            }
            ExpKind::NonReloc => {
                if (e.info as i32) < self.nvarstack() {
                    e.info as i32
                } else if e.info as i32 == self.freereg - 1 {
                    e.info as i32
                } else {
                    let r = self.alloc_reg();
                    self.code_abc(OpCode::MOVE, r, e.info as i32, 0);
                    r
                }
            }
            ExpKind::Call => {
                if e.info as i32 >= self.nvarstack() && e.info as i32 == self.freereg - 1 {
                    e.info as i32
                } else {
                    let r = self.alloc_reg();
                    self.code_abc(OpCode::MOVE, r, e.info as i32, 0);
                    if e.info2 == -2 {
                        self.code_abc(OpCode::NOT, r, r, 0);
                    }
                    r
                }
            }
            ExpKind::Relocable | ExpKind::Vararg => {
                if e.info2 >= 0 {
                    // 模拟 C 的 freeexps: 非局部寄存器且位于 freereg-1 时先释放
                    if e.info as i32 == self.freereg - 1 && e.info as i32 >= self.nvarstack() {
                        self.free_reg();
                    }
                    let r = self.alloc_reg();
                    self.set_a(e.info2, r);
                    r
                } else if e.info as i32 == self.freereg - 1 {
                    if e.info2 == -2 {
                        self.code_abc(OpCode::NOT, e.info as i32, e.info as i32, 0);
                    }
                    e.info as i32
                } else {
                    let r = self.alloc_reg();
                    self.code_abc(OpCode::MOVE, r, e.info as i32, 0);
                    if e.info2 == -2 {
                        self.code_abc(OpCode::NOT, r, r, 0);
                    }
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

    /// Like exp_to_reg, but always places the result in a newly allocated register.
    /// Equivalent to C++ luaK_exp2nextreg. Used in table constructors where
    /// array values must occupy consecutive registers after the table register.
    fn exp_to_next_reg(&mut self, e: &ExpDesc) -> i32 {
        let r = self.expr_to_reg(e);
        if r == self.freereg - 1 {
            // Already in the last allocated register
            self.resolve_jumps(e, r);
            r
        } else {
            // Need to move to a new register
            let dst = self.alloc_reg();
            self.code_abc(OpCode::MOVE, dst, r, 0);
            self.resolve_jumps(e, dst);
            dst
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
        if node < 1 || (node as usize) >= self.proto.code.len() + 1 {
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
        self.nvarstack_up_to(self.locals.len())
    }

    /// 计算 saved_nlocals 范围内活跃变量的寄存器层级
    /// 用于 set_freereg（恢复寄存器），只计算当前活跃的变量
    fn nvarstack_up_to(&self, saved_nlocals: usize) -> i32 {
        let mut reglevel = 0;
        for i in 0..saved_nlocals {
            if self.locals[i].active && self.locals[i].kind <= RDKTOCLOSE {
                reglevel = self.locals[i].reg + 1;
            }
        }
        reglevel
    }

    /// 将变量索引 nvar 转换为寄存器层级（对应 C 的 reglevel）
    /// C 的 reglevel 从 nvar-1 向下迭代到 0，找到第一个 varinreg 的变量
    /// C 的紧凑数组中 0..nactvar 的所有变量都"活跃"，但 Rust 的 locals 数组中
    /// 有 inactive 变量（已退出块的变量仍保留在 Vec 中），需要跳过它们才能
    /// 得到正确的寄存器层级，否则会找到已退出块的变量返回错误的 reg+1。
    fn reglevel(&self, nvar: i32) -> i32 {
        for i in (0..nvar as usize).rev() {
            if i < self.locals.len() && self.locals[i].active && self.locals[i].kind <= RDKTOCLOSE {
                return self.locals[i].reg + 1;
            }
        }
        0
    }

    // Like C's markupval: mark the block where the variable at the given
    // locals array index was declared as having upvalues.
    // Uses saved_nlocals (locals array length at block entry) for block identification.
    // C's markupval uses bl->nactvar which is an index in the compact array;
    // we use saved_nlocals which is the equivalent index in our sparse array.
    /// `var_idx` is the index in the `locals` array of the variable being captured.
    fn mark_block_upval(&mut self, var_idx: usize) {
        self.needclose = true;
        // Like C's markupval: find the first block where nactvar <= level.
        // C's 'level' is the variable's vidx (nactvar at declaration time).
        // In Rust, vidx = locals.len() at declaration time (index in locals array),
        // and saved_nlocals is the block's nactvar equivalent.
        let var_vidx = self.locals[var_idx].vidx;
        for i in (0..self.block_stack.len()).rev() {
            if self.block_stack[i].saved_nlocals as i32 <= var_vidx {
                self.block_stack[i].has_upval = true;
                return;
            }
        }
    }

    // Check if the current (innermost) block has been marked as having upvalues
    fn current_block_has_upval(&self) -> bool {
        self.block_stack.last().map(|b| b.has_upval).unwrap_or(false)
    }

    // Returns true if current block has to-be-closed variables
    fn current_block_has_tbc(&self, saved_nlocals: usize) -> bool {
        self.locals[saved_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active)
    }

    /// Mark the innermost non-function block as having upvalues.
    /// Used for !in_stack upvalues where the parent also has an upvalue.
    fn mark_block_for_upval(&mut self) {
        self.needclose = true;
        for i in (0..self.block_stack.len()).rev() {
            if !self.block_stack[i].is_function_body {
                self.block_stack[i].has_upval = true;
                return;
            }
        }
    }

    /// For !in_stack upvalues, recursively mark ancestor functions' blocks.
    /// The upvalue at `idx` in the current function references an upvalue
    /// in the parent function. We need to ensure the parent's blocks are
    /// also marked, using the correct vidx (like C's markupval).
    fn mark_ancestor_blocks_for_upval(&mut self, idx: usize) {
        let prev_ptr = self.prev;
        if prev_ptr.is_null() { return; }
        // SAFETY: prev is set in parse_body to point to the parent FuncState,
        // which is guaranteed to be alive for the duration of the child's compilation.
        let prev = unsafe { &mut *prev_ptr };
        if idx < prev.proto.upvalues.len() {
            let parent_uv = &prev.proto.upvalues[idx];
            if parent_uv.in_stack {
                // The parent's upvalue is in_stack, so the variable is a local
                // in the parent. Call mark_block_upval with the local's index
                // (like C's markupval with the variable's vidx).
                let local_idx = parent_uv.parent_local_idx;
                let is_active = local_idx < prev.locals.len() && prev.locals[local_idx].active;
                if is_active {
                    prev.mark_block_upval(local_idx);
                }
            } else {
                // The parent's upvalue is also !in_stack, so we need to
                // mark the parent's innermost non-function block and
                // recursively continue up the chain.
                prev.needclose = true;
                for i in (0..prev.block_stack.len()).rev() {
                    if !prev.block_stack[i].is_function_body {
                        prev.block_stack[i].has_upval = true;
                        break;
                    }
                }
                let parent_uv_idx = parent_uv.idx as usize;
                prev.mark_ancestor_blocks_for_upval(parent_uv_idx);
            }
        }
    }
}

/// 编译后处理：对 RETURN 指令做最终优化和调整
/// 参照 C++ luaK_finish 实现
fn parse_chunk_finish(fs: &mut FuncState) {
    let proto = &mut fs.proto;
    // If function uses a vararg table, it will not use hidden args
    if proto.flag & PF_VATAB != 0 {
        proto.flag &= !PF_VAHID;
    }
    for i in 0..proto.code.len() {
        let inst = &mut proto.code[i];
        let op = get_opcode(*inst);
        match op {
            OpCode::RETURN0 | OpCode::RETURN1 => {
                if !(fs.needclose || (proto.flag & PF_VAHID) != 0) {
                    continue;
                }
                SET_OPCODE(inst, OpCode::RETURN);
            }
            OpCode::GETVARG => {
                if proto.flag & PF_VATAB != 0 {
                    SET_OPCODE(inst, OpCode::GETTABLE);
                }
            }
            OpCode::VARARG => {
                if proto.flag & PF_VATAB != 0 {
                    SETARG_k(inst, 1);
                }
            }
            _ => {}
        }
        match op {
            OpCode::RETURN0 | OpCode::RETURN1 | OpCode::RETURN | OpCode::TAILCALL => {
                if fs.needclose {
                    SETARG_k(inst, 1);
                }
                if (proto.flag & PF_VAHID) != 0 {
                    SETARG_C(inst, proto.num_params as i32 + 1);
                }
            }
            _ => {}
        }
    }

    // Check for unresolved gotos
    if !fs.gotos.is_empty() {
        let gt = &fs.gotos[0];
        fs.error(&format!("no visible label '{}' for goto", gt.name));
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
// Goto and Label support
// ============================================================================

/// 在 labels 中查找标签
fn find_label(fs: &FuncState, name: &str) -> Option<usize> {
    for (i, lb) in fs.labels.iter().enumerate().rev() {
        if lb.name == name {
            return Some(i);
        }
    }
    None
}

/// Like C's findlabel: search labels from the end, starting at or after given index
fn find_label_from(fs: &FuncState, name: &str, start_idx: usize) -> Option<usize> {
    for (i, lb) in fs.labels.iter().enumerate().rev() {
        if i >= start_idx && lb.name == name {
            return Some(i);
        }
    }
    None
}

/// 创建标签 (like C's createlabel)
/// `last`: whether the label is the last non-op statement in its block.
/// When true, locals are assumed to already be out of scope, so nactvar
/// is set to the block's entry level (bl->nactvar), not the current level.
fn create_label(fs: &mut FuncState, name: &str, line: i32, last: bool) {
    let pc = fs.get_label();
    fs.lasttarget = fs.pc;  // mark label position as jump target (like luaK_getlabel)
    // C's newlabelentry uses ls->fs->nactvar (current count of active variables
    // at label creation time). In Rust, fs.locals.len() is the equivalent.
    let mut nactvar = fs.locals.len() as i32;
    if last {
        // C's createlabel: "assume that locals are already out of scope"
        // Use the block's nactvar (saved_nlocals) instead of current level.
        if let Some(blk) = fs.block_stack.last() {
            nactvar = blk.saved_nlocals as i32;
        }
    }

    fs.labels.push(LabelDesc {
        name: name.to_string(),
        pc,
        nactvar,
        line,
    });
}

/// 解决匹配的 goto：遍历 gotos，找到名字匹配的，修补跳转
fn solve_goto(fs: &mut FuncState, name: &str) {
    let mut i = 0;
    while i < fs.gotos.len() {
        if fs.gotos[i].name == name {
            // Check if label exists before removing the goto
            if let Some(lb_idx) = find_label(fs, name) {
                let gt = fs.gotos.remove(i);
                let mut gt_pc = gt.pc;
                // C closegoto condition: gt->close || (label->nactvar < gt->nactvar && bup)
                // nactvar is now an INDEX (like C), so compare directly
                let lb = &fs.labels[lb_idx];
                let need_close = gt.close || lb.nactvar < gt.nactvar;

                if need_close {
                    // Like C's closegoto: move JMP to gt_pc+1, create new CLOSE at gt_pc
                    let stklevel = fs.reglevel(lb.nactvar);
                    fs.proto.code[(gt_pc + 1) as usize] = fs.proto.code[gt_pc as usize];
                    fs.proto.code[gt_pc as usize] = create_abck(OpCode::CLOSE, stklevel, 0, 0, 0);
                    gt_pc += 1; // Now JMP is at gt_pc
                }
                // Patch the JMP to jump to the label
                let lb = &fs.labels[lb_idx];

                fs.fix_jump(gt_pc, lb.pc, false);
            } else {
                i += 1; // Label not found yet, keep the goto
            }
        } else {
            i += 1;
        }
    }
}

/// 解析 goto NAME 语句
fn parse_goto_stat(fs: &mut FuncState) {
    let name = get_name(fs);
    let line = fs.ls().lastline;
    let pc = fs.jump();
    fs.code_abc(OpCode::CLOSE, 0, 1, 0);
    // Like C's newgotoentry: nactvar = fs->nactvar (index of active variables)
    let nactvar = fs.locals.len() as i32;
    fs.gotos.push(GotoDesc {
        name: name.clone(),
        pc,
        line,
        nactvar,
        close: false,
    });
    // Don't solve goto here - defer to block exit (like C's solvegotos)
    // so that bup (has_upval) is correctly determined
}

/// 块退出时处理 goto：解决当前块中的 goto，清理 labels
fn solve_gotos_for_block(fs: &mut FuncState, saved_nlabels: usize, saved_nlocals: usize, needclose: bool) {
    let nactvar = saved_nlocals as i32;  // block's nactvar (index, like C's bl->nactvar)
    let mut i = 0;
    while i < fs.gotos.len() {
        let gt_name = fs.gotos[i].name.clone();
        // Only resolve against labels that belong to this block (at or after saved_nlabels)
        if let Some(lb_idx) = find_label_from(fs, &gt_name, saved_nlabels) {
            // Found a matching label in this block - solve it
            // (like C's closegoto)
            let gt = fs.gotos.remove(i);
            let mut gt_pc = gt.pc;
            // C closegoto condition: gt->close || (label->nactvar < gt->nactvar && bup)
            let lb = &fs.labels[lb_idx];
            let need_close = gt.close || (needclose && lb.nactvar < gt.nactvar);

            if need_close {
                // Like C's closegoto: move jump to CLOSE+1, put CLOSE at original position
                let stklevel = fs.reglevel(lb.nactvar);
                fs.proto.code[(gt_pc + 1) as usize] = fs.proto.code[gt_pc as usize];
                fs.proto.code[gt_pc as usize] = create_abck(OpCode::CLOSE, stklevel, 0, 0, 0);
                gt_pc += 1;
            }
            let lb = &fs.labels[lb_idx];
            fs.fix_jump(gt_pc, lb.pc, false);
        } else {
            // Unresolved goto: if block has upvalue and goto escapes scope, mark close=true
            // C: if (bl->upval && reglevel(fs, gt->nactvar) > outlevel) gt->close = 1;
            let outlevel = fs.reglevel(nactvar);
            if needclose && fs.reglevel(fs.gotos[i].nactvar) > outlevel {
                fs.gotos[i].close = true;
            }
            // Like C: gt->nactvar = bl->nactvar
            fs.gotos[i].nactvar = nactvar;
            i += 1;
        }
    }
    // Remove local labels
    fs.labels.truncate(saved_nlabels);
}

// ============================================================================
// Parser entry
// ============================================================================

/// ANTLR4: `chunk: block ;` — 解析顶层脚本块，末了生成 RETURN 指令
fn parse_chunk(fs: &mut FuncState) {
    // Like C's open_func: create a function body block (nactvar=0, previous=NULL).
    // This block is needed so mark_block_upval can find it when a closure
    // captures function-level variables. We use is_function_body=true to
    // prevent generating CLOSE on exit (C's leaveblock skips CLOSE when
    // bl->previous==NULL).
    let saved_nlocals = fs.locals.len();
    let saved_nlabels = fs.labels.len();
    fs.block_stack.push(BlockEntry { saved_nlocals, has_upval: false, is_function_body: true });

    let is_last = block_follow(fs, true);
    if !is_last {
        parse_chunk_stmts(fs);
    }
    let nvarstack = fs.nvarstack();
    fs.return_stat_gen(nvarstack, 0);

    // Leave function body block (like C's leaveblock for the function body block).
    // Don't generate CLOSE because this is the function body block (previous=NULL in C).
    let has_upval = fs.current_block_has_upval();
    fs.block_stack.pop();
    // Solve gotos for the function body block (like C's solvegotos in leaveblock)
    solve_gotos_for_block(fs, saved_nlabels, saved_nlocals, has_upval);
    // Check for unresolved gotos (like C's leaveblock: if bl->previous==NULL && pending gotos)
    if !fs.gotos.is_empty() {
        // There are unresolved gotos - this is an error
        let gt = &fs.gotos[0];
        fs.errors.push(format!("{}: no visible label '{}' for goto", gt.line, gt.name));
    }
    // Deactivate any remaining variables (shouldn't be any, but just in case)
    for local in &mut fs.locals[saved_nlocals..] {
        local.active = false;
    }

    parse_chunk_finish(fs);
}

/// Like C's block(): enterblock + chunk + leaveblock.
/// Creates a block scope, parses statements, then leaves the block.
fn parse_block(fs: &mut FuncState) {
    let saved_nlocals = fs.locals.len();
    let saved_nlabels = fs.labels.len();
    fs.block_stack.push(BlockEntry { saved_nlocals, has_upval: false, is_function_body: false });

    parse_chunk_stmts(fs);

    // Leave block (like C's leaveblock)
    let has_upval = fs.current_block_has_upval();
    fs.block_stack.pop();
    let has_tbc = fs.locals[saved_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
    let close_reg = fs.nvarstack_up_to(saved_nlocals);
    if has_tbc || has_upval {
        fs.code_abc(OpCode::CLOSE, close_reg, 0, 0);
    }
    solve_gotos_for_block(fs, saved_nlabels, saved_nlocals, has_tbc || has_upval);
    // Like C's removevars: deactivate block-local variables
    for local in &mut fs.locals[saved_nlocals..] {
        local.active = false;
    }
    fs.set_freereg(close_reg);
}

/// Like C's chunk(): parse a sequence of statements without creating a block scope.
fn parse_chunk_stmts(fs: &mut FuncState) {
    while !block_follow(fs, true) {
        if check(fs, &Token::Return) {
            parse_statement(fs);
            return;
        }
        if check(fs, &Token::ColonColon) {
            let line = fs.ls().lastline;
            fs.ls_mut().next();  // skip '::'
            let name = get_name(fs);
            expect(fs, &Token::ColonColon);  // skip closing '::'
            // Like C's labelstat: skip other no-op statements before creating label,
            // then check if label is last statement in block (block_follow with until=0).
            while check(fs, &Token::Semi) { fs.ls_mut().next(); }
            while check(fs, &Token::ColonColon) {
                // nested label statement (no-op)
                parse_statement(fs);
            }
            create_label(fs, &name, line, block_follow(fs, false));
            continue;
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

/// Emit GETTABUP or fallback to GETUPVAL+LOADK+GETTABLE when constant index > MAXINDEXRK
/// Returns the PC of the instruction that produces the result.
fn code_gettabup(fs: &mut FuncState, r: i32, upval: i32, k: i32) -> i32 {
    if (k as u32) <= crate::opcodes::MAXINDEXRK {
        fs.code_abc(OpCode::GETTABUP, r, upval, k)
    } else {
        fs.code_abc(OpCode::GETUPVAL, r, upval, 0);
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        let pc = fs.code_abc(OpCode::GETTABLE, r, r, kr);
        fs.free_reg();
        pc
    }
}

/// Emit SETTABUP or fallback to GETUPVAL+LOADK+SETTABLE when constant index > MAXINDEXRK
fn code_settabup(fs: &mut FuncState, upval: i32, k: i32, val: i32) {
    if (k as u32) <= crate::opcodes::MAXINDEXRK {
        fs.code_abc(OpCode::SETTABUP, upval, k, val);
    } else {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::GETUPVAL, r, upval, 0);
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        fs.code_abc_k(OpCode::SETTABLE, r, kr, val, false);
        fs.free_reg(); // kr
        fs.free_reg(); // r
    }
}

/// Emit SETTABUP with k-bit or fallback to GETUPVAL+LOADK+SETTABLE when constant index > MAXINDEXRK
fn code_settabup_k(fs: &mut FuncState, upval: i32, k: i32, val: i32, is_k: bool) {
    if (k as u32) <= crate::opcodes::MAXINDEXRK {
        fs.code_abc_k(OpCode::SETTABUP, upval, k, val, is_k);
    } else {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::GETUPVAL, r, upval, 0);
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        fs.code_abc_k(OpCode::SETTABLE, r, kr, val, is_k);
        fs.free_reg(); // kr
        fs.free_reg(); // r
    }
}

/// Emit GETFIELD or fallback to LOADK+GETTABLE when constant index > MAXINDEXRK
/// Returns the PC of the instruction that produces the result.
fn code_getfield(fs: &mut FuncState, r: i32, table: i32, k: i32) -> i32 {
    if (k as u32) <= crate::opcodes::MAXINDEXRK {
        fs.code_abc(OpCode::GETFIELD, r, table, k)
    } else if r != table {
        // Optimization: use result register for LOADK (same as C++ compiler's codegetfield)
        fs.code_abx(OpCode::LOADK, r, k);
        fs.code_abc(OpCode::GETTABLE, r, table, r)
    } else {
        // r == table: can't use r for LOADK as it would overwrite the table value
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        let pc = fs.code_abc(OpCode::GETTABLE, r, table, kr);
        fs.free_reg(); // kr
        pc
    }
}

/// Emit SETFIELD or fallback to LOADK+SETTABLE when constant index > MAXINDEXRK
fn code_setfield(fs: &mut FuncState, table: i32, k: i32, val: i32) {
    if (k as u32) <= crate::opcodes::MAXINDEXRK {
        fs.code_abc_k(OpCode::SETFIELD, table, k, val, false);
    } else {
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        fs.code_abc_k(OpCode::SETTABLE, table, kr, val, false);
        fs.free_reg(); // kr
    }
}

/// Emit SETFIELD with k-bit or fallback to LOADK+SETTABLE when constant index > MAXINDEXRK
fn code_setfield_k(fs: &mut FuncState, table: i32, k: i32, val: i32, is_k: bool) {
    if (k as u32) <= crate::opcodes::MAXINDEXRK {
        fs.code_abc_k(OpCode::SETFIELD, table, k, val, is_k);
    } else {
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        fs.code_abc_k(OpCode::SETTABLE, table, kr, val, is_k);
        fs.free_reg(); // kr
    }
}

/// 检查全局变量是否存在: GETTABUP + ERRNNIL
fn checkglobal(fs: &mut FuncState, varname: &str, _line: i32) {
    let r = fs.alloc_reg();
    let k = fs.string_k(varname);
    code_gettabup(fs, r, 0, k);
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
                    code_settabup_k(fs, 0, k_name, k_val, true);
                } else {
                    let val_reg = fs.expr_to_reg(val);
                    code_settabup(fs, 0, k_name, val_reg);
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
    code_settabup(fs, 0, k, r);
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
            fs.ls_mut().next();
            // Use goto mechanism like C does (newgotoentry with ls->brkn)
            let pc = fs.jump();
            fs.code_abc(OpCode::CLOSE, 0, 1, 0);  // placeholder CLOSE (B=1);
            let nactvar = fs.locals.len() as i32;  // like C's fs->nactvar (index)
            fs.gotos.push(GotoDesc {
                name: "break".to_string(),
                pc,
                line: fs.ls().lastline,
                nactvar,
                close: false,
            });
            None
        },
        Token::Goto => {
            fs.ls_mut().next();  // skip 'goto'
            parse_goto_stat(fs);
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
/// check_conflict: if a non-indexed variable (local or upvalue) conflicts with
/// previously parsed indexed variables (same table/key register), save the
/// original value to a temp register so the indexed variable uses the correct
/// table/key after the local/upvalue is overwritten.
/// Matches C's check_conflict in lparser.cpp.
fn check_conflict_for_var(fs: &mut FuncState, prev_vars: &mut Vec<PrefixResult>, new_var: &PrefixResult) {
    let local_idx = new_var.local_idx;
    let upval_idx = new_var.upval_idx;
    if local_idx.is_none() && upval_idx.is_none() {
        return;
    }

    let mut has_conflict = false;
    for u in prev_vars.iter() {
        if u.table_reg.is_some() {
            if let Some(lidx) = local_idx {
                if u.table_reg == Some(lidx) {
                    has_conflict = true;
                    break;
                }
                if !u.table_key_is_const && !u.table_key_is_int && u.table_key == Some(lidx) {
                    has_conflict = true;
                    break;
                }
            }
            if let Some(uidx) = upval_idx {
                if u.is_env_upvalue && u.table_reg == Some(uidx) {
                    has_conflict = true;
                    break;
                }
            }
        }
    }

    if has_conflict {
        let saved_reg = fs.alloc_reg();
        if let Some(lidx) = local_idx {
            fs.code_abc(OpCode::MOVE, saved_reg, lidx, 0);
        } else if let Some(uidx) = upval_idx {
            fs.code_abc(OpCode::GETUPVAL, saved_reg, uidx, 0);
        }
        for u in prev_vars.iter_mut() {
            if let Some(lidx) = local_idx {
                if u.table_reg == Some(lidx) {
                    u.table_reg = Some(saved_reg);
                }
                if !u.table_key_is_const && !u.table_key_is_int && u.table_key == Some(lidx) {
                    u.table_key = Some(saved_reg);
                }
            }
            if let Some(uidx) = upval_idx {
                if u.is_env_upvalue && u.table_reg == Some(uidx) {
                    u.table_reg = Some(saved_reg);
                    u.is_env_upvalue = false;
                }
            }
        }
    }
}

fn parse_assign_or_call(fs: &mut FuncState) {
    let mut first = parse_prefix_exp(fs);
    
    let mut has_call = first.has_call;
    let mut freg: i32 = first.reg.unwrap_or(-1);
    let mut call_pc: i32 = first.call_pc;
    if !has_call && (check(fs, &Token::LParen) || check(fs, &Token::Colon) || check(fs, &Token::LBrace) || matches!(&fs.ls().token, Token::String(..))) {
        has_call = true;
        let is_method = check(fs, &Token::Colon);
        let (fr, _ef, func_allocated, src_reg) = load_func(fs, &first, is_method);
        freg = fr;
        call_pc = parse_func_args(fs, freg, src_reg);
        loop {
            match &fs.ls().token {
                Token::LParen | Token::LBrace | Token::String(_) | Token::Colon => {
                    call_pc = parse_func_args(fs, freg, None);
                }
                _ => break,
            }
        }
        first.reg = Some(freg);
        first.allocated_reg = func_allocated;
    }
    
    loop {
        match &fs.ls().token {
            Token::Dot => {
                fs.ls_mut().next();
                let field = get_name(fs);
                let k = fs.string_k(&field);

                // Handle VVARGVAR: generate GETVARG instead of GETFIELD
                if first.is_vvargvar {
                    let base_reg = first.reg.unwrap();
                    fs.proto.flag |= PF_VATAB;
                    let r = fs.alloc_reg();
                    fs.code_abc(OpCode::GETVARG, r, base_reg, k);
                    let is_short_str = field.len() <= crate::strings::LUAI_MAXSHORTLEN && (k as u32) <= crate::opcodes::MAXINDEXRK;
                    let (table_key, table_key_is_const, key_allocated_reg) = if is_short_str {
                        (k, true, false)
                    } else {
                        let kr = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, kr, k);
                        (kr, false, true)
                    };
                    first = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(r),
                        table_reg: Some(r), table_key: Some(table_key), table_key_is_const: table_key_is_const, table_key_is_int: false,
                        key_allocated_reg: key_allocated_reg,
                        allocated_reg: true,
                        is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1,
                        has_call: false, call_pc: -1, is_vvargvar: false,
                    };
                    continue;
                }

                let is_short_str = field.len() <= crate::strings::LUAI_MAXSHORTLEN && (k as u32) <= crate::opcodes::MAXINDEXRK;
                let (base_reg, gettabup_pc) = if let Some(r) = first.reg {
                    (r, -1)
                } else if first.is_env_upvalue {
                    if !is_short_str {
                        // Key exceeds MAXINDEXRK: must load _ENV into a register
                        let r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETUPVAL, r, 0, 0);
                        (r, -1)
                    } else {
                        (0, -1)
                    }
                } else {
                    let r = fs.alloc_reg();
                    let pc = if let Some(key) = first.key {
                        code_gettabup(fs, r, 0, key);
                        fs.pc - 1
                    } else {
                        -1
                    };
                    (r, pc)
                };
                let (table_key, table_key_is_const, key_allocated_reg) = if is_short_str {
                    (k, true, false)
                } else {
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);
                    (kr, false, true)
                };
                let new_is_env_upvalue = first.is_env_upvalue && is_short_str;
                first = PrefixResult {
                    var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                    table_reg: Some(base_reg), table_key: Some(table_key), table_key_is_const: table_key_is_const, table_key_is_int: false,
                    key_allocated_reg: key_allocated_reg,
                    allocated_reg: if new_is_env_upvalue { false } else { first.allocated_reg || first.reg.is_none() || (first.is_env_upvalue && !is_short_str) },
                    is_env_upvalue: new_is_env_upvalue,
                    upval_idx: first.upval_idx,
                    env_gettabup_pc: if new_is_env_upvalue { -1 } else { if gettabup_pc >= 0 { gettabup_pc } else { first.env_gettabup_pc } },
                    has_call: false, call_pc: -1, is_vvargvar: false,
                };
            }
            Token::LBracket => {
                fs.ls_mut().next();

                // Handle VVARGVAR: generate GETVARG instead of GETTABLE
                if first.is_vvargvar {
                    let base_reg = first.reg.unwrap();
                    let ei = parse_expr(fs);
                    expect(fs, &Token::RBracket);
                    let key_reg = fs.expr_to_reg(&ei.exp);
                    fs.proto.flag |= PF_VATAB;
                    let r = fs.alloc_reg();
                    fs.code_abc(OpCode::GETVARG, r, base_reg, key_reg);
                    fs.free_reg(); // free key_reg
                    first = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(r),
                        table_reg: Some(r), table_key: Some(key_reg), table_key_is_const: false, table_key_is_int: false,
                        key_allocated_reg: false, allocated_reg: true,
                        is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1,
                        has_call: false, call_pc: -1, is_vvargvar: false,
                    };
                    continue;
                }

                // For _ENV upvalue: we need to decide whether to load _ENV into a register
                // before parsing the key expression. C compiler calls luaK_exp2anyregup
                // before yindex, then luaK_indexed may call luaK_exp2anyreg for _ENV
                // if the key is not a Kstr. To match C's instruction order (GETUPVAL
                // before LOADK), we emit GETUPVAL now and remove it later if not needed.
                let mut env_getupval_pc: i32 = -1;
                let mut env_getupval_reg: i32 = -1;
                let (base_reg, gettabup_pc) = if let Some(r) = first.reg {
                    (r, -1)
                } else if first.is_env_upvalue {
                    // Emit GETUPVAL now to match C's instruction order
                    let r = fs.alloc_reg();
                    env_getupval_pc = fs.code_abc(OpCode::GETUPVAL, r, 0, 0);
                    env_getupval_reg = r;
                    (r, -1)  // tentative; may be reverted
                } else {
                    let r = fs.alloc_reg();
                    let pc = if let Some(key) = first.key {
                        code_gettabup(fs, r, 0, key);
                        fs.pc - 1
                    } else {
                        -1
                    };
                    (r, pc)
                };
                let saved_freereg_before = fs.freereg;
                let ei = parse_expr(fs);
                expect(fs, &Token::RBracket);
                let (kr, key_is_const, key_is_int) = if ei.exp.kind == ExpKind::Str {
                    let k = fs.get_str_k(&ei.exp);
                    // C++ compiler: isKstr checks ttisshrstring AND k <= MAXINDEXRK
                    if let TValue::Str(crate::strings::LuaString::Short(_)) = fs.proto.constants[k as usize] {
                        if (k as u32) <= crate::opcodes::MAXINDEXRK {
                            (k, true, false)
                        } else {
                            let kr = fs.alloc_reg();
                            fs.code_abx(OpCode::LOADK, kr, k);
                            (kr, false, false)
                        }
                    } else {
                        let kr = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, kr, k);
                        (kr, false, false)
                    }
                } else if ei.exp.kind == ExpKind::Int
                    && ei.exp.info >= 0
                    && ei.exp.info <= ((1u32 << SIZE_C) - 1) as i64
                {
                    (ei.exp.info as i32, true, true)
                } else if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() && ei.exp.info2 < 0 {
                    (ei.exp.info as i32, false, false)
                } else {
                    (fs.expr_to_reg(&ei.exp), false, false)
                };
                let key_allocated = !key_is_const && fs.freereg > saved_freereg_before;
                // Now decide: if _ENV was loaded but SETTABUP can be used, revert the GETUPVAL
                let (base_reg, new_is_env_upvalue, allocated_reg) = if env_getupval_pc >= 0 {
                    let can_use_settabup = key_is_const && !key_is_int
                        && (kr as u32) <= crate::opcodes::MAXINDEXRK;
                    if can_use_settabup {
                        // Revert: remove GETUPVAL, free the register
                        fs.proto.code.remove(env_getupval_pc as usize);
                        fs.pc -= 1;
                        fs.free_reg();
                        (0, true, false)  // SETTABUP will be used, base_reg=0 is sentinel
                    } else {
                        // Keep GETUPVAL; _ENV is now in a register
                        (env_getupval_reg, false, true)
                    }
                } else {
                    (base_reg, first.is_env_upvalue, first.allocated_reg || first.reg.is_none())
                };
                first = PrefixResult {
                    var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                    table_reg: Some(base_reg), table_key: Some(kr), table_key_is_const: key_is_const, table_key_is_int: key_is_int,
                    key_allocated_reg: key_allocated,
                    allocated_reg: allocated_reg,
                    is_env_upvalue: new_is_env_upvalue,
                    upval_idx: first.upval_idx,
                    env_gettabup_pc: if new_is_env_upvalue { -1 } else { if gettabup_pc >= 0 { gettabup_pc } else { first.env_gettabup_pc } },
                    has_call: false, call_pc: -1, is_vvargvar: false,
                };
            }
            _ => break,
        }
    }
    
    if has_call && !check(fs, &Token::Eq) && !check(fs, &Token::Comma) {
        if call_pc >= 0 {
            fs.set_c(call_pc, 1);
        }
        fs.set_freereg(fs.nvarstack());
        return;
    }
    
    if check(fs, &Token::Eq) || check(fs, &Token::Comma) {
        let mut vars = vec![first];
        while check(fs, &Token::Comma) {
            fs.ls_mut().next();
            let new_var = parse_prefix_exp(fs);
            // check_conflict: if a non-indexed variable conflicts with previous
            // indexed variables (same table/key register), save the original
            // value to a temp register (matching C's check_conflict in lparser.cpp).
            if new_var.table_reg.is_none() {
                check_conflict_for_var(fs, &mut vars, &new_var);
            }
            vars.push(new_var);
        }
        expect(fs, &Token::Eq);
        
        // C compiler evaluates the left side before the right side.
        // For var_name with key > MAXINDEXRK, SETTABUP can't be used, so we must
        // emit GETUPVAL+LOADK now (before evaluating the right side), matching C's
        // luaK_indexed which calls luaK_exp2anyreg for non-Kstr keys on upvalues.
        // Skip _ENV itself (it uses SETUPVAL, not SETTABUP).
        for v in &mut vars {
            if let Some(ref name) = v.var_name {
                if v.key.is_some() && !v.is_env_upvalue {
                    let k_name = fs.string_k(name);
                    if (k_name as u32) > crate::opcodes::MAXINDEXRK {
                        let r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETUPVAL, r, 0, 0);
                        let kr = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, kr, k_name);
                        v.var_name = None;
                        v.key = None;
                        v.reg = Some(r);
                        v.table_reg = Some(r);
                        v.table_key = Some(kr);
                        v.table_key_is_const = false;
                        v.table_key_is_int = false;
                        v.key_allocated_reg = true;
                        v.allocated_reg = true;
                        v.is_env_upvalue = false;
                        v.upval_idx = None;
                        v.env_gettabup_pc = -1;
                    }
                }
            }
        }
        
        let mut exps: Vec<ExpDesc> = Vec::new();
        loop {
            let ei = parse_expr(fs);
            let has_comma = check(fs, &Token::Comma);
            if has_comma {
                let r = if ei.exp.kind == ExpKind::NonReloc && (ei.exp.info as i32) < fs.nvarstack() {
                    let new_r = fs.alloc_reg();
                    if ei.exp.info as i32 != new_r {
                        fs.code_abc(OpCode::MOVE, new_r, ei.exp.info as i32, 0);
                    }
                    new_r
                } else {
                    fs.expr_to_reg(&ei.exp)
                };
                exps.push(ExpDesc::new(ExpKind::NonReloc, r as i64));
                fs.ls_mut().next();
            } else {
                exps.push(ei.exp);
                break;
            }
        }
        
        let last_is_call = exps.last().map_or(false, |e| e.kind == ExpKind::Call && e.info2 >= 0);
        let extra_vars = if vars.len() > exps.len() {
            vars.len() - exps.len()
        } else {
            0
        };
        let (last_exp_reg, nil_reg_start) = if extra_vars > 0 && !last_is_call {
            let last_exp = exps.last().unwrap();
            let exp_reg = fs.expr_to_reg(last_exp);
            for _ in 0..extra_vars {
                fs.alloc_reg();
            }
            fs.code_nil(exp_reg + 1, extra_vars as i32);
            (Some(exp_reg), exp_reg + 1)
        } else {
            (None, -1)
        };
        if extra_vars > 0 && last_is_call {
            let nresults = (extra_vars + 1) as i32;
            fs.set_c(exps.last().unwrap().info2, nresults + 1);
        }
        if exps.len() > vars.len() {
            if last_is_call {
                fs.set_c(exps.last().unwrap().info2, 1);
            } else {
                // Match C original: adjust_assign calls luaK_exp2nextreg for last exp
                let last_exp = exps.last().unwrap();
                fs.exp_to_reg(last_exp);
            }
            // Match C original: freereg -= (nexps - nvars)
            let excess = (exps.len() - vars.len()) as i32;
            fs.set_freereg(fs.freereg - excess);
        }
        for i in (0..vars.len()).rev() {
            if i < exps.len() {
                let v = &vars[i];
                let val = &exps[i];
                if let (Some(table_reg), Some(table_key)) = (v.table_reg, v.table_key) {
                    let can_settabup = v.is_env_upvalue && v.table_key_is_const && !v.table_key_is_int;
                    if can_settabup {
                        let gettabup_pc = v.env_gettabup_pc;
                        let (env_k, adjusted_key) = if gettabup_pc >= 0 && (gettabup_pc as usize) < fs.proto.code.len() {
                            let gettabup_inst = fs.proto.code.remove(gettabup_pc as usize);
                            fs.pc -= 1;
                            let env_k = getarg_c(gettabup_inst);
                            fs.proto.constants.remove(env_k as usize);
                            let adjusted_key = if (env_k as i32) < table_key { table_key - 1 } else { table_key };
                            (env_k, adjusted_key)
                        } else {
                            (0i32, table_key)
                        };
                        let use_last_reg = i == exps.len() - 1 && last_exp_reg.is_some();
                        let k_opt = if use_last_reg { None } else { exp_to_k(fs, val) };
                        if let Some(k_val) = k_opt {
                            code_settabup_k(fs, 0, adjusted_key, k_val, true);
                        } else {
                            let val_reg = if use_last_reg {
                                last_exp_reg.unwrap()
                            } else {
                                fs.exp_to_reg(val)
                            };
                            code_settabup(fs, 0, adjusted_key, val_reg);
                            if val_reg >= fs.nvarstack() {
                                fs.free_reg();
                            }
                        }
                        if v.allocated_reg {
                            fs.free_reg();
                        }
                    } else {
                        let use_last_reg = i == exps.len() - 1 && last_exp_reg.is_some();
                        let k_opt = if use_last_reg { None } else { exp_to_k(fs, val) };
                        if v.table_key_is_int {
                            if let Some(k_val) = k_opt {
                                fs.code_abc_k(OpCode::SETI, table_reg, table_key, k_val, true);
                            } else {
                                let val_reg = if use_last_reg {
                                    last_exp_reg.unwrap()
                                } else {
                                    fs.exp_to_reg(val)
                                };
                                fs.code_abc_k(OpCode::SETI, table_reg, table_key, val_reg, false);
                                if val_reg >= fs.nvarstack() {
                                    fs.free_reg();
                                }
                            }
                        } else if v.table_key_is_const {
                            if let Some(k_val) = k_opt {
                                code_setfield_k(fs, table_reg, table_key, k_val, true);
                            } else {
                                let val_reg = if use_last_reg {
                                    last_exp_reg.unwrap()
                                } else {
                                    fs.exp_to_reg(val)
                                };
                                code_setfield(fs, table_reg, table_key, val_reg);
                                if val_reg >= fs.nvarstack() {
                                    fs.free_reg();
                                }
                            }
                        } else {
                            if let Some(k_val) = k_opt {
                                fs.code_abc_k(OpCode::SETTABLE, table_reg, table_key, k_val, true);
                            } else {
                                let val_reg = if use_last_reg {
                                    last_exp_reg.unwrap()
                                } else {
                                    fs.exp_to_reg(val)
                                };
                                fs.code_abc_k(OpCode::SETTABLE, table_reg, table_key, val_reg, false);
                                if val_reg >= fs.nvarstack() {
                                    fs.free_reg();
                                }
                            }
                        }
                        if v.key_allocated_reg && table_key == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        if v.allocated_reg && table_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                    }
                } else if let Some(upval_idx) = v.upval_idx {
                    let val_reg = if i == exps.len() - 1 && last_exp_reg.is_some() {
                        last_exp_reg.unwrap()
                    } else {
                        fs.exp_to_reg(val)
                    };
                    fs.code_abc(OpCode::SETUPVAL, val_reg, upval_idx, 0);
                    if val_reg >= fs.nvarstack() {
                        fs.free_reg();
                    }
                    if v.allocated_reg {
                        fs.free_reg();
                    }
                } else if let Some(ref name) = v.var_name {
                    let k_name = fs.string_k(name);
                    let use_last_reg = i == exps.len() - 1 && last_exp_reg.is_some();
                    let k_opt = if use_last_reg { None } else { exp_to_k(fs, val) };
                    if let Some(k_val) = k_opt {
                        code_settabup_k(fs, 0, k_name, k_val, true);
                    } else {
                        let val_reg = if use_last_reg {
                            last_exp_reg.unwrap()
                        } else {
                            fs.exp_to_reg(val)
                        };
                        code_settabup(fs, 0, k_name, val_reg);
                        if val_reg >= fs.nvarstack() {
                            fs.free_reg();
                        }
                    }
                } else if let Some(idx) = v.local_idx {
                    if i == exps.len() - 1 {
                        if let Some(val_reg) = last_exp_reg {
                            if idx != val_reg {
                                fs.code_abc(OpCode::MOVE, idx, val_reg, 0);
                            }
                        } else {
                            store_expr_to_local(fs, val, idx);
                        }
                    } else {
                        let val_reg = val.info as i32;
                        if idx != val_reg {
                            fs.code_abc(OpCode::MOVE, idx, val_reg, 0);
                        }
                        fs.free_reg();
                    }
                }
            } else if extra_vars > 0 {
                let v = &vars[i];
                let result_reg = if last_is_call {
                    let freg = exps.last().unwrap().info as i32;
                    freg + (i - exps.len() + 1) as i32
                } else {
                    nil_reg_start + (i - exps.len()) as i32
                };
                if let (Some(table_reg), Some(table_key)) = (v.table_reg, v.table_key) {
                    let can_settabup = v.is_env_upvalue && v.table_key_is_const && !v.table_key_is_int;
                    if can_settabup {
                        code_settabup(fs, 0, table_key, result_reg);
                    } else if v.table_key_is_const {
                        code_setfield(fs, table_reg, table_key, result_reg);
                    } else {
                        fs.code_abc_k(OpCode::SETTABLE, table_reg, table_key, result_reg, false);
                    }
                } else if let Some(upval_idx) = v.upval_idx {
                    fs.code_abc(OpCode::SETUPVAL, result_reg, upval_idx, 0);
                } else if let Some(ref name) = v.var_name {
                    let k_name = fs.string_k(name);
                    code_settabup(fs, 0, k_name, result_reg);
                } else if let Some(idx) = v.local_idx {
                    fs.code_abc(OpCode::MOVE, idx, result_reg, 0);
                }
            }
        }
        if extra_vars > 0 {
            fs.set_freereg(fs.nvarstack());
        }
        fs.set_freereg(fs.nvarstack());
        return;
    }
    
    if !has_call {
        let (_r, _, _, _) = load_func(fs, &first, false);
        fs.free_reg();
    }
}

/// 将常量表达式转换为常量表索引 (≤255 则返回)
fn exp_to_k(fs: &mut FuncState, e: &ExpDesc) -> Option<i32> {
    if e.t != NO_JUMP || e.f != NO_JUMP {
        return None;
    }
    let info = match e.kind {
        ExpKind::Int => fs.int_k(e.info),
        ExpKind::Float => {
            let f = f64::from_bits(e.info as u64);
            fs.float_k(f)
        }
        ExpKind::Str => fs.get_str_k(e),
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
            if e.t != NO_JUMP || e.f != NO_JUMP {
                fs.code_nil(dest, 1);
                fs.resolve_jumps(e, dest);
                return;
            }
            fs.code_nil(dest, 1);
        }
        ExpKind::Boolean => {
            if e.t != NO_JUMP || e.f != NO_JUMP {
                if e.info != 0 {
                    fs.code_abc(OpCode::LOADTRUE, dest, 0, 0);
                } else {
                    fs.code_abc(OpCode::LOADFALSE, dest, 0, 0);
                }
                fs.resolve_jumps(e, dest);
                return;
            }
            if e.info != 0 {
                fs.code_abc(OpCode::LOADTRUE, dest, 0, 0);
            } else {
                fs.code_abc(OpCode::LOADFALSE, dest, 0, 0);
            }
        }
        ExpKind::Int => {
            if e.t != NO_JUMP || e.f != NO_JUMP {
                let v = e.info;
                if fits_sbx(v) {
                    fs.code_asbx(OpCode::LOADI, dest, v as i32);
                } else {
                    let k = fs.int_k(v);
                    fs.code_abx(OpCode::LOADK, dest, k);
                }
                fs.resolve_jumps(e, dest);
                return;
            }
            let v = e.info;
            if fits_sbx(v) {
                fs.code_asbx(OpCode::LOADI, dest, v as i32);
            } else {
                let k = fs.int_k(v);
                fs.code_abx(OpCode::LOADK, dest, k);
            }
        }
        ExpKind::Float => {
            if e.t != NO_JUMP || e.f != NO_JUMP {
                let f = f64::from_bits(e.info as u64);
                let fi = f as i64;
                if (fi as f64) == f && fits_sbx(fi) {
                    fs.code_asbx(OpCode::LOADF, dest, fi as i32);
                } else {
                    let k = fs.float_k(f);
                    fs.code_abx(OpCode::LOADK, dest, k);
                }
                fs.resolve_jumps(e, dest);
                return;
            }
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
            let k = fs.get_str_k(e);
            if e.t != NO_JUMP || e.f != NO_JUMP {
                fs.code_abx(OpCode::LOADK, dest, k);
                fs.resolve_jumps(e, dest);
                return;
            }
            fs.code_abx(OpCode::LOADK, dest, k);
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
            if e.info2 > 0 {
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
            } else if e.kind == ExpKind::NonReloc && e.info2 == 0 {
                let val_reg = e.info as i32;
                if dest != val_reg {
                    fs.code_abc(OpCode::MOVE, dest, val_reg, 0);
                }
                if val_reg >= fs.nvarstack() && val_reg == fs.freereg - 1 {
                    fs.free_reg();
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
/// 返回 (函数寄存器, 是否需要额外释放基寄存器, 是否已分配寄存器, 方法调用原始源寄存器)
fn load_func(fs: &mut FuncState, p: &PrefixResult, is_method: bool) -> (i32, bool, bool, Option<i32>) {
    if let (Some(table_reg), Some(table_key)) = (p.table_reg, p.table_key) {
        if p.table_key_is_const {
            // Free table register if it was allocated as a temporary
            if p.allocated_reg {
                fs.free_reg();
            }
            // Allocate result register (reuses the just-freed register if applicable)
            let r = fs.alloc_reg();
            code_getfield(fs, r, table_reg, table_key);
            (r, true, true, None)
        } else {
            // Free key register if it was allocated as a temporary (it's at freereg-1)
            if p.key_allocated_reg {
                fs.free_reg();
            }
            // Free table register if it was allocated as a temporary (now at freereg-1 after freeing key)
            if p.allocated_reg {
                fs.free_reg();
            }
            // Allocate result register (reuses the just-freed register(s))
            let r = fs.alloc_reg();
            fs.code_abc(OpCode::GETTABLE, r, table_reg, table_key);
            (r, true, true, None)
        }
    } else if let Some(reg) = p.local_idx {
        let r = fs.alloc_reg();
        if is_method {
            (r, false, true, Some(reg))
        } else {
            fs.code_abc(OpCode::MOVE, r, reg, 0);
            (r, false, true, None)
        }
    } else if let Some(key) = p.key {
        let r = fs.alloc_reg();
        code_gettabup(fs, r, 0, key);
        (r, false, true, None)
    } else if let Some(reg) = p.reg {
        let r = fs.alloc_reg();
        if is_method {
            (r, false, true, Some(reg))
        } else {
            fs.code_abc(OpCode::MOVE, r, reg, 0);
            (r, false, true, None)
        }
    } else {
        (fs.alloc_reg(), false, true, None)
    }
}

/// ANTLR4: `args: '(' explist? ')' | tableconstructor | STRING ;` 及 `':' NAME args ;` — 解析函数参数并生成 CALL 指令
fn parse_func_args(fs: &mut FuncState, freg: i32, src_reg: Option<i32>) -> i32 {
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
        // C++ funcargs: call removes function and arguments and leaves one result
        fs.set_freereg(freg + 1);
        return pc;
    }

    if check(fs, &Token::LBrace) {
        let (tr, _n) = parse_constructor(fs);
        if freg + 1 != tr {
            fs.code_abc(OpCode::MOVE, freg + 1, tr, 0);
            fs.free_reg();
        }
        let pc = fs.code_abc(OpCode::CALL, freg, 2, 2);
        // C++ funcargs: call removes function and arguments and leaves one result
        fs.set_freereg(freg + 1);
        return pc;
    }
    
    if check(fs, &Token::Colon) {
        fs.ls_mut().next();
        let method = get_name(fs);
        let k = fs.string_k(&method);
        let src = src_reg.unwrap_or(freg);
        // C++ compiler: luaK_self checks strisshr — long method names can't use SELF opcode
        if method.len() <= crate::strings::LUAI_MAXSHORTLEN {
            fs.code_abc(OpCode::SELF, freg, src, k);
        } else {
            // Long method name: use MOVE + GETTABLE instead of SELF
            let kr = fs.alloc_reg();
            fs.code_abx(OpCode::LOADK, kr, k);
            fs.code_abc(OpCode::MOVE, freg + 1, src, 0);
            fs.code_abc(OpCode::GETTABLE, freg, src, kr);
            fs.free_reg();  // free key register
        }
        if check(fs, &Token::LParen) {
            fs.ls_mut().next();
            if fs.freereg <= freg + 1 {
                fs.alloc_reg();
            }
            let (na, na_multret) = parse_args(fs);
            expect(fs, &Token::RParen);
            let na_adj = if na_multret { 0 } else { na + 2 };
            let pc = fs.code_abc(OpCode::CALL, freg, na_adj, 2);
            // C++ funcargs: call removes function and arguments and leaves one result
            fs.set_freereg(freg + 1);
            return pc;
        }
        return -1;
    }
    
    if check(fs, &Token::LParen) {
        fs.ls_mut().next();
        if fs.freereg <= freg {
            fs.set_freereg(freg + 1);
        }
        let (nparams, nparams_multret) = parse_args(fs);
        expect(fs, &Token::RParen);
        let nparams_adj = if nparams_multret { 0 } else { nparams + 1 };
        let pc = fs.code_abc(OpCode::CALL, freg, nparams_adj, 2);
        // C++ funcargs: call removes function and arguments and leaves one result
        fs.set_freereg(freg + 1);
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
    let _r = if matches!(ei.exp.kind, ExpKind::Relocable) && ei.exp.info2 >= 0 && !ei.exp.has_jumps() {
        // For Relocable expressions with a pending instruction (info2 >= 0),
        // allocate a new register and patch the instruction's A field.
        // This matches C's luaK_exp2nextreg behavior for VRELOC expressions.
        fs.exp_to_reg(&ei.exp)
    } else if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() {
        if ei.exp.info2 >= 0 {
            fs.set_a(ei.exp.info2, ei.exp.info as i32);
        }
        ei.exp.info as i32
    } else {
        fs.exp_to_reg(&ei.exp)
    };
    if matches!(ei.exp.kind, ExpKind::NonReloc) && (ei.exp.info as i32) < fs.nvarstack() && !ei.exp.has_jumps() {
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
        let _r2 = if matches!(ei2.exp.kind, ExpKind::Relocable) && ei2.exp.info2 >= 0 && !ei2.exp.has_jumps() {
            fs.exp_to_reg(&ei2.exp)
        } else if matches!(ei2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei2.exp.has_jumps() {
            if ei2.exp.info2 >= 0 {
                fs.set_a(ei2.exp.info2, ei2.exp.info as i32);
            }
            ei2.exp.info as i32
        } else {
            fs.exp_to_reg(&ei2.exp)
        };
        if matches!(ei2.exp.kind, ExpKind::NonReloc) && (ei2.exp.info as i32) < fs.nvarstack() && !ei2.exp.has_jumps() {
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
    table_key_is_int: bool,
    key_allocated_reg: bool,
    allocated_reg: bool,
    is_env_upvalue: bool,
    upval_idx: Option<i32>,
    env_gettabup_pc: i32,
    has_call: bool,
    call_pc: i32,
    is_vvargvar: bool,
}

/// ANTLR4: `prefixexp: varOrExp | functioncall | '(' expr ')' ;` 以及 `var: NAME | prefixexp '[' expr ']' | prefixexp '.' NAME ;`
fn parse_prefix_exp(fs: &mut FuncState) -> PrefixResult {
    match &fs.ls().token {
        Token::Name(name) => {
            let name = name.clone();
            fs.ls_mut().next();
            let mut result = if let Some(mut ctc) = fs.find_local_ctc(&name) {
                let r = fs.alloc_reg();
                match ctc.kind {
                    ExpKind::Int => {
                        let val = ctc.info;
                        if fits_sbx(val) {
                            fs.code_asbx(OpCode::LOADI, r, val as i32);
                        } else {
                            let k = fs.int_k(val);
                            fs.code_abx(OpCode::LOADK, r, k);
                        }
                    }
                    ExpKind::Float => {
                        let f = f64::from_bits(ctc.info as u64);
                        let k = fs.float_k(f);
                        fs.code_abx(OpCode::LOADK, r, k);
                    }
                    ExpKind::Str => {
                        let k = fs.discharge_str(&mut ctc);
                        fs.code_abx(OpCode::LOADK, r, k);
                    }
                    ExpKind::Boolean => {
                        if ctc.info != 0 {
                            fs.code_abc(OpCode::LOADTRUE, r, 0, 0);
                        } else {
                            fs.code_abc(OpCode::LOADFALSE, r, 0, 0);
                        }
                    }
                    ExpKind::Nil => {
                        fs.code_nil(r, 1);
                    }
                    _ => {
                        let k = fs.discharge_str(&mut ctc);
                        fs.code_abx(OpCode::LOADK, r, k);
                    }
                };
                PrefixResult { var_name: None, local_idx: Some(r), key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: true, is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false }
            } else if let Some((reg, kind)) = fs.find_local_ex(&name) {
                let is_vvargvar = kind == RDKVAVAR;
                PrefixResult { var_name: None, local_idx: Some(reg), key: None, reg: Some(reg), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: false, is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar }
            } else if let Some(upval_idx) = fs.find_upvalue(&name) {
                let r = fs.alloc_reg();
                fs.code_abc(OpCode::GETUPVAL, r, upval_idx, 0);
                PrefixResult { var_name: Some(name), local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: false, is_env_upvalue: false, upval_idx: Some(upval_idx), env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false }
            } else {
                let is_env = name == "_ENV";
                let k = if is_env { 0 } else { fs.string_k(&name) };
                // Like C buildglobal: check if _ENV is a local variable
                // If so, use GETFIELD (table_reg + table_key); otherwise use GETTABUP (key + is_env_upvalue)
                if let Some(env_reg) = fs.find_local("_ENV") {
                    let is_short_str = name.len() <= crate::strings::LUAI_MAXSHORTLEN
                        && (k as u32) <= crate::opcodes::MAXINDEXRK;
                    if is_short_str {
                        PrefixResult { var_name: Some(name), local_idx: None, key: None, reg: None, table_reg: Some(env_reg), table_key: Some(k), table_key_is_const: true, table_key_is_int: false, key_allocated_reg: false, allocated_reg: false, is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false }
                    } else {
                        // _ENV is local but key is not short string: load _ENV into temp register
                        let env_r = fs.alloc_reg();
                        fs.code_abc(OpCode::MOVE, env_r, env_reg, 0);
                        let kr = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, kr, k);
                        PrefixResult { var_name: Some(name), local_idx: None, key: None, reg: None, table_reg: Some(env_r), table_key: Some(kr), table_key_is_const: false, table_key_is_int: false, key_allocated_reg: true, allocated_reg: true, is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false }
                    }
                } else {
                    PrefixResult { var_name: Some(name), local_idx: None, key: Some(k), reg: None, table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: false, is_env_upvalue: is_env, upval_idx: if is_env { Some(0) } else { None }, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false }
                }
            };

            loop {
                match &fs.ls().token {
                    Token::Dot => {
                        fs.ls_mut().next();
                        let field = get_name(fs);
                        let k = fs.string_k(&field);

                        // Handle VVARGVAR: generate GETVARG instead of GETFIELD
                        if result.is_vvargvar {
                            let base_reg = result.reg.unwrap();
                            fs.proto.flag |= PF_VATAB;
                            let r = fs.alloc_reg();
                            fs.code_abc(OpCode::GETVARG, r, base_reg, k);
                            result = PrefixResult {
                                var_name: None, local_idx: None, key: None, reg: Some(r),
                                table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false,
                                key_allocated_reg: false, allocated_reg: true,
                                is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1,
                                has_call: false, call_pc: -1, is_vvargvar: false,
                            };
                            continue;
                        }

                        if result.table_reg.is_some() {
                            let prev_table = result.table_reg.unwrap();
                            let prev_key = result.table_key.unwrap();
                            if result.table_key_is_int {
                                if result.allocated_reg {
                                    fs.free_reg();
                                }
                                let r = fs.alloc_reg();
                                fs.code_abc(OpCode::GETI, r, prev_table, prev_key);
                                result.reg = Some(r);
                                result.allocated_reg = true;
                            } else if result.table_key_is_const {
                                if result.allocated_reg {
                                    fs.free_reg();
                                }
                                let r = fs.alloc_reg();
                                code_getfield(fs, r, prev_table, prev_key);
                                result.reg = Some(r);
                                result.allocated_reg = true;
                            } else {
                                if result.key_allocated_reg {
                                    fs.free_reg();
                                }
                                if result.allocated_reg {
                                    fs.free_reg();
                                }
                                let r = fs.alloc_reg();
                                fs.code_abc(OpCode::GETTABLE, r, prev_table, prev_key);
                                result.reg = Some(r);
                                result.allocated_reg = true;
                            }
                            result.table_reg = None;
                            result.table_key = None;
                            result.table_key_is_const = false;
                            result.table_key_is_int = false;
                            result.key_allocated_reg = false;
                        }
                        let is_short_str = field.len() <= crate::strings::LUAI_MAXSHORTLEN && (k as u32) <= crate::opcodes::MAXINDEXRK;
                        let (base_reg, gettabup_pc) = if let Some(r) = result.reg {
                            (r, -1)
                        } else if result.is_env_upvalue {
                            if !is_short_str {
                                // Key exceeds MAXINDEXRK: must load _ENV into a register
                                // before the value expression is evaluated (matching C compiler order)
                                let r = fs.alloc_reg();
                                fs.code_abc(OpCode::GETUPVAL, r, 0, 0);
                                (r, -1)
                            } else {
                                (0, -1)
                            }
                        } else {
                            let r = fs.alloc_reg();
                            let gk = result.key.unwrap_or(0);
                            code_gettabup(fs, r, 0, gk);
                            (r, fs.pc - 1)
                        };
                        let (table_key, table_key_is_const, key_allocated_reg) = if is_short_str {
                            (k, true, false)
                        } else {
                            let kr = fs.alloc_reg();
                            fs.code_abx(OpCode::LOADK, kr, k);
                            (kr, false, true)
                        };
                        let new_is_env_upvalue = result.is_env_upvalue && is_short_str;
                        result = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                        table_reg: Some(base_reg), table_key: Some(table_key), table_key_is_const: table_key_is_const, table_key_is_int: false,
                        key_allocated_reg: key_allocated_reg,
                        allocated_reg: if new_is_env_upvalue { false } else { result.allocated_reg || result.reg.is_none() || (result.is_env_upvalue && !is_short_str) },
                        is_env_upvalue: new_is_env_upvalue,
                        upval_idx: result.upval_idx,
                        env_gettabup_pc: if new_is_env_upvalue { -1 } else { if gettabup_pc >= 0 { gettabup_pc } else { result.env_gettabup_pc } },
                        has_call: false, call_pc: -1, is_vvargvar: false,
                    };
                }
                Token::LBracket => {
                    fs.ls_mut().next();

                    // Handle VVARGVAR: generate GETVARG instead of GETTABLE
                    if result.is_vvargvar {
                        let base_reg = result.reg.unwrap();
                        let ei = parse_expr(fs);
                        expect(fs, &Token::RBracket);
                        let key_reg = fs.expr_to_reg(&ei.exp);
                        fs.proto.flag |= PF_VATAB;
                        let r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETVARG, r, base_reg, key_reg);
                        fs.free_reg(); // free key_reg
                        result = PrefixResult {
                            var_name: None, local_idx: None, key: None, reg: Some(r),
                            table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false,
                            key_allocated_reg: false, allocated_reg: true,
                            is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1,
                            has_call: false, call_pc: -1, is_vvargvar: false,
                        };
                        continue;
                    }

                    if result.table_reg.is_some() {
                        let prev_table = result.table_reg.unwrap();
                        let prev_key = result.table_key.unwrap();
                        if result.table_key_is_int {
                            if result.allocated_reg {
                                fs.free_reg();
                            }
                            let r = fs.alloc_reg();
                            fs.code_abc(OpCode::GETI, r, prev_table, prev_key);
                            result.reg = Some(r);
                            result.allocated_reg = true;
                        } else if result.table_key_is_const {
                            if result.allocated_reg {
                                fs.free_reg();
                            }
                            let r = fs.alloc_reg();
                            code_getfield(fs, r, prev_table, prev_key);
                            result.reg = Some(r);
                            result.allocated_reg = true;
                        } else {
                            if result.key_allocated_reg {
                                fs.free_reg();
                            }
                            if result.allocated_reg {
                                fs.free_reg();
                            }
                            let r = fs.alloc_reg();
                            fs.code_abc(OpCode::GETTABLE, r, prev_table, prev_key);
                            result.reg = Some(r);
                            result.allocated_reg = true;
                        }
                        result.table_reg = None;
                        result.table_key = None;
                        result.table_key_is_const = false;
                        result.table_key_is_int = false;
                        result.key_allocated_reg = false;
                    }
                    // For _ENV upvalue: we need to decide whether to load _ENV into a register
                    // before parsing the key expression. C compiler calls luaK_exp2anyregup
                    // before yindex, then luaK_indexed may call luaK_exp2anyreg for _ENV
                    // if the key is not a Kstr. To match C's instruction order (GETUPVAL
                    // before LOADK), we emit GETUPVAL now and remove it later if not needed.
                    let mut env_getupval_pc: i32 = -1;
                    let mut env_getupval_reg: i32 = -1;
                    let (base_reg, gettabup_pc) = if let Some(r) = result.reg {
                        (r, -1)
                    } else if result.is_env_upvalue {
                        // Emit GETUPVAL now to match C's instruction order
                        let r = fs.alloc_reg();
                        env_getupval_pc = fs.code_abc(OpCode::GETUPVAL, r, 0, 0);
                        env_getupval_reg = r;
                        (r, -1)  // tentative; may be reverted
                    } else {
                        let r = fs.alloc_reg();
                        let gk = result.key.unwrap_or(0);
                        code_gettabup(fs, r, 0, gk);
                        (r, fs.pc - 1)
                    };
                    let saved_freereg_before = fs.freereg;
                    let ei = parse_expr(fs);
                    expect(fs, &Token::RBracket);
                    let (kr, key_is_const, key_is_int) = if ei.exp.kind == ExpKind::Str {
                        let k = fs.get_str_k(&ei.exp);
                        // C++ compiler: isKstr checks ttisshrstring AND k <= MAXINDEXRK
                        if let TValue::Str(crate::strings::LuaString::Short(_)) = fs.proto.constants[k as usize] {
                            if (k as u32) <= crate::opcodes::MAXINDEXRK {
                                (k, true, false)
                            } else {
                                let kr = fs.alloc_reg();
                                fs.code_abx(OpCode::LOADK, kr, k);
                                (kr, false, false)
                            }
                        } else {
                            let kr = fs.alloc_reg();
                            fs.code_abx(OpCode::LOADK, kr, k);
                            (kr, false, false)
                        }
                    } else if ei.exp.kind == ExpKind::Int
                        && ei.exp.info >= 0
                        && ei.exp.info <= ((1u32 << SIZE_C) - 1) as i64
                    {
                        (ei.exp.info as i32, true, true)
                    } else if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() && ei.exp.info2 < 0 {
                        (ei.exp.info as i32, false, false)
                    } else {
                        (fs.expr_to_reg(&ei.exp), false, false)
                    };
                    let key_allocated = !key_is_const && fs.freereg > saved_freereg_before;
                    // Now decide: if _ENV was loaded but SETTABUP can be used, revert the GETUPVAL
                    let (base_reg, new_is_env_upvalue, allocated_reg) = if env_getupval_pc >= 0 {
                        let can_use_settabup = key_is_const && !key_is_int
                            && (kr as u32) <= crate::opcodes::MAXINDEXRK;
                        if can_use_settabup {
                            // Revert: remove GETUPVAL, free the register
                            fs.proto.code.remove(env_getupval_pc as usize);
                            fs.pc -= 1;
                            fs.free_reg();
                            (0, true, false)  // SETTABUP will be used, base_reg=0 is sentinel
                        } else {
                            // Keep GETUPVAL; _ENV is now in a register
                            (env_getupval_reg, false, true)
                        }
                    } else {
                        (base_reg, result.is_env_upvalue, result.allocated_reg || result.reg.is_none())
                    };
                    result = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                        table_reg: Some(base_reg), table_key: Some(kr), table_key_is_const: key_is_const, table_key_is_int: key_is_int,
                        key_allocated_reg: key_allocated,
                        allocated_reg: allocated_reg,
                        is_env_upvalue: new_is_env_upvalue,
                        upval_idx: result.upval_idx,
                        env_gettabup_pc: if new_is_env_upvalue { -1 } else { if gettabup_pc >= 0 { gettabup_pc } else { result.env_gettabup_pc } },
                        has_call: false, call_pc: -1, is_vvargvar: false,
                    };
                    }
                    Token::LParen | Token::LBrace | Token::String(..) | Token::Colon => {
                        let is_method = matches!(&fs.ls().token, Token::Colon);
                        // If result is already a call result in the right position, reuse freg
                        let (freg, _ef, func_allocated, src_reg) = if is_method {
                            load_func(fs, &result, true)
                        } else if result.has_call {
                            if let Some(reg) = result.reg {
                                if reg == fs.freereg - 1 {
                                    // Already in the right position, no need to load
                                    (reg, false, result.allocated_reg, None)
                                } else {
                                    load_func(fs, &result, false)
                                }
                            } else {
                                load_func(fs, &result, false)
                            }
                        } else {
                            load_func(fs, &result, false)
                        };
                        let mut last_call_pc = parse_func_args(fs, freg, src_reg);
                        // Match C original: funcargs sets fs->freereg = base + 1
                        fs.set_freereg(freg + 1);
                        loop {
                            match &fs.ls().token {
                                Token::LParen | Token::LBrace | Token::String(_) | Token::Colon => {
                                    last_call_pc = parse_func_args(fs, freg, None);
                                    fs.set_freereg(freg + 1);
                                }
                                _ => break,
                            }
                        }
                        result = PrefixResult {
                            var_name: None, local_idx: None, key: None, reg: Some(freg),
                            table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false,
                            key_allocated_reg: false,
                            allocated_reg: func_allocated,
                            is_env_upvalue: false,
                            upval_idx: None,
                            env_gettabup_pc: -1,
                            has_call: true, call_pc: last_call_pc, is_vvargvar: false,
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
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: true, is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false }
        }
        _ => {
            let e = parse_simple_exp(fs);
            let r = fs.expr_to_reg(&e.exp);
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: true, is_env_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false }
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
                    let k = e_left.info2 == -2
                        || (e_left.info2 >= 0
                            && (e_left.info2 as usize) < fs.proto.code.len()
                            && get_opcode(fs.proto.code[e_left.info2 as usize]) == OpCode::NOT);
                    if k && e_left.info2 >= 0 {
                        let idx = e_left.info2 as usize;
                        fs.proto.code.remove(idx);
                        fs.pc -= 1;
                    }
                    if k {
                        fs.code_abc_k(OpCode::TEST, reg, 0, 0, k);
                    } else {
                        if reg_alloc {
                            fs.free_reg();
                        } else if matches!(e_left.kind, ExpKind::NonReloc | ExpKind::Relocable) && reg >= fs.nvarstack() {
                            fs.free_reg();
                        }
                        let _test_pc = fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, reg, 0, false);
                    }
                    let jmp_pc = fs.jump();
                    fs.concat_jump(&mut e_left.f, jmp_pc);
                    let here = fs.pc;
                    fs.patch_true_jumps(e_left.t, here);
                    e_left.t = NO_JUMP;
                    if k {
                        if reg_alloc || (matches!(e_left.kind, ExpKind::NonReloc | ExpKind::Relocable) && reg >= fs.nvarstack()) { fs.free_reg(); }
                    }
                }
            }
            
            let e2 = parse_subexpr(fs, PREC_AND + 1);
            let mut e2_exp = e2.exp.clone();
            if e2_exp.kind == ExpKind::Call {
                e2_exp.kind = ExpKind::NonReloc;
            }
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
                    let k = e_left.info2 == -2
                        || (e_left.info2 >= 0
                            && (e_left.info2 as usize) < fs.proto.code.len()
                            && get_opcode(fs.proto.code[e_left.info2 as usize]) == OpCode::NOT);
                    if k && e_left.info2 >= 0 {
                        fs.pc -= 1;
                        fs.proto.code.pop();
                    }
                    if k {
                        fs.code_abc_k(OpCode::TEST, reg, 0, 0, !k);
                    } else {
                        if reg_alloc {
                            fs.free_reg();
                        } else if matches!(e_left.kind, ExpKind::NonReloc | ExpKind::Relocable) && reg >= fs.nvarstack() {
                            fs.free_reg();
                        }
                        fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, reg, 0, true);
                    }
                    let jmp_pc = fs.jump();
                    fs.concat_jump(&mut e_left.t, jmp_pc);
                    let here = fs.pc;
                    fs.patch_false_jumps(e_left.f, here);
                    e_left.f = NO_JUMP;
                    if k {
                        if reg_alloc || (matches!(e_left.kind, ExpKind::NonReloc | ExpKind::Relocable) && reg >= fs.nvarstack()) { fs.free_reg(); }
                    }
                }
            }
            
            let e2 = parse_subexpr(fs, PREC_AND);
            let mut e2_exp = e2.exp.clone();
            if e2_exp.kind == ExpKind::Call {
                e2_exp.kind = ExpKind::NonReloc;
            }
            fs.concat_jump(&mut e2_exp.t, e_left.t);
            
            e = ExprItem { exp: e2_exp };
            matched = true;
        }
        
        if limit <= PREC_COMP && check_compare(fs) {
            let mut ec = e.exp.clone();
            let op_tok = fs.ls().token.clone();
            let is_gt = matches!(op_tok, Token::Gt | Token::GtEq);
            let is_eq = matches!(op_tok, Token::EqEq | Token::TildeEq);

            // C++ luaK_infix: 在解析右操作数前，先将左操作数放入寄存器
            // 对于 EQ/NE: exp2RK (尝试转为常量，否则放入寄存器)
            //   C++ 对 VCALL 调用 dischargevars → setoneret，直接转为 VNONRELOC，不生成 MOVE
            // 对于 LT/LE/GT/GE: 如果不是 SC 数值，luaK_exp2anyreg
            //   C++ 同样通过 dischargevars 将 VCALL 转为 VNONRELOC
            if is_eq {
                if !matches!(ec.kind, ExpKind::Int | ExpKind::Float | ExpKind::Str | ExpKind::Boolean | ExpKind::Nil) {
                    if matches!(ec.kind, ExpKind::Call) && !ec.has_jumps() {
                        // 类似 C++ dischargevars + setoneret 对 VCALL 的处理:
                        // 直接转为 NonReloc，不生成 MOVE
                        ec.kind = ExpKind::NonReloc;
                        ec.info2 = -1;
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                    } else {
                        let r = fs.exp_to_reg(&ec);
                        ec.kind = ExpKind::NonReloc;
                        ec.info = r as i64;
                        ec.info2 = -1;
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                    }
                }
            } else {
                if is_sc_number(&ec).is_none() && !matches!(ec.kind, ExpKind::NonReloc) {
                    if matches!(ec.kind, ExpKind::Call) && !ec.has_jumps() {
                        // 类似 C++ dischargevars + setoneret 对 VCALL 的处理:
                        // 直接转为 NonReloc，不生成 MOVE
                        ec.kind = ExpKind::NonReloc;
                        ec.info2 = -1;
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                    } else {
                        let r = fs.exp_to_reg(&ec);
                        ec.kind = ExpKind::NonReloc;
                        ec.info = r as i64;
                        ec.info2 = -1;
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                    }
                }
            }

            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_COMP + 1);
            let k = if matches!(op_tok, Token::TildeEq) { 0 } else { 1 };

            let sc_imm = if is_eq || is_gt {
                is_sc_number(&ec)
            } else {
                is_sc_number(&e2.exp)
            };

            if let Some(sc_val) = sc_imm {
                let (reg, imm, reg_alloc) = if is_eq {
                    let alloc = !matches!(e2.exp.kind, ExpKind::NonReloc);
                    let reg = if alloc {
                        fs.exp_to_reg(&e2.exp)
                    } else if e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        fs.resolve_jumps(&e2.exp, reg);
                        reg
                    } else {
                        let reg = e2.exp.info as i32;
                        if e2.exp.info2 >= 0 {
                            fs.set_a(e2.exp.info2, reg);
                        }
                        reg
                    };
                    (reg, int_to_sc(sc_val), alloc)
                } else if is_gt {
                    let alloc = !matches!(e2.exp.kind, ExpKind::NonReloc);
                    let reg = if alloc {
                        fs.exp_to_reg(&e2.exp)
                    } else if e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        fs.resolve_jumps(&e2.exp, reg);
                        reg
                    } else {
                        let reg = e2.exp.info as i32;
                        if e2.exp.info2 >= 0 {
                            fs.set_a(e2.exp.info2, reg);
                        }
                        reg
                    };
                    (reg, int_to_sc(sc_val), alloc)
                } else {
                    let alloc = !matches!(ec.kind, ExpKind::NonReloc);
                    let reg = if alloc {
                        fs.exp_to_reg(&ec)
                    } else if ec.has_jumps() {
                        let reg = ec.info as i32;
                        fs.resolve_jumps(&ec, reg);
                        reg
                    } else {
                        let reg = ec.info as i32;
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, reg);
                        }
                        reg
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
                if reg_alloc
                    || (matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && reg >= fs.nvarstack())
                    || (matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && reg >= fs.nvarstack())
                {
                    fs.free_reg();
                }
                e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
            } else if is_gt {
                if let Some(sc_val) = is_sc_number(&e2.exp) {
                    let r_alloc = !matches!(ec.kind, ExpKind::NonReloc);
                    let reg = if r_alloc {
                        fs.exp_to_reg(&ec)
                    } else if ec.has_jumps() {
                        let reg = ec.info as i32;
                        fs.resolve_jumps(&ec, reg);
                        reg
                    } else {
                        let reg = ec.info as i32;
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, reg);
                        }
                        reg
                    };
                    let imm = int_to_sc(sc_val);
                    let imm_op = match op_tok {
                        Token::Gt => OpCode::GTI,
                        Token::GtEq => OpCode::GEI,
                        _ => OpCode::GTI,
                    };
                    fs.code_abc_k(imm_op, reg, imm, 0, k != 0);
                    let jmp_pc = fs.jump();
                    if r_alloc
                        || (matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && reg >= fs.nvarstack())
                    {
                        fs.free_reg();
                    }
                    e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                } else {
                    let r_alloc = !matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg);
                    let r = if r_alloc {
                        fs.exp_to_reg(&ec)
                    } else if ec.has_jumps() {
                        let reg = ec.info as i32;
                        fs.resolve_jumps(&ec, reg);
                        reg
                    } else {
                        let reg = ec.info as i32;
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, reg);
                        }
                        if ec.info2 == -2 {
                            fs.code_abc(OpCode::NOT, reg, reg, 0);
                        }
                        reg
                    };
                    let r2_alloc = !matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg);
                    let r2 = if r2_alloc {
                        fs.exp_to_reg(&e2.exp)
                    } else if e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        fs.resolve_jumps(&e2.exp, reg);
                        reg
                    } else {
                        let reg = e2.exp.info as i32;
                        if e2.exp.info2 >= 0 {
                            fs.set_a(e2.exp.info2, reg);
                        }
                        if e2.exp.info2 == -2 {
                            fs.code_abc(OpCode::NOT, reg, reg, 0);
                        }
                        reg
                    };
                    let (op, k) = match op_tok {
                        Token::Gt => (OpCode::LT, 1),
                        Token::GtEq => (OpCode::LE, 1),
                        _ => (OpCode::LT, 1),
                    };
                    fs.code_abc_k(op, r2, r, 0, k != 0);
                    let jmp_pc = fs.jump();
                    if r2_alloc || (matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r2 >= fs.nvarstack()) { fs.free_reg(); }
                    if r_alloc || (matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r >= fs.nvarstack()) { fs.free_reg(); }
                    e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                }
            } else {
                let is_eq_op = matches!(op_tok, Token::EqEq);
                if is_eq {
                    let ec_const_k = if !ec.has_jumps() {
                        exp_to_const_k(fs, &ec)
                    } else {
                        None
                    };
                    if let Some(k_idx) = ec_const_k {
                        let r2_alloc = !matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg);
                        let r2 = if r2_alloc {
                            fs.exp_to_reg(&e2.exp)
                        } else if e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            fs.resolve_jumps(&e2.exp, reg);
                            reg
                        } else {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            if e2.exp.info2 == -2 {
                                fs.code_abc(OpCode::NOT, reg, reg, 0);
                            }
                            reg
                        };
                        fs.code_abc_k(OpCode::EQK, r2, k_idx, 0, is_eq_op);
                        let jmp_pc = fs.jump();
                        if r2_alloc || (matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r2 >= fs.nvarstack()) { fs.free_reg(); }
                        e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                    } else {
                        let r_alloc = !matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg);
                        let r = if r_alloc {
                            fs.exp_to_reg(&ec)
                        } else if ec.has_jumps() {
                            let reg = ec.info as i32;
                            fs.resolve_jumps(&ec, reg);
                            reg
                        } else {
                        let reg = ec.info as i32;
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, reg);
                        }
                        if ec.info2 == -2 {
                            fs.code_abc(OpCode::NOT, reg, reg, 0);
                        }
                        reg
                    };
                    if let Some(sc_val) = is_sc_number(&e2.exp) {
                            let sc = int_to_sc(sc_val);
                            fs.code_abc_k(OpCode::EQI, r, sc, 0, is_eq_op);
                            let jmp_pc = fs.jump();
                            if r_alloc || (matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r >= fs.nvarstack()) { fs.free_reg(); }
                            e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                        } else {
                            let const_k = exp_to_const_k(fs, &e2.exp);
                            if let Some(k_idx) = const_k {
                                fs.code_abc_k(OpCode::EQK, r, k_idx, 0, is_eq_op);
                                let jmp_pc = fs.jump();
                                if r_alloc || (matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r >= fs.nvarstack()) { fs.free_reg(); }
                                e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                            } else {
                                let r2_alloc = !matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg);
                                let r2 = if r2_alloc {
                                    fs.exp_to_reg(&e2.exp)
                                } else if e2.exp.has_jumps() {
                                    let reg = e2.exp.info as i32;
                                    fs.resolve_jumps(&e2.exp, reg);
                                    reg
                                } else {
                                let reg = e2.exp.info as i32;
                                if e2.exp.info2 >= 0 {
                                    fs.set_a(e2.exp.info2, reg);
                                }
                                if e2.exp.info2 == -2 {
                                    fs.code_abc(OpCode::NOT, reg, reg, 0);
                                }
                                reg
                            };
                            fs.code_abc_k(OpCode::EQ, r, r2, 0, is_eq_op);
                                let jmp_pc = fs.jump();
                                if r2_alloc || (matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r2 >= fs.nvarstack()) { fs.free_reg(); }
                                if r_alloc || (matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r >= fs.nvarstack()) { fs.free_reg(); }
                                e = ExprItem { exp: ExpDesc::new(ExpKind::VJMP, jmp_pc as i64) };
                            }
                        }
                    }
                } else {
                    let r_alloc = !matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg);
                    let r = if r_alloc {
                        fs.exp_to_reg(&ec)
                    } else if ec.has_jumps() {
                        let reg = ec.info as i32;
                        fs.resolve_jumps(&ec, reg);
                        reg
                    } else {
                        let reg = ec.info as i32;
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, reg);
                        }
                        if ec.info2 == -2 {
                            fs.code_abc(OpCode::NOT, reg, reg, 0);
                        }
                        reg
                    };
                    let r2_alloc = !matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg);
                    let r2 = if r2_alloc {
                        fs.exp_to_reg(&e2.exp)
                    } else if e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        fs.resolve_jumps(&e2.exp, reg);
                        reg
                    } else {
                        let reg = e2.exp.info as i32;
                        if e2.exp.info2 >= 0 {
                            fs.set_a(e2.exp.info2, reg);
                        }
                        if e2.exp.info2 == -2 {
                            fs.code_abc(OpCode::NOT, reg, reg, 0);
                        }
                        reg
                    };
                    let (op, k) = match op_tok {
                        Token::Lt => (OpCode::LT, 1),
                        Token::LtEq => (OpCode::LE, 1),
                        _ => (OpCode::LT, 1),
                    };
                    fs.code_abc_k(op, r, r2, 0, k != 0);
                    let jmp_pc = fs.jump();
                    if r2_alloc || (matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r2 >= fs.nvarstack()) { fs.free_reg(); }
                    if r_alloc || (matches!(ec.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r >= fs.nvarstack()) { fs.free_reg(); }
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
                        let sc = int_to_sc(ec.info);
                        let pc = fs.code_abc(OpCode::SHLI, r2, r2, sc);
                        fs.code_abc_k(OpCode::MMBINI, r2, sc, 16, true);
                        fs.free_exp_reg(&e2.exp);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r2 as i64, pc) };
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
                        let sc_neg = int_to_sc(-v);
                        let sc_pos = int_to_sc(v);
                        let pc = fs.code_abc(OpCode::SHRI, r, r, sc_neg);
                        fs.code_abc_k(OpCode::MMBINI, r, sc_pos, 16, false);
                        fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                        e = ExprItem { exp: ec.into_reloc_with_pc(r as i64, pc) };
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
                        // Reuse operand register if it's a temporary (>= nvarstack),
                        // matching C compiler's behavior. Otherwise allocate new.
                        let dest = if r >= fs.nvarstack() { r } else { fs.alloc_reg() };
                        let sc = int_to_sc(v);
                        let pc = fs.code_abc(OpCode::SHRI, dest, r, sc);
                        fs.code_abc(OpCode::MMBINI, r, sc, 17);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(dest as i64, pc) };
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
            let freereg_before_r2 = fs.freereg;
            let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                let reg = e2.exp.info as i32;
                if e2.exp.info2 >= 0 {
                    fs.set_a(e2.exp.info2, reg);
                }
                reg
            } else {
                fs.exp_to_reg(&e2.exp)
            };
            if r2 != r + 1 {
                fs.code_abc(OpCode::MOVE, r + 1, r2, 0);
            }
            if fs.freereg > freereg_before_r2 {
                fs.free_reg();
            } else if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                fs.free_reg();
            }
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
                            // Like C's finishbinexpval: generate ADDI with A=0,
                            // free e2's register, allocate result register, set A.
                            let pc = fs.code_abc(OpCode::ADDI, 0, r2, sc);
                            // Free e2's register if it's a temp - like C's freeexps
                            if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            // Allocate result register and set A
                            let r_dest = fs.alloc_reg();
                            fs.set_a(pc, r_dest);
                            fs.code_abc_k(OpCode::MMBINI, r2, sc, 6, true);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                        } else {
                            let k = fs.int_k(ec.info);
                            if k <= 255 {
                                // Like C's finishbinexpval: generate ADDK with A=0,
                                // free e2's register, allocate result register, set A.
                                let pc = fs.code_abc(OpCode::ADDK, 0, r2, k);
                                // Free e2's register if it's a temp - like C's freeexps
                                if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                // Allocate result register and set A
                                let r_dest = fs.alloc_reg();
                                fs.set_a(pc, r_dest);
                                fs.code_abc_k(OpCode::MMBINK, r2, k, 6, true);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                            } else {
                                let r = fs.expr_to_reg(&ec);
                                // Like C's finishbinexpval: generate ADD with A=0,
                                // free registers, allocate result register, set A.
                                let pc = fs.code_abc(OpCode::ADD, 0, r2, r);
                                // Free registers in descending order - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 && r != r2 {
                                    fs.free_reg();
                                }
                                if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                // Allocate result register and set A
                                let r_dest = fs.alloc_reg();
                                fs.set_a(pc, r_dest);
                                fs.code_abc(OpCode::MMBIN, r2, r, 6);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                            }
                        }
                    } else {
                        // !is_add with Int ec: SUBK pattern
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
                        // Free registers in descending order - like C's freeexps
                        if r >= fs.nvarstack() && r == fs.freereg - 1 && r != r2 {
                            fs.free_reg();
                        }
                        if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        let r_dest = fs.alloc_reg();
                        fs.set_a(pc, r_dest);
                        fs.code_abc_k(OpCode::MMBINK, r, r2, 7, false);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                    }
                }
                _ => {
                    // Like C's finishbinexpval: luaK_exp2anyreg(fs, e1) to get v1,
                    // then code instruction with A=0, freeexps, then VRELOC.
                    let r_src = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        ec.info = r as i64;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc) {
                        // NonReloc without jumps: just use the register directly
                        ec.info as i32
                    } else if matches!(ec.kind, ExpKind::Relocable) {
                        // Relocable: like C's luaK_exp2anyreg for VRELOC,
                        // allocate register and set A of the relocatable instruction
                        fs.expr_to_reg(&ec)
                    } else {
                        fs.expr_to_reg(&ec)
                    };
                    if !is_add && matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) && fits_sc_neg(e2.exp.info) {
                        let v = e2.exp.info;
                        let sc_neg = int_to_sc(-v);
                        let sc_pos = int_to_sc(v);
                        let pc = fs.code_abc(OpCode::ADDI, 0, r_src, sc_neg);
                        // Free r_src if it's a temp - like C's freeexps
                        if r_src >= fs.nvarstack() && r_src == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        let r_dest = fs.alloc_reg();
                        fs.set_a(pc, r_dest);
                        fs.code_abc(OpCode::MMBINI, r_src, sc_pos, 7);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                    } else if is_add && matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) {
                        let v = e2.exp.info;
                        let sc = int_to_sc(v);
                        let pc = fs.code_abc(OpCode::ADDI, 0, r_src, sc);
                        // Free r_src if it's a temp - like C's freeexps
                        if r_src >= fs.nvarstack() && r_src == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        let r_dest = fs.alloc_reg();
                        fs.set_a(pc, r_dest);
                        fs.code_abc(OpCode::MMBINI, r_src, sc, 6);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                    } else if is_add && matches!(e2.exp.kind, ExpKind::Float) {
                        let f = f64::from_bits(e2.exp.info as u64);
                        let k = fs.float_k(f);
                        if k <= 255 {
                            let pc = fs.code_abc(OpCode::ADDK, 0, r_src, k);
                            // Free r_src if it's a temp - like C's freeexps
                            if r_src >= fs.nvarstack() && r_src == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            let r_dest = fs.alloc_reg();
                            fs.set_a(pc, r_dest);
                            fs.code_abc_k(OpCode::MMBINK, r_src, k, 6, false);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
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
                            let pc = fs.code_abc(OpCode::ADD, 0, r_src, r2);
                            // Free registers in descending order - like C's freeexps
                            if r_src >= fs.nvarstack() && r_src == fs.freereg - 1 && r_src != r2 {
                                fs.free_reg();
                            }
                            if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            let r_dest = fs.alloc_reg();
                            fs.set_a(pc, r_dest);
                            fs.code_abc(OpCode::MMBIN, r_src, r2, 6);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                        }
                    } else if !is_add && matches!(e2.exp.kind, ExpKind::Float) {
                        let f = f64::from_bits(e2.exp.info as u64);
                        let k = fs.float_k(f);
                        if k <= 255 {
                            let pc = fs.code_abc(OpCode::SUBK, 0, r_src, k);
                            // Free r_src if it's a temp - like C's freeexps
                            if r_src >= fs.nvarstack() && r_src == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            let r_dest = fs.alloc_reg();
                            fs.set_a(pc, r_dest);
                            fs.code_abc_k(OpCode::MMBINK, r_src, k, 7, false);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
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
                            let pc = fs.code_abc(OpCode::SUB, 0, r_src, r2);
                            // Free registers in descending order - like C's freeexps
                            if r_src >= fs.nvarstack() && r_src == fs.freereg - 1 && r_src != r2 {
                                fs.free_reg();
                            }
                            if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            let r_dest = fs.alloc_reg();
                            fs.set_a(pc, r_dest);
                            fs.code_abc(OpCode::MMBIN, r_src, r2, 7);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
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
                        let mm_tm = if is_add { 6 } else { 7 };
                        let pc = fs.code_abc(op, 0, r_src, r2);
                        // Free registers in descending order - like C's freeexps
                        if r_src >= fs.nvarstack() && r_src == fs.freereg - 1 && r_src != r2 {
                            fs.free_reg();
                        }
                        if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        let r_dest = fs.alloc_reg();
                        fs.set_a(pc, r_dest);
                        fs.code_abc(OpCode::MMBIN, r_src, r2, mm_tm);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
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
                        if e2.exp.info != 0 {
                            let val = ec.info as f64 / e2.exp.info as f64;
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                            fs.code_abc(OpCode::MMBINK, r, k, 11);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                        }
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
                        if e2.exp.info != 0 {
                            let val = f / (e2.exp.info as f64);
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                            fs.code_abc(OpCode::MMBINK, r, k, 11);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                        }
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
                        if f != 0.0 {
                            let val = (ec.info as f64) / f;
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k = fs.float_k(f);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 11);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::DIV, r, r, r2);
                                fs.code_abc(OpCode::MMBIN, r, r2, 11);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
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
                        if f2 != 0.0 {
                            let val = f1 / f2;
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.expr_to_reg(&ec);
                            let k = fs.float_k(f2);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                                fs.code_abc(OpCode::MMBINK, r, k, 11);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.expr_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::DIV, r, r, r2);
                                fs.code_abc(OpCode::MMBIN, r, r2, 11);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
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
                (ExpKind::Int, _) if is_mul => {
                    let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        if e2.exp.info2 >= 0 {
                            fs.set_a(e2.exp.info2, reg);
                        }
                        reg
                    } else {
                        fs.exp_to_reg(&e2.exp)
                    };
                    let k = fs.int_k(ec.info);
                    if k <= 255 {
                        // Like C's finishbinexpval: generate MULK with A=0,
                        // free e2's register, allocate result register, set A.
                        let pc = fs.code_abc(OpCode::MULK, 0, r2, k);
                        // Free e2's register if it's a temp (not in varstack) - like C's freeexps
                        if matches!(e2.exp.kind, ExpKind::NonReloc) && r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                            fs.free_reg();
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && r2 == fs.freereg - 1 && r2 >= fs.nvarstack() {
                            fs.free_reg();
                        }
                        // Allocate result register and set A - like C's luaK_exp2nextreg for VRELOC
                        let r_dest = fs.alloc_reg();
                        fs.set_a(pc, r_dest);
                        fs.code_abc_k(OpCode::MMBINK, r2, k, 8, true);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                    } else {
                        let r = fs.expr_to_reg(&ec);
                        let pc = fs.code_abc(OpCode::MUL, 0, r2, r);
                        // Free registers in descending order - like C's freeexps/freeregs
                        if r >= fs.nvarstack() && r == fs.freereg - 1 && r != r2 {
                            fs.free_reg();
                        }
                        if matches!(e2.exp.kind, ExpKind::NonReloc) && r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                            fs.free_reg();
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && r2 == fs.freereg - 1 && r2 >= fs.nvarstack() {
                            fs.free_reg();
                        }
                        // Allocate result register and set A
                        let r_dest = fs.alloc_reg();
                        fs.set_a(pc, r_dest);
                        fs.code_abc(OpCode::MMBIN, r2, r, 8);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                    }
                }
                (ExpKind::Float, _) if is_mul => {
                    let r2 = if matches!(e2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        if e2.exp.info2 >= 0 {
                            fs.set_a(e2.exp.info2, reg);
                        }
                        reg
                    } else {
                        fs.exp_to_reg(&e2.exp)
                    };
                    let f = f64::from_bits(ec.info as u64);
                    let k = fs.float_k(f);
                    if k <= 255 {
                        // Like C's finishbinexpval: generate MULK with A=0,
                        // free e2's register, allocate result register, set A.
                        let pc = fs.code_abc(OpCode::MULK, 0, r2, k);
                        // Free e2's register if it's a temp (not in varstack) - like C's freeexps
                        if matches!(e2.exp.kind, ExpKind::NonReloc) && r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                            fs.free_reg();
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && r2 == fs.freereg - 1 && r2 >= fs.nvarstack() {
                            fs.free_reg();
                        }
                        // Allocate result register and set A - like C's luaK_exp2nextreg for VRELOC
                        let r_dest = fs.alloc_reg();
                        fs.set_a(pc, r_dest);
                        fs.code_abc_k(OpCode::MMBINK, r2, k, 8, true);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                    } else {
                        let r = fs.expr_to_reg(&ec);
                        let pc = fs.code_abc(OpCode::MUL, 0, r2, r);
                        // Free registers in descending order - like C's freeexps/freeregs
                        if r >= fs.nvarstack() && r == fs.freereg - 1 && r != r2 {
                            fs.free_reg();
                        }
                        if matches!(e2.exp.kind, ExpKind::NonReloc) && r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                            fs.free_reg();
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && r2 == fs.freereg - 1 && r2 >= fs.nvarstack() {
                            fs.free_reg();
                        }
                        // Allocate result register and set A
                        let r_dest = fs.alloc_reg();
                        fs.set_a(pc, r_dest);
                        fs.code_abc(OpCode::MMBIN, r2, r, 8);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
                    }
                }
                _ => {
                    let r = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc) {
                        if ec.info2 >= 0 {
                            fs.set_a(ec.info2, ec.info as i32);
                        }
                        ec.info as i32
                    } else if matches!(ec.kind, ExpKind::Relocable) {
                        // Like C's luaK_exp2anyreg for VRELOC: allocate register and set A
                        fs.expr_to_reg(&ec)
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
                        let r_dest = if matches!(ec.kind, ExpKind::NonReloc) && (ec.info as i32) < fs.nvarstack() {
                            fs.alloc_reg();
                            (fs.freereg - 1) as i32
                        } else {
                            r
                        };
                        let pc = fs.code_abc(op, r_dest, r, k);
                        let tm = if is_mul { 8 } else if is_div { 11 } else { 9 };
                        fs.code_abc(OpCode::MMBINK, r, k, tm);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r_dest as i64, pc) };
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
        ExpKind::Str => fs.get_str_k(e),
        ExpKind::Boolean => {
            let tv = if e.info != 0 { TValue::Boolean(true) } else { TValue::Boolean(false) };
            fs.const_k(tv)
        }
        ExpKind::Nil => fs.const_k(TValue::Nil(NilKind::Strict)),
        ExpKind::Float => {
            let f = f64::from_bits(e.info as u64);
            let i = f as i64;
            if (i as f64 - f).abs() < f64::EPSILON && (i as i8 as i64) == i {
                return None;
            }
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
            ExpKind::Str => fs.get_str_k(e),
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
            ExpDesc::new_str(s)
        }
        Token::DotDotDot => {
            fs.ls_mut().next();
            // Check if function has a named vararg parameter (RDKVAVAR)
            let vararg_local = fs.locals.iter().rev().find(|lv| lv.active && lv.kind == RDKVAVAR);
            if let Some(vl) = vararg_local {
                ExpDesc::new(ExpKind::VVARGVAR, vl.reg as i64)
            } else {
                let r = fs.alloc_reg();
                fs.code_abc(OpCode::VARARG, r, 0, 0);
                ExpDesc::new(ExpKind::Vararg, r as i64)
            }
        }
        Token::LBrace => {
            let (r, _n) = parse_constructor(fs);
            return ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
        }
        Token::Name(name) => {
            let name = name.clone();
            fs.ls_mut().next();
            if let Some(ctc) = fs.find_local_ctc(&name) {
                ctc
            } else if let Some((reg, kind)) = fs.find_local_ex(&name) {
                if kind == RDKVAVAR {
                    ExpDesc::new(ExpKind::VVARGVAR, reg as i64)
                } else {
                    ExpDesc::new(ExpKind::NonReloc, reg as i64)
                }
            } else if let Some(upval_idx) = fs.find_upvalue(&name) {
                let r = fs.alloc_reg();
                let pc = fs.code_abc(OpCode::GETUPVAL, r, upval_idx, 0);
                ExpDesc { kind: ExpKind::NonReloc, info: r as i64, info2: pc, t: NO_JUMP, f: NO_JUMP, str_val: None }
            } else if name == "_ENV" {
                if let Some(env_reg) = fs.find_local("_ENV") {
                    ExpDesc::new(ExpKind::NonReloc, env_reg as i64)
                } else {
                    let r = fs.alloc_reg();
                    let pc = fs.code_abc(OpCode::GETUPVAL, r, 0, 0);
                    ExpDesc { kind: ExpKind::NonReloc, info: r as i64, info2: pc, t: NO_JUMP, f: NO_JUMP, str_val: None }
                }
            } else {
                let k = fs.string_k(&name);
                // Like C's singlevar + luaK_indexed: resolve _ENV as local, upvalue, or implicit
                let is_short_str = name.len() <= crate::strings::LUAI_MAXSHORTLEN
                    && (k as u32) <= crate::opcodes::MAXINDEXRK;
                let env_local = fs.find_local("_ENV");
                let env_upval = if env_local.is_none() { fs.find_upvalue("_ENV") } else { None };
                let (r, pc) = if let Some(env_reg) = env_local {
                    // _ENV is a local variable in current function: use GETFIELD
                    if is_short_str {
                        let r = fs.alloc_reg();
                        let pc = code_getfield(fs, r, env_reg, k);
                        (r, pc)
                    } else {
                        let env_r = fs.alloc_reg();
                        fs.code_abc(OpCode::MOVE, env_r, env_reg, 0);
                        let r = fs.alloc_reg();
                        let pc = code_getfield(fs, r, env_r, k);
                        fs.free_reg(); // free env_r
                        (r, pc)
                    }
                } else if let Some(uv_idx) = env_upval {
                    // _ENV is an upvalue captured from parent: use GETTABUP with actual upvalue index
                    if is_short_str {
                        let r = fs.alloc_reg();
                        let pc = code_gettabup(fs, r, uv_idx, k);
                        (r, pc)
                    } else {
                        let env_r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETUPVAL, env_r, uv_idx, 0);
                        let kr = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, kr, k);
                        let pc = fs.code_abc(OpCode::GETTABLE, env_r, env_r, kr);
                        fs.free_reg(); // free kr
                        (env_r, pc)
                    }
                } else {
                    // _ENV is the implicit upvalue #0 (top-level function)
                    if is_short_str {
                        let r = fs.alloc_reg();
                        let pc = code_gettabup(fs, r, 0, k);
                        (r, pc)
                    } else {
                        let env_r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETUPVAL, env_r, 0, 0);
                        let kr = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, kr, k);
                        let pc = fs.code_abc(OpCode::GETTABLE, env_r, env_r, kr);
                        fs.free_reg(); // free kr
                        (env_r, pc)
                    }
                };
                ExpDesc { kind: ExpKind::NonReloc, info: r as i64, info2: pc, t: NO_JUMP, f: NO_JUMP, str_val: None }
            }
        }
        Token::LParen => {
            fs.ls_mut().next();
            let ei = parse_expr(fs);
            expect(fs, &Token::RParen);
            match ei.exp.kind {
                ExpKind::Call => {
                    let call_pc = ei.exp.info2;
                    if call_pc >= 0 {
                        setarg(&mut fs.proto.code[call_pc as usize], 2, POS_C, SIZE_C);
                    }
                    ExpDesc { kind: ExpKind::NonReloc, info: ei.exp.info, info2: 0, t: NO_JUMP, f: NO_JUMP, str_val: None }
                }
                _ => ei.exp,
            }
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
                            if ei.exp.has_jumps() {
                                let r = fs.expr_to_reg(&ei.exp);
                                let pc = fs.code_abc(OpCode::NOT, 0, r, 0);
                                let mut e = ExpDesc::new_reloc_with_pc(r as i64, pc);
                                e.t = ei.exp.f;
                                e.f = ei.exp.t;
                                fs.remove_values(e.t);
                                fs.remove_values(e.f);
                                e
                            } else {
                                let r = if ei.exp.kind == ExpKind::Relocable {
                                    if ei.exp.info2 >= 0 {
                                        let old_r = ei.exp.info as i32;
                                        if old_r < fs.nvarstack() {
                                            let r = fs.alloc_reg();
                                            fs.set_a(ei.exp.info2, r);
                                            fs.free_reg();
                                            r
                                        } else {
                                            fs.set_a(ei.exp.info2, old_r);
                                            if old_r >= fs.freereg {
                                                fs.freereg = old_r + 1;
                                            }
                                            old_r
                                        }
                                    } else {
                                        ei.exp.info as i32
                                    }
                                } else if matches!(ei.exp.kind, ExpKind::NonReloc | ExpKind::Call) {
                                    let r = ei.exp.info as i32;
                                    if r >= fs.freereg {
                                        fs.freereg = r + 1;
                                    }
                                    r
                                } else {
                                    fs.expr_to_reg(&ei.exp)
                                };
                                let pc = fs.code_abc(OpCode::NOT, 0, r, 0);
                                ExpDesc::new_reloc_with_pc(r as i64, pc)
                            }
                        }
                    }
                }
                Token::Minus => {
                    if ei.exp.has_jumps() {
                        let r = fs.exp_to_reg(&ei.exp);
                        let pc = fs.code_abc(OpCode::UNM, 0, r, 0);
                        fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                        ExpDesc::new_reloc_with_pc(r as i64, pc)
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
                                    let pc = fs.code_abc(OpCode::UNM, 0, r, 0);
                                    fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                                    ExpDesc::new_reloc_with_pc(r as i64, pc)
                                } else {
                                    ExpDesc::new(ExpKind::Float, result.to_bits() as i64)
                                }
                            }
                            _ => {
                                let r = fs.expr_to_reg(&ei.exp);
                                let pc = fs.code_abc(OpCode::UNM, 0, r, 0);
                                fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                                ExpDesc::new_reloc_with_pc(r as i64, pc)
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
                let mut freg = fs.exp_to_reg(&e);
                let is_method = matches!(&fs.ls().token, Token::Colon);
                let src_reg = if matches!(e.kind, ExpKind::NonReloc) && freg < fs.nvarstack() {
                    if is_method {
                        let src = freg;
                        freg = fs.alloc_reg();
                        Some(src)
                    } else {
                        let new_freg = fs.alloc_reg();
                        if new_freg != freg {
                            fs.code_abc(OpCode::MOVE, new_freg, freg, 0);
                        }
                        freg = new_freg;
                        None
                    }
                } else if is_method {
                    Some(freg)
                } else {
                    None
                };
                let call_pc = parse_func_args(fs, freg, src_reg);
                e = if call_pc >= 0 {
                    ExpDesc { kind: ExpKind::Call, info: freg as i64, info2: call_pc, t: NO_JUMP, f: NO_JUMP, str_val: None }
                } else {
                    ExpDesc::new(ExpKind::Relocable, freg as i64)
                };
            }
            Token::Dot => {
                fs.ls_mut().next();
                let field = get_name(fs);
                let k = fs.string_k(&field);
                let base_reg = fs.expr_to_reg(&e);
                let is_env_upval = fs.pc > 0 && {
                    let last_ins = fs.proto.code[fs.pc as usize - 1];
                    get_opcode(last_ins) == OpCode::GETUPVAL && getarg_b(last_ins) == 0
                };
                if is_env_upval {
                    let last_idx = fs.pc as usize - 1;
                    fs.proto.code.remove(last_idx);
                    fs.pc -= 1;
                    let pc = code_gettabup(fs, base_reg, 0, k);
                    e = ExpDesc::new_reloc_with_pc(base_reg as i64, pc);
                } else {
                    let result_reg = if matches!(e.kind, ExpKind::NonReloc) && (e.info as i32) < fs.nvarstack() {
                        fs.alloc_reg()
                    } else {
                        base_reg
                    };
                    // C++ compiler: isKstr checks ttisshrstring — only short strings can use GETFIELD
                    let inst_pc = if let TValue::Str(crate::strings::LuaString::Short(_)) = fs.proto.constants[k as usize] {
                        code_getfield(fs, result_reg, base_reg, k)
                    } else {
                        // Long string: load key into register, use GETTABLE
                        fs.code_abx(OpCode::LOADK, result_reg, k);
                        fs.code_abc(OpCode::GETTABLE, result_reg, base_reg, result_reg)
                    };
                    e = ExpDesc { kind: ExpKind::NonReloc, info: result_reg as i64, info2: inst_pc, t: NO_JUMP, f: NO_JUMP, str_val: None };
                }
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
                let saved_pc_before_expr = fs.pc;
                let is_env_before = {
                    if fs.pc > 0 {
                        let last_ins = fs.proto.code[fs.pc as usize - 1];
                        get_opcode(last_ins) == OpCode::GETUPVAL && getarg_b(last_ins) == 0
                    } else {
                        false
                    }
                };
                let ei = parse_expr(fs);
                expect(fs, &Token::RBracket);
                if is_env_before && ei.exp.kind == ExpKind::Str {
                    fs.proto.code.truncate((saved_pc_before_expr - 1) as usize);
                    fs.pc = saved_pc_before_expr - 1;
                    let k = fs.get_str_k(&ei.exp);
                    let pc = code_gettabup(fs, base_reg, 0, k);
                    e = ExpDesc::new_reloc_with_pc(base_reg as i64, pc);
                } else {
                    let result_reg;
                    let inst_pc;
                    if ei.exp.kind == ExpKind::Int
                        && ei.exp.info >= 0
                        && ei.exp.info <= ((1u32 << SIZE_C) - 1) as i64
                    {
                        result_reg = if base_is_nonreloc_local {
                            fs.alloc_reg()
                        } else {
                            base_reg
                        };
                        inst_pc = fs.code_abc(OpCode::GETI, result_reg, base_reg, ei.exp.info as i32);
                    } else {
                        let key_reg = if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() {
                            if ei.exp.info2 >= 0 {
                                // 模拟 C 的 luaK_exp2anyreg：如果源寄存器在栈顶且非局部变量，先释放再分配（复用同一寄存器）
                                if (ei.exp.info as i32) == fs.freereg - 1 && (ei.exp.info as i32) >= fs.nvarstack() {
                                    fs.free_reg();
                                }
                                let r = fs.alloc_reg();
                                fs.set_a(ei.exp.info2, r);
                                r
                            } else {
                                ei.exp.info as i32
                            }
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
                        inst_pc = fs.code_abc(OpCode::GETTABLE, result_reg, base_reg, key_reg);
                        if result_reg != key_reg && key_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                    }
                    e = ExpDesc { kind: ExpKind::NonReloc, info: result_reg as i64, info2: inst_pc, t: NO_JUMP, f: NO_JUMP, str_val: None };
                }
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
    let entry_freereg = fs.freereg;
    fs.ls_mut().next();
    let ei = parse_expr(fs);

    let is_const_true = matches!(ei.exp.kind, ExpKind::Boolean) && ei.exp.info != 0;

    let mut if_jmp = NO_JUMP;

    if !is_const_true {
        if ei.exp.kind == ExpKind::VJMP {
            let jmp_pc = ei.exp.info as i32;
            fs.negate_condition(jmp_pc);
            let mut false_list = ei.exp.f;
            fs.concat_jump(&mut false_list, jmp_pc);
            fs.patch_true_jumps(ei.exp.t, fs.pc);
            if_jmp = false_list;
        } else {
            let pre_freereg = fs.freereg;
            let is_not_vreloc = ei.exp.info2 >= 0
                && (ei.exp.info2 as usize) < fs.proto.code.len()
                && get_opcode(fs.proto.code[ei.exp.info2 as usize]) == OpCode::NOT;
            let (cond_reg, test_with_k) = if (ei.exp.info2 == -2 || is_not_vreloc)
                && matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc)
            {
                let reg = ei.exp.info as i32;
                if is_not_vreloc {
                    fs.pc -= 1;
                    fs.proto.code.pop();
                } else if !fs.proto.code.is_empty()
                    && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT
                {
                    fs.proto.code.pop();
                    fs.pc -= 1;
                }
                (reg, true)
            } else {
                let reg = fs.cond_to_reg(&ei.exp);
                let k = matches!(ei.exp.kind, ExpKind::Relocable)
                    && !fs.proto.code.is_empty()
                    && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT;
                if k {
                    fs.proto.code.pop();
                    fs.pc -= 1;
                }
                (reg, k)
            };
            fs.code_abc_k(OpCode::TEST, cond_reg, 0, 0, test_with_k);
            if_jmp = fs.jump();
            if fs.freereg > pre_freereg {
                fs.free_reg();
            }
        }
    }

    expect(fs, &Token::Then);
    fs.set_freereg(entry_freereg);
    parse_block(fs);  // Like C's block(ls) in test_then_block
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
            if ei2.exp.kind == ExpKind::VJMP {
                let jmp_pc = ei2.exp.info as i32;
                fs.negate_condition(jmp_pc);
                let mut false_list = ei2.exp.f;
                fs.concat_jump(&mut false_list, jmp_pc);
                fs.patch_true_jumps(ei2.exp.t, fs.pc);
                if_jmp = false_list;
            } else {
                let pre_freereg2 = fs.freereg;
                let is_not_vreloc2 = ei2.exp.info2 >= 0
                    && (ei2.exp.info2 as usize) < fs.proto.code.len()
                    && get_opcode(fs.proto.code[ei2.exp.info2 as usize]) == OpCode::NOT;
                let (cr2, test_with_k2) = if (ei2.exp.info2 == -2 || is_not_vreloc2)
                    && matches!(ei2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc)
                {
                    let reg = ei2.exp.info as i32;
                    if is_not_vreloc2 {
                        fs.pc -= 1;
                        fs.proto.code.pop();
                    } else if !fs.proto.code.is_empty()
                        && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT
                    {
                        fs.proto.code.pop();
                        fs.pc -= 1;
                    }
                    (reg, true)
                } else {
                    let reg = fs.cond_to_reg(&ei2.exp);
                    let k = matches!(ei2.exp.kind, ExpKind::Relocable)
                        && !fs.proto.code.is_empty()
                        && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT;
                    if k {
                        fs.proto.code.pop();
                        fs.pc -= 1;
                    }
                    (reg, k)
                };
                fs.code_abc_k(OpCode::TEST, cr2, 0, 0, test_with_k2);
                if_jmp = fs.jump();
                if fs.freereg > pre_freereg2 {
                    fs.free_reg();
                }
            }
        } else {
            if_jmp = NO_JUMP;
        }
        expect(fs, &Token::Then);
        fs.set_freereg(entry_freereg);
        parse_block(fs);  // Like C's block(ls) in test_then_block
    }

    if check(fs, &Token::Else) {
        let j = fs.jump();
        exit_jumps.push(j);
        if if_jmp != NO_JUMP {
            fs.fix_jump(if_jmp, fs.pc, false);
        }
        fs.ls_mut().next();
        fs.set_freereg(entry_freereg);
        parse_block(fs);  // Like C's block(ls) in ifstat's else part
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
    let entry_freereg = fs.freereg;
    fs.ls_mut().next();
    let loop_start = fs.pc;
    fs.lasttarget = fs.pc;  // mark while start as jump target (like luaK_getlabel)
    let mut ei = parse_expr(fs);
    let pre_freereg = fs.freereg;
    
    let condexit = if ei.exp.kind == ExpKind::VJMP {
        // Handle VJMP like luaK_goiftrue in C: negate condition, 
        // add JMP to false list, patch true list to here (body start)
        let saved_jmp = ei.exp.info as i32;
        fs.negate_condition(saved_jmp);
        fs.concat_jump(&mut ei.exp.f, saved_jmp);
        let here = fs.pc;
        fs.patch_true_jumps(ei.exp.t, here);
        ei.exp.f
    } else {
        let is_not_vreloc = ei.exp.info2 >= 0
            && (ei.exp.info2 as usize) < fs.proto.code.len()
            && get_opcode(fs.proto.code[ei.exp.info2 as usize]) == OpCode::NOT;
        let (r, test_with_k) = if (ei.exp.info2 == -2 || is_not_vreloc)
            && matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc)
        {
            let reg = ei.exp.info as i32;
            if is_not_vreloc {
                fs.pc -= 1;
                fs.proto.code.pop();
            } else if !fs.proto.code.is_empty()
                && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT
            {
                fs.proto.code.pop();
                fs.pc -= 1;
            }
            (reg, true)
        } else {
            let reg = fs.cond_to_reg(&ei.exp);
            let k = matches!(ei.exp.kind, ExpKind::Relocable)
                && !fs.proto.code.is_empty()
                && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT;
            if k {
                fs.proto.code.pop();
                fs.pc -= 1;
            }
            (reg, k)
        };
        fs.code_abc_k(OpCode::TEST, r, 0, 0, test_with_k);
        let jmp = fs.jump();
        if fs.freereg > pre_freereg {
            fs.free_reg();
        }
        jmp
    };
    expect(fs, &Token::Do);

    let saved_breaklist = fs.break_list;
    fs.break_list = NO_JUMP;

    // Push outer block (while loop block, like C's enterblock with isloop=1)
    let saved_nlocals = fs.locals.len();
    let saved_nlabels = fs.labels.len();
    fs.block_stack.push(BlockEntry { saved_nlocals, has_upval: false, is_function_body: false });
    fs.set_freereg(entry_freereg);

    parse_block(fs);  // inner body block (like C's block(ls))

    // JMP back to loop start (BEFORE outer leaveblock, like C's luaK_jumpto)
    fs.code_sj(OpCode::JMP, loop_start - fs.pc - 1, 0);

    // Leave outer block (while loop block, like C's leaveblock)
    let has_upval = fs.current_block_has_upval();
    fs.block_stack.pop();
    let has_tbc = fs.locals[saved_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
    let close_reg = fs.nvarstack_up_to(saved_nlocals);
    if has_tbc || has_upval {
        fs.code_abc(OpCode::CLOSE, close_reg, 0, 0);
    }
    solve_gotos_for_block(fs, saved_nlabels, saved_nlocals, has_tbc || has_upval);
    for local in &mut fs.locals[saved_nlocals..] {
        local.active = false;
    }
    fs.set_freereg(close_reg);

    // Patch condexit (AFTER outer leaveblock, like C's luaK_patchtohere)
    let mut cur = condexit;
    while cur != NO_JUMP {
        let next = fs.get_jump(cur);
        fs.fix_jump(cur, fs.pc, false);
        cur = next;
    }

    // Create break label for goto-based break resolution
    fs.labels.push(LabelDesc {
        name: "break".to_string(),
        pc: fs.pc,
        nactvar: saved_nlocals as i32,
        line: 0,
    });
    fs.patch_breaks(fs.pc);
    fs.break_list = saved_breaklist;

    expect(fs, &Token::End);
}

/// ANTLR4: `'do' block 'end' ;`
fn parse_do(fs: &mut FuncState) {
    fs.ls_mut().next();
    parse_block(fs);  // Like C's block(ls) in dostat
    expect(fs, &Token::End);
}

/// ANTLR4: `'repeat' block 'until' expr ;`
fn parse_repeat(fs: &mut FuncState) {
    let entry_freereg = fs.freereg;
    fs.ls_mut().next();
    let loop_start = fs.pc;
    fs.lasttarget = fs.pc;  // mark loop start as jump target (like luaK_getlabel)

    let saved_breaklist = fs.break_list;
    fs.break_list = NO_JUMP;

    // Push bl1 (loop block, like C's enterblock with isloop=1)
    let bl1_nlocals = fs.locals.len();
    let bl1_nlabels = fs.labels.len();
    fs.block_stack.push(BlockEntry { saved_nlocals: bl1_nlocals, has_upval: false, is_function_body: false });

    // Push bl2 (scope block, like C's enterblock with isloop=0)
    let bl2_nlocals = fs.locals.len();
    let bl2_nlabels = fs.labels.len();
    fs.block_stack.push(BlockEntry { saved_nlocals: bl2_nlocals, has_upval: false, is_function_body: false });

    fs.set_freereg(entry_freereg);
    parse_chunk_stmts(fs);  // Like C's statlist(ls)

    expect(fs, &Token::Until);

    // Parse condition INSIDE bl2 (like C's cond(ls) inside scope block)
    let ei = parse_expr(fs);

    // Leave bl2 (finish scope, like C's leaveblock)
    let bl2_has_upval = fs.current_block_has_upval();
    fs.block_stack.pop();
    let bl2_has_tbc = fs.locals[bl2_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
    let bl2_close_reg = fs.nvarstack_up_to(bl2_nlocals);
    if bl2_has_tbc || bl2_has_upval {
        fs.code_abc(OpCode::CLOSE, bl2_close_reg, 0, 0);
    }
    solve_gotos_for_block(fs, bl2_nlabels, bl2_nlocals, bl2_has_tbc || bl2_has_upval);

    let is_const_true = matches!(ei.exp.kind, ExpKind::Boolean if ei.exp.info != 0)
        || matches!(ei.exp.kind, ExpKind::Int | ExpKind::Float | ExpKind::Str);

    // Handle condition and upvalue CLOSE logic (like C's repeatstat)
    let mut condexit: i32 = NO_JUMP;

    if !is_const_true {
        if ei.exp.kind == ExpKind::VJMP {
            let jmp_pc = ei.exp.info as i32;
            fs.negate_condition(jmp_pc);
            let mut false_list = ei.exp.f;
            fs.concat_jump(&mut false_list, jmp_pc);
            fs.patch_true_jumps(ei.exp.t, fs.pc);
            condexit = false_list;
        } else {
            let pre_freereg = fs.freereg;
            let is_not_vreloc3 = ei.exp.info2 >= 0
                && (ei.exp.info2 as usize) < fs.proto.code.len()
                && get_opcode(fs.proto.code[ei.exp.info2 as usize]) == OpCode::NOT;
            let (r, eq_with_k) = if (ei.exp.info2 == -2 || is_not_vreloc3)
                && matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc)
            {
                let reg = ei.exp.info as i32;
                if is_not_vreloc3 {
                    fs.pc -= 1;
                    fs.proto.code.pop();
                } else if !fs.proto.code.is_empty()
                    && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT
                {
                    fs.proto.code.pop();
                    fs.pc -= 1;
                }
                (reg, false)
            } else {
                let reg = fs.cond_to_reg(&ei.exp);
                let k = !(matches!(ei.exp.kind, ExpKind::Relocable)
                    && !fs.proto.code.is_empty()
                    && get_opcode(*fs.proto.code.last().unwrap()) == OpCode::NOT);
                if !k {
                    fs.proto.code.pop();
                    fs.pc -= 1;
                }
                (reg, k)
            };
            fs.code_abc_k(OpCode::EQ, r, 0, 0, eq_with_k);
            condexit = fs.jump();
            if fs.freereg > pre_freereg {
                fs.free_reg();
            }
        }
    }

    // If bl2 has upvalues, emit CLOSE fix (like C's repeatstat upvalue handling)
    if bl2_has_upval {
        let exit = fs.jump();  // normal exit must jump over fix
        // Patch condexit to here: repetition must close upvalues
        let mut cur = condexit;
        while cur != NO_JUMP {
            let next = fs.get_jump(cur);
            fs.fix_jump(cur, fs.pc, false);
            cur = next;
        }
        fs.code_abc(OpCode::CLOSE, bl2_close_reg, 0, 0);
        condexit = fs.jump();  // repeat after closing upvalues
        fs.fix_jump(exit, fs.pc, false);  // normal exit comes to here
    }

    // Patch condexit to loop_start (like C's luaK_patchlist)
    let mut cur = condexit;
    while cur != NO_JUMP {
        let next = fs.get_jump(cur);
        fs.fix_jump(cur, loop_start, true);
        cur = next;
    }

    // Create break label for goto-based break resolution
    fs.labels.push(LabelDesc {
        name: "break".to_string(),
        pc: fs.pc,
        nactvar: bl1_nlocals as i32,
        line: 0,
    });
    fs.patch_breaks(fs.pc);
    fs.break_list = saved_breaklist;

    // Leave bl1 (finish loop, like C's leaveblock)
    let bl1_has_upval = fs.current_block_has_upval();
    fs.block_stack.pop();
    let bl1_has_tbc = fs.locals[bl1_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
    let bl1_close_reg = fs.nvarstack_up_to(bl1_nlocals);
    if bl1_has_tbc || bl1_has_upval {
        fs.code_abc(OpCode::CLOSE, bl1_close_reg, 0, 0);
    }
    solve_gotos_for_block(fs, bl1_nlabels, bl1_nlocals, bl1_has_tbc || bl1_has_upval);
    for local in &mut fs.locals[bl1_nlocals..] {
        local.active = false;
    }
    fs.set_freereg(bl1_close_reg);
}

/// ANTLR4: `'for' NAME '=' expr ',' expr (',' expr)? 'do' block 'end' ;` (numeric for) 以及 `'for' namelist 'in' explist 'do' block 'end' ;` (generic for)
fn parse_for(fs: &mut FuncState) {
    fs.ls_mut().next();
    let name = get_name(fs);
    
    if check(fs, &Token::Eq) {
        fs.ls_mut().next();
        let saved_freereg = fs.freereg;
        let base = fs.freereg;

        // Push forstat block (like C's enterblock in forstat)
        let forstat_nlocals = fs.locals.len();
        let forstat_nlabels = fs.labels.len();
        fs.block_stack.push(BlockEntry { saved_nlocals: forstat_nlocals, has_upval: false, is_function_body: false });

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
        
        // Like C's fornum: activate the first 2 internal variables before FORPREP,
        // then activate the loop variable inside the body block (after enterblock).
        // This ensures the body block's nactvar does NOT include the loop variable,
        // so markupval correctly marks the body block when the loop variable is captured.
        fs.add_local_kind_reg("(for state)", fs.pc, RDKCONST, base);
        fs.add_local_kind_reg("(for state)", fs.pc, RDKCONST, base + 1);
        
        let prep = fs.code_abx(OpCode::FORPREP, base, 0);

        let saved_breaklist = fs.break_list;
        fs.break_list = NO_JUMP;

        fs.lasttarget = fs.pc;  // mark for body start as jump target (like luaK_getlabel)

        // Like C's forbody: enterblock BEFORE activating the loop variable
        let body_nlocals = fs.locals.len();  // Only 2 "(for state)" vars at this point
        let body_nlabels = fs.labels.len();
        fs.block_stack.push(BlockEntry { saved_nlocals: body_nlocals, has_upval: false, is_function_body: false });

        // Like C's adjustlocalvars(ls, nvars): activate loop variable INSIDE body block
        fs.add_local_kind_reg(&name, fs.pc, RDKCONST, base + 2);

        // parse_block creates the inner block (like C's block() in forbody)
        parse_block(fs);

        // Leave body block (like C's leaveblock for forbody's block)
        let has_body_upval = fs.current_block_has_upval();
        fs.block_stack.pop();
        let body_has_tbc = fs.locals[body_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
        let body_close_reg = fs.nvarstack_up_to(body_nlocals);
        if body_has_tbc || has_body_upval {
            fs.code_abc(OpCode::CLOSE, body_close_reg, 0, 0);
        }
        solve_gotos_for_block(fs, body_nlabels, body_nlocals, body_has_tbc || has_body_upval);
        // Like C's leaveblock: deactivate variables and set freereg
        for local in &mut fs.locals[body_nlocals..] {
            local.active = false;
        }
        fs.set_freereg(body_close_reg);

        fs.fix_jump(prep, fs.pc, false);
        let loop_pc = fs.code_abx(OpCode::FORLOOP, base, 0);
        fs.fix_jump(loop_pc, prep + 1, true);

        // Handle forstat block exit (like C's leaveblock for forstat) [NUMERIC FOR]
        // C order: 1) CLOSE  2) createlabel(break)  3) solvegotos
        let has_forstat_upval = fs.current_block_has_upval();
        let forstat_has_tbc = fs.locals[forstat_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
        let forstat_close_reg = fs.nvarstack_up_to(forstat_nlocals);
        fs.block_stack.pop();
        if has_forstat_upval || forstat_has_tbc {
            fs.code_abc(OpCode::CLOSE, forstat_close_reg, 0, 0);
        }

        // Create break label AFTER forstat CLOSE (like C's createlabel after CLOSE)
        fs.labels.push(LabelDesc {
            name: "break".to_string(),
            pc: fs.pc,
            nactvar: forstat_nlocals as i32,
            line: 0,
        });
        fs.patch_breaks(fs.pc);
        fs.break_list = saved_breaklist;

        solve_gotos_for_block(fs, forstat_nlabels, forstat_nlocals, has_forstat_upval || forstat_has_tbc);

        for local in &mut fs.locals[forstat_nlocals..] {
            local.active = false;
        }
        fs.set_freereg(forstat_close_reg);
        expect(fs, &Token::End);
    } else {
        let mut vars = vec![name];
        while check(fs, &Token::Comma) {
            fs.ls_mut().next();
            let var = get_name(fs);
            vars.push(var);
        }
        expect(fs, &Token::In);
        
        let saved_freereg = fs.freereg;
        let base = fs.freereg;

        // Push forstat block (like C's enterblock in forstat)
        let forstat_nlocals = fs.locals.len();
        let forstat_nlabels = fs.labels.len();
        fs.block_stack.push(BlockEntry { saved_nlocals: forstat_nlocals, has_upval: false, is_function_body: false });
        
        // Like C's forlist: declare all variables, but only activate the 3 internal
        // ones before expression parsing. User-declared variables are activated inside
        // the body block (like C's adjustlocalvars in forbody).
        fs.add_local_kind("(for state)", fs.pc, RDKCONST);
        fs.add_local_kind("(for state)", fs.pc, RDKCONST);
        fs.add_local_kind("(for state)", fs.pc, RDKTOCLOSE);
        let ncontrol = vars.len() as i32;
        // Add user-declared variables as INACTIVE (they'll be activated inside body block)
        for var_name in &vars {
            let vidx = fs.locals.len() as i32;
            fs.locals.push(LocalVar {
                name: var_name.clone(),
                start_pc: fs.pc,
                active: false,
                reg: 0,
                kind: RDKCONST,
                ctc_kind: None,
                ctc_info: None,
                ctc_str: None,
                vidx,
            });
        }
        // Deactivate internal variables too during expression parsing
        for lv in &mut fs.locals[forstat_nlocals..] {
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
        // Like C's adjustlocalvars(ls, 3): activate only the 3 internal variables
        for (i, lv) in fs.locals[forstat_nlocals..].iter_mut().enumerate() {
            if i < 3 {
                lv.active = true;
            }
        }
        // Like C's marktobeclosed(fs): third internal var is to-be-closed,
        // so mark the forstat block as having upvalues (bl->upval = 1)
        if let Some(block) = fs.block_stack.last_mut() {
            block.has_upval = true;
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
            fs.code_nil(base + nexps, 4 - nexps);
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
        fs.lasttarget = fs.pc;  // mark for body start as jump target (like luaK_getlabel)

        // Like C's forbody: enterblock BEFORE activating user-declared variables
        // body_nlocals = forstat_nlocals + 3 (only the 3 internal variables)
        let body_nlocals = fs.locals.len() - vars.len();  // Exclude user-declared vars
        let body_nlabels = fs.labels.len();
        fs.block_stack.push(BlockEntry { saved_nlocals: body_nlocals, has_upval: false, is_function_body: false });

        // Like C's adjustlocalvars(ls, nvars): activate user-declared variables INSIDE body block
        // Also assign registers to them (like C's luaK_reserveregs)
        for (i, lv) in fs.locals[body_nlocals..].iter_mut().enumerate() {
            lv.active = true;
            lv.reg = base + 3 + i as i32;
        }

        // parse_block creates the inner block (like C's block() in forbody)
        parse_block(fs);

        // Leave body block (like C's leaveblock for forbody's block) [GENERIC FOR]
        let has_body_upval = fs.current_block_has_upval();
        fs.block_stack.pop();
        let body_has_tbc = fs.locals[body_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
        let body_close_reg = fs.nvarstack_up_to(body_nlocals);
        if body_has_tbc || has_body_upval {
            fs.code_abc(OpCode::CLOSE, body_close_reg, 0, 0);
        }
        solve_gotos_for_block(fs, body_nlabels, body_nlocals, body_has_tbc || has_body_upval);
        // Like C's leaveblock: deactivate variables and set freereg
        for local in &mut fs.locals[body_nlocals..] {
            local.active = false;
        }
        fs.set_freereg(body_close_reg);
        
        fs.fix_jump(prep, fs.pc, false);
        fs.code_abc(OpCode::TFORCALL, base, 0, ncontrol);
        let loop_pc = fs.code_abx(OpCode::TFORLOOP, base, 0);
        fs.fix_jump(loop_pc, prep + 1, true);

        // Handle forstat block exit (like C's leaveblock for forstat) [GENERIC FOR]
        // C order: 1) CLOSE  2) createlabel(break)  3) solvegotos
        let has_forstat_upval = fs.current_block_has_upval();
        let forstat_has_tbc = fs.locals[forstat_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
        let forstat_close_reg = fs.nvarstack_up_to(forstat_nlocals);
        fs.block_stack.pop();
        if has_forstat_upval || forstat_has_tbc {
            fs.code_abc(OpCode::CLOSE, forstat_close_reg, 0, 0);
        }

        // Create break label AFTER forstat CLOSE (like C's createlabel after CLOSE)
        fs.labels.push(LabelDesc {
            name: "break".to_string(),
            pc: fs.pc,
            nactvar: forstat_nlocals as i32,
            line: 0,
        });
        fs.patch_breaks(fs.pc);
        fs.break_list = saved_breaklist;

        expect(fs, &Token::End);

        solve_gotos_for_block(fs, forstat_nlabels, forstat_nlocals, has_forstat_upval || forstat_has_tbc);
        for local in &mut fs.locals[forstat_nlocals..] {
            local.active = false;
        }
        fs.set_freereg(forstat_close_reg);
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
            // Like C's funcstat: resolve through _ENV (which may be local or upvalue)
            let k = fs.string_k(name);
            let is_short_str = name.len() <= crate::strings::LUAI_MAXSHORTLEN
                && (k as u32) <= crate::opcodes::MAXINDEXRK;
            if let Some(env_reg) = fs.find_local("_ENV") {
                // _ENV is a local variable: use SETFIELD
                if is_short_str {
                    fs.code_abc(OpCode::SETFIELD, env_reg, k, r);
                } else {
                    // Key exceeds MAXINDEXRK: load _ENV into temp register first
                    let env_r = fs.alloc_reg();
                    fs.code_abc(OpCode::MOVE, env_r, env_reg, 0);
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);
                    fs.code_abc(OpCode::SETTABLE, env_r, kr, r);
                    fs.free_reg(); // free kr
                    fs.free_reg(); // free env_r
                }
            } else {
                // _ENV is an upvalue: use SETTABUP
                if is_short_str {
                    code_settabup(fs, 0, k, r);
                } else {
                    // Key exceeds MAXINDEXRK: load _ENV into temp register first
                    let env_r = fs.alloc_reg();
                    fs.code_abc(OpCode::GETUPVAL, env_r, 0, 0);
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);
                    fs.code_abc(OpCode::SETTABLE, env_r, kr, r);
                    fs.free_reg(); // free kr
                    fs.free_reg(); // free env_r
                }
            }
        }
        fs.free_reg();
        return;
    }

    let first_name = &chain[0].1;
    let mut base_reg = if let Some(reg) = fs.find_local(first_name) {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::MOVE, r, reg, 0);
        r
    } else if let Some(env_reg) = fs.find_local("_ENV") {
        // _ENV is a local variable: use GETFIELD
        let r = fs.alloc_reg();
        let k = fs.string_k(first_name);
        code_getfield(fs, r, env_reg, k);
        r
    } else {
        let r = fs.alloc_reg();
        let k = fs.string_k(first_name);
        code_gettabup(fs, r, 0, k);
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
                            let _ = fs.exp_to_reg(&ei.exp);
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

            if last_is_ctc {
                let popped = fs.proto.code.pop();
                fs.pc -= 1;
                // If the popped instruction is LOADK, the constant it references
                // was added for this CTC variable and should also be removed.
                // (LOADI/LOADF don't add constants, so no pop needed for those.)
                if let Some(inst) = popped {
                    if crate::opcodes::get_opcode(inst) == OpCode::LOADK {
                        fs.proto.constants.pop();
                    }
                }
            }

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
                let pc = fs.pc;
                let last_e = last_exp.as_ref().unwrap();
                let ctc_str = if last_e.kind == ExpKind::Str {
                    // String constant was already popped above (LOADK case).
                    // Reconstruct the string from the expression's str_val.
                    last_e.str_val.clone()
                } else {
                    None
                };
                let ctc_info = if last_e.kind == ExpKind::Str { 0 } else { last_e.info };
                let vidx = fs.locals.len() as i32;
                fs.locals.push(LocalVar {
                    name: names[nvars - 1].clone(),
                    start_pc: pc,
                    active: true,
                    reg: 0,
                    kind: RDKCTC,
                    ctc_kind: Some(last_e.kind.clone()),
                    ctc_info: Some(ctc_info),
                    ctc_str,
                    vidx,
                });
            }

            fs.set_freereg(saved_freereg + n_reg as i32);

            if !last_is_call && n_vals < n_reg {
                let remaining = n_reg - n_vals;
                fs.code_nil(saved_freereg + n_vals as i32, remaining as i32);
            }
        } else {
            let start_reg = fs.add_local_kind(&names[0], fs.pc, kinds[0]);
            for i in 1..nvars {
                fs.add_local_kind(&names[i], fs.pc, kinds[i]);
            }
            fs.code_nil(start_reg, nvars as i32);
        }

        for (i, &kind) in kinds.iter().enumerate() {
            if kind == RDKTOCLOSE {
                if let Some(reg) = fs.find_local(&names[i]) {
                    fs.code_abc(OpCode::TBC, reg, 0, 0);
                    // Like C's marktobeclosed(fs): mark current block as having upvalues
                    if let Some(block) = fs.block_stack.last_mut() {
                        block.has_upval = true;
                    }
                    fs.needclose = true;
                    break;
                }
            }
        }
    }
}

/// ANTLR4: `'return' explist? (';')? ;`
/// Matches C's retstat: when nret > 1, all values must go to the top of the stack.
fn parse_return(fs: &mut FuncState) {
    fs.ls_mut().next();
    if block_follow(fs, true) || check(fs, &Token::Semi) {
        fs.return_stat_gen(fs.nvarstack(), 0);
        fs.set_freereg(fs.nvarstack());
        if check(fs, &Token::Semi) { fs.ls_mut().next(); }
        return;
    }
    
    let first = fs.nvarstack();
    let ei = parse_expr(fs);

    if check(fs, &Token::Comma) {
        fs.ls_mut().next();
        // Multiple return values: force first expression to next reg (like C's luaK_exp2nextreg)
        fs.exp_to_next_reg(&ei.exp);
        let nret = 1 + parse_expr_list(fs);
        fs.return_stat_gen(first, nret);
    } else {
        // Single return value: can use original slot (like C's luaK_exp2anyreg)
        let r = fs.exp_to_reg(&ei.exp);
        fs.return_stat_gen(r, 1);
    }
    fs.set_freereg(fs.nvarstack());
    if check(fs, &Token::Semi) { fs.ls_mut().next(); }
}

/// ANTLR4: `explist: expr (',' expr)* ;` — 解析逗号分隔的表达式列表
/// Matches C's explist: force each expression to next reg before parsing the next one.
/// The last expression is NOT forced (caller decides).
fn parse_expr_list(fs: &mut FuncState) -> i32 {
    let mut ei = parse_expr(fs);
    let mut n = 1;
    while check(fs, &Token::Comma) {
        fs.ls_mut().next();
        // Force previous expression to next reg (like C's luaK_exp2nextreg in explist)
        fs.exp_to_next_reg(&ei.exp);
        ei = parse_expr(fs);
        n += 1;
    }
    // Force the last expression to next reg too (for return context, caller handles this)
    fs.exp_to_next_reg(&ei.exp);
    n
}

/// Compute a limit for how many registers a constructor can use before
/// emitting a SETLIST instruction, based on how many registers are available.
fn maxtostore(fs: &FuncState) -> i32 {
    let numfreeregs = (MAX_FSTACK as i32) - fs.freereg;
    if numfreeregs >= 160 {
        numfreeregs / 5
    } else if numfreeregs >= 80 {
        10
    } else {
        1
    }
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
    let maxtostore = maxtostore(fs);

    if !check(fs, &Token::RBrace) {
        loop {
            if check(fs, &Token::LBracket) {
                // closelistfield: discharge previous list item
                if let Some(prev) = last_list_exp.take() {
                    fs.exp_to_next_reg(&prev);
                    tostore += 1;
                }
                // Only flush SETLIST if tostore >= maxtostore
                if tostore > 0 && tostore >= maxtostore {
                    fs.code_abc(OpCode::SETLIST, table_r, tostore, need_array);
                    need_array += tostore;
                    tostore = 0;
                    fs.set_freereg(table_r + 1);
                }
                // recfield: save freereg, process, restore freereg
                let saved_freereg = fs.freereg;
                fs.ls_mut().next();
                let ek = parse_expr(fs);
                expect(fs, &Token::RBracket);
                expect(fs, &Token::Eq);
                // Discharge key constant first to match C++ constant table order
                let key_k = if ek.exp.kind == ExpKind::Str {
                    Some(fs.get_str_k(&ek.exp))
                } else {
                    None
                };
                let is_seti = ek.exp.kind == ExpKind::Int
                    && ek.exp.info >= 0
                    && ek.exp.info <= ((1u32 << SIZE_C) - 1) as i64;
                // For SETTABLE, discharge key to register before parsing value,
                // so key's register is at freereg-1 when discharged
                let k_r = if key_k.is_none() && !is_seti {
                    Some(fs.exp_to_reg(&ek.exp))
                } else {
                    None
                };
                let ev = parse_expr(fs);
                let (v_rk, is_k) = exp2rk(fs, &ev.exp);
                if let Some(k) = key_k {
                    // C++ compiler: isKstr checks ttisshrstring — only short strings use SETFIELD
                    if let TValue::Str(crate::strings::LuaString::Short(_)) = fs.proto.constants[k as usize] {
                        code_setfield_k(fs, table_r, k, v_rk, is_k);
                    } else {
                        // Long string: load key into register, use SETTABLE
                        let kr = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, kr, k);
                        fs.code_abc_k(OpCode::SETTABLE, table_r, kr, v_rk, is_k);
                    }
                } else if is_seti {
                    let k = ek.exp.info as i32;
                    fs.code_abc_k(OpCode::SETI, table_r, k, v_rk, is_k);
                } else {
                    fs.code_abc_k(OpCode::SETTABLE, table_r, k_r.unwrap(), v_rk, is_k);
                }
                fs.freereg = saved_freereg;  /* free registers used by recfield */
                need_hash += 1;
            } else if let Token::Name(s) = &fs.ls().token {
                let name = s.clone();
                let next_is_eq = fs.ls_mut().lookahead_next().0 == Token::Eq;
                if next_is_eq {
                    // closelistfield: discharge previous list item
                    if let Some(prev) = last_list_exp.take() {
                        fs.exp_to_next_reg(&prev);
                        tostore += 1;
                    }
                    // Only flush SETLIST if tostore >= maxtostore
                    if tostore > 0 && tostore >= maxtostore {
                        fs.code_abc(OpCode::SETLIST, table_r, tostore, need_array);
                        need_array += tostore;
                        tostore = 0;
                        fs.set_freereg(table_r + 1);
                    }
                    // recfield: save freereg, process, restore freereg
                    // C++ order: process key first (luaK_indexed may emit LOADK),
                    // then parse value (expr), then store (luaK_storevar).
                    let saved_freereg = fs.freereg;
                    fs.ls_mut().next();
                    fs.ls_mut().next();
                    let k = fs.string_k(&name);
                    // C++ compiler: isKstr checks ttisshrstring AND k->u.info <= MAXINDEXRK
                    let key_needs_reg = name.len() > crate::strings::LUAI_MAXSHORTLEN || (k as u32) > crate::opcodes::MAXINDEXRK;
                    let kr = if key_needs_reg {
                        let kr = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, kr, k);
                        kr
                    } else {
                        -1
                    };
                    let ev = parse_expr(fs);
                    let (v_rk, is_k) = exp2rk(fs, &ev.exp);
                    if key_needs_reg {
                        fs.code_abc_k(OpCode::SETTABLE, table_r, kr, v_rk, is_k);
                    } else {
                        fs.code_abc_k(OpCode::SETFIELD, table_r, k, v_rk, is_k);
                    }
                    fs.freereg = saved_freereg;  /* free registers used by recfield */
                    need_hash += 1;
                } else {
                    if let Some(prev) = last_list_exp.take() {
                        fs.exp_to_next_reg(&prev);
                        tostore += 1;
                    }
                    last_list_exp = Some(parse_expr(fs).exp);
                }
            } else {
                if let Some(prev) = last_list_exp.take() {
                    fs.exp_to_next_reg(&prev);
                    tostore += 1;
                }
                last_list_exp = Some(parse_expr(fs).exp);
            }

            if !check(fs, &Token::Comma) && !check(fs, &Token::Semi) { break; }
            fs.ls_mut().next();
            if check(fs, &Token::RBrace) { break; }
        }
    }

    // lastlistfield
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
            fs.exp_to_next_reg(&last);
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
    let mut vararg_named = false;
    if ismethod {
        param_names.push("self".to_string());
        n_params = 1;
    }
    if has_params {
        loop {
            if check(fs, &Token::DotDotDot) {
                is_vararg = true;
                fs.ls_mut().next();
                // Lua 5.5: ...name syntax for named vararg parameter
                if let Token::Name(name) = &fs.ls().token {
                    let name = name.clone();
                    fs.ls_mut().next();
                    // Add as RDKVAVAR kind local variable
                    param_names.push(name);
                    n_params += 1;
                    vararg_named = true;
                } else {
                    // Traditional ... without name
                    param_names.push("(vararg table)".to_string());
                    n_params += 1;
                }
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
    new_fs.prev = fs as *mut FuncState;  // like C's fs->prev = ls->fs
    new_fs.proto.num_params = n_params;
    if is_vararg {
        new_fs.proto.flag = PF_VAHID;
        new_fs.code_abc(OpCode::VARARGPREP, 0, 0, 0);
    }

    for (i, name) in param_names.iter().enumerate() {
        if vararg_named && i == param_names.len() - 1 {
            new_fs.add_local_kind(name, 2, RDKVAVAR);
        } else {
            new_fs.add_local(name, 2);
        }
    }

    for (i, local) in fs.locals.iter().enumerate() {
        if local.active && local.kind <= RDKTOCLOSE {
            new_fs.parent_locals.push(ParentVar {
                name: local.name.clone(),
                is_local: true,
                reg: local.reg,
                local_idx: i,
                upval_idx: 0,
            });
        }
    }
    // Inherit grandparent variables as is_local=false.
    // upval_idx will be resolved lazily in find_upvalue.
    for gp_var in fs.parent_locals.iter() {
        new_fs.parent_locals.push(ParentVar {
            name: gp_var.name.clone(),
            is_local: false,
            reg: 0,
            local_idx: 0,
            upval_idx: 0,
        });
    }

    parse_chunk(&mut new_fs);
    expect(&mut new_fs, &Token::End);

    // Like C's markupval: for each upvalue captured by the child,
    // mark the appropriate block as needing CLOSE.
    // For in_stack upvalues: mark the parent's block containing the local variable.
    // For !in_stack upvalues: mark the parent's block (since the parent also has
    // an upvalue that needs closing), and recursively mark ancestor blocks.
    for uv in &new_fs.proto.upvalues {
        if uv.in_stack {
            let local_idx = uv.parent_local_idx;
            let is_active = local_idx < fs.locals.len() && fs.locals[local_idx].active;
            if is_active {
                fs.mark_block_upval(local_idx);
            }
        } else {
            // in_stack=false: the parent also has an upvalue for this variable.
            // We need to mark the parent's block. Since we don't have a specific
            // local_idx, we mark based on the upvalue chain.
            // In C, singlevaraux calls markupval at each level when recursing.
            // The parent has an upvalue, so its block needs to be marked.
            // We use a simple heuristic: mark the current innermost non-function block.
            fs.mark_block_for_upval();
            // Also recursively mark ancestor functions' blocks
            fs.mark_ancestor_blocks_for_upval(uv.idx as usize);
        }
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
        (TValue::Str(a), TValue::Str(b)) => a.as_str() == b.as_str(),
        _ => false,
    }
}