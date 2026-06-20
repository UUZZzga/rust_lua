use crate::objects::*;
use crate::objects::PF_VAHID;
use crate::opcodes::*;
use super::lexer::{LexState, Token};

use crate::objects::Instruction;

const NO_JUMP: i32 = -1;
const LUAI_MAXCCALLS: u32 = 200;  // max recursion depth (like C's LUAI_MAXCCALLS)

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
    /// Upvalue not yet loaded into a register (like C's VUPVAL).
    /// info = upvalue index. GETUPVAL is emitted when the value is needed.
    Upval,
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
    is_global: bool,      // true = GDKREG/GDKCONST variable (global declaration)
    is_ctc: bool,         // true = RDKCTC variable (compile-time constant, not an upvalue)
    is_vararg: bool,      // true = RDKVAVAR variable (named vararg parameter, needs PF_VATAB)
    ctc_kind: Option<ExpKind>,  // constant kind (for is_ctc)
    ctc_info: Option<i64>,      // constant info (for is_ctc)
    ctc_str: Option<String>,    // constant string value (for is_ctc Str)
    // is_local=true:
    reg: i32,             // register in direct parent
    local_idx: usize,     // index in direct parent's locals array
    // is_local=false:
    upval_idx: usize,     // index in direct parent's upvalues array (0 = not yet created)
    // is_local=false && is_parent_upval=true: variable is an upvalue in the direct parent
    // is_local=false && is_parent_upval=false: variable is inherited from a grandparent
    is_parent_upval: bool,
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
    nactvar: i32, // compact active variable count at declaration time (like C's fs->nactvar)
    pidx: i32,  // index into proto.locvars (-1 if no debug info, like C's vd.pidx)
}

/// Result of searching for a variable in parent/grandparent scope.
/// Like C's singlevaraux: VCONST variables are returned as constants,
/// while VLOCAL/VUPVAL variables create upvalues.
enum UpvalueOrCtc {
    Upvalue(i32),
    CtcConst(ExpDesc),
}

struct LabelDesc {
    name: String,
    pc: i32,       // label 位置（跳转目标）
    nactvar: i32,  // label 处的活跃变量计数（对应 C 的 bl->nactvar）
    nlocals: usize, // label 处的 locals 数组长度（等价于 C 的 nactvar 作为紧凑数组索引）
    reglevel: i32, // label 处的寄存器级别（对应 C 的 reglevel(fs, nactvar)）
    line: i32,
}

struct GotoDesc {
    name: String,
    pc: i32,       // JMP 指令的 pc
    line: i32,
    nactvar: i32,  // goto 处的活跃变量计数（对应 C 的 fs->nactvar）
    nlocals: usize, // goto 处的 locals 数组长度（等价于 C 的 nactvar 作为紧凑数组索引）
    reglevel: i32, // goto 处的寄存器级别
    close: bool,   // 是否需要 CLOSE
}

/// Corresponds to C's BlockCnt - tracks block nesting and upvalue flags
#[derive(Clone, Copy)]
struct BlockEntry {
    saved_nlocals: usize,   // locals index at block entry (like C's bl->nactvar in compact array)
    saved_ngotos: usize,    // gotos index at block entry (like C's bl->firstgoto)
    has_upval: bool,        // like C's bl->upval
    is_function_body: bool, // true for the function body block (C's bl->previous==NULL)
    nactvar: i32,           // active variable count at block entry (like C's bl->nactvar)
    reglevel: i32,          // register level at block entry (like C's reglevel(fs, bl->nactvar))
    insidetbc: bool,        // true if inside the scope of a to-be-closed var (inherited)
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
    // 每条指令对应的行号（与 code 数组平行），用于在 finalize 时计算 line_info
    inst_lines: Vec<i32>,
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

    // Like C's mainfunc: register _ENV as upvalue #0 (instack=1, idx=0).
    // In C, _ENV is also a local variable at register 0, but we handle it
    // differently: _ENV is treated as an upvalue in the main function,
    // and global variable access uses GETTABUP/SETTABUP instead of GETFIELD/SETFIELD.
    // This avoids the register offset issue that would occur if _ENV were a local.
    fs.proto.upvalues.push(crate::objects::UpvalDesc {
        name: Some(crate::strings::LuaString::Short(std::sync::Arc::new(
            crate::strings::ShortString { hash: 0, contents: "_ENV".to_string() }
        ))),
        in_stack: true,
        idx: 0,
        parent_local_idx: 0,
    });
    fs.proto.size_upvalues = 1;

    ls.next();
    fs.code_abc(OpCode::VARARGPREP, 0, 0, 0);
    parse_chunk(&mut fs);

    if !fs.errors.is_empty() {
        return Err(fs.errors.join("\n"));
    }

    let mut proto = fs.proto;
    // size/max_stack_size 字段已在 parse_chunk_finish 中设置
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
            inst_lines: Vec::new(),
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

    /// Increment nesting level; error if too deep (like C's enterlevel/luaE_incCstack).
    /// Returns false if nesting level exceeded (caller should bail out).
    fn enterlevel(&mut self) -> bool {
        self.ls_mut().nesting_level += 1;
        if self.ls().nesting_level >= LUAI_MAXCCALLS {
            self.error("C stack overflow");
            return false;
        }
        true
    }

    /// Decrement nesting level (like C's leavelevel)
    fn leavelevel(&mut self) {
        self.ls_mut().nesting_level -= 1;
    }

    /// Check if there are pending errors (for bailing out of recursion)
    fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

// ============================================================================
// Instruction emission
// ============================================================================

/// 行号差值限制 (C: LIMLINEDIFF = 0x80)
const LIMLINEDIFF: i32 = 0x80;
/// 绝对行号标记 (C: ABSLINEINFO = -0x80)
const ABSLINEINFO: i8 = -0x80;
/// 连续指令数上限，超过则插入绝对行号 (C: MAXIWTHABS = 128)
const MAXIWTHABS: i32 = 128;

impl FuncState {
    /// 发射指令到原型代码数组，返回当前 pc 并自增
    fn emit(&mut self, ins: Instruction) -> i32 {
        self.proto.code.push(ins);
        self.inst_lines.push(self.ls().lastline);
        let cur = self.pc;
        self.pc += 1;
        cur
    }

    /// Like C's luaK_fixline: change line information for the last instruction.
    /// In Rust, we just update inst_lines; the final line_info/abs_line_info
    /// computation in parse_chunk_finish will use the corrected value.
    fn fixline(&mut self, line: i32) {
        if let Some(last) = self.inst_lines.last_mut() {
            *last = line;
        }
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
    ///
    /// 模拟 C 版本 `luaK_numberK` 的去重逻辑：
    /// - 对于 `r == 0`，正常去重（C 版本使用 FuncState 指针作为 key，不会碰撞）
    /// - 对于其他值，计算 `k = r * (1 + 2^-52)` 作为 key
    /// - 如果 `k` 是整数值（即 `r >= 2^53` 或 `r` 是 `2^53` 的倍数），不去重，
    ///   直接添加新常量。这是因为整数值的 key 会与整数常量的 key 碰撞。
    /// - 否则，正常去重
    fn float_k(&mut self, f: f64) -> i32 {
        if f == 0.0 {
            // 零：正常去重（C 版本使用 FuncState 指针作为 key，不会碰撞）
            self.const_k(TValue::Float(f))
        } else {
            // 计算 key：k = r * (1 + 2^-52)，等价于 C 版本的 r * (1 + q)
            let q = 2f64.powi(-52);
            let k = f * (1.0 + q);
            // 检查 k 是否为整数值（等价于 C 版本 luaV_flttointeger(k, &ik, F2Ieq)）
            if float_is_integer(k) {
                // k 是整数值，不去重，直接添加新常量
                let idx = self.proto.constants.len() as i32;
                self.proto.constants.push(TValue::Float(f));
                idx
            } else {
                // k 不是整数值，正常去重
                self.const_k(TValue::Float(f))
            }
        }
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

    /// C 的 luaK_checkstack: 检查寄存器栈水平，更新 max_freereg
    /// newstack = freereg + n; max_freereg = max(max_freereg, newstack)
    fn checkstack(&mut self, n: i32) {
        let newstack = self.freereg + n;
        if newstack > self.max_freereg {
            self.max_freereg = newstack;
        }
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

    /// Deactivate locals from index `start` onward (like C's removevars).
    /// 对 kind <= RDKTOCLOSE (varinreg) 的变量设置 proto.loc_vars[pidx].end_pc = pc。
    fn deactivate_locals_range(&mut self, start: usize) {
        let pc = self.pc;
        let end = self.locals.len();
        for i in start..end {
            self.locals[i].active = false;
            let pidx = self.locals[i].pidx;
            let kind = self.locals[i].kind;
            if kind <= RDKTOCLOSE && pidx >= 0 {
                self.proto.loc_vars[pidx as usize].end_pc = pc;
            }
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
        // Like C's luaK_checkstack: update max_freereg when freereg increases
        if new_val > self.max_freereg {
            self.max_freereg = new_val;
        }
    }

    /// 添加局部变量 (VDKREG)，分配寄存器并返回
    fn add_local(&mut self, name: &str, start_pc: i32) -> i32 {
        let reg = self.alloc_reg();
        let vidx = self.locals.len() as i32;
        let nactvar = self.active_nactvar();
        // 注册到 proto.locvars (对应 C 的 registerlocalvar)
        let pidx = self.proto.loc_vars.len() as i32;
        self.proto.loc_vars.push(LocVar {
            varname: Some(crate::strings::LuaString::Short(std::sync::Arc::new(
                crate::strings::ShortString { hash: 0, contents: name.to_string() }
            ))),
            start_pc,
            end_pc: 0,
        });
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
            nactvar,
            pidx,
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
        let nactvar = self.active_nactvar();
        // C: registerlocalvar 只在 adjustlocalvars 中调用，只对 varinreg (kind <= RDKTOCLOSE)
        // 的变量注册到 locvars。global 变量 (kind >= GDKREG) 不注册。
        let pidx = if in_reg {
            let p = self.proto.loc_vars.len() as i32;
            self.proto.loc_vars.push(LocVar {
                varname: Some(crate::strings::LuaString::Short(std::sync::Arc::new(
                    crate::strings::ShortString { hash: 0, contents: name.to_string() }
                ))),
                start_pc,
                end_pc: 0,
            });
            p
        } else {
            -1
        };
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
            nactvar,
            pidx,
        });
        // Like C's marktobeclosed: mark current block as needing CLOSE
        if kind == RDKTOCLOSE {
            if let Some(blk) = self.block_stack.last_mut() {
                blk.has_upval = true;
                blk.insidetbc = true;  // like C's bl->insidetbc = 1
            }
            self.needclose = true;
        }
        reg
    }

    /// 添加指定寄存器的局部变量
    fn add_local_kind_reg(&mut self, name: &str, start_pc: i32, kind: i32, reg: i32) {
        let vidx = self.locals.len() as i32;
        let nactvar = self.active_nactvar();
        // C: registerlocalvar 只在 adjustlocalvars 中调用，只对 varinreg (kind <= RDKTOCLOSE)
        // 的变量注册到 locvars。global 变量 (kind >= GDKREG) 不注册。
        let in_reg = kind <= RDKTOCLOSE;
        let pidx = if in_reg {
            let p = self.proto.loc_vars.len() as i32;
            self.proto.loc_vars.push(LocVar {
                varname: Some(crate::strings::LuaString::Short(std::sync::Arc::new(
                    crate::strings::ShortString { hash: 0, contents: name.to_string() }
                ))),
                start_pc,
                end_pc: 0,
            });
            p
        } else {
            -1
        };
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
            nactvar,
            pidx,
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
                if !lv.active { continue; }
                if lv.name == name {
                    if lv.kind == RDKCTC {
                        let kind = lv.ctc_kind.clone().unwrap();
                        if kind == ExpKind::Str {
                            if let Some(ref s) = lv.ctc_str {
                                found = Some(s.clone());
                            }
                        }
                    }
                    break;  // Stop at first match (shadowing), regardless of kind
                }
            }
            found
        };
        if let Some(s) = ctc_str {
            return Some(ExpDesc::new_str(s));
        }
        for lv in self.locals.iter().rev() {
            if !lv.active { continue; }
            if lv.name == name {
                if lv.kind == RDKCTC {
                    return Some(ExpDesc::new(lv.ctc_kind.clone().unwrap(), lv.ctc_info.unwrap()));
                }
                break;  // Stop at first match (shadowing), regardless of kind
            }
        }
        None
    }

    /// 在父作用域中查找上值，若找到则创建 UpvalDesc 并返回上值索引
    /// 匹配 C 的 searchvar + singlevaraux 逻辑：
    /// - 遇到 GDKREG/GDKCONST 变量时，名字匹配返回 None（全局变量，不是 upvalue）
    /// - 遇到 RDKCTC 变量时，名字匹配返回 CtcConst（编译时常量，不创建 upvalue）
    ///   对应 C 中 singlevaraux 找到 VCONST 时不创建 upvalue 直接返回
    /// - 遇到 RDKREG/RDKCONST/RDKTOCLOSE 变量时，名字匹配则创建 upvalue
    fn find_upvalue(&mut self, name: &str) -> Option<UpvalueOrCtc> {
        for (i, uv) in self.proto.upvalues.iter().enumerate() {
            if let Some(ref n) = uv.name {
                if n.as_str() == name {
                    return Some(UpvalueOrCtc::Upvalue(i as i32));
                }
            }
        }
        // Like C's singlevaraux(fs, n, var, base=1):
        //   searchvar(fs, n)  -> not found (we're looking for an upvalue)
        //   searchupvalue(fs, n) -> not found (checked above)
        //   singlevaraux(fs->prev, n, var, 0):
        //     searchvar(prev, n) -> if found, markupval + VLOCAL
        //     searchupvalue(prev, n) -> if found, VUPVAL
        //     singlevaraux(prev->prev, n, var, 0) -> recurse
        //
        // parent_locals layout (in order):
        //   [0..A)   is_local=true                    -> parent's active locals (searchvar target)
        //   [A..B)   is_local=false, is_parent_upval=true  -> parent's upvalues (searchupvalue target)
        //   [B..)    is_local=false, is_parent_upval=false -> grandparent vars (recurse target)
        //
        // Step 1: search parent's locals (is_local=true), from end to start
        //         (like C's searchvar which searches from nactvar-1 down)
        //         This handles global declarations, CTC constants, and regular locals.
        //         Like C's searchvar, we must handle the interaction between
        //         collective (global *) and named global declarations.
        //         Note: _ENV is special - in C it's a local, but in Rust it's an upvalue.
        //         So _ENV should not be covered by global * declarations; it should
        //         be found in Step 2 (parent's upvalues) instead.
        {
            let mut global_star_active = false;
            for pvar in self.parent_locals.iter().rev() {
                if !pvar.is_local { continue; }
                if pvar.is_global {
                    if pvar.name == "(global *)" {
                        if !global_star_active {
                            global_star_active = true;
                        }
                    } else if pvar.name == name {
                        // named global declaration matches: variable is global, not an upvalue
                        return None;
                    } else {
                        // named global declaration doesn't match:
                        // invalidate any previous global * declaration
                        global_star_active = false;
                    }
                } else if pvar.name == name {
                    // Found a matching non-global variable in parent's locals
                    if pvar.is_ctc {
                        // Compile-time constant: like C's singlevaraux returning VCONST,
                        // don't create an upvalue, return the constant value directly.
                        let exp = if let Some(ref s) = pvar.ctc_str {
                            ExpDesc::new_str(s.clone())
                        } else {
                            ExpDesc::new(pvar.ctc_kind.clone().unwrap(), pvar.ctc_info.unwrap())
                        };
                        return Some(UpvalueOrCtc::CtcConst(exp));
                    }
                    let pvar = pvar.clone();
                    return Some(self.create_upvalue_from_parent_local(&pvar));
                }
            }
            // global * covers this name: variable is global, not an upvalue.
            // But _ENV is special: it's an upvalue in Rust, not covered by global *.
            if global_star_active && name != "_ENV" {
                return None;
            }
        }
        // Step 2: search parent's upvalues (is_local=false, is_parent_upval=true)
        //         (like C's searchupvalue)
        for pvar in self.parent_locals.iter().rev() {
            if !pvar.is_local && pvar.is_parent_upval && pvar.name == name {
                // Found in parent's upvalues: create instack=false upvalue
                let idx = self.proto.upvalues.len() as i32;
                let ls = crate::strings::new_lstr(&crate::strings::StringTable::new(), name);
                self.proto.upvalues.push(crate::objects::UpvalDesc {
                    name: Some(ls),
                    in_stack: false,
                    idx: pvar.upval_idx as u8,
                    parent_local_idx: 0,
                });
                self.proto.size_upvalues = self.proto.upvalues.len() as i32;
                return Some(UpvalueOrCtc::Upvalue(idx));
            }
        }
        // Step 3: search grandparent variables (is_local=false, is_parent_upval=false)
        //         (like C's singlevaraux recursing to grandparent)
        for pvar in self.parent_locals.iter().rev() {
            if !pvar.is_local && !pvar.is_parent_upval && pvar.name == name {
                if pvar.is_global {
                    // Found a matching global declaration: variable is global, not an upvalue
                    return None;
                }
                if pvar.is_ctc {
                    // Found a compile-time constant: like C's singlevaraux returning VCONST,
                    // don't create an upvalue, return the constant value directly.
                    let exp = if let Some(ref s) = pvar.ctc_str {
                        ExpDesc::new_str(s.clone())
                    } else {
                        ExpDesc::new(pvar.ctc_kind.clone().unwrap(), pvar.ctc_info.unwrap())
                    };
                    return Some(UpvalueOrCtc::CtcConst(exp));
                }
                // Variable is inherited from a grandparent function.
                // Need to create an upvalue in the parent first, then reference it.
                let parent_upval_idx = self.find_or_create_parent_upvalue(name);
                if parent_upval_idx == usize::MAX {
                    // Variable is a global declaration in an ancestor: not an upvalue
                    return None;
                }
                let idx = self.proto.upvalues.len() as i32;
                let ls = crate::strings::new_lstr(&crate::strings::StringTable::new(), name);
                self.proto.upvalues.push(crate::objects::UpvalDesc {
                    name: Some(ls),
                    in_stack: false,
                    idx: parent_upval_idx as u8,
                    parent_local_idx: 0,
                });
                self.proto.size_upvalues = self.proto.upvalues.len() as i32;
                return Some(UpvalueOrCtc::Upvalue(idx));
            }
        }
        None
    }

    /// Helper: create an upvalue from a parent local variable (is_local=true).
    /// Like C's newupvalue with VLOCAL: instack=true, idx=ridx.
    fn create_upvalue_from_parent_local(&mut self, pvar: &ParentVar) -> UpvalueOrCtc {
        let idx = self.proto.upvalues.len() as i32;
        let ls = crate::strings::new_lstr(&crate::strings::StringTable::new(), &pvar.name);
        self.proto.upvalues.push(crate::objects::UpvalDesc {
            name: Some(ls),
            in_stack: true,
            idx: pvar.reg as u8,
            parent_local_idx: pvar.local_idx,
        });
        self.proto.size_upvalues = self.proto.upvalues.len() as i32;
        UpvalueOrCtc::Upvalue(idx)
    }

    /// Find or create an upvalue in the direct parent function for a variable
    /// inherited from a grandparent. Returns the upvalue index in the parent.
    /// Returns usize::MAX if the variable is a global declaration or CTC constant (not an upvalue).
    ///
    /// Like C's singlevaraux, searches in order:
    ///   1. parent's locals (searchvar) -> in_stack=true upvalue
    ///   2. parent's upvalues (searchupvalue) -> in_stack=false upvalue
    ///   3. grandparent vars (recurse singlevaraux) -> in_stack=false upvalue
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
        // Step 1: search parent's locals (is_local=true), like C's searchvar.
        // Must handle global declarations correctly.
        {
            let mut global_star_active = false;
            let mut found_local: Option<usize> = None;
            for (j, pvar) in prev.parent_locals.iter().enumerate().rev() {
                if !pvar.is_local { continue; }
                if pvar.is_global {
                    if pvar.name == "(global *)" {
                        if !global_star_active { global_star_active = true; }
                    } else if pvar.name == name {
                        // named global declaration matches: not an upvalue
                        return usize::MAX;
                    } else {
                        global_star_active = false;
                    }
                } else if pvar.name == name {
                    if pvar.is_ctc {
                        return usize::MAX;
                    }
                    found_local = Some(j);
                    break;
                }
            }
            if let Some(j) = found_local {
                let pvar = &prev.parent_locals[j];
                let t = crate::strings::StringTable::new();
                let ls = crate::strings::new_lstr(&t, name);
                let idx = prev.proto.upvalues.len();
                prev.proto.upvalues.push(crate::objects::UpvalDesc {
                    name: Some(ls),
                    in_stack: true,
                    idx: pvar.reg as u8,
                    parent_local_idx: pvar.local_idx,
                });
                prev.proto.size_upvalues = prev.proto.upvalues.len() as i32;
                return idx;
            }
            if global_star_active && name != "_ENV" {
                return usize::MAX;
            }
        }
        // Step 2: search parent's upvalues (is_local=false, is_parent_upval=true)
        for (j, pvar) in prev.parent_locals.iter().enumerate().rev() {
            if pvar.is_local || !pvar.is_parent_upval { continue; }
            if pvar.name == name {
                let t = crate::strings::StringTable::new();
                let ls = crate::strings::new_lstr(&t, name);
                let idx = prev.proto.upvalues.len();
                prev.proto.upvalues.push(crate::objects::UpvalDesc {
                    name: Some(ls),
                    in_stack: false,
                    idx: pvar.upval_idx as u8,
                    parent_local_idx: 0,
                });
                prev.proto.size_upvalues = prev.proto.upvalues.len() as i32;
                return idx;
            }
        }
        // Step 3: search grandparent vars (is_local=false, is_parent_upval=false)
        for (j, pvar) in prev.parent_locals.iter().enumerate().rev() {
            if pvar.is_local || pvar.is_parent_upval { continue; }
            if pvar.name == name {
                if pvar.is_global {
                    return usize::MAX;
                }
                if pvar.is_ctc {
                    return usize::MAX;
                }
                // Variable is inherited from a grandparent function.
                // Recurse to create upvalue in parent first.
                let grandparent_upval_idx = prev.find_or_create_parent_upvalue(name);
                if grandparent_upval_idx == usize::MAX {
                    return usize::MAX;
                }
                let t = crate::strings::StringTable::new();
                let ls = crate::strings::new_lstr(&t, name);
                let idx = prev.proto.upvalues.len();
                prev.proto.upvalues.push(crate::objects::UpvalDesc {
                    name: Some(ls),
                    in_stack: false,
                    idx: grandparent_upval_idx as u8,
                    parent_local_idx: 0,
                });
                prev.proto.size_upvalues = prev.proto.upvalues.len() as i32;
                return idx;
            }
        }
        // Should not happen if the variable exists somewhere in the chain
        0
    }

    /// 查找局部变量并返回 (寄存器号, 种类)
    /// 匹配 C 的 searchvar 逻辑：遇到 GDKREG/GDKCONST 变量时，
    /// 如果名字匹配则不继续搜索（返回 None 表示是全局变量）。
    /// 遇到 global * 时不停止搜索，继续查找 local 变量。
    fn find_local_ex(&self, name: &str) -> Option<(i32, i32)> {
        for lv in self.locals.iter().rev() {
            if !lv.active { continue; }
            if lv.kind >= GDKREG {
                // global declaration: if name matches, stop searching
                // (the variable is global, not local)
                if lv.name == name {
                    return None;  // found named global declaration, not a local
                }
                // global * or non-matching named global: continue searching
            } else if lv.name == name && lv.kind <= RDKTOCLOSE {
                return Some((lv.reg, lv.kind));
            }
        }
        None
    }

    /// 搜索 global 声明，匹配 C 的 searchvar 逻辑。
    /// 返回：
    ///   Some(kind) - 找到匹配的 global 变量（GDKREG 或 GDKCONST）
    ///   None - 没有找到匹配的 global 声明
    ///   如果遇到 global * 但名字不匹配，会设置 global_star_active
    fn find_global_decl(&self, name: &str) -> Option<i32> {
        let mut global_star_active = false;
        for lv in self.locals.iter().rev() {
            if !lv.active { continue; }
            if lv.kind >= GDKREG {
                // global declaration
                if lv.name == "(global *)" {
                    // collective declaration (global *)
                    if !global_star_active {
                        global_star_active = true;
                    }
                } else if lv.name == name {
                    // named global declaration matches
                    return Some(lv.kind);
                } else {
                    // named global declaration doesn't match:
                    // invalidate any previous global * declaration
                    // (C: var->u.info = -2)
                    global_star_active = false;
                }
            } else {
                // non-global variable: like C's searchvar, check if name matches.
                // If it matches, this is a local, not a global (return None).
                // If it doesn't match, continue searching for global declarations.
                if lv.name == name {
                    return None;
                }
            }
        }
        if global_star_active {
            // global * covers this name
            Some(GDKREG)
        } else {
            None
        }
    }

    /// 只查找具名 global 声明（如 `global a`），不包含 collective `global *`。
    /// 匹配 C 的 searchvar：具名 global 匹配时立即返回 VGLOBAL，优先于 upvalue 查找。
    /// 而 `global *` 只记录不返回，upvalue 查找优先。
    fn find_named_global_decl(&self, name: &str) -> Option<i32> {
        for lv in self.locals.iter().rev() {
            if !lv.active { continue; }
            if lv.kind >= GDKREG {
                if lv.name == "(global *)" {
                    // collective declaration: skip (handled by find_global_decl)
                    continue;
                } else if lv.name == name {
                    // named global declaration matches
                    return Some(lv.kind);
                }
                // named global non-match: continue
            } else {
                // non-global variable: if name matches, it's a local, not a global
                if lv.name == name {
                    return None;
                }
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
        let r = match e.kind {
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
                // Like C's dischargevars for VCALL: setoneret converts to VNONRELOC
                // Call's info is the result register (CALL's A field)
                // Like C's luaK_exp2anyreg for VNONRELOC: return info directly
                // if no jumps (don't require it to be the top-of-stack register)
                if !e.has_jumps() {
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
            ExpKind::Vararg => {
                // Like C's setoneret for VVARARG: SETARG_C(pc, 2), then VRELOC
                // info2 stores the VARARG instruction's PC
                let pc = e.info2;
                self.set_c(pc, 2);
                // Now treat as VRELOC: allocate register and set A
                let r = self.alloc_reg();
                self.set_a(pc, r);
                r
            }
            ExpKind::Relocable => {
                if e.info2 >= 0 {
                    // VRELOC mode: info2 holds the PC of the pending instruction.
                    // Like C's luaK_reserveregs + exp2reg: always allocate a new
                    // register and patch the instruction's A field.
                    // (Do NOT try to reuse the freed operand register stored in
                    // info, because it may have been reallocated for something
                    // else between freeexp and this discharge — e.g. the table
                    // register in luaK_indexed for `upval[#upval]`.)
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
            ExpKind::Upval => {
                // Like C's discharge2reg for VUPVAL: emit GETUPVAL
                let r = self.alloc_reg();
                self.code_abc(OpCode::GETUPVAL, r, e.info as i32, 0);
                r
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
        };
        r
    }

    /// Like C's discharge2anyreg: ensure expression value is in a register,
    /// making it NonReloc. Does NOT resolve jump lists.
    /// If already NonReloc, do nothing. Otherwise, allocate a register and
    /// discharge the expression value into it.
    /// For Call, like C's dischargevars(setoneret) + discharge2anyreg:
    /// convert Call to NonReloc first, then return the register.
    fn discharge_to_any_reg(&mut self, e: &ExpDesc) -> i32 {
        if e.kind == ExpKind::NonReloc {
            return e.info as i32;
        }
        if e.kind == ExpKind::Call {
            // Like C's dischargevars for VCALL: setoneret converts to NonReloc
            // Call's info is the function register; result is also in that register
            return e.info as i32;
        }
        // Allocate a register (like C's luaK_reserveregs(fs, 1))
        let reg = self.alloc_reg();
        // discharge2reg: put value into reg
        self.discharge_to_reg(e, reg);
        reg
    }

    /// Like C's discharge2reg: put expression value into a specific register.
    /// Does NOT handle jump lists. After this, expression becomes NonReloc.
    fn discharge_to_reg(&mut self, e: &ExpDesc, reg: i32) {
        match e.kind {
            ExpKind::Nil => {
                self.code_nil(reg, 1);
            }
            ExpKind::Boolean => {
                if e.info != 0 {
                    self.code_abc(OpCode::LOADTRUE, reg, 0, 0);
                } else {
                    self.code_abc(OpCode::LOADFALSE, reg, 0, 0);
                }
            }
            ExpKind::Int => {
                let val = e.info;
                if fits_sbx(val) {
                    self.code_asbx(OpCode::LOADI, reg, val as i32);
                } else {
                    let k = self.int_k(val);
                    self.code_abx(OpCode::LOADK, reg, k);
                }
            }
            ExpKind::Float => {
                let f = f64::from_bits(e.info as u64);
                let fi = f as i64;
                if (fi as f64) == f && fits_sbx(fi) {
                    self.code_asbx(OpCode::LOADF, reg, fi as i32);
                } else {
                    let k = self.float_k(f);
                    self.code_abx(OpCode::LOADK, reg, k);
                }
            }
            ExpKind::Str => {
                let k = self.get_str_k(e);
                self.code_abx(OpCode::LOADK, reg, k);
            }
            ExpKind::Vararg => {
                // Like C's setoneret for VVARARG: SETARG_C(pc, 2), then VRELOC
                let pc = e.info2;
                self.set_c(pc, 2);
                // VRELOC: set A field of the VARARG instruction
                self.set_a(pc, reg);
            }
            ExpKind::Relocable => {
                if e.info2 >= 0 {
                    // VRELOC: set A field of the pending instruction
                    self.set_a(e.info2, reg);
                } else {
                    // Already has a register, MOVE if needed
                    if reg != e.info as i32 {
                        self.code_abc(OpCode::MOVE, reg, e.info as i32, 0);
                    }
                }
            }
            ExpKind::NonReloc => {
                if reg != e.info as i32 {
                    self.code_abc(OpCode::MOVE, reg, e.info as i32, 0);
                }
            }
            ExpKind::Call => {
                if reg != e.info as i32 {
                    self.code_abc(OpCode::MOVE, reg, e.info as i32, 0);
                }
            }
            ExpKind::Upval => {
                // Like C's discharge2reg for VUPVAL: emit GETUPVAL into reg
                self.code_abc(OpCode::GETUPVAL, reg, e.info as i32, 0);
            }
            ExpKind::VJMP => {
                // VJMP has no value to discharge; nothing to do
                return;
            }
            _ => {}
        }
    }

    /// Like C's luaK_exp2anyreg: ensure expression result is in some register.
    /// If NonReloc with no jumps, return existing register.
    /// If NonReloc with jumps and reg >= nvarstack, resolve jumps to that register.
    /// Otherwise, use exp_to_next_reg (allocate new register).
    fn exp_to_reg(&mut self, e: &ExpDesc) -> i32 {
        // Like C's luaK_exp2anyreg: dischargevars first, then check VNONRELOC
        // Call is like VNONRELOC after dischargevars (setoneret converts VCALL to VNONRELOC)
        if e.kind == ExpKind::NonReloc || e.kind == ExpKind::Call {
            let info_reg = e.info as i32;
            if e.t == NO_JUMP && e.f == NO_JUMP {
                // No jumps, already in a register
                return info_reg;
            }
            if info_reg >= self.nvarstack() {
                // Register is not a local, can resolve jumps to it
                self.resolve_jumps(e, info_reg);
                return info_reg;
            }
            // Register is a local with jumps, need to move to new register
            // Like C: fall through to luaK_exp2nextreg
            let r = self.alloc_reg();
            self.code_abc(OpCode::MOVE, r, info_reg, 0);
            self.resolve_jumps(e, r);
            return r;
        }
        // For other kinds, use expr_to_reg + resolve_jumps
        let r = self.expr_to_reg(e);
        self.resolve_jumps(e, r);
        r
    }

    /// Like C's luaK_exp2nextreg: discharge, free, reserve a register, then exp2reg.
    /// Always allocates a new register for the expression result.
    fn exp_to_next_reg(&mut self, e: &ExpDesc) -> i32 {
        // Like C: dischargevars + freeexp + reserveregs(1) + exp2reg
        // For NonReloc locals (info < nvarstack) with jumps, we need a new register.
        // For Call, dischargevars converts to NonReloc first.
        // For other types, expr_to_reg handles allocation.
        match e.kind {
            ExpKind::NonReloc => {
                // freeexp: release register if it's a temp at top of stack
                if (e.info as i32) >= self.nvarstack() && (e.info as i32) == self.freereg - 1 {
                    self.free_reg();
                }
                // reserveregs(1): allocate a new register
                let reg = self.alloc_reg();
                // exp2reg: discharge2reg + patchlistaux
                if reg != e.info as i32 {
                    self.code_abc(OpCode::MOVE, reg, e.info as i32, 0);
                }
                self.resolve_jumps(e, reg);
                reg
            }
            ExpKind::VVARGVAR => {
                // Like C's dischargevars for VVARGVAR: luaK_vapar2local sets PF_VATAB
                // and converts to VLOCAL, then FALLTHROUGH to VLOCAL which converts to
                // VNONRELOC with e.info = e.ridx. Then exp2nextreg allocates a new reg
                // and exp2reg generates MOVE if needed.
                self.proto.flag |= PF_VATAB;
                let src_reg = e.info as i32;
                // reserveregs(1): allocate a new register
                let reg = self.alloc_reg();
                // exp2reg: discharge2reg for VNONRELOC generates MOVE if reg != src_reg
                if reg != src_reg {
                    self.code_abc(OpCode::MOVE, reg, src_reg, 0);
                }
                self.resolve_jumps(e, reg);
                reg
            }
            ExpKind::Call => {
                // Like C's dischargevars for VCALL: setoneret converts to NonReloc
                // Call's info is the function register; result is also in that register
                let call_reg = e.info as i32;
                // freeexp: release the call's register if it's a temp
                if call_reg >= self.nvarstack() && call_reg == self.freereg - 1 {
                    self.free_reg();
                }
                // reserveregs(1): allocate a new register
                let reg = self.alloc_reg();
                // discharge2reg for Call: MOVE if needed
                if reg != call_reg {
                    self.code_abc(OpCode::MOVE, reg, call_reg, 0);
                }
                self.resolve_jumps(e, reg);
                reg
            }
            _ => {
                let r = self.expr_to_reg(e);
                self.resolve_jumps(e, r);
                r
            }
        }
    }

    fn cond_to_reg(&mut self, e: &ExpDesc) -> i32 {
        if matches!(e.kind, ExpKind::Void | ExpKind::Nil) {
            let r = self.alloc_reg();
            self.code_abc(OpCode::LOADFALSE, r, 0, 0);
            r
        } else {
            self.exp_to_reg(e)
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
        let op = get_opcode(i);
        if op != OpCode::TESTSET {
            return false;
        }
        let b = getarg_b(i);
        let old_a = getarg_a(i);
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

    /// Patch all jumps in list to target (like C's luaK_patchlist)
    fn patch_list(&mut self, list: i32, target: i32) {
        if list != NO_JUMP {
            self.patch_list_aux(list, target, NO_REG as i32, target);
        }
    }

    /// Patch all jumps in list to current PC (like C's luaK_patchtohere)
    fn patch_to_here(&mut self, list: i32) {
        self.patch_list(list, self.pc);
    }

    /// 生成 RETURN 指令: 像 C 的 luaK_ret 一样，仅根据 nret 选择 RETURN0/RETURN1/RETURN。
    /// needclose 和 PF_VAHID 的调整由 parse_chunk_finish() 统一处理。
    fn return_stat_gen(&mut self, first: i32, nret: i32) {
        match nret {
            0 => {
                self.code_abc(OpCode::RETURN0, first, 1, 0);
            }
            1 => {
                self.code_abc(OpCode::RETURN1, first, 2, 0);
            }
            _ => {
                self.code_abc(OpCode::RETURN, first, nret + 1, 0);
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

    /// 计算当前活跃变量的数量（等价于 C 的 fs->nactvar）
    /// C 中 nactvar 包含所有变量（包括 GDKREG/GDKCONST/RDKCTC），
    /// 变量离开作用域时通过 removevars 减小。
    /// Rust 中 locals 是稀疏数组，变量只是标记 active=false 而非移除，
    /// 所以 locals.len() 包含 inactive 变量，不能直接用作 nactvar。
    fn active_nactvar(&self) -> i32 {
        self.locals.iter().filter(|l| l.active).count() as i32
    }

    /// 将 saved_nlocals（块入口时的 locals.len()）转换为等价的活跃变量计数
    /// 用于 goto 传播时设置 gt.nactvar（等价于 C 的 gt->nactvar = bl->nactvar）
    /// C 中 bl->nactvar 是块入口时的 fs->nactvar（活跃变量计数，包含所有变量），
    /// Rust 中 saved_nlocals 是块入口时的 locals.len()（包含所有变量）。
    /// 我们需要计算 saved_nlocals 之前有多少活跃变量。
    fn nactvar_up_to(&self, saved_nlocals: usize) -> i32 {
        self.locals[..saved_nlocals].iter().filter(|l| l.active).count() as i32
    }

    /// 给定 locals 数组长度 nlocals，计算对应的寄存器级别
    /// 等价于 C 的 reglevel(fs, nvar)：遍历 locals[0..nlocals]，
    /// 从后往前找第一个在寄存器中的变量（varinreg: kind <= RDKTOCLOSE），返回 reg+1。
    /// 不依赖 active 标志，因为 locals[0..nlocals] 在创建时都是 active 的，
    /// 即使后来被 deactivate，kind 和 reg 不变。
    fn reglevel_for_nlocals(&self, nlocals: usize) -> i32 {
        for i in (0..nlocals).rev() {
            if self.locals[i].kind <= RDKTOCLOSE {
                return self.locals[i].reg + 1;
            }
        }
        0
    }

    /// 给定活跃变量计数 nactvar，计算对应的寄存器级别
    /// 等价于 C 的 reglevel(fs, nvar)：从前 nvar 个变量中，
    /// 从后往前找第一个在寄存器中的变量（varinreg: kind <= RDKTOCLOSE），返回 ridx+1。
    /// C 的 nactvar 包含所有变量（含 GDKREG/GDKCONST），所以这里遍历前 nactvar 个
    /// active 变量（不限 kind），找最后一个 kind <= RDKTOCLOSE 的变量。
    fn reglevel_for_nactvar(&self, nactvar: i32) -> i32 {
        if nactvar == 0 { return 0; }
        // Collect the first nactvar active variables' indices
        let mut count = 0;
        let mut last_in_reg = -1i32;
        for i in 0..self.locals.len() {
            if self.locals[i].active {
                count += 1;
                if self.locals[i].kind <= RDKTOCLOSE {
                    last_in_reg = i as i32;
                }
                if count == nactvar { break; }
            }
        }
        if last_in_reg >= 0 {
            self.locals[last_in_reg as usize].reg + 1
        } else {
            0
        }
    }

    // Like C's markupval: mark the block where the variable at the given
    // locals array index was declared as having upvalues.
    // Uses nactvar (compact active variable count) for block identification,
    // matching C's markupval which compares bl->nactvar with the variable's vidx.
    fn mark_block_upval(&mut self, var_idx: usize) {
        self.needclose = true;
        // Like C's markupval: find the first block where nactvar <= level.
        // C's 'level' is the variable's vidx (nactvar at declaration time).
        // We use the variable's cached nactvar and compare with block's nactvar.
        let var_nactvar = self.locals[var_idx].nactvar;
        for i in (0..self.block_stack.len()).rev() {
            if self.block_stack[i].nactvar <= var_nactvar {
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
                    // Verify that the local variable matches the upvalue name.
                    // In Rust, _ENV is not a local, so parent_local_idx may point
                    // to a different variable. If names don't match, skip marking
                    // this level (the variable is actually an upvalue in the parent).
                    let local_name = prev.locals[local_idx].name.as_str();
                    let uv_name = parent_uv.name.as_ref().map(|s| s.as_str()).unwrap_or("");
                    if local_name == uv_name {
                        prev.mark_block_upval(local_idx);
                    } else {
                        // The parent's upvalue references a variable that is not a local
                        // in the parent (e.g., _ENV). Don't mark any block at this level.
                        // The variable exists at a higher scope, so no CLOSE is needed here.
                    }
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
    // JMP→JMP optimization: like C's luaK_finish, process in-place
    // so that earlier optimizations are visible to later ones
    let code_len = fs.proto.code.len();
    for i in 0..code_len {
        if get_opcode(fs.proto.code[i]) == OpCode::JMP {
            let target = final_target(&fs.proto.code, i as i32);
            fs.fix_jump(i as i32, target, false);
        }
    }

    // Other optimizations
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
        let op = get_opcode(proto.code[i]);  // re-read after possible SET_OPCODE
        match op {
            OpCode::RETURN0 | OpCode::RETURN1 | OpCode::RETURN | OpCode::TAILCALL => {
                if fs.needclose {
                    SETARG_k(&mut proto.code[i], 1);
                }
                if (proto.flag & PF_VAHID) != 0 {
                    SETARG_C(&mut proto.code[i], proto.num_params as i32 + 1);
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

    // 从 inst_lines 计算 line_info 和 abs_line_info
    // (对应 C 的 savelineinfo，在 luaK_code 中每条指令发射时调用)
    // C 的 open_func 中: fs->previousline = f->linedefined
    let mut previousline: i32 = fs.proto.line_defined;
    let mut iwthabs: i32 = 0;
    for (pc, &line) in fs.inst_lines.iter().enumerate() {
        let linedif = line - previousline;
        if linedif.abs() >= LIMLINEDIFF || iwthabs >= MAXIWTHABS {
            fs.proto.abs_line_info.push(crate::objects::AbsLineInfo {
                pc: pc as i32,
                line,
            });
            fs.proto.line_info.push(ABSLINEINFO);
            iwthabs = 1;
        } else {
            fs.proto.line_info.push(linedif as i8);
            iwthabs += 1;
        }
        previousline = line;
    }

    // 设置 Proto 的 size 和 max_stack_size 字段
    // (对应 C 的 close_func 中 luaK_finish 之后的处理)
    fs.proto.max_stack_size = std::cmp::max(2, fs.max_freereg) as u8;
    fs.proto.size_code = fs.proto.code.len() as i32;
    fs.proto.size_k = fs.proto.constants.len() as i32;
    fs.proto.size_p = fs.proto.protos.len() as i32;
    fs.proto.size_upvalues = fs.proto.upvalues.len() as i32;
    fs.proto.size_line_info = fs.proto.line_info.len() as i32;
    fs.proto.size_loc_vars = fs.proto.loc_vars.len() as i32;
    fs.proto.size_abs_line_info = fs.proto.abs_line_info.len() as i32;
}

/// Like C's finaltarget: follow JMP chain to find the final target
fn final_target(code: &[u32], mut i: i32) -> i32 {
    for _ in 0..100 {  // avoid infinite loops
        let inst = code[i as usize];
        if get_opcode(inst) != OpCode::JMP {
            break;
        }
        let sj = getarg_sj(inst);
        i += sj + 1;
    }
    i
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
    // at label creation time). Must use active_nactvar() to get the correct count,
    // not locals.len() which includes inactive variables.
    let mut nactvar = fs.active_nactvar();
    if last {
        // C's createlabel: "assume that locals are already out of scope"
        // Use the block's nactvar (saved_nlocals) instead of current level.
        // Convert saved_nlocals to active variable count.
        if let Some(blk) = fs.block_stack.last() {
            nactvar = fs.nactvar_up_to(blk.saved_nlocals);
        }
    }

    let reglevel = fs.reglevel_for_nactvar(nactvar);
    let nlocals = if last {
        if let Some(blk) = fs.block_stack.last() {
            blk.saved_nlocals
        } else {
            fs.locals.len()
        }
    } else {
        fs.locals.len()
    };
    fs.labels.push(LabelDesc {
        name: name.to_string(),
        pc,
        nactvar,
        nlocals,
        reglevel,
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
                    // C uses reglevel(fs, label->nactvar), but in Rust we can't recompute
                    // it after deactivation (locals array is sparse). Use the saved reglevel
                    // from label creation time, which is equivalent.
                    let stklevel = lb.reglevel;
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
    // C's newgotoentry: nactvar = fs->nactvar (count of active variables)
    // Must use active_nactvar() to match C's behavior.
    let nactvar = fs.active_nactvar();
    let reglevel = fs.reglevel_for_nactvar(nactvar);
    let nlocals = fs.locals.len();
    fs.gotos.push(GotoDesc {
        name: name.clone(),
        pc,
        line,
        nactvar,
        nlocals,
        reglevel,
        close: false,
    });
    // Don't solve goto here - defer to block exit (like C's solvegotos)
    // so that bup (has_upval) is correctly determined
}

/// 块退出时处理 goto：解决当前块中的 goto，清理 labels
fn solve_gotos_for_block(fs: &mut FuncState, saved_nlabels: usize, saved_nlocals: usize, saved_ngotos: usize, needclose: bool, nactvar: i32, outlevel: i32) {
    let mut i = saved_ngotos;
    while i < fs.gotos.len() {
        let gt_name = fs.gotos[i].name.clone();
        // Only resolve against labels that belong to this block (at or after saved_nlabels)
        if let Some(lb_idx) = find_label_from(fs, &gt_name, saved_nlabels) {
            // Found a matching label in this block - solve it
            // In C's solvegotos: if (gt->close) closegoto(...); else patchlist(...)
            // Only gt->close determines whether to apply closegoto
            let gt = fs.gotos.remove(i);
            let mut gt_pc = gt.pc;

            if gt.close || (fs.labels[lb_idx].nactvar < gt.nactvar && needclose) {
                // Like C's closegoto: move jump to CLOSE+1, put CLOSE at original position
                // C uses reglevel(fs, label->nactvar), but in Rust we can't recompute
                // it after deactivation (locals array is sparse). Use the saved reglevel
                // from label creation time, which is equivalent.
                let stklevel = fs.labels[lb_idx].reglevel;
                fs.proto.code[(gt_pc + 1) as usize] = fs.proto.code[gt_pc as usize];
                fs.proto.code[gt_pc as usize] = create_abck(OpCode::CLOSE, stklevel, 0, 0, 0);
                gt_pc += 1;
            }
            let lb = &fs.labels[lb_idx];
            fs.fix_jump(gt_pc, lb.pc, false);
        } else {
            // Unresolved goto: if block has upvalue and goto escapes scope, mark close=true
            // C: if (bl->upval && reglevel(fs, gt->nactvar) > outlevel) gt->close = 1;
            // Use the saved reglevel from goto creation time, as we can't recompute
            // it after deactivation (locals array is sparse).
            let gt_reglevel = fs.gotos[i].reglevel;
            if needclose && gt_reglevel > outlevel {
                fs.gotos[i].close = true;
            }
            // Like C: gt->nactvar = bl->nactvar
            fs.gotos[i].nactvar = nactvar;
            fs.gotos[i].reglevel = outlevel;
            fs.gotos[i].nlocals = saved_nlocals;
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
    let saved_ngotos = fs.gotos.len();
    let entry_nactvar = fs.active_nactvar();
    let entry_reglevel = fs.reglevel_for_nactvar(entry_nactvar);
    fs.block_stack.push(BlockEntry { saved_nlocals, saved_ngotos, has_upval: false, is_function_body: true, nactvar: entry_nactvar, reglevel: entry_reglevel, insidetbc: false });

    let is_last = block_follow(fs, true);
    if !is_last {
        parse_chunk_stmts(fs);
    }
    let nvarstack = fs.nvarstack();
    fs.return_stat_gen(nvarstack, 0);

    // Leave function body block (like C's leaveblock for the function body block).
    // Don't generate CLOSE because this is the function body block (previous=NULL in C).
    let has_upval = fs.current_block_has_upval();
    let has_tbc = fs.locals[saved_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
    let block_entry = fs.block_stack.pop().unwrap();
    // C's leaveblock order: removevars before solvegotos
    fs.deactivate_locals_range(saved_nlocals);
    // Solve gotos for the function body block (like C's solvegotos in leaveblock)
    solve_gotos_for_block(fs, saved_nlabels, saved_nlocals, block_entry.saved_ngotos, has_tbc || has_upval, block_entry.nactvar, block_entry.reglevel);
    // Check for unresolved gotos (like C's leaveblock: if bl->previous==NULL && pending gotos)
    if !fs.gotos.is_empty() {
        // There are unresolved gotos - this is an error
        let gt = &fs.gotos[0];
        fs.errors.push(format!("{}: no visible label '{}' for goto", gt.line, gt.name));
    }

    parse_chunk_finish(fs);
}

/// Like C's block(): enterblock + chunk + leaveblock.
/// Creates a block scope, parses statements, then leaves the block.
fn parse_block(fs: &mut FuncState) {
    let saved_nlocals = fs.locals.len();
    let saved_nlabels = fs.labels.len();
    let saved_ngotos = fs.gotos.len();
    let entry_nactvar = fs.active_nactvar();
    let entry_reglevel = fs.reglevel_for_nactvar(entry_nactvar);
    let parent_insidetbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
    fs.block_stack.push(BlockEntry { saved_nlocals, saved_ngotos, has_upval: false, is_function_body: false, nactvar: entry_nactvar, reglevel: entry_reglevel, insidetbc: parent_insidetbc });

    parse_chunk_stmts(fs);

    // Leave block (like C's leaveblock)
    let has_upval = fs.current_block_has_upval();
    let block_entry = fs.block_stack.pop().unwrap();
    let has_tbc = fs.locals[saved_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
    let close_reg = fs.nvarstack_up_to(saved_nlocals);
    if has_tbc || has_upval {
        fs.code_abc(OpCode::CLOSE, close_reg, 0, 0);
    }
    // Like C's leaveblock order: 1) CLOSE  2) freereg  3) removevars(deactivate)  4) solvegotos
    fs.deactivate_locals_range(saved_nlocals);
    fs.set_freereg(close_reg);
    solve_gotos_for_block(fs, saved_nlabels, saved_nlocals, block_entry.saved_ngotos, has_tbc || has_upval, block_entry.nactvar, block_entry.reglevel);
}

/// Like C's chunk(): parse a sequence of statements without creating a block scope.
fn parse_chunk_stmts(fs: &mut FuncState) {
    while !block_follow(fs, true) && !fs.has_errors() {
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
                // nested label statement (like C's labelstat: while (testnext(ls, TK_DBCOLON)) labelstat(ls, line))
                let line2 = fs.ls().lastline;
                fs.ls_mut().next();  // skip '::'
                let name2 = get_name(fs);
                expect(fs, &Token::ColonColon);  // skip closing '::'
                while check(fs, &Token::Semi) { fs.ls_mut().next(); }
                create_label(fs, &name2, line2, block_follow(fs, false));
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

/// Emit GETTABUP or fallback to GETUPVAL+LOADK+GETTABLE when constant is not a short string
/// Returns the PC of the instruction that produces the result.
fn code_gettabup(fs: &mut FuncState, r: i32, upval: i32, k: i32) -> i32 {
    if is_kstr(fs, k) {
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

/// Emit SETTABUP or fallback to GETUPVAL+LOADK+SETTABLE when constant is not a short string
fn code_settabup(fs: &mut FuncState, upval: i32, k: i32, val: i32) {
    if is_kstr(fs, k) {
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

/// Emit SETTABUP with k-bit or fallback to GETUPVAL+LOADK+SETTABLE when constant is not a short string
fn code_settabup_k(fs: &mut FuncState, upval: i32, k: i32, val: i32, is_k: bool) {
    if is_kstr(fs, k) {
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

/// Check if a constant at index k is a short string that can be used with
/// GETFIELD/SETFIELD/GETTABUP/SETTABUP (matches C's isKstr).
/// GETFIELD/SETFIELD can only encode short string constants in their operand;
/// long strings must be loaded via LOADK + GETTABLE/SETTABLE.
fn is_kstr(fs: &FuncState, k: i32) -> bool {
    if (k as u32) > crate::opcodes::MAXINDEXRK {
        return false;
    }
    if let Some(tv) = fs.proto.constants.get(k as usize) {
        matches!(tv, TValue::Str(crate::strings::LuaString::Short(_)))
    } else {
        false
    }
}

/// Emit GETFIELD or fallback to LOADK+GETTABLE when constant is not a short string
/// Returns the PC of the instruction that produces the result.
fn code_getfield(fs: &mut FuncState, r: i32, table: i32, k: i32) -> i32 {
    if is_kstr(fs, k) {
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

/// Emit SETFIELD or fallback to LOADK+SETTABLE when constant is not a short string
fn code_setfield(fs: &mut FuncState, table: i32, k: i32, val: i32) {
    if is_kstr(fs, k) {
        fs.code_abc_k(OpCode::SETFIELD, table, k, val, false);
    } else {
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        fs.code_abc_k(OpCode::SETTABLE, table, kr, val, false);
        fs.free_reg(); // kr
    }
}

/// Emit SETFIELD with k-bit or fallback to LOADK+SETTABLE when constant is not a short string
fn code_setfield_k(fs: &mut FuncState, table: i32, k: i32, val: i32, is_k: bool) {
    if is_kstr(fs, k) {
        fs.code_abc_k(OpCode::SETFIELD, table, k, val, is_k);
    } else {
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, k);
        fs.code_abc_k(OpCode::SETTABLE, table, kr, val, is_k);
        fs.free_reg(); // kr
    }
}

/// 检查全局变量是否存在: GETTABUP + ERRNNIL
fn checkglobal(fs: &mut FuncState, varname: &str, line: i32) {
    let r = fs.alloc_reg();
    let k = fs.string_k(varname);
    code_gettabup(fs, r, 0, k);
    // Like C's luaK_codecheckglobal: luaK_fixline(fs, line) after exp2anyreg
    fs.fixline(line);
    let k_bx = if k >= crate::opcodes::MAXARG_BX as i32 { 0 } else { k + 1 };
    fs.code_abx(OpCode::ERRNNIL, r, k_bx);
    // Like C's luaK_codecheckglobal: luaK_fixline(fs, line) after ERRNNIL
    fs.fixline(line);
    fs.free_reg();
}

/// ANTLR4: 全局变量声明 — 解析带有 global 前缀的属性变量声明列表
/// C 的 initglobal 递归逻辑：对每个变量从后往前，buildglobal → (表达式) → checkglobal → storevartop
/// 关键：当 key > MAXINDEXRK 时，buildglobal 会在解析表达式之前发射 GETUPVAL + LOADK
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

    // Like C's globalnames: declare variables first (new_varkind),
    // but DON'T activate them yet (nactvar not increased).
    // DON'T add variable names to constant table yet (C only does this
    // in initglobal via buildglobal->codestring, not in globalnames).
    let first_local_idx = fs.locals.len();
    for i in 0..nvars {
        fs.add_local_kind(&names[i], fs.pc, kinds[i]);
    }

    // Mark the newly declared variables as inactive
    for i in first_local_idx..fs.locals.len() {
        fs.locals[i].active = false;
    }

    if has_init {
        fs.ls_mut().next();
        // Like C's initglobal(ls, ..., ls->linenumber): capture the line number
        // of the first expression token (after '=') for checkglobal's fixline.
        let init_line = fs.ls().linenumber;

        // Now add variable names to constant table (like C's buildglobal->codestring)
        let mut var_k_names: Vec<i32> = Vec::new();
        for i in 0..nvars {
            let k = fs.string_k(&names[i]);
            var_k_names.push(k);
        }

        // Like C's initglobal: for each variable, pre-evaluate the table/key
        // BEFORE parsing expressions (matching C's buildglobal + luaK_indexed).
        // When key is a Kstr (index <= MAXINDEXRK), no pre-evaluation is needed
        // because SETTABUP can encode it directly. When key > MAXINDEXRK,
        // we must emit GETUPVAL + LOADK before expressions are parsed.
        // Structure: PreEvalInfo { table_reg, key_reg } for non-Kstr keys.
        struct PreEvalInfo {
            table_reg: i32,  // register holding _ENV (or -1 if not pre-evaluated)
            key_reg: i32,    // register holding key constant (or -1)
        }
        let mut pre_evals: Vec<PreEvalInfo> = Vec::new();
        // 检查 _ENV 是否是 VVARGVAR（命名 vararg 参数）
        let env_local_ex = fs.find_local_ex("_ENV");
        let env_is_vvargvar = env_local_ex.map(|(_, kind)| kind == RDKVAVAR).unwrap_or(false);
        let env_reg = env_local_ex.map(|(reg, _)| reg).unwrap_or(-1);
        for i in 0..nvars {
            if !is_kstr(fs, var_k_names[i]) {
                // Key is not a short string constant (index > MAXINDEXRK)
                // Pre-emit GETUPVAL + LOADK, matching C's luaK_indexed behavior
                let table_reg = fs.alloc_reg();
                fs.code_abc(OpCode::GETUPVAL, table_reg, 0, 0);
                let key_reg = fs.alloc_reg();
                fs.code_abx(OpCode::LOADK, key_reg, var_k_names[i]);
                pre_evals.push(PreEvalInfo { table_reg, key_reg });
            } else if env_is_vvargvar {
                // _ENV 是 VVARGVAR：预评估 key（LOADK），匹配 C 的 buildglobal
                // （C 的 luaK_indexed 对 VVARGVAR 总是将 key 加载到寄存器）
                let key_reg = fs.alloc_reg();
                fs.code_abx(OpCode::LOADK, key_reg, var_k_names[i]);
                pre_evals.push(PreEvalInfo { table_reg: env_reg, key_reg });
            } else {
                pre_evals.push(PreEvalInfo { table_reg: -1, key_reg: -1 });
            }
        }

        // Now parse expressions
        let mut last_exp = ExpDesc::new(ExpKind::Void, 0);
        let mut nexps = 0;
        loop {
            let ei = parse_expr(fs);
            nexps += 1;
            if check(fs, &Token::Comma) {
                fs.exp_to_next_reg(&ei.exp);
                fs.ls_mut().next();
            } else {
                last_exp = ei.exp;
                break;
            }
        }

        // Like C's adjust_assign
        let needed = nvars as i32 - nexps as i32;
        // C: luaK_checkstack(fs, needed) in adjust_assign
        fs.checkstack(needed);
        if last_exp.kind == ExpKind::Vararg || last_exp.kind == ExpKind::Call {
            let extra = if needed + 1 > 0 { needed + 1 } else { 0 };
            if last_exp.kind == ExpKind::Call {
                let call_pc = last_exp.info2;
                if call_pc >= 0 {
                    let c_val = extra + 1;
                    fs.set_c(call_pc, c_val);
                }
            } else {
                // Vararg: like C's luaK_setreturns for VVARARG
                // SETARG_C(*pc, nresults + 1); SETARG_A(*pc, fs->freereg); reserveregs(1)
                let pc = last_exp.info2;
                let c_val = extra + 1;
                fs.set_c(pc, c_val);
                let r = fs.alloc_reg();
                fs.set_a(pc, r);
            }
        } else {
            if nexps > 0 {
                fs.exp_to_next_reg(&last_exp);
            }
            if needed > 0 {
                fs.code_abc(OpCode::LOADNIL, fs.freereg, needed - 1, 0);
            }
        }
        if needed > 0 {
            for _ in 0..needed {
                fs.alloc_reg();
            }
        } else {
            fs.freereg = (fs.freereg as i32 + needed) as i32;
        }

        // Like C's initglobal unwind: for each variable from last to first,
        // checkglobal then storevartop
        for i in (0..nvars).rev() {
            let pe = &pre_evals[i];
            if pe.table_reg >= 0 && !env_is_vvargvar {
                // Key was pre-evaluated (not VVARGVAR): checkglobal re-loads _ENV + key (like C's
                // checkglobal which calls buildglobal again), then storevartop uses
                // the pre-evaluated registers for SETTABLE.
                // checkglobal: GETUPVAL + LOADK + GETTABLE + ERRNNIL
                let cr = fs.alloc_reg();
                fs.code_abc(OpCode::GETUPVAL, cr, 0, 0);
                let ckr = fs.alloc_reg();
                fs.code_abx(OpCode::LOADK, ckr, var_k_names[i]);
                fs.code_abc(OpCode::GETTABLE, cr, cr, ckr);
                // Like C's luaK_codecheckglobal: luaK_fixline(fs, line) after exp2anyreg
                fs.fixline(init_line);
                let k_bx = if var_k_names[i] >= crate::opcodes::MAXARG_BX as i32 { 0 } else { var_k_names[i] + 1 };
                fs.code_abx(OpCode::ERRNNIL, cr, k_bx);
                // Like C's luaK_codecheckglobal: luaK_fixline(fs, line) after ERRNNIL
                fs.fixline(init_line);
                fs.free_reg(); // ckr
                fs.free_reg(); // cr
                // storevartop: SETTABLE table_reg key_reg val_reg
                let val_reg = fs.freereg - 1;
                fs.code_abc_k(OpCode::SETTABLE, pe.table_reg, pe.key_reg, val_reg, false);
                fs.free_reg(); // val_reg
                fs.free_reg(); // key_reg
                fs.free_reg(); // table_reg
            } else if env_is_vvargvar {
                // _ENV 是 VVARGVAR：checkglobal 使用 LOADK + GETVARG + ERRNNIL
                // （GETVARG 会被 luaK_finish 转为 GETTABLE，因为有 PF_VATAB）
                // storevartop 使用 SETTABLE + PF_VATAB
                let kr = fs.alloc_reg();
                fs.code_abx(OpCode::LOADK, kr, var_k_names[i]);
                // Free key register (like C's freeregs in VVARGIND discharge)
                if kr >= fs.nvarstack() && kr == fs.freereg - 1 {
                    fs.free_reg();
                }
                // Generate GETVARG with A=0 (relocatable), like C compiler
                let pc = fs.code_abc(OpCode::GETVARG, 0, env_reg, kr);
                // Allocate result register (reuses kr)
                let cr = fs.alloc_reg();
                fs.set_a(pc, cr);
                // Like C's luaK_codecheckglobal: luaK_fixline(fs, line) after exp2anyreg
                fs.fixline(init_line);
                let k_bx = if var_k_names[i] >= crate::opcodes::MAXARG_BX as i32 { 0 } else { var_k_names[i] + 1 };
                fs.code_abx(OpCode::ERRNNIL, cr, k_bx);
                // Like C's luaK_codecheckglobal: luaK_fixline(fs, line) after ERRNNIL
                fs.fixline(init_line);
                fs.free_reg(); // cr
                // storevartop: SETTABLE + PF_VATAB (like C's needvatab for VVARGIND)
                fs.proto.flag |= PF_VATAB;
                let val_reg = fs.freereg - 1;
                fs.code_abc_k(OpCode::SETTABLE, env_reg, pe.key_reg, val_reg, false);
                fs.free_reg(); // val_reg
                fs.free_reg(); // key_reg
            } else {
                // Key is a Kstr: use the simpler checkglobal + code_settabup path
                checkglobal(fs, &names[i], init_line);
                let val_reg = fs.freereg - 1;
                code_settabup(fs, 0, var_k_names[i], val_reg);
                fs.free_reg();
            }
        }
    }

    // Like C's globalnames: now activate the declaration
    for i in first_local_idx..fs.locals.len() {
        fs.locals[i].active = true;
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
/// C 顺序: buildglobal → body → checkglobal → storevar
fn globalfunc(fs: &mut FuncState, line: i32) {
    let fname = get_name(fs);
    fs.add_local_kind(&fname, fs.pc, GDKREG);
    let k = fs.string_k(&fname);

    // Like C's buildglobal: if key is not a Kstr, pre-evaluate GETUPVAL + LOADK
    // before parse_body, matching C's luaK_indexed behavior
    let mut pre_eval: Option<(i32, i32)> = None;  // (table_reg, key_reg)
    if !is_kstr(fs, k) {
        let table_reg = fs.alloc_reg();
        fs.code_abc(OpCode::GETUPVAL, table_reg, 0, 0);
        let key_reg = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, key_reg, k);
        pre_eval = Some((table_reg, key_reg));
    }

    let r = parse_body(fs, None);

    // C order: checkglobal before storevar
    if let Some((table_reg, key_reg)) = pre_eval {
        // checkglobal: re-load _ENV + key (like C's checkglobal which calls buildglobal again)
        let cr = fs.alloc_reg();
        fs.code_abc(OpCode::GETUPVAL, cr, 0, 0);
        let ckr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, ckr, k);
        fs.code_abc(OpCode::GETTABLE, cr, cr, ckr);
        let k_bx = if k >= crate::opcodes::MAXARG_BX as i32 { 0 } else { k + 1 };
        fs.code_abx(OpCode::ERRNNIL, cr, k_bx);
        fs.free_reg(); // ckr
        fs.free_reg(); // cr
        // storevar: SETTABLE table_reg key_reg r
        fs.code_abc_k(OpCode::SETTABLE, table_reg, key_reg, r, false);
        fs.free_reg(); // r
        fs.free_reg(); // key_reg
        fs.free_reg(); // table_reg
    } else {
        checkglobal(fs, &fname, line);
        code_settabup(fs, 0, k, r);
        fs.free_reg();
    }
    // Like C's globalfunc: luaK_fixline(fs, line) after storevar
    fs.fixline(line);
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
    if !fs.enterlevel() {
        return;
    }
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
            let nactvar = fs.active_nactvar();  // like C's fs->nactvar (active variable count)
            let reglevel = fs.reglevel_for_nactvar(nactvar);
            let nlocals = fs.locals.len();
            fs.gotos.push(GotoDesc {
                name: "break".to_string(),
                pc,
                line: fs.ls().lastline,
                nactvar,
                nlocals,
                reglevel,
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
            let _r = fs.exp_to_reg(&ei.exp);
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
    fs.leavelevel();
}

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
                if u.is_upvalue && u.table_reg == Some(uidx) {
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
                if u.is_upvalue && u.table_reg == Some(uidx) {
                    u.table_reg = Some(saved_reg);
                    u.is_upvalue = false;
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

                // Handle VVARGVAR: like C's luaK_indexed, create VVARGIND-like PrefixResult
                // (delay GETVARG generation until read; assignment uses SETTABLE with PF_VATAB)
                // C's luaK_indexed always puts key in a register for VVARGVAR (luaK_exp2anyreg),
                // so we always LOADK, never use SETFIELD/GETFIELD with constant index.
                if first.is_vvargvar {
                    let base_reg = first.reg.unwrap();
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);
                    first = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                        table_reg: Some(base_reg), table_key: Some(kr), table_key_is_const: false, table_key_is_int: false,
                        key_allocated_reg: true,
                        allocated_reg: false,
                        is_upvalue: false, upval_idx: None, env_gettabup_pc: -1,
                        has_call: false, call_pc: -1, is_vvargvar: true, is_readonly: false,
                    };
                    continue;
                }

                let is_short_str = field.len() <= crate::strings::LUAI_MAXSHORTLEN && (k as u32) <= crate::opcodes::MAXINDEXRK;
                // Check if we can revert a GETUPVAL and use SETTABUP/GETTABUP instead.
                // Only attempt revert if the last instruction is actually a GETUPVAL
                // generated for this result (i.e., result was just loaded from an upvalue
                // and not yet indexed). This matches C's VINDEXUP optimization where
                // _ENV (as upvalue) indexed by a short string stays as VINDEXUP.
                let can_revert_getupval = first.reg.is_some() && first.upval_idx.is_some() && is_short_str
                    && !first.is_upvalue && first.table_reg.is_none()
                    && first.allocated_reg  // must be a register we allocated
                    && (fs.pc > 0)  // safety check
                    && {
                        let last_inst = fs.proto.code[(fs.pc - 1) as usize];
                        get_opcode(last_inst) == OpCode::GETUPVAL
                            && getarg_a(last_inst) == first.reg.unwrap()
                    };
                let (base_reg, gettabup_pc) = if can_revert_getupval {
                    // Revert: remove the GETUPVAL instruction, free the register
                    let getupval_pc = fs.pc - 1;
                    fs.proto.code.remove(getupval_pc as usize);
                    fs.inst_lines.remove(getupval_pc as usize);
                    fs.pc -= 1;
                    fs.free_reg();
                    let uv_idx = first.upval_idx.unwrap();
                    (uv_idx, -1)  // Use upvalue index as base_reg (for SETTABUP/GETTABUP)
                } else if let Some(r) = first.reg {
                    (r, -1)
                } else if first.is_upvalue {
                    if !is_short_str {
                        // Key exceeds MAXINDEXRK: must load upvalue into a register
                        let r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETUPVAL, r, first.upval_idx.unwrap_or(0), 0);
                        (r, -1)
                    } else {
                        // Defer: SETTABUP/GETTABUP can be used directly
                        (first.upval_idx.unwrap_or(0), -1)
                    }
                } else {
                    let r = fs.alloc_reg();
                    let pc = if let Some(key) = first.key {
                        code_gettabup(fs, r, first.upval_idx.unwrap_or(0), key);
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
                let new_is_upvalue = (first.is_upvalue || can_revert_getupval) && is_short_str;
                first = PrefixResult {
                    var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                    table_reg: Some(base_reg), table_key: Some(table_key), table_key_is_const: table_key_is_const, table_key_is_int: false,
                    key_allocated_reg: key_allocated_reg,
                    allocated_reg: if new_is_upvalue { false } else { first.allocated_reg || first.reg.is_none() || (first.is_upvalue && !is_short_str) },
                    is_upvalue: new_is_upvalue,
                    upval_idx: first.upval_idx,
                    env_gettabup_pc: if new_is_upvalue { -1 } else { if gettabup_pc >= 0 { gettabup_pc } else { first.env_gettabup_pc } },
                    has_call: false, call_pc: -1, is_vvargvar: false, is_readonly: false,
                };
            }
            Token::LBracket => {
                fs.ls_mut().next();

                // Handle VVARGVAR: like C's luaK_indexed, create VVARGIND-like PrefixResult
                // (delay GETVARG generation until read; assignment uses SETTABLE with PF_VATAB)
                if first.is_vvargvar {
                    let base_reg = first.reg.unwrap();
                    let ei = parse_expr(fs);
                    expect(fs, &Token::RBracket);
                    // Like C's luaK_exp2anyreg: if key is already in a local register, use it directly
                    let key_reg = if ei.exp.kind == ExpKind::NonReloc && (ei.exp.info as i32) < fs.nvarstack() {
                        ei.exp.info as i32
                    } else {
                        fs.exp_to_next_reg(&ei.exp)
                    };
                    first = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                        table_reg: Some(base_reg), table_key: Some(key_reg), table_key_is_const: false, table_key_is_int: false,
                        key_allocated_reg: false, allocated_reg: false,
                        is_upvalue: false, upval_idx: None, env_gettabup_pc: -1,
                        has_call: false, call_pc: -1, is_vvargvar: true, is_readonly: false,
                    };
                    continue;
                }

                // For upvalue tables: match C's suffixedexp '[' + luaK_indexed behavior.
                // C compiler flow: yindex(expr + luaK_exp2val) → luaK_indexed
                // luaK_exp2val emits code for comparisons (LFALSESKIP+LOADTRUE) but NOT for
                // simple expressions (VTRUE). Then luaK_indexed emits GETUPVAL, and then
                // luaK_exp2anyreg for the key emits LOADTRUE for simple expressions.
                // So the order depends on key type:
                // - Comparison key: key load code → GETUPVAL (luaK_exp2val emits first)
                // - Simple key (true, nil, etc.): GETUPVAL → key load code (luaK_exp2anyreg emits after)
                // - Short string key: GETTABUP/SETTABUP (no GETUPVAL needed)
                //
                // Key insight: C's luaK_indexed for VUPVAL + non-Kstr key does:
                //   1. luaK_exp2anyreg(t) - allocate register for table FIRST
                //   2. luaK_exp2anyreg(k) - allocate register for key SECOND
                // But yindex already generated key's instruction (VRELOC, A=0 placeholder).
                // So instruction order is: key's instruction, GETUPVAL table.
                // But register allocation order is: table first, key second.
                // This means GETUPVAL table gets the lower register, key gets the higher.
                let is_upvalue_table = first.is_upvalue && first.reg.is_none();
                let saved_upval_idx = first.upval_idx.unwrap_or(0);
                let (base_reg, gettabup_pc) = if let Some(r) = first.reg {
                    (r, -1)
                } else if is_upvalue_table {
                    (-1, -1)  // placeholder: will emit GETUPVAL at the right time
                } else {
                    let r = fs.alloc_reg();
                    let pc = if let Some(key) = first.key {
                        code_gettabup(fs, r, first.upval_idx.unwrap_or(0), key);
                        fs.pc - 1
                    } else {
                        -1
                    };
                    (r, pc)
                };
                let saved_freereg_before = fs.freereg;
                let ei = parse_expr(fs);
                expect(fs, &Token::RBracket);
                // Check if key expression has jumps (like a comparison).
                // In C, luaK_exp2val emits code for comparisons but not for simple exprs.
                let key_has_jumps = ei.exp.has_jumps();
                // For upvalue tables with non-constant, non-simple keys that have jumps:
                // emit key load code first (like C's luaK_exp2val), then GETUPVAL
                // Note: Upval key is handled separately (upval_key_placeholder_pc),
                // because C's luaK_exp2val discharges VUPVAL to VRELOC (GETUPVAL key)
                // before luaK_indexed emits GETUPVAL table.
                let getupval_emitted_before_key = is_upvalue_table && !key_has_jumps
                    && !matches!(ei.exp.kind, ExpKind::Str | ExpKind::Int)
                    && !matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc)
                    && ei.exp.kind != ExpKind::Upval;
                // Save table register when GETUPVAL is emitted so we can use it later
                // (key may have been allocated BEFORE the table, e.g. Call expressions)
                let getupval_table_reg = if getupval_emitted_before_key {
                    // Simple expression (VTRUE, etc.): emit GETUPVAL first (like C's luaK_indexed)
                    let r = fs.alloc_reg();
                    fs.code_abc(OpCode::GETUPVAL, r, saved_upval_idx, 0);
                    Some(r)
                } else {
                    None
                };
                // For upvalue table + Upval key (like a[i]): C generates GETUPVAL key first
                // (VRELOC, A=0 placeholder in yindex's luaK_exp2val), then GETUPVAL table
                // (in luaK_indexed's luaK_exp2anyreg), then patches key's A.
                // We need to emit GETUPVAL key now (placeholder), emit GETUPVAL table, then patch.
                let upval_key_placeholder_pc = if is_upvalue_table && !getupval_emitted_before_key
                    && ei.exp.kind == ExpKind::Upval && !key_has_jumps
                {
                    // Emit GETUPVAL key with A=0 placeholder (will be patched later)
                    let pc = fs.code_abc(OpCode::GETUPVAL, 0, ei.exp.info as i32, 0);
                    Some(pc)
                } else {
                    None
                };
                let (kr, key_is_const, key_is_int) = if ei.exp.kind == ExpKind::Str {
                    let k = fs.get_str_k(&ei.exp);
                    if let TValue::Str(crate::strings::LuaString::Short(_)) = fs.proto.constants[k as usize] {
                        if (k as u32) <= crate::opcodes::MAXINDEXRK {
                            (k, true, false)
                        } else {
                            // Long string key (index > MAXINDEXRK): for upvalue tables,
                            // defer LOADK until after table's GETUPVAL (matching C's luaK_indexed:
                            // luaK_exp2anyreg(t) first, then luaK_exp2anyreg(k)).
                            if is_upvalue_table && !getupval_emitted_before_key && !key_has_jumps {
                                (-1, false, false)
                            } else {
                                let kr = fs.alloc_reg();
                                fs.code_abx(OpCode::LOADK, kr, k);
                                (kr, false, false)
                            }
                        }
                    } else {
                        // Long string (not Short): for upvalue tables, defer LOADK until after
                        // table's GETUPVAL (matching C's luaK_indexed order).
                        if is_upvalue_table && !getupval_emitted_before_key && !key_has_jumps {
                            (-1, false, false)
                        } else {
                            let kr = fs.alloc_reg();
                            fs.code_abx(OpCode::LOADK, kr, k);
                            (kr, false, false)
                        }
                    }
                } else if ei.exp.kind == ExpKind::Int
                    && ei.exp.info >= 0
                    && ei.exp.info <= ((1u32 << SIZE_C) - 1) as i64
                {
                    (ei.exp.info as i32, true, true)
                } else if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() && ei.exp.info2 < 0 {
                    (ei.exp.info as i32, false, false)
                } else if is_upvalue_table && upval_key_placeholder_pc.is_none()
                    && !getupval_emitted_before_key
                    && matches!(ei.exp.kind, ExpKind::Upval | ExpKind::Relocable)
                    && !key_has_jumps
                {
                    // For upvalue table + Upval/Relocable key: defer key register allocation
                    // until after table's GETUPVAL is emitted (matching C's luaK_indexed order).
                    // Return placeholder; will be resolved after table GETUPVAL.
                    (-1, false, false)
                } else {
                    (fs.exp_to_reg(&ei.exp), false, false)
                };
                let key_allocated = !key_is_const && kr != -1 && fs.freereg > saved_freereg_before;
                // For upvalue tables: emit GETUPVAL at the right time based on key type
                let (base_reg, new_is_upvalue, allocated_reg) = if is_upvalue_table {
                    let can_use_settabup = key_is_const && !key_is_int
                        && (kr as u32) <= crate::opcodes::MAXINDEXRK;
                    if can_use_settabup {
                        // Short string key: use GETTABUP/SETTABUP directly (C's VINDEXUP)
                        // If we emitted GETUPVAL before key load, revert it
                        if getupval_emitted_before_key {
                            fs.proto.code.remove(fs.pc as usize - 1);
                            fs.inst_lines.remove(fs.pc as usize - 1);
                            fs.pc -= 1;
                            fs.free_reg();
                        }
                        (saved_upval_idx, true, false)
                    } else if getupval_emitted_before_key {
                        // GETUPVAL already emitted before key load code.
                        // Use the saved table register (key may have been allocated
                        // BEFORE the table, e.g. Call expressions like t[a()])
                        (getupval_table_reg.unwrap(), false, true)
                    } else {
                        // Comparison or constant key: emit GETUPVAL after key load code
                        // (matching C's luaK_indexed: luaK_exp2anyreg(t) allocates table reg FIRST)
                        let r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETUPVAL, r, saved_upval_idx, 0);
                        (r, false, true)
                    }
                } else {
                    (base_reg, first.is_upvalue, first.allocated_reg || first.reg.is_none())
                };
                // Now resolve deferred key register allocation (Upval/Relocable/Str key for upvalue table)
                let (kr, key_allocated) = if kr == -1 && is_upvalue_table {
                    // Key register allocation was deferred: now allocate after table's GETUPVAL
                    if let Some(pc) = upval_key_placeholder_pc {
                        // Upval key: patch the placeholder GETUPVAL's A with new register
                        let key_r = fs.alloc_reg();
                        fs.set_a(pc, key_r);
                        (key_r, true)
                    } else if ei.exp.kind == ExpKind::Str {
                        // Long string key: emit LOADK now (after table's GETUPVAL)
                        let k = fs.get_str_k(&ei.exp);
                        let key_r = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, key_r, k);
                        (key_r, true)
                    } else {
                        // Relocable key (e.g., i+j): patch the VRELOC instruction's A
                        let key_r = fs.exp_to_reg(&ei.exp);
                        (key_r, true)
                    }
                } else {
                    (kr, key_allocated)
                };
                first = PrefixResult {
                    var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                    table_reg: Some(base_reg), table_key: Some(kr), table_key_is_const: key_is_const, table_key_is_int: key_is_int,
                    key_allocated_reg: key_allocated,
                    allocated_reg: allocated_reg,
                    is_upvalue: new_is_upvalue,
                    upval_idx: first.upval_idx,
                    env_gettabup_pc: if new_is_upvalue { -1 } else { if gettabup_pc >= 0 { gettabup_pc } else { first.env_gettabup_pc } },
                    has_call: false, call_pc: -1, is_vvargvar: false, is_readonly: false,
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
                if v.key.is_some() && !v.is_upvalue {
                    let k_name = fs.string_k(name);
                    if (k_name as u32) > crate::opcodes::MAXINDEXRK {
                        let r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETUPVAL, r, v.upval_idx.unwrap_or(0), 0);
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
                        v.is_upvalue = false;
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
                    fs.exp_to_reg(&ei.exp)
                };
                exps.push(ExpDesc::new(ExpKind::NonReloc, r as i64));
                fs.ls_mut().next();
            } else {
                exps.push(ei.exp);
                break;
            }
        }

        // C: luaK_checkstack(fs, needed) in adjust_assign; needed = nvars - nexps
        let needed_assign = vars.len() as i32 - exps.len() as i32;
        fs.checkstack(needed_assign);

        let last_is_call = exps.last().map_or(false, |e| e.kind == ExpKind::Call && e.info2 >= 0);
        let extra_vars = if vars.len() > exps.len() {
            vars.len() - exps.len()
        } else {
            0
        };
        let (last_exp_reg, nil_reg_start) = if extra_vars > 0 && !last_is_call {
            let last_exp = exps.last().unwrap();
            let exp_reg = fs.exp_to_reg(last_exp);
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
                    // VVARGVAR assignment: like C's luaK_storevar for VVARGIND,
                    // set PF_VATAB then fall through to SETTABLE
                    if v.is_vvargvar {
                        fs.proto.flag |= PF_VATAB;
                    }
                    let can_settabup = v.is_upvalue && v.table_key_is_const && !v.table_key_is_int;
                    if can_settabup {
                        let upval_idx = v.upval_idx.unwrap_or(0);
                        let gettabup_pc = v.env_gettabup_pc;
                        let (env_k, adjusted_key) = if gettabup_pc >= 0 && (gettabup_pc as usize) < fs.proto.code.len() {
                            let gettabup_inst = fs.proto.code.remove(gettabup_pc as usize);
                            fs.inst_lines.remove(gettabup_pc as usize);
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
                            code_settabup_k(fs, upval_idx, adjusted_key, k_val, true);
                        } else {
                            let val_reg = if use_last_reg {
                                last_exp_reg.unwrap()
                            } else {
                                fs.exp_to_reg(val)
                            };
                            code_settabup(fs, upval_idx, adjusted_key, val_reg);
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
                } else if !v.is_upvalue && v.key.is_some() && v.upval_idx.is_some() {
                    // Global variable assignment: use SETTABUP (_ENV[key] = val)
                    let uv_idx = v.upval_idx.unwrap();
                    let k_name = v.key.unwrap();
                    let use_last_reg = i == exps.len() - 1 && last_exp_reg.is_some();
                    let k_opt = if use_last_reg { None } else { exp_to_k(fs, val) };
                    if let Some(k_val) = k_opt {
                        code_settabup_k(fs, uv_idx, k_name, k_val, true);
                    } else {
                        let val_reg = if use_last_reg {
                            last_exp_reg.unwrap()
                        } else {
                            fs.exp_to_reg(val)
                        };
                        code_settabup(fs, uv_idx, k_name, val_reg);
                        if val_reg >= fs.nvarstack() {
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
                        code_settabup_k(fs, v.upval_idx.unwrap_or(0), k_name, k_val, true);
                    } else {
                        let val_reg = if use_last_reg {
                            last_exp_reg.unwrap()
                        } else {
                            fs.exp_to_reg(val)
                        };
                        code_settabup(fs, v.upval_idx.unwrap_or(0), k_name, val_reg);
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
                    // VVARGVAR assignment: like C's luaK_storevar for VVARGIND,
                    // set PF_VATAB then fall through to SETTABLE
                    if v.is_vvargvar {
                        fs.proto.flag |= PF_VATAB;
                    }
                    let can_settabup = v.is_upvalue && v.table_key_is_const && !v.table_key_is_int;
                    if can_settabup {
                        code_settabup(fs, v.upval_idx.unwrap_or(0), table_key, result_reg);
                    } else if v.table_key_is_const {
                        code_setfield(fs, table_reg, table_key, result_reg);
                    } else {
                        fs.code_abc_k(OpCode::SETTABLE, table_reg, table_key, result_reg, false);
                    }
                } else if !v.is_upvalue && v.key.is_some() && v.upval_idx.is_some() {
                    // Global variable assignment: use SETTABUP
                    code_settabup(fs, v.upval_idx.unwrap(), v.key.unwrap(), result_reg);
                } else if let Some(upval_idx) = v.upval_idx {
                    fs.code_abc(OpCode::SETUPVAL, result_reg, upval_idx, 0);
                } else if let Some(ref name) = v.var_name {
                    let k_name = fs.string_k(name);
                    code_settabup(fs, v.upval_idx.unwrap_or(0), k_name, result_reg);
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
            // Like C's exp2reg: discharge2reg + patchlistaux (resolve jumps)
            if e.has_jumps() {
                // Expression has jumps - must resolve them to target dest
                if e.info2 >= 0 {
                    if e.kind == ExpKind::Call {
                        if e.info as i32 != dest {
                            fs.code_abc(OpCode::MOVE, dest, e.info as i32, 0);
                            fs.free_reg();
                        }
                        fs.resolve_jumps(e, dest);
                    } else {
                        let prev_dest = e.info as i32;
                        fs.set_a(e.info2, dest);
                        if prev_dest != dest && (prev_dest >= fs.nvarstack() || prev_dest == fs.freereg - 1) {
                            fs.free_reg();
                        }
                        fs.resolve_jumps(e, dest);
                    }
                } else {
                    // info2 < 0: expression is already in a register (like C's VNONRELOC).
                    // This covers NonReloc, Call (already discharged), and Relocable with
                    // info2 < 0 (e.g., CLOSURE result that was directly allocated to a reg).
                    // Like C's exp2reg for VNONRELOC: MOVE if needed, then patchlistaux.
                    let val_reg = e.info as i32;
                    if dest != val_reg {
                        fs.code_abc(OpCode::MOVE, dest, val_reg, 0);
                    }
                    if val_reg >= fs.nvarstack() && val_reg == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    fs.resolve_jumps(e, dest);
                }
            } else {
                // No jumps - original logic (same structure as above without resolve_jumps)
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
                    // info2 < 0: expression is already in a register (like C's VNONRELOC).
                    let val_reg = e.info as i32;
                    if dest != val_reg {
                        fs.code_abc(OpCode::MOVE, dest, val_reg, 0);
                    }
                    if val_reg >= fs.nvarstack() && val_reg == fs.freereg - 1 {
                        fs.free_reg();
                    }
                }
            }
        }
    }
}

/// ANTLR4: functioncall 帮助 — 将函数值加载到寄存器以便调用
/// 返回 (函数寄存器, 是否需要额外释放基寄存器, 是否已分配寄存器, 方法调用原始源寄存器)
fn load_func(fs: &mut FuncState, p: &PrefixResult, is_method: bool) -> (i32, bool, bool, Option<i32>) {
    if let (Some(table_reg), Some(table_key)) = (p.table_reg, p.table_key) {
        if p.is_vvargvar {
            // VVARGVAR indexed: generate GETVARG (like C's VVARGIND discharge)
            if p.key_allocated_reg {
                fs.free_reg();
            }
            let r = fs.alloc_reg();
            fs.code_abc(OpCode::GETVARG, r, table_reg, table_key);
            (r, true, true, None)
        } else if p.is_upvalue {
            // Upvalue table with short string key: use GETTABUP instead of GETFIELD
            let r = fs.alloc_reg();
            code_gettabup(fs, r, table_reg, table_key);
            (r, true, true, None)
        } else if p.table_key_is_const {
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
    } else if !p.is_upvalue && p.key.is_some() && p.upval_idx.is_some() {
        // Global variable accessed through _ENV upvalue: use GETTABUP
        let r = fs.alloc_reg();
        code_gettabup(fs, r, p.upval_idx.unwrap(), p.key.unwrap());
        (r, false, true, None)
    } else if let Some(upval_idx) = p.upval_idx {
        // Upvalue variable without suffix (e.g., _ENV itself): load it into a register
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::GETUPVAL, r, upval_idx, 0);
        (r, false, true, None)
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
    // Like C's funcargs: save line at start for luaK_fixline after CALL
    let line = fs.ls().lastline;
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
        // C++ funcargs: luaK_fixline(fs, line) after CALL
        fs.fixline(line);
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
        // C++ funcargs: luaK_fixline(fs, line) after CALL
        fs.fixline(line);
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
            // Like C's luaK_self: reserve base and base+1 first, then alloc key reg
            // This ensures key reg is at base+2, not overlapping with base+1 (self copy)
            while fs.freereg < freg + 2 {
                fs.alloc_reg();
            }
            let kr = fs.alloc_reg();   // key reg at freg+2 or higher
            fs.code_abx(OpCode::LOADK, kr, k);
            fs.code_abc(OpCode::MOVE, freg + 1, src, 0);
            fs.code_abc(OpCode::GETTABLE, freg, src, kr);
            fs.free_reg();  // free key register
        }
        // After SELF (or MOVE+GETTABLE), freereg must be at least freg+2
        // (freg=method, freg+1=self copy)
        while fs.freereg < freg + 2 {
            fs.alloc_reg();
        }
        if matches!(&fs.ls().token, Token::String(..)) {
            // colon call with string argument: obj:method"string"
            let str_s = match &fs.ls().token {
                Token::String(s) => s.clone(),
                _ => String::new(),
            };
            fs.ls_mut().next();
            let k = fs.string_k(&str_s);
            let kr = fs.alloc_reg();
            fs.code_abx(OpCode::LOADK, kr, k);
            let pc = fs.code_abc(OpCode::CALL, freg, 3, 2);
            fs.fixline(line);
            fs.set_freereg(freg + 1);
            return pc;
        }
        if check(fs, &Token::LBrace) {
            // colon call with table argument: obj:method{...}
            let (tr, _n) = parse_constructor(fs);
            if freg + 2 != tr {
                fs.code_abc(OpCode::MOVE, freg + 2, tr, 0);
                fs.free_reg();
            }
            let pc = fs.code_abc(OpCode::CALL, freg, 3, 2);
            fs.fixline(line);
            fs.set_freereg(freg + 1);
            return pc;
        }
        if check(fs, &Token::LParen) {
            fs.ls_mut().next();
            let (na, na_multret) = parse_args(fs);
            expect(fs, &Token::RParen);
            let na_adj = if na_multret { 0 } else { na + 2 };
            let pc = fs.code_abc(OpCode::CALL, freg, na_adj, 2);
            // C++ funcargs: luaK_fixline(fs, line) after CALL
            fs.fixline(line);
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
        // C++ funcargs: luaK_fixline(fs, line) after CALL
        fs.fixline(line);
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
    let mut last_is_vararg = ei.exp.kind == ExpKind::Vararg;
    let mut last_vararg_pc = if last_is_vararg { ei.exp.info2 } else { -1 };
    // Like C's explist: only force to next reg if not the last expression.
    // For Call/Vararg (hasmultret), don't discharge - caller handles multret.
    let _r = if last_is_call || last_is_vararg {
        // Don't discharge Call/Vararg here; they'll be handled after the loop
        -1
    } else if matches!(ei.exp.kind, ExpKind::Relocable) && ei.exp.info2 >= 0 && !ei.exp.has_jumps() {
        fs.exp_to_reg(&ei.exp)
    } else if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() {
        if ei.exp.info2 >= 0 {
            fs.set_a(ei.exp.info2, ei.exp.info as i32);
        }
        ei.exp.info as i32
    } else {
        fs.exp_to_reg(&ei.exp)
    };
    if !last_is_call && !last_is_vararg && matches!(ei.exp.kind, ExpKind::NonReloc) && (ei.exp.info as i32) < fs.nvarstack() && !ei.exp.has_jumps() {
        let target = fs.alloc_reg();
        if ei.exp.info as i32 != target {
            fs.code_abc(OpCode::MOVE, target, ei.exp.info as i32, 0);
        }
    }
    let mut n = 1;
    while check(fs, &Token::Comma) {
        fs.ls_mut().next();
        // Previous expression was not the last, force it to next reg
        if last_is_call {
            fs.set_c(last_call_pc, 2);  // setoneret: single return value
            // Call is now NonReloc at call_reg
            // Need to ensure it's in a register - Call's info is the register
        } else if last_is_vararg {
            // setoneret for Vararg: C=2, allocate register, set A
            fs.set_c(last_vararg_pc, 2);
            let r = fs.alloc_reg();
            fs.set_a(last_vararg_pc, r);
        }
        let ei2 = parse_expr(fs);
        last_is_call = ei2.exp.kind == ExpKind::Call;
        last_call_pc = if last_is_call { ei2.exp.info2 } else { -1 };
        last_is_vararg = ei2.exp.kind == ExpKind::Vararg;
        last_vararg_pc = if last_is_vararg { ei2.exp.info2 } else { -1 };
        let _r2 = if last_is_call || last_is_vararg {
            -1
        } else if matches!(ei2.exp.kind, ExpKind::Relocable) && ei2.exp.info2 >= 0 && !ei2.exp.has_jumps() {
            fs.exp_to_reg(&ei2.exp)
        } else if matches!(ei2.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei2.exp.has_jumps() {
            if ei2.exp.info2 >= 0 {
                fs.set_a(ei2.exp.info2, ei2.exp.info as i32);
            }
            ei2.exp.info as i32
        } else {
            fs.exp_to_reg(&ei2.exp)
        };
        if !last_is_call && !last_is_vararg && matches!(ei2.exp.kind, ExpKind::NonReloc) && (ei2.exp.info as i32) < fs.nvarstack() && !ei2.exp.has_jumps() {
            let target = fs.alloc_reg();
            if ei2.exp.info as i32 != target {
                fs.code_abc(OpCode::MOVE, target, ei2.exp.info as i32, 0);
            }
        }
        n += 1;
    }
    if last_is_call {
        fs.set_c(last_call_pc, 0);
    } else if last_is_vararg {
        // Like C's luaK_setmultret for VVARARG: SETARG_C(pc, 0), SETARG_A(pc, freereg), reserveregs(1)
        fs.set_c(last_vararg_pc, 0);  // LUA_MULTRET + 1 = 0
        let r = fs.alloc_reg();
        fs.set_a(last_vararg_pc, r);
    }
    (n, last_is_call || last_is_vararg)
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
    is_upvalue: bool,  // true if variable is an upvalue not yet loaded into a register
    upval_idx: Option<i32>,
    env_gettabup_pc: i32,
    has_call: bool,
    call_pc: i32,
    is_vvargvar: bool,
    is_readonly: bool,
}

/// 生成通过 _ENV[name] 访问全局变量的 PrefixResult。
/// 匹配 C 的 buildglobal：_ENV 可以是 local、local const (CTC) 或 upvalue。
/// 用于具名 global 声明（如 `global a`）、collective `global *` 和隐式全局。
fn code_global_via_env_prefix(fs: &mut FuncState, name: &str) -> PrefixResult {
    let is_env = name == "_ENV";
    let k = if is_env { 0 } else { fs.string_k(name) };
    // 检查是否有 global <const> 声明（read-only）
    let is_readonly = !is_env && fs.find_global_decl(name) == Some(GDKCONST);
    // Like C buildglobal: singlevaraux(fs, "_ENV", ...) finds _ENV.
    // _ENV can be a local (VLOCAL), a local const (VCONST), or an upvalue (VUPVAL).
    // For VCONST, luaK_exp2anyregup discharges it to a register first.
    if let Some(env_ctc) = fs.find_local_ctc("_ENV") {
        // _ENV is a compile-time constant (local _ENV <const> = ...).
        // Like C's luaK_exp2anyregup + luaK_indexed:
        // discharge the constant to a register, then use GETFIELD/SETFIELD.
        let env_r = fs.alloc_reg();
        match env_ctc.kind {
            ExpKind::Int => {
                let val = env_ctc.info;
                if fits_sbx(val) {
                    fs.code_asbx(OpCode::LOADI, env_r, val as i32);
                } else {
                    let kk = fs.int_k(val);
                    fs.code_abx(OpCode::LOADK, env_r, kk);
                }
            }
            ExpKind::Float => {
                let f = f64::from_bits(env_ctc.info as u64);
                let fi = f as i64;
                if (fi as f64) == f && fits_sbx(fi) {
                    fs.code_asbx(OpCode::LOADF, env_r, fi as i32);
                } else {
                    let kk = fs.float_k(f);
                    fs.code_abx(OpCode::LOADK, env_r, kk);
                }
            }
            ExpKind::Str => {
                let kk = fs.get_str_k(&env_ctc);
                fs.code_abx(OpCode::LOADK, env_r, kk);
            }
            ExpKind::Boolean => {
                if env_ctc.info != 0 {
                    fs.code_abc(OpCode::LOADTRUE, env_r, 0, 0);
                } else {
                    fs.code_abc(OpCode::LOADFALSE, env_r, 0, 0);
                }
            }
            ExpKind::Nil => {
                fs.code_nil(env_r, 1);
            }
            _ => {
                let kk = fs.discharge_str(&mut env_ctc.clone());
                fs.code_abx(OpCode::LOADK, env_r, kk);
            }
        }
        let is_short_str = name.len() <= crate::strings::LUAI_MAXSHORTLEN
            && (k as u32) <= crate::opcodes::MAXINDEXRK;
        if is_short_str {
            PrefixResult { var_name: Some(name.to_string()), local_idx: None, key: None, reg: None, table_reg: Some(env_r), table_key: Some(k), table_key_is_const: true, table_key_is_int: false, key_allocated_reg: false, allocated_reg: true, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly }
        } else {
            let kr = fs.alloc_reg();
            fs.code_abx(OpCode::LOADK, kr, k);
            PrefixResult { var_name: Some(name.to_string()), local_idx: None, key: None, reg: None, table_reg: Some(env_r), table_key: Some(kr), table_key_is_const: false, table_key_is_int: false, key_allocated_reg: true, allocated_reg: true, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly }
        }
    } else if let Some((env_reg, kind)) = fs.find_local_ex("_ENV") {
        let is_vvargvar = kind == RDKVAVAR;
        if is_vvargvar {
            // _ENV 是命名 vararg 参数（VVARGVAR）：键必须在寄存器中。
            // 匹配 C 的 luaK_indexed：对 VVARGVAR 调用 luaK_exp2anyreg 强制键到寄存器，
            // 使用 SETTABLE/GETTABLE 而非 SETFIELD/GETFIELD。
            // is_vvargvar: true 让赋值时设置 PF_VATAB，这样：
            // 1. GETVARG 会被 luaK_finish 转为 GETTABLE
            // 2. RETURN1 不会被转为 RETURN（PF_VAHID 被清除）
            let kr = fs.alloc_reg();
            fs.code_abx(OpCode::LOADK, kr, k);
            PrefixResult { var_name: Some(name.to_string()), local_idx: None, key: None, reg: None, table_reg: Some(env_reg), table_key: Some(kr), table_key_is_const: false, table_key_is_int: false, key_allocated_reg: true, allocated_reg: false, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: true, is_readonly }
        } else {
            let is_short_str = name.len() <= crate::strings::LUAI_MAXSHORTLEN
                && (k as u32) <= crate::opcodes::MAXINDEXRK;
            if is_short_str {
                PrefixResult { var_name: Some(name.to_string()), local_idx: None, key: None, reg: None, table_reg: Some(env_reg), table_key: Some(k), table_key_is_const: true, table_key_is_int: false, key_allocated_reg: false, allocated_reg: false, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly }
            } else {
                // _ENV is local but key is not short string: load _ENV into temp register
                let env_r = fs.alloc_reg();
                fs.code_abc(OpCode::MOVE, env_r, env_reg, 0);
                let kr = fs.alloc_reg();
                fs.code_abx(OpCode::LOADK, kr, k);
                PrefixResult { var_name: Some(name.to_string()), local_idx: None, key: None, reg: None, table_reg: Some(env_r), table_key: Some(kr), table_key_is_const: false, table_key_is_int: false, key_allocated_reg: true, allocated_reg: true, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly }
            }
        }
    } else {
        // _ENV is not a local: it must be an upvalue.
        // Register _ENV as an upvalue first (like C's singlevaraux searching for _ENV),
        // so it gets the correct upvalue index before any user upvalues are created.
        let env_upval_idx = match fs.find_upvalue("_ENV") {
            Some(UpvalueOrCtc::Upvalue(idx)) => idx,
            _ => 0, // fallback: implicit _ENV at upvalue #0
        };
        PrefixResult { var_name: Some(name.to_string()), local_idx: None, key: Some(k), reg: None, table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: false, is_upvalue: is_env, upval_idx: if is_env { Some(env_upval_idx) } else { Some(env_upval_idx) }, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly }
    }
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
                PrefixResult { var_name: None, local_idx: Some(r), key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: true, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly: false }
            } else if let Some((reg, kind)) = fs.find_local_ex(&name) {
                let is_vvargvar = kind == RDKVAVAR;
                PrefixResult { var_name: None, local_idx: Some(reg), key: None, reg: Some(reg), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: false, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar, is_readonly: false }
            } else if fs.find_named_global_decl(&name).is_some() {
                // 具名 global 声明（如 `global a`）：优先于 upvalue，通过 _ENV[name] 访问。
                // 匹配 C 的 searchvar：具名 global 匹配时立即返回 VGLOBAL，优先于 upvalue 查找。
                code_global_via_env_prefix(fs, &name)
            } else if let Some(result) = fs.find_upvalue(&name) {
                match result {
                    UpvalueOrCtc::Upvalue(upval_idx) => {
                        // Don't eagerly load the upvalue into a register.
                        // Like C's singlevar which returns VUPVAL, we delay the GETUPVAL
                        // until load_func or the Dot/LBracket suffix handlers need it.
                        // This avoids duplicate GETUPVAL instructions and matches C's behavior.
                        PrefixResult { var_name: None, local_idx: None, key: None, reg: None, table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: false, is_upvalue: true, upval_idx: Some(upval_idx), env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly: false }
                    }
                    UpvalueOrCtc::CtcConst(mut ctc) => {
                        // Like find_local_ctc handling: load constant into a register
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
                        PrefixResult { var_name: None, local_idx: Some(r), key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: true, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly: false }
                    }
                }
            } else {
                // 全局变量（collective `global *` 或隐式全局）通过 _ENV[name] 访问。
                // 匹配 C 的 buildvar：先 singlevaraux 查找 local/upvalue（已在上面处理），
                // 未找到则 buildglobal 通过 _ENV[name] 访问。
                code_global_via_env_prefix(fs, &name)
            };

            loop {
                match &fs.ls().token {
                    Token::Dot => {
                        fs.ls_mut().next();
                        let field = get_name(fs);
                        let k = fs.string_k(&field);

                        // Handle VVARGVAR: like C's luaK_indexed, create VVARGIND-like PrefixResult
                        // C's luaK_indexed always puts key in a register for VVARGVAR (luaK_exp2anyreg),
                        // so we always LOADK, never use SETFIELD/GETFIELD with constant index.
                        if result.is_vvargvar {
                            let base_reg = result.reg.unwrap();
                            let kr = fs.alloc_reg();
                            fs.code_abx(OpCode::LOADK, kr, k);
                            result = PrefixResult {
                                var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                                table_reg: Some(base_reg), table_key: Some(kr), table_key_is_const: false, table_key_is_int: false,
                                key_allocated_reg: true,
                                allocated_reg: false,
                                is_upvalue: false, upval_idx: None, env_gettabup_pc: -1,
                                has_call: false, call_pc: -1, is_vvargvar: true, is_readonly: false,
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
                        // Check if we can revert a GETUPVAL and use SETTABUP/GETTABUP instead.
                        // Only attempt revert if the last instruction is actually a GETUPVAL
                        // generated for this result (i.e., result was just loaded from an upvalue
                        // and not yet indexed). This matches C's VINDEXUP optimization where
                        // _ENV (as upvalue) indexed by a short string stays as VINDEXUP.
                        let can_revert_getupval = result.reg.is_some() && result.upval_idx.is_some() && is_short_str
                            && !result.is_upvalue && result.table_reg.is_none()
                            && result.allocated_reg  // must be a register we allocated
                            && (fs.pc > 0)  // safety check
                            && {
                                let last_inst = fs.proto.code[(fs.pc - 1) as usize];
                                get_opcode(last_inst) == OpCode::GETUPVAL
                                    && getarg_a(last_inst) == result.reg.unwrap()
                            };
                        let (base_reg, gettabup_pc) = if can_revert_getupval {
                            // Revert: remove the GETUPVAL instruction, free the register
                            let getupval_pc = fs.pc - 1;
                            fs.proto.code.remove(getupval_pc as usize);
                            fs.inst_lines.remove(getupval_pc as usize);
                            fs.pc -= 1;
                            fs.free_reg();
                            let uv_idx = result.upval_idx.unwrap();
                            (uv_idx, -1)  // Use upvalue index as base_reg (for SETTABUP/GETTABUP)
                        } else if let Some(r) = result.reg {
                            (r, -1)
                        } else if result.is_upvalue {
                            if !is_short_str {
                                // Key exceeds MAXINDEXRK: must load upvalue into a register
                                let r = fs.alloc_reg();
                                fs.code_abc(OpCode::GETUPVAL, r, result.upval_idx.unwrap_or(0), 0);
                                (r, -1)
                            } else {
                                // Defer: SETTABUP/GETTABUP can be used directly
                                (result.upval_idx.unwrap_or(0), -1)
                            }
                        } else {
                            let r = fs.alloc_reg();
                            let gk = result.key.unwrap_or(0);
                            let env_uv_idx = result.upval_idx.unwrap_or(0);
                            code_gettabup(fs, r, env_uv_idx, gk);
                            (r, fs.pc - 1)
                        };
                        let (table_key, table_key_is_const, key_allocated_reg) = if is_short_str {
                            (k, true, false)
                        } else {
                            let kr = fs.alloc_reg();
                            fs.code_abx(OpCode::LOADK, kr, k);
                            (kr, false, true)
                        };
                        let new_is_upvalue = (result.is_upvalue || can_revert_getupval) && is_short_str;
                        result = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                        table_reg: Some(base_reg), table_key: Some(table_key), table_key_is_const: table_key_is_const, table_key_is_int: false,
                        key_allocated_reg: key_allocated_reg,
                        allocated_reg: if new_is_upvalue { false } else { result.allocated_reg || result.reg.is_none() || (result.is_upvalue && !is_short_str) },
                        is_upvalue: new_is_upvalue,
                        upval_idx: result.upval_idx,
                        env_gettabup_pc: if new_is_upvalue { -1 } else { if gettabup_pc >= 0 { gettabup_pc } else { result.env_gettabup_pc } },
                        has_call: false, call_pc: -1, is_vvargvar: false, is_readonly: false,
                    };
                }
                Token::LBracket => {
                    fs.ls_mut().next();

                    // Handle VVARGVAR: like C's luaK_indexed, create VVARGIND-like PrefixResult
                    if result.is_vvargvar {
                        let base_reg = result.reg.unwrap();
                        let ei = parse_expr(fs);
                        expect(fs, &Token::RBracket);
                        // Like C's luaK_exp2anyreg: if key is already in a local register, use it directly
                        let key_reg = if ei.exp.kind == ExpKind::NonReloc && (ei.exp.info as i32) < fs.nvarstack() {
                            ei.exp.info as i32
                        } else {
                            fs.exp_to_next_reg(&ei.exp)
                        };
                        result = PrefixResult {
                            var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                            table_reg: Some(base_reg), table_key: Some(key_reg), table_key_is_const: false, table_key_is_int: false,
                            key_allocated_reg: false, allocated_reg: false,
                            is_upvalue: false, upval_idx: None, env_gettabup_pc: -1,
                            has_call: false, call_pc: -1, is_vvargvar: true, is_readonly: false,
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
                    // For upvalue tables: match C's suffixedexp '[' + luaK_indexed behavior.
                    // C compiler flow: yindex(expr + luaK_exp2val) → luaK_indexed
                    // luaK_exp2val emits code for comparisons but NOT for simple exprs (VTRUE).
                    // So the order depends on key type:
                    // - Comparison key: key load code → GETUPVAL
                    // - Simple key (true, nil, etc.): GETUPVAL → key load code
                    // - Short string key: GETTABUP/SETTABUP (no GETUPVAL needed)
                    let is_upvalue_table = result.is_upvalue && result.reg.is_none();
                    let saved_upval_idx = result.upval_idx.unwrap_or(0);
                    let (base_reg, gettabup_pc) = if let Some(r) = result.reg {
                        (r, -1)
                    } else if is_upvalue_table {
                        (-1, -1)  // placeholder
                    } else {
                        let r = fs.alloc_reg();
                        let gk = result.key.unwrap_or(0);
                        code_gettabup(fs, r, result.upval_idx.unwrap_or(0), gk);
                        (r, fs.pc - 1)
                    };
                    let saved_freereg_before = fs.freereg;
                    let ei = parse_expr(fs);
                    expect(fs, &Token::RBracket);
                    let key_has_jumps = ei.exp.has_jumps();
                    // For upvalue tables with non-constant, non-simple keys that have jumps:
                    // emit key load code first (like C's luaK_exp2val), then GETUPVAL
                    // Note: Upval key is handled separately (upval_key_placeholder_pc),
                    // because C's luaK_exp2val discharges VUPVAL to VRELOC (GETUPVAL key)
                    // before luaK_indexed emits GETUPVAL table.
                    let getupval_emitted_before_key = is_upvalue_table && !key_has_jumps
                        && !matches!(ei.exp.kind, ExpKind::Str | ExpKind::Int)
                        && !matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc)
                        && ei.exp.kind != ExpKind::Upval;
                    // Save table register when GETUPVAL is emitted so we can use it later
                    // (key may have been allocated BEFORE the table, e.g. Call expressions)
                    let getupval_table_reg = if getupval_emitted_before_key {
                        // Simple expression (VTRUE, etc.): emit GETUPVAL first (like C's luaK_indexed)
                        let r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETUPVAL, r, saved_upval_idx, 0);
                        Some(r)
                    } else {
                        None
                    };
                    // For upvalue table + Upval key (like a[i]): C generates GETUPVAL key first
                    // (VRELOC, A=0 placeholder in yindex's luaK_exp2val), then GETUPVAL table
                    // (in luaK_indexed's luaK_exp2anyreg), then patches key's A.
                    let upval_key_placeholder_pc = if is_upvalue_table && !getupval_emitted_before_key
                        && ei.exp.kind == ExpKind::Upval && !key_has_jumps
                    {
                        let pc = fs.code_abc(OpCode::GETUPVAL, 0, ei.exp.info as i32, 0);
                        Some(pc)
                    } else {
                        None
                    };
                    let (kr, key_is_const, key_is_int) = if ei.exp.kind == ExpKind::Str {
                        let k = fs.get_str_k(&ei.exp);
                        if let TValue::Str(crate::strings::LuaString::Short(_)) = fs.proto.constants[k as usize] {
                            if (k as u32) <= crate::opcodes::MAXINDEXRK {
                                (k, true, false)
                            } else {
                                // Long string key (index > MAXINDEXRK): for upvalue tables,
                                // defer LOADK until after table's GETUPVAL (matching C's luaK_indexed:
                                // luaK_exp2anyreg(t) first, then luaK_exp2anyreg(k)).
                                if is_upvalue_table && !getupval_emitted_before_key && !key_has_jumps {
                                    (-1, false, false)
                                } else {
                                    let kr = fs.alloc_reg();
                                    fs.code_abx(OpCode::LOADK, kr, k);
                                    (kr, false, false)
                                }
                            }
                        } else {
                            // Long string (not Short): for upvalue tables, defer LOADK until after
                            // table's GETUPVAL (matching C's luaK_indexed order).
                            if is_upvalue_table && !getupval_emitted_before_key && !key_has_jumps {
                                (-1, false, false)
                            } else {
                                let kr = fs.alloc_reg();
                                fs.code_abx(OpCode::LOADK, kr, k);
                                (kr, false, false)
                            }
                        }
                    } else if ei.exp.kind == ExpKind::Int
                        && ei.exp.info >= 0
                        && ei.exp.info <= ((1u32 << SIZE_C) - 1) as i64
                    {
                        (ei.exp.info as i32, true, true)
                    } else if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() && ei.exp.info2 < 0 {
                        (ei.exp.info as i32, false, false)
                    } else if is_upvalue_table
                        && !getupval_emitted_before_key
                        && matches!(ei.exp.kind, ExpKind::Upval | ExpKind::Relocable)
                        && !key_has_jumps
                    {
                        // For upvalue table + Upval/Relocable key: defer key register allocation
                        // until after table's GETUPVAL is emitted (matching C's luaK_indexed order).
                        // Upval key: placeholder GETUPVAL already emitted, A will be patched later.
                        // Relocable key: instruction already emitted, A will be patched later.
                        (-1, false, false)
                    } else {
                        (fs.exp_to_reg(&ei.exp), false, false)
                    };
                    let key_allocated = !key_is_const && kr != -1 && fs.freereg > saved_freereg_before;
                    let (base_reg, new_is_upvalue, allocated_reg) = if is_upvalue_table {
                        let can_use_settabup = key_is_const && !key_is_int
                            && (kr as u32) <= crate::opcodes::MAXINDEXRK;
                        if can_use_settabup {
                            if getupval_emitted_before_key {
                                fs.proto.code.remove(fs.pc as usize - 1);
                                fs.inst_lines.remove(fs.pc as usize - 1);
                                fs.pc -= 1;
                                fs.free_reg();
                            }
                            (saved_upval_idx, true, false)
                        } else if getupval_emitted_before_key {
                            // GETUPVAL already emitted before key load code.
                            // Use the saved table register (key may have been allocated
                            // BEFORE the table, e.g. Call expressions like t[a()])
                            (getupval_table_reg.unwrap(), false, true)
                        } else {
                            let r = fs.alloc_reg();
                            fs.code_abc(OpCode::GETUPVAL, r, saved_upval_idx, 0);
                            (r, false, true)
                        }
                    } else {
                        (base_reg, result.is_upvalue, result.allocated_reg || result.reg.is_none())
                    };
                    // Now resolve deferred key register allocation (Upval/Relocable/Str key for upvalue table)
                    let (kr, key_allocated) = if kr == -1 && is_upvalue_table {
                        if let Some(pc) = upval_key_placeholder_pc {
                            let key_r = fs.alloc_reg();
                            fs.set_a(pc, key_r);
                            (key_r, true)
                        } else if ei.exp.kind == ExpKind::Str {
                            // Long string key: emit LOADK now (after table's GETUPVAL)
                            let k = fs.get_str_k(&ei.exp);
                            let key_r = fs.alloc_reg();
                            fs.code_abx(OpCode::LOADK, key_r, k);
                            (key_r, true)
                        } else {
                            let key_r = fs.exp_to_reg(&ei.exp);
                            (key_r, true)
                        }
                    } else {
                        (kr, key_allocated)
                    };
                    result = PrefixResult {
                        var_name: None, local_idx: None, key: None, reg: Some(base_reg),
                        table_reg: Some(base_reg), table_key: Some(kr), table_key_is_const: key_is_const, table_key_is_int: key_is_int,
                        key_allocated_reg: key_allocated,
                        allocated_reg: allocated_reg,
                        is_upvalue: new_is_upvalue,
                        upval_idx: result.upval_idx,
                        env_gettabup_pc: if new_is_upvalue { -1 } else { if gettabup_pc >= 0 { gettabup_pc } else { result.env_gettabup_pc } },
                        has_call: false, call_pc: -1, is_vvargvar: false, is_readonly: false,
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
                            is_upvalue: false,
                            upval_idx: None,
                            env_gettabup_pc: -1,
                            has_call: true, call_pc: last_call_pc, is_vvargvar: false, is_readonly: false,
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
            let r = fs.exp_to_reg(&e.exp);
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: true, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly: false }
        }
        _ => {
            let e = parse_simple_exp(fs);
            let r = fs.exp_to_reg(&e.exp);
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r), table_reg: None, table_key: None, table_key_is_const: false, table_key_is_int: false, key_allocated_reg: false, allocated_reg: true, is_upvalue: false, upval_idx: None, env_gettabup_pc: -1, has_call: false, call_pc: -1, is_vvargvar: false, is_readonly: false }
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
    if !fs.enterlevel() {
        return ExprItem { exp: ExpDesc::new(ExpKind::Void, 0) };
    }
    let mut e = parse_simple_exp(fs);
    
    loop {
        if fs.has_errors() { break; }
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
                    // Like C's jumponcond: check for VRELOC+NOT first
                    let is_vreloc_not = e_left.kind == ExpKind::Relocable && e_left.info2 >= 0
                        && (e_left.info2 as usize) < fs.proto.code.len()
                        && get_opcode(fs.proto.code[e_left.info2 as usize]) == OpCode::NOT;
                    if is_vreloc_not {
                        // Like C: remove NOT, use NOT's B operand as TEST's A
                        // and: goiftrue → jumponcond(cond=0) → TEST b k=!cond=true
                        let not_inst = fs.proto.code[e_left.info2 as usize];
                        let b = getarg_b(not_inst);
                        fs.proto.code.remove(e_left.info2 as usize);
                        fs.inst_lines.remove(e_left.info2 as usize);
                        fs.pc -= 1;
                        fs.code_abc_k(OpCode::TEST, b, 0, 0, true);
                        let jmp_pc = fs.jump();
                        fs.concat_jump(&mut e_left.f, jmp_pc);
                        let here = fs.pc;
                        fs.patch_true_jumps(e_left.t, here);
                        e_left.t = NO_JUMP;
                    } else {
                        // Like C's jumponcond: discharge2anyreg + freeexp + TESTSET + JMP
                        let r = fs.discharge_to_any_reg(&e_left);
                        // freeexp: free the register if it's a temp at top of stack
                        if r >= fs.nvarstack() && r == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        // and: goiftrue → jumponcond(cond=0) → TESTSET NO_REG r 0 false
                        fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, r, 0, false);
                        let jmp_pc = fs.jump();
                        fs.concat_jump(&mut e_left.f, jmp_pc);
                        let here = fs.pc;
                        fs.patch_true_jumps(e_left.t, here);
                        e_left.t = NO_JUMP;
                    }
                }
            }
            
            let e2 = parse_subexpr(fs, PREC_AND + 1);
            let mut e2_exp = e2.exp.clone();
            if e2_exp.kind == ExpKind::Call {
                // Like C's luaK_dischargevars + setoneret for VCALL:
                // VCALL → VNONRELOC (info = A register), clear info2
                e2_exp.kind = ExpKind::NonReloc;
                e2_exp.info2 = -1;
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
                    // Like C's jumponcond: check for VRELOC+NOT first
                    let is_vreloc_not = e_left.kind == ExpKind::Relocable && e_left.info2 >= 0
                        && (e_left.info2 as usize) < fs.proto.code.len()
                        && get_opcode(fs.proto.code[e_left.info2 as usize]) == OpCode::NOT;
                    if is_vreloc_not {
                        // Like C: remove NOT, use NOT's B operand as TEST's A
                        // or: goiffalse → jumponcond(cond=1) → TEST b k=!cond=false
                        let not_inst = fs.proto.code[e_left.info2 as usize];
                        let b = getarg_b(not_inst);
                        fs.proto.code.remove(e_left.info2 as usize);
                        fs.inst_lines.remove(e_left.info2 as usize);
                        fs.pc -= 1;
                        fs.code_abc_k(OpCode::TEST, b, 0, 0, false);
                        let jmp_pc = fs.jump();
                        fs.concat_jump(&mut e_left.t, jmp_pc);
                        let here = fs.pc;
                        fs.patch_false_jumps(e_left.f, here);
                        e_left.f = NO_JUMP;
                    } else {
                        // Like C's jumponcond: discharge2anyreg + freeexp + TESTSET + JMP
                        let r = fs.discharge_to_any_reg(&e_left);
                        // freeexp: free the register if it's a temp at top of stack
                        if r >= fs.nvarstack() && r == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        // or: goiffalse → jumponcond(cond=1) → TESTSET NO_REG r 0 true
                        fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, r, 0, true);
                        let jmp_pc = fs.jump();
                        fs.concat_jump(&mut e_left.t, jmp_pc);
                        let here = fs.pc;
                        fs.patch_false_jumps(e_left.f, here);
                        e_left.f = NO_JUMP;
                    }
                }
            }
            
            let e2 = parse_subexpr(fs, PREC_AND);
            let mut e2_exp = e2.exp.clone();
            if e2_exp.kind == ExpKind::Call {
                // Like C's luaK_dischargevars + setoneret for VCALL:
                // VCALL → VNONRELOC (info = A register), clear info2
                e2_exp.kind = ExpKind::NonReloc;
                e2_exp.info2 = -1;
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
                    } else if matches!(ec.kind, ExpKind::Vararg) && !ec.has_jumps() {
                        // Like C's dischargevars + setoneret for VVARARG:
                        // SETARG_C(pc, 2), then VRELOC → discharge to register
                        let pc = ec.info2;
                        fs.set_c(pc, 2);
                        let r = fs.alloc_reg();
                        fs.set_a(pc, r);
                        ec.kind = ExpKind::NonReloc;
                        ec.info = r as i64;
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
                    } else if matches!(ec.kind, ExpKind::Vararg) && !ec.has_jumps() {
                        // Like C's dischargevars + setoneret for VVARARG
                        let pc = ec.info2;
                        fs.set_c(pc, 2);
                        let r = fs.alloc_reg();
                        fs.set_a(pc, r);
                        ec.kind = ExpKind::NonReloc;
                        ec.info = r as i64;
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
                        // Free e2's register (like C's freeexps)
                        if r2_alloc || (matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && r2 >= fs.nvarstack()) { fs.free_reg(); }
                        // Free ec's register if it was allocated in infix phase
                        // (C's freeexps also frees e1 when it's VNONRELOC)
                        if matches!(ec.kind, ExpKind::NonReloc) && (ec.info as i32) >= fs.nvarstack() && (ec.info as i32) == fs.freereg - 1 {
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
                                } else if matches!(e2.exp.kind, ExpKind::Vararg) && !e2.exp.has_jumps() {
                                    // Discharge Vararg: setoneret (C=2) + alloc reg + set A
                                    let pc = e2.exp.info2;
                                    fs.set_c(pc, 2);
                                    let reg = fs.alloc_reg();
                                    fs.set_a(pc, reg);
                                    reg
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
                    // LT/LE case: check if left operand is SC number
                    // (transform A < B to B > A, A <= B to B >= A)
                    if let Some(sc_val) = is_sc_number(&ec) {
                        let r_alloc = !matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg);
                        let reg = if r_alloc {
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
                        let imm = int_to_sc(sc_val);
                        let imm_op = match op_tok {
                            Token::Lt => OpCode::GTI,
                            Token::LtEq => OpCode::GEI,
                            _ => OpCode::GTI,
                        };
                        fs.code_abc_k(imm_op, reg, imm, 0, k != 0);
                        let jmp_pc = fs.jump();
                        if r_alloc || (matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call | ExpKind::Vararg) && reg >= fs.nvarstack()) {
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
            }
            matched = true;
        }
        
        if limit <= PREC_BOR && check(fs, &Token::Pipe) {
            let mut ec = e.exp.clone();
            let mut flip = false;
            // Like C's luaK_infix for OPR_BOR: if e1 is not a numeral, compile to register
            if !matches!(ec.kind, ExpKind::Int | ExpKind::Float) || ec.has_jumps() {
                let r = fs.exp_to_reg(&ec);
                ec.t = NO_JUMP;
                ec.f = NO_JUMP;
                ec.info = r as i64;
                ec.kind = ExpKind::NonReloc;  // Mark as already in register
            }
            fs.ls_mut().next();
            let mut e2 = parse_subexpr(fs, PREC_BOR + 1);
            // Like C's codebitwise: if e1 is an integer constant, swap operands
            if matches!(ec.kind, ExpKind::Int) && !ec.has_jumps() {
                std::mem::swap(&mut ec, &mut e2.exp);
                flip = true;
            }
            let v1_opt = to_int_const(&ec);
            let v2_opt = to_int_const(&e2.exp);
            if v1_opt.is_some() && v2_opt.is_some() {
                let val = v1_opt.unwrap() | v2_opt.unwrap();
                e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
            } else {
                // Like C's isSHint/isSCint: only use K variant if e2 has no jumps
                let k_idx = if !e2.exp.has_jumps() {
                    match &e2.exp.kind {
                        ExpKind::Int => {
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 { Some(k) } else { None }
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(k) = k_idx {
                    // BORK variant: like C's codebinK
                    // C's codebinK -> finishbinexpval compiles e1
                    let v1 = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        ec.info = r as i64;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc) {
                        ec.info as i32
                    } else if matches!(ec.kind, ExpKind::Relocable) {
                        fs.exp_to_reg(&ec)
                    } else {
                        fs.exp_to_reg(&ec)
                    };
                    let pc = fs.code_abc(OpCode::BORK, 0, v1, k);
                    // Free v1 if it's a temp - like C's freeexps
                    if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc_k(OpCode::MMBINK, v1, k, 14, flip);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                } else {
                    // BOR variant: like C's codebinexpval
                    // C's infix already compiled e1 to register (if not numeral)
                    // codebinexpval compiles e2 first, then finishbinexpval gets e1's register
                    let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                        e2.exp.info as i32
                    } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                        e2.exp.info as i32
                    } else {
                        fs.exp_to_reg(&e2.exp)
                    };
                    // Now get e1's register (already compiled in infix if not numeral)
                    let v1 = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        ec.info = r as i64;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc) {
                        ec.info as i32
                    } else if matches!(ec.kind, ExpKind::Relocable) {
                        fs.exp_to_reg(&ec)
                    } else {
                        fs.exp_to_reg(&ec)
                    };
                    let pc = fs.code_abc(OpCode::BOR, 0, v1, v2);
                    // Free registers in descending order - like C's freeregs
                    let (hi, lo) = if v1 > v2 { (v1, v2) } else { (v2, v1) };
                    if hi >= fs.nvarstack() && hi == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    if lo >= fs.nvarstack() && lo == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc(OpCode::MMBIN, v1, v2, 14);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                }
            }
            matched = true;
        }

        if limit <= PREC_BXOR && check(fs, &Token::Tilde) {
            let mut ec = e.exp.clone();
            let mut flip = false;
            // Like C's luaK_infix for OPR_BXOR: if e1 is not a numeral, compile to register
            if !matches!(ec.kind, ExpKind::Int | ExpKind::Float) || ec.has_jumps() {
                let r = fs.exp_to_reg(&ec);
                ec.t = NO_JUMP;
                ec.f = NO_JUMP;
                ec.info = r as i64;
                ec.kind = ExpKind::NonReloc;  // Mark as already in register
            }
            fs.ls_mut().next();
            let mut e2 = parse_subexpr(fs, PREC_BXOR + 1);
            // Like C's codebitwise: if e1 is an integer constant, swap operands
            if matches!(ec.kind, ExpKind::Int) && !ec.has_jumps() {
                std::mem::swap(&mut ec, &mut e2.exp);
                flip = true;
            }
            let v1_opt = to_int_const(&ec);
            let v2_opt = to_int_const(&e2.exp);
            if v1_opt.is_some() && v2_opt.is_some() {
                let val = v1_opt.unwrap() ^ v2_opt.unwrap();
                e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
            } else {
                // Like C's isSHint/isSCint: only use K variant if e2 has no jumps
                let k_idx = if !e2.exp.has_jumps() {
                    match &e2.exp.kind {
                        ExpKind::Int => {
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 { Some(k) } else { None }
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(k) = k_idx {
                    // BXORK variant: like C's codebinK
                    // C's codebinK -> finishbinexpval compiles e1
                    let v1 = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        ec.info = r as i64;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc) {
                        ec.info as i32
                    } else if matches!(ec.kind, ExpKind::Relocable) {
                        fs.exp_to_reg(&ec)
                    } else {
                        fs.exp_to_reg(&ec)
                    };
                    let pc = fs.code_abc(OpCode::BXORK, 0, v1, k);
                    // Free v1 if it's a temp - like C's freeexps
                    if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc_k(OpCode::MMBINK, v1, k, 15, flip);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                } else {
                    // BXOR variant: like C's codebinexpval
                    // C's infix already compiled e1 to register (if not numeral)
                    // codebinexpval compiles e2 first, then finishbinexpval gets e1's register
                    let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                        e2.exp.info as i32
                    } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                        e2.exp.info as i32
                    } else {
                        fs.exp_to_reg(&e2.exp)
                    };
                    // Now get e1's register (already compiled in infix if not numeral)
                    let v1 = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        ec.info = r as i64;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc) {
                        ec.info as i32
                    } else if matches!(ec.kind, ExpKind::Relocable) {
                        fs.exp_to_reg(&ec)
                    } else {
                        fs.exp_to_reg(&ec)
                    };
                    let pc = fs.code_abc(OpCode::BXOR, 0, v1, v2);
                    // Free registers in descending order - like C's freeregs
                    let (hi, lo) = if v1 > v2 { (v1, v2) } else { (v2, v1) };
                    if hi >= fs.nvarstack() && hi == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    if lo >= fs.nvarstack() && lo == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc(OpCode::MMBIN, v1, v2, 15);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                }
            }
            matched = true;
        }
        
        if limit <= PREC_BAND && check(fs, &Token::Ampersand) {
            let mut ec = e.exp.clone();
            let mut flip = false;
            // Like C's luaK_infix for OPR_BAND: if e1 is not a numeral, compile to register
            if !matches!(ec.kind, ExpKind::Int | ExpKind::Float) || ec.has_jumps() {
                let r = fs.exp_to_reg(&ec);
                ec.t = NO_JUMP;
                ec.f = NO_JUMP;
                ec.info = r as i64;
                ec.kind = ExpKind::NonReloc;  // Mark as already in register
            }
            fs.ls_mut().next();
            let mut e2 = parse_subexpr(fs, PREC_BAND + 1);
            // Like C's codebitwise: if e1 is an integer constant, swap operands
            if matches!(ec.kind, ExpKind::Int) && !ec.has_jumps() {
                std::mem::swap(&mut ec, &mut e2.exp);
                flip = true;
            }
            let v1_opt = to_int_const(&ec);
            let v2_opt = to_int_const(&e2.exp);
            if v1_opt.is_some() && v2_opt.is_some() {
                let val = v1_opt.unwrap() & v2_opt.unwrap();
                e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
            } else {
                // Like C's isSHint/isSCint: only use K variant if e2 has no jumps
                let k_idx = if !e2.exp.has_jumps() {
                    match &e2.exp.kind {
                        ExpKind::Int => {
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 { Some(k) } else { None }
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(k) = k_idx {
                    // BANDK variant: like C's codebinK
                    // C's codebinK -> finishbinexpval compiles e1
                    let v1 = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        ec.info = r as i64;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc) {
                        ec.info as i32
                    } else if matches!(ec.kind, ExpKind::Relocable) {
                        fs.exp_to_reg(&ec)
                    } else {
                        fs.exp_to_reg(&ec)
                    };
                    let pc = fs.code_abc(OpCode::BANDK, 0, v1, k);
                    // Free v1 if it's a temp - like C's freeexps
                    if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc_k(OpCode::MMBINK, v1, k, 13, flip);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                } else {
                    // BAND variant: like C's codebinNoK + codebinexpval
                    // When flip=true, C's codebinNoK swaps back to original order,
                    // then codebinexpval processes e2 (original) first, then e1.
                    // We need to match this order for correct register allocation.
                    let (first_ec, first_e2) = if flip {
                        // Swap back to original order: ec was e2, e2 was e1
                        // C processes original e2 first (now ec), then original e1 (now e2)
                        (&ec.clone(), &e2.exp.clone())
                    } else {
                        (&e2.exp.clone(), &ec.clone())
                    };
                    // Process the first operand (original e2 in C's codebinexpval)
                    let v2 = if matches!(first_ec.kind, ExpKind::NonReloc) && !first_ec.has_jumps() {
                        first_ec.info as i32
                    } else if matches!(first_ec.kind, ExpKind::Relocable) && !first_ec.has_jumps() && first_ec.info2 < 0 {
                        first_ec.info as i32
                    } else {
                        fs.exp_to_reg(first_ec)
                    };
                    // Process the second operand (original e1 in C's finishbinexpval)
                    let v1 = if first_e2.has_jumps() {
                        let r = fs.exp_to_reg(first_e2);
                        r
                    } else if matches!(first_e2.kind, ExpKind::NonReloc) {
                        first_e2.info as i32
                    } else if matches!(first_e2.kind, ExpKind::Relocable) {
                        if first_e2.info2 >= 0 {
                            fs.exp_to_reg(first_e2)
                        } else {
                            first_e2.info as i32
                        }
                    } else {
                        fs.exp_to_reg(first_e2)
                    };
                    let pc = fs.code_abc(OpCode::BAND, 0, v1, v2);
                    // Free registers in descending order - like C's freeregs
                    let (hi, lo) = if v1 > v2 { (v1, v2) } else { (v2, v1) };
                    if hi >= fs.nvarstack() && hi == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    if lo >= fs.nvarstack() && lo == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc(OpCode::MMBIN, v1, v2, 13);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                }
            }
            matched = true;
        }
        
        if limit <= PREC_SHL && check(fs, &Token::LtLt) {
            let mut ec = e.exp.clone();
            // Like C's luaK_infix for OPR_SHL: if e1 is not a numeral, compile to register
            if !matches!(ec.kind, ExpKind::Int | ExpKind::Float) || ec.has_jumps() {
                let r = fs.exp_to_reg(&ec);
                ec.t = NO_JUMP;
                ec.f = NO_JUMP;
                ec.info = r as i64;
                ec.kind = ExpKind::NonReloc;  // Mark as already in register
            }
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_SHL + 1);
            let v1_opt = to_int_const(&ec);
            let shift_opt = to_int_const(&e2.exp);
            if v1_opt.is_some() && shift_opt.is_some() {
                // Match C's luaV_shiftl: if shift >= NBITS or <= -NBITS, result is 0
                let v1 = v1_opt.unwrap();
                let shift = shift_opt.unwrap();
                let val = if shift >= 64 || shift <= -64 {
                    0i64
                } else if shift < 0 {
                    // Right shift (unsigned/logical)
                    ((v1 as u64) >> (-shift as u32)) as i64
                } else {
                    // Left shift (unsigned)
                    ((v1 as u64) << (shift as u32)) as i64
                };
                e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
            } else {
                if matches!(ec.kind, ExpKind::Int) && fits_sc(&ec) && !ec.has_jumps() {
                        // SHLI: left operand is small constant, like C's codebini(OP_SHLI, ..., flip=1)
                        let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            e2.exp.info as i32
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                            e2.exp.info as i32
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let sc = int_to_sc(ec.info);
                        let pc = fs.code_abc(OpCode::SHLI, 0, v2, sc);
                        // Free v2 if it's a temp - like C's freeexps
                        if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc_k(OpCode::MMBINI, v2, sc, 16, true);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    } else if matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) && fits_sc_neg(e2.exp.info) && !e2.exp.has_jumps() {
                        // SHRI (for SHL): right operand is small constant, like C's finishbinexpneg
                        let v1 = if ec.has_jumps() {
                            let r = fs.exp_to_reg(&ec);
                            ec.t = NO_JUMP;
                            ec.f = NO_JUMP;
                            ec.info = r as i64;
                            r
                        } else if matches!(ec.kind, ExpKind::NonReloc) {
                            ec.info as i32
                        } else if matches!(ec.kind, ExpKind::Relocable) {
                            fs.exp_to_reg(&ec)
                        } else {
                            fs.exp_to_reg(&ec)
                        };
                        let v = e2.exp.info;
                        let sc_neg = int_to_sc(-v);
                        let sc_pos = int_to_sc(v);
                        let pc = fs.code_abc(OpCode::SHRI, 0, v1, sc_neg);
                        // Free v1 if it's a temp - like C's freeexps
                        if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc_k(OpCode::MMBINI, v1, sc_pos, 16, false);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    } else {
                        // SHL: general case, like C's codebinexpval
                        // C's infix already compiled e1 to register (if not numeral)
                        // codebinexpval compiles e2 first, then finishbinexpval gets e1's register
                        let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            e2.exp.info as i32
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                            e2.exp.info as i32
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        // Now get e1's register (already compiled in infix if not numeral)
                        let v1 = if ec.has_jumps() {
                            let r = fs.exp_to_reg(&ec);
                            ec.t = NO_JUMP;
                            ec.f = NO_JUMP;
                            ec.info = r as i64;
                            r
                        } else if matches!(ec.kind, ExpKind::NonReloc) {
                            ec.info as i32
                        } else if matches!(ec.kind, ExpKind::Relocable) {
                            fs.exp_to_reg(&ec)
                        } else {
                            fs.exp_to_reg(&ec)
                        };
                        let pc = fs.code_abc(OpCode::SHL, 0, v1, v2);
                        // Free registers in descending order - like C's freeregs
                        let (hi, lo) = if v1 > v2 { (v1, v2) } else { (v2, v1) };
                        if hi >= fs.nvarstack() && hi == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        if lo >= fs.nvarstack() && lo == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc(OpCode::MMBIN, v1, v2, 16);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    }
            }
            matched = true;
        }

        if limit <= PREC_SHL && check(fs, &Token::GtGt) {
            let mut ec = e.exp.clone();
            // Like C's luaK_infix for OPR_SHR: if e1 is not a numeral, compile to register
            if !matches!(ec.kind, ExpKind::Int | ExpKind::Float) || ec.has_jumps() {
                let r = fs.exp_to_reg(&ec);
                ec.t = NO_JUMP;
                ec.f = NO_JUMP;
                ec.info = r as i64;
                ec.kind = ExpKind::NonReloc;  // Mark as already in register
            }
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_SHL + 1);
            let v1_opt = to_int_const(&ec);
            let shift_opt = to_int_const(&e2.exp);
            if v1_opt.is_some() && shift_opt.is_some() {
                // Match C's luaV_shiftr = luaV_shiftl(x, -y)
                // If shift >= NBITS or <= -NBITS, result is 0
                let v1 = v1_opt.unwrap();
                let shift = shift_opt.unwrap();
                let val = if shift >= 64 || shift <= -64 {
                    0i64
                } else if shift < 0 {
                    // Negative right shift = left shift
                    ((v1 as u64) << (-shift as u32)) as i64
                } else {
                    // Unsigned (logical) right shift
                    ((v1 as u64) >> (shift as u32)) as i64
                };
                e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
            } else {
                if matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) && !e2.exp.has_jumps() {
                    // SHRI: right operand is small constant, like C's codebini(OP_SHRI, ..., flip=0)
                    // C's codebini calls finishbinexpval which compiles e1
                    let v1 = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        ec.info = r as i64;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc) {
                        ec.info as i32
                    } else if matches!(ec.kind, ExpKind::Relocable) {
                        fs.exp_to_reg(&ec)
                    } else {
                        fs.exp_to_reg(&ec)
                    };
                    let v = e2.exp.info;
                    let sc = int_to_sc(v);
                    let pc = fs.code_abc(OpCode::SHRI, 0, v1, sc);
                    // Free v1 if it's a temp - like C's freeexps
                    if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc(OpCode::MMBINI, v1, sc, 17);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                } else {
                    // SHR: general case, like C's codebinexpval
                    // C's infix already compiled e1 to register (if not numeral)
                    // codebinexpval compiles e2 first, then finishbinexpval gets e1's register
                    let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                        e2.exp.info as i32
                    } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                        e2.exp.info as i32
                    } else {
                        fs.exp_to_reg(&e2.exp)
                    };
                    // Now get e1's register (already compiled in infix if not numeral)
                    let v1 = if ec.has_jumps() {
                        let r = fs.exp_to_reg(&ec);
                        ec.t = NO_JUMP;
                        ec.f = NO_JUMP;
                        ec.info = r as i64;
                        r
                    } else if matches!(ec.kind, ExpKind::NonReloc) {
                        ec.info as i32
                    } else if matches!(ec.kind, ExpKind::Relocable) {
                        fs.exp_to_reg(&ec)
                    } else {
                        fs.exp_to_reg(&ec)
                    };
                    let pc = fs.code_abc(OpCode::SHR, 0, v1, v2);
                    // Free registers in descending order - like C's freeregs
                    let (hi, lo) = if v1 > v2 { (v1, v2) } else { (v2, v1) };
                    if hi >= fs.nvarstack() && hi == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    if lo >= fs.nvarstack() && lo == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc(OpCode::MMBIN, v1, v2, 17);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
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
            } else if matches!(ec.kind, ExpKind::NonReloc) {
                if ec.info2 >= 0 {
                    fs.set_a(ec.info2, ec.info as i32);
                }
                ec.info as i32
            } else if matches!(ec.kind, ExpKind::Relocable) {
                if ec.info2 >= 0 {
                    // VRELOC mode: need to allocate register
                    fs.exp_to_reg(&ec)
                } else {
                    ec.info as i32
                }
            } else {
                fs.exp_to_reg(&ec)
            };
            if r < fs.nvarstack() {
                let new_r = fs.alloc_reg();
                fs.code_abc(OpCode::MOVE, new_r, r, 0);
                r = new_r;
            }
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_CONCAT);
            let freereg_before_r2 = fs.freereg;
            let r2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                let reg = e2.exp.info as i32;
                if e2.exp.info2 >= 0 {
                    fs.set_a(e2.exp.info2, reg);
                }
                reg
            } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                e2.exp.info as i32
            } else {
                fs.exp_to_reg(&e2.exp)
            };
            if r2 != r + 1 {
                // Like C's luaK_exp2nextreg: ensure register r+1 is allocated
                // before generating MOVE (updates max_freereg/maxstacksize).
                // C calls luaK_reserveregs(1) which updates maxstacksize via
                // luaK_checkstack before the MOVE.
                if fs.freereg <= r + 1 {
                    fs.alloc_reg();
                }
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
                fs.inst_lines.pop();
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
            // Like C's luaK_infix: if e1 is not a numeral, put it in a register.
            // A numeral with jumps (e.g., from `a or 1`) must also be put in a register,
            // because C's luaK_exp2anyreg calls luaK_dischargevars (no-op for VKINT)
            // and then falls through to luaK_exp2nextreg since VKINT != VNONRELOC.
            if !matches!(ec.kind, ExpKind::Int | ExpKind::Float) || ec.has_jumps() {
                let r = fs.exp_to_reg(&ec);
                ec = ExpDesc::new(ExpKind::NonReloc, r as i64);
            }
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_ADD + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let val = if is_add { ec.info.wrapping_add(e2.exp.info) } else { ec.info.wrapping_sub(e2.exp.info) };
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                }
                (ExpKind::Float, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f = f64::from_bits(ec.info as u64);
                    let val = if is_add { f + (e2.exp.info as f64) } else { f - (e2.exp.info as f64) };
                    if !val.is_nan() && val != 0.0 {
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else {
                        let r = fs.exp_to_reg(&ec);
                        let k = fs.int_k(e2.exp.info);
                        if k <= 255 {
                            let op = if is_add { OpCode::ADDK } else { OpCode::SUBK };
                            let pc = fs.code_abc(op, r, r, k);
                            fs.code_abc(OpCode::MMBINK, r, k, if is_add { 6 } else { 7 });
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                        } else {
                            let r2 = fs.exp_to_reg(&e2.exp);
                            let op = if is_add { OpCode::ADD } else { OpCode::SUB };
                            let pc = fs.code_abc(op, r, r, r2);
                            fs.code_abc(OpCode::MMBIN, r, r2, if is_add { 6 } else { 7 });
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                        }
                    }
                }
                (ExpKind::Int, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f = f64::from_bits(e2.exp.info as u64);
                    let val = if is_add { (ec.info as f64) + f } else { (ec.info as f64) - f };
                    if !val.is_nan() && val != 0.0 {
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else {
                        let r = fs.exp_to_reg(&ec);
                        let k = fs.float_k(f);
                        if k <= 255 {
                            let op = if is_add { OpCode::ADDK } else { OpCode::SUBK };
                            let pc = fs.code_abc(op, r, r, k);
                            fs.code_abc(OpCode::MMBINK, r, k, if is_add { 6 } else { 7 });
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                        } else {
                            let r2 = fs.exp_to_reg(&e2.exp);
                            let op = if is_add { OpCode::ADD } else { OpCode::SUB };
                            let pc = fs.code_abc(op, r, r, r2);
                            fs.code_abc(OpCode::MMBIN, r, r2, if is_add { 6 } else { 7 });
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                        }
                    }
                }
                (ExpKind::Float, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f1 = f64::from_bits(ec.info as u64);
                    let f2 = f64::from_bits(e2.exp.info as u64);
                    let val = if is_add { f1 + f2 } else { f1 - f2 };
                    if !val.is_nan() && val != 0.0 {
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else {
                        let r = fs.exp_to_reg(&ec);
                        let k = fs.float_k(f2);
                        if k <= 255 {
                            let op = if is_add { OpCode::ADDK } else { OpCode::SUBK };
                            let pc = fs.code_abc(op, r, r, k);
                            fs.code_abc(OpCode::MMBINK, r, k, if is_add { 6 } else { 7 });
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                        } else {
                            let r2 = fs.exp_to_reg(&e2.exp);
                            let op = if is_add { OpCode::ADD } else { OpCode::SUB };
                            let pc = fs.code_abc(op, r, r, r2);
                            fs.code_abc(OpCode::MMBIN, r, r2, if is_add { 6 } else { 7 });
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                        }
                    }
                }
                (ExpKind::Int, _) => {
                    if is_add {
                        let r2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                            e2.exp.info as i32
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        if fits_sc(&ec) {
                            let sc = int_to_sc(ec.info);
                            // Like C's finishbinexpval: generate ADDI with A=0,
                            // free e2's register, then VRELOC (no result register).
                            let pc = fs.code_abc(OpCode::ADDI, 0, r2, sc);
                            // Free e2's register if it's a temp - like C's freeexps
                            if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                            fs.code_abc_k(OpCode::MMBINI, r2, sc, 6, true);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        } else {
                            let k = fs.int_k(ec.info);
                            if k <= 255 {
                                // Like C's finishbinexpval: generate ADDK with A=0,
                                // free e2's register, then VRELOC (no result register).
                                let pc = fs.code_abc(OpCode::ADDK, 0, r2, k);
                                // Free e2's register if it's a temp - like C's freeexps
                                if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                                fs.code_abc_k(OpCode::MMBINK, r2, k, 6, true);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                            } else {
                                // Like C's codebinNoK with flip=1: swap back to original order
                                // (ec was the left operand constant, r2 was e2's register)
                                // C swaps e1/e2 back before calling codebinexpval, so we
                                // generate ADD 0, r(ec), r2(e2) + MMBIN r, r2
                                let r = fs.exp_to_reg(&ec);
                                let pc = fs.code_abc(OpCode::ADD, 0, r, r2);
                                // Free registers in descending order - like C's freeexps
                                if r > r2 {
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                } else {
                                    if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                }
                                // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                                fs.code_abc(OpCode::MMBIN, r, r2, 6);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                            }
                        }
                    } else {
                        // !is_add with Int ec: use SUB (not SUBK, since e2 is not a constant)
                        // Like C's codebinexpval: process e2 first, then e1
                        let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                            e2.exp.info as i32
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let v1 = fs.exp_to_reg(&ec);
                        let pc = fs.code_abc(OpCode::SUB, 0, v1, v2);
                        // Free registers in descending order - like C's freeexps
                        if v1 > v2 {
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        } else {
                            if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc(OpCode::MMBIN, v1, v2, 7);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    }
                }
                (ExpKind::Float, _) => {
                    // Like C's codecommutative: swap operands for commutative ops
                    if is_add {
                        // Swap: put e2 (non-Float) on the left, Float on the right
                        // Like C's codearith -> codebinK: use ADDK + MMBINK
                        let r2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                            e2.exp.info as i32
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let f = f64::from_bits(ec.info as u64);
                        let k = fs.float_k(f);
                        if k <= 255 {
                            // Like C's finishbinexpval: generate ADDK with A=0,
                            // free e2's register, then VRELOC (no result register).
                            let pc = fs.code_abc(OpCode::ADDK, 0, r2, k);
                            // Free e2's register if it's a temp - like C's freeexps
                            if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                            // flip=1 because operands were swapped
                            fs.code_abc_k(OpCode::MMBINK, r2, k, 6, true);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        } else {
                            // Like C's codebinNoK with flip=1: swap back to original order
                            // (ec was the left operand Float constant, r2 was e2's register)
                            // C swaps e1/e2 back before calling codebinexpval, so we
                            // generate ADD 0, r(ec), r2(e2) + MMBIN r, r2
                            let r = fs.exp_to_reg(&ec);
                            let pc = fs.code_abc(OpCode::ADD, 0, r, r2);
                            // Free registers in descending order - like C's freeexps
                            if r > r2 {
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            } else {
                                if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            }
                            fs.code_abc(OpCode::MMBIN, r, r2, 6);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        }
                    } else {
                        // Subtraction: Float - something, not commutative
                        // Like C's codebinexpval: process e2 first, then e1
                        let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                            e2.exp.info as i32
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let v1 = fs.exp_to_reg(&ec);
                        let pc = fs.code_abc(OpCode::SUB, 0, v1, v2);
                        // Free registers in descending order - like C's freeexps
                        if v1 > v2 {
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        } else {
                            if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        }
                        fs.code_abc(OpCode::MMBIN, v1, v2, 7);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    }
                }
                _ => {
                    // Like C's codebinexpval: process e2 first, then e1
                    if !is_add && matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) && fits_sc_neg(e2.exp.info) && !e2.exp.has_jumps() {
                        // SUB with small negative Int: use ADDI
                        let v1 = if ec.has_jumps() {
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
                            if ec.info2 >= 0 {
                                fs.exp_to_reg(&ec)
                            } else {
                                ec.info as i32
                            }
                        } else {
                            fs.exp_to_reg(&ec)
                        };
                        let v = e2.exp.info;
                        let sc_neg = int_to_sc(-v);
                        let sc_pos = int_to_sc(v);
                        let pc = fs.code_abc(OpCode::ADDI, 0, v1, sc_neg);
                        // Free v1 if it's a temp - like C's freeexps
                        if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc(OpCode::MMBINI, v1, sc_pos, 7);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    } else if is_add && matches!(e2.exp.kind, ExpKind::Int) && fits_sc(&e2.exp) && !e2.exp.has_jumps() {
                        // ADD with small Int: use ADDI
                        let v1 = if ec.has_jumps() {
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
                            if ec.info2 >= 0 {
                                fs.exp_to_reg(&ec)
                            } else {
                                ec.info as i32
                            }
                        } else {
                            fs.exp_to_reg(&ec)
                        };
                        let v = e2.exp.info;
                        let sc = int_to_sc(v);
                        let pc = fs.code_abc(OpCode::ADDI, 0, v1, sc);
                        // Free v1 if it's a temp - like C's freeexps
                        if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc(OpCode::MMBINI, v1, sc, 6);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    } else if is_add && matches!(e2.exp.kind, ExpKind::Int) && !e2.exp.has_jumps() {
                        // Int constant doesn't fit SC, try ADDK (like C's codearith → codebinK)
                        let k = fs.int_k(e2.exp.info);
                        if k <= 255 {
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            let pc = fs.code_abc(OpCode::ADDK, 0, v1, k);
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            fs.code_abc_k(OpCode::MMBINK, v1, k, 6, false);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        } else {
                            // Constant table index too large, fall back to ADD
                            // Like C's codebinexpval: process e2 first, then e1
                            let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                                let reg = e2.exp.info as i32;
                                if e2.exp.info2 >= 0 {
                                    fs.set_a(e2.exp.info2, reg);
                                }
                                reg
                            } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                                e2.exp.info as i32
                            } else {
                                fs.exp_to_reg(&e2.exp)
                            };
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            let pc = fs.code_abc(OpCode::ADD, 0, v1, v2);
                            // Free registers in descending order - like C's freeexps
                            if v1 > v2 {
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            } else {
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            }
                            fs.code_abc(OpCode::MMBIN, v1, v2, 6);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        }
                    } else if !is_add && matches!(e2.exp.kind, ExpKind::Int) && !e2.exp.has_jumps() {
                        // Int constant doesn't fit SC, try SUBK (like C's codearith → codebinK)
                        let k = fs.int_k(e2.exp.info);
                        if k <= 255 {
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            let pc = fs.code_abc(OpCode::SUBK, 0, v1, k);
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            fs.code_abc_k(OpCode::MMBINK, v1, k, 7, false);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        } else {
                            // Constant table index too large, fall back to SUB
                            // Like C's codebinexpval: process e2 first, then e1
                            let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                                let reg = e2.exp.info as i32;
                                if e2.exp.info2 >= 0 {
                                    fs.set_a(e2.exp.info2, reg);
                                }
                                reg
                            } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                                e2.exp.info as i32
                            } else {
                                fs.exp_to_reg(&e2.exp)
                            };
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            let pc = fs.code_abc(OpCode::SUB, 0, v1, v2);
                            // Free registers in descending order - like C's freeexps
                            if v1 > v2 {
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            } else {
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            }
                            fs.code_abc(OpCode::MMBIN, v1, v2, 7);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        }
                    } else if is_add && matches!(e2.exp.kind, ExpKind::Float) && !e2.exp.has_jumps() {
                        let f = f64::from_bits(e2.exp.info as u64);
                        let k = fs.float_k(f);
                        if k <= 255 {
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            let pc = fs.code_abc(OpCode::ADDK, 0, v1, k);
                            // Free v1 if it's a temp - like C's freeexps
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                            fs.code_abc_k(OpCode::MMBINK, v1, k, 6, false);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        } else {
                            // Float constant doesn't fit K, fall back to ADD
                            // Like C's codebinexpval: process e2 first, then e1
                            let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                                let reg = e2.exp.info as i32;
                                if e2.exp.info2 >= 0 {
                                    fs.set_a(e2.exp.info2, reg);
                                }
                                reg
                            } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                                e2.exp.info as i32
                            } else {
                                fs.exp_to_reg(&e2.exp)
                            };
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            let pc = fs.code_abc(OpCode::ADD, 0, v1, v2);
                            // Free registers in descending order - like C's freeexps
                            if v1 > v2 {
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            } else {
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            }
                            // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                            fs.code_abc(OpCode::MMBIN, v1, v2, 6);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        }
                    } else if !is_add && matches!(e2.exp.kind, ExpKind::Float) && !e2.exp.has_jumps() {
                        let f = f64::from_bits(e2.exp.info as u64);
                        let k = fs.float_k(f);
                        if k <= 255 {
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            let pc = fs.code_abc(OpCode::SUBK, 0, v1, k);
                            // Free v1 if it's a temp - like C's freeexps
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                            fs.code_abc_k(OpCode::MMBINK, v1, k, 7, false);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        } else {
                            // Float constant doesn't fit K, fall back to SUB
                            // Like C's codebinexpval: process e2 first, then e1
                            let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                                let reg = e2.exp.info as i32;
                                if e2.exp.info2 >= 0 {
                                    fs.set_a(e2.exp.info2, reg);
                                }
                                reg
                            } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                                e2.exp.info as i32
                            } else {
                                fs.exp_to_reg(&e2.exp)
                            };
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            let pc = fs.code_abc(OpCode::SUB, 0, v1, v2);
                            // Free registers in descending order - like C's freeexps
                            if v1 > v2 {
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            } else {
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            }
                            // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                            fs.code_abc(OpCode::MMBIN, v1, v2, 7);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        }
                    } else {
                        // General ADD/SUB: Like C's codebinexpval: process e2 first, then e1
                        let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                            e2.exp.info as i32
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let v1 = if ec.has_jumps() {
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
                            if ec.info2 >= 0 {
                                fs.exp_to_reg(&ec)
                            } else {
                                ec.info as i32
                            }
                        } else {
                            fs.exp_to_reg(&ec)
                        };
                        let op = if is_add { OpCode::ADD } else { OpCode::SUB };
                        let mm_tm = if is_add { 6 } else { 7 };
                        let pc = fs.code_abc(op, 0, v1, v2);
                        // Free registers in descending order - like C's freeexps
                        if v1 > v2 {
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        } else {
                            if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc(OpCode::MMBIN, v1, v2, mm_tm);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
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
            // Like C's luaK_infix: if e1 is not a numeral, put it in a register
            if !matches!(ec.kind, ExpKind::Int | ExpKind::Float) {
                let r = fs.exp_to_reg(&ec);
                ec = ExpDesc::new(ExpKind::NonReloc, r as i64);
            }
            fs.ls_mut().next();
            let mut e2 = parse_subexpr(fs, PREC_MUL + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    if is_idiv {
                        let denom = e2.exp.info;
                        if denom != 0 {
                            let q = ec.info / denom;
                            let val = if (ec.info ^ denom) < 0 && ec.info % denom != 0 { q - 1 } else { q };
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::IDIVK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 12);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::IDIV, r, r, r2);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
                    } else if is_div {
                        if e2.exp.info != 0 {
                            let val = ec.info as f64 / e2.exp.info as f64;
                            if !val.is_nan() && val != 0.0 {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.int_k(e2.exp.info);
                                let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 11);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                            // Free e1's register if it's a temp - like C's freeexps
                            if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            fs.code_abc(OpCode::MMBINK, r, k, 11);
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                        }
                    } else if is_mul {
                        let val = ec.info.wrapping_mul(e2.exp.info);
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                    } else {
                        let m = ec.info;
                        let n = e2.exp.info;
                        if n != 0 {
                            let r = m % n;
                            let val = if r != 0 && (r ^ n) < 0 { r + n } else { r };
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::MODK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 9);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
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
                        if !val.is_nan() && val != 0.0 {
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::MULK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 8);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::MUL, r, r, r2);
                                fs.code_abc(OpCode::MMBIN, r, r2, 8);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
                    } else if is_div {
                        if e2.exp.info != 0 {
                            let val = f / (e2.exp.info as f64);
                            if !val.is_nan() && val != 0.0 {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.int_k(e2.exp.info);
                                if k <= 255 {
                                    let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                                    // Free e1's register if it's a temp - like C's freeexps
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    fs.code_abc(OpCode::MMBINK, r, k, 11);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                } else {
                                    let r2 = fs.exp_to_reg(&e2.exp);
                                    let pc = fs.code_abc(OpCode::DIV, r, r, r2);
                                    fs.code_abc(OpCode::MMBIN, r, r2, 11);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                }
                            }
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                            // Free e1's register if it's a temp - like C's freeexps
                            if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                fs.free_reg();
                            }
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
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.int_k(e2.exp.info);
                                if k <= 255 {
                                    let pc = fs.code_abc(OpCode::IDIVK, r, r, k);
                                    // Free e1's register if it's a temp - like C's freeexps
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    fs.code_abc(OpCode::MMBINK, r, k, 12);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                } else {
                                    let r2 = fs.exp_to_reg(&e2.exp);
                                    let pc = fs.code_abc(OpCode::IDIV, r, r, r2);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                }
                            }
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.int_k(e2.exp.info);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::IDIVK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 12);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::IDIV, r, r, r2);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
                    } else {
                        // MOD: Like C's constfolding, fold Float % Int when valid
                        let n = e2.exp.info;
                        if n != 0 {
                            let val = f % (n as f64);
                            // C's luaV_modf sign correction
                            let val = if val != 0.0 && (f.signum() as i64 * n.signum() as i64) < 0 { val + n as f64 } else { val };
                            if !val.is_nan() && val != 0.0 {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                // Can't fold (result is NaN or 0.0), generate MODK/MOD
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.int_k(n);
                                if k <= 255 {
                                    let pc = fs.code_abc(OpCode::MODK, 0, r, k);
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    fs.code_abc_k(OpCode::MMBINK, r, k, 9, false);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                                } else {
                                    let v2 = fs.exp_to_reg(&e2.exp);
                                    let pc = fs.code_abc(OpCode::MOD, 0, r, v2);
                                    if r > v2 {
                                        if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                        if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                    } else {
                                        if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                        if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                    }
                                    fs.code_abc(OpCode::MMBIN, r, v2, 9);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                                }
                            }
                        } else {
                            // Divisor is 0, generate MODK/MOD (runtime error)
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.int_k(n);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::MODK, 0, r, k);
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc_k(OpCode::MMBINK, r, k, 9, false);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                            } else {
                                let v2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::MOD, 0, r, v2);
                                if r > v2 {
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                    if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                } else {
                                    if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                }
                                fs.code_abc(OpCode::MMBIN, r, v2, 9);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                            }
                        }
                    }
                }
                (ExpKind::Int, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f = f64::from_bits(e2.exp.info as u64);
                    if is_mul {
                        let val = (ec.info as f64) * f;
                        if !val.is_nan() && val != 0.0 {
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.float_k(f);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::MULK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 8);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::MUL, r, r, r2);
                                fs.code_abc(OpCode::MMBIN, r, r2, 8);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
                    } else if is_div {
                        if f != 0.0 {
                            let val = (ec.info as f64) / f;
                            if !val.is_nan() && val != 0.0 {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.float_k(f);
                                if k <= 255 {
                                    let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                                    // Free e1's register if it's a temp - like C's freeexps
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    fs.code_abc(OpCode::MMBINK, r, k, 11);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                } else {
                                    let r2 = fs.exp_to_reg(&e2.exp);
                                    let pc = fs.code_abc(OpCode::DIV, r, r, r2);
                                    fs.code_abc(OpCode::MMBIN, r, r2, 11);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                }
                            }
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.float_k(f);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 11);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
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
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.float_k(f);
                                if k <= 255 {
                                    let pc = fs.code_abc(OpCode::IDIVK, r, r, k);
                                    // Free e1's register if it's a temp - like C's freeexps
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    fs.code_abc(OpCode::MMBINK, r, k, 12);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                } else {
                                    let r2 = fs.exp_to_reg(&e2.exp);
                                    let pc = fs.code_abc(OpCode::IDIV, r, r, r2);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                }
                            }
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.float_k(f);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::IDIVK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 12);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::IDIV, r, r, r2);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
                    } else {
                        // MOD: Like C's constfolding, fold Int % Float when valid
                        if f != 0.0 {
                            let m = ec.info as f64;
                            let val = m % f;
                            // C's luaV_modf sign correction
                            let val = if val != 0.0 && (m.signum() as i64 * f.signum() as i64) < 0 { val + f } else { val };
                            if !val.is_nan() && val != 0.0 {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                // Can't fold (result is NaN or 0.0), generate MODK/MOD
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.float_k(f);
                                if k <= 255 {
                                    let pc = fs.code_abc(OpCode::MODK, 0, r, k);
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    fs.code_abc_k(OpCode::MMBINK, r, k, 9, false);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                                } else {
                                    let v2 = fs.exp_to_reg(&e2.exp);
                                    let pc = fs.code_abc(OpCode::MOD, 0, r, v2);
                                    if r > v2 {
                                        if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                        if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                    } else {
                                        if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                        if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                    }
                                    fs.code_abc(OpCode::MMBIN, r, v2, 9);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                                }
                            }
                        } else {
                            // Divisor is 0, generate MODK/MOD (runtime error)
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.float_k(f);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::MODK, 0, r, k);
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc_k(OpCode::MMBINK, r, k, 9, false);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                            } else {
                                let v2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::MOD, 0, r, v2);
                                if r > v2 {
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                    if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                } else {
                                    if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                }
                                fs.code_abc(OpCode::MMBIN, r, v2, 9);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                            }
                        }
                    }
                }
                (ExpKind::Float, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let f1 = f64::from_bits(ec.info as u64);
                    let f2 = f64::from_bits(e2.exp.info as u64);
                    if is_mul {
                        let val = f1 * f2;
                        if !val.is_nan() && val != 0.0 {
                            e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.float_k(f2);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::MULK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 8);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::MUL, r, r, r2);
                                fs.code_abc(OpCode::MMBIN, r, r2, 8);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
                    } else if is_div {
                        if f2 != 0.0 {
                            let val = f1 / f2;
                            if !val.is_nan() && val != 0.0 {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.float_k(f2);
                                if k <= 255 {
                                    let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                                    // Free e1's register if it's a temp - like C's freeexps
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    fs.code_abc(OpCode::MMBINK, r, k, 11);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                } else {
                                    let r2 = fs.exp_to_reg(&e2.exp);
                                    let pc = fs.code_abc(OpCode::DIV, r, r, r2);
                                    fs.code_abc(OpCode::MMBIN, r, r2, 11);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                }
                            }
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.float_k(f2);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::DIVK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 11);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
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
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.float_k(f2);
                                if k <= 255 {
                                    let pc = fs.code_abc(OpCode::IDIVK, r, r, k);
                                    // Free e1's register if it's a temp - like C's freeexps
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    fs.code_abc(OpCode::MMBINK, r, k, 12);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                } else {
                                    let r2 = fs.exp_to_reg(&e2.exp);
                                    let pc = fs.code_abc(OpCode::IDIV, r, r, r2);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                                }
                            }
                        } else {
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.float_k(f2);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::IDIVK, r, r, k);
                                // Free e1's register if it's a temp - like C's freeexps
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc(OpCode::MMBINK, r, k, 12);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            } else {
                                let r2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::IDIV, r, r, r2);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(r as i64, pc) };
                            }
                        }
                    } else {
                        // MOD: Like C's constfolding, fold Float % Float when valid
                        if f2 != 0.0 {
                            let val = f1 % f2;
                            // C's luaV_modf sign correction
                            let val = if val != 0.0 && (f1.signum() as i64 * f2.signum() as i64) < 0 { val + f2 } else { val };
                            if !val.is_nan() && val != 0.0 {
                                e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                            } else {
                                // Can't fold (result is NaN or 0.0), generate MODK/MOD
                                let r = fs.exp_to_reg(&ec);
                                let k = fs.float_k(f2);
                                if k <= 255 {
                                    let pc = fs.code_abc(OpCode::MODK, 0, r, k);
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                        fs.free_reg();
                                    }
                                    fs.code_abc_k(OpCode::MMBINK, r, k, 9, false);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                                } else {
                                    let v2 = fs.exp_to_reg(&e2.exp);
                                    let pc = fs.code_abc(OpCode::MOD, 0, r, v2);
                                    if r > v2 {
                                        if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                        if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                    } else {
                                        if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                        if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                    }
                                    fs.code_abc(OpCode::MMBIN, r, v2, 9);
                                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                                }
                            }
                        } else {
                            // Divisor is 0, generate MODK/MOD (runtime error)
                            let r = fs.exp_to_reg(&ec);
                            let k = fs.float_k(f2);
                            if k <= 255 {
                                let pc = fs.code_abc(OpCode::MODK, 0, r, k);
                                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                fs.code_abc_k(OpCode::MMBINK, r, k, 9, false);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                            } else {
                                let v2 = fs.exp_to_reg(&e2.exp);
                                let pc = fs.code_abc(OpCode::MOD, 0, r, v2);
                                if r > v2 {
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                    if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                } else {
                                    if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 { fs.free_reg(); }
                                    if r >= fs.nvarstack() && r == fs.freereg - 1 { fs.free_reg(); }
                                }
                                fs.code_abc(OpCode::MMBIN, r, v2, 9);
                                e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                            }
                        }
                    }
                }
                (ExpKind::Int, _) if is_mul => {
                    let r2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        if e2.exp.info2 >= 0 {
                            fs.set_a(e2.exp.info2, reg);
                        }
                        reg
                    } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                        e2.exp.info as i32
                    } else {
                        let r = fs.exp_to_reg(&e2.exp);
                        e2.exp.kind = ExpKind::NonReloc;
                        e2.exp.info = r as i64;
                        e2.exp.info2 = -1;
                        r
                    };
                    let k = fs.int_k(ec.info);
                    if k <= 255 {
                        // Like C's finishbinexpval: generate MULK with A=0,
                        // free e2's register, allocate result register, set A.
                        let pc = fs.code_abc(OpCode::MULK, 0, r2, k);
                        // Free e2's register if it's a temp (not in varstack) - like C's freeexps
                        // C's dischargevars converts VCALL to VNONRELOC, then freeexps frees it.
                        // In Rust, Call expressions keep their kind, so we need to handle them too.
                        if matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call) && r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                            fs.free_reg();
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && r2 == fs.freereg - 1 && r2 >= fs.nvarstack() {
                            fs.free_reg();
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc_k(OpCode::MMBINK, r2, k, 8, true);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    } else {
                        // Like C's codebinNoK with flip=1: swap back to original order
                        // (ec was the left operand Int constant, r2 was e2's register)
                        // C swaps e1/e2 back before calling codebinexpval, so we
                        // generate MUL 0, r(ec), r2(e2) + MMBIN r, r2
                        let r = fs.exp_to_reg(&ec);
                        let pc = fs.code_abc(OpCode::MUL, 0, r, r2);
                        // Free registers in descending order - like C's freeexps/freeregs
                        if r > r2 {
                            if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        } else {
                            if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc(OpCode::MMBIN, r, r2, 8);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    }
                }
                (ExpKind::Float, _) if is_mul => {
                    let r2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        if e2.exp.info2 >= 0 {
                            fs.set_a(e2.exp.info2, reg);
                        }
                        reg
                    } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                        e2.exp.info as i32
                    } else {
                        let r = fs.exp_to_reg(&e2.exp);
                        e2.exp.kind = ExpKind::NonReloc;
                        e2.exp.info = r as i64;
                        e2.exp.info2 = -1;
                        r
                    };
                    let f = f64::from_bits(ec.info as u64);
                    let k = fs.float_k(f);
                    if k <= 255 {
                        // Like C's finishbinexpval: generate MULK with A=0,
                        // free e2's register, allocate result register, set A.
                        let pc = fs.code_abc(OpCode::MULK, 0, r2, k);
                        // Free e2's register if it's a temp (not in varstack) - like C's freeexps
                        // C's dischargevars converts VCALL to VNONRELOC, then freeexps frees it.
                        // In Rust, Call expressions keep their kind, so we need to handle them too.
                        if matches!(e2.exp.kind, ExpKind::NonReloc | ExpKind::Call) && r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                            fs.free_reg();
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && r2 == fs.freereg - 1 && r2 >= fs.nvarstack() {
                            fs.free_reg();
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc_k(OpCode::MMBINK, r2, k, 8, true);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    } else {
                        // Like C's codebinNoK with flip=1: swap back to original order
                        // (ec was the left operand Float constant, r2 was e2's register)
                        // C swaps e1/e2 back before calling codebinexpval, so we
                        // generate MUL 0, r(ec), r2(e2) + MMBIN r, r2
                        let r = fs.exp_to_reg(&ec);
                        let pc = fs.code_abc(OpCode::MUL, 0, r, r2);
                        // Free registers in descending order - like C's freeexps/freeregs
                        if r > r2 {
                            if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        } else {
                            if r2 >= fs.nvarstack() && r2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        }
                        // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                        fs.code_abc(OpCode::MMBIN, r, r2, 8);
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    }
                }
                _ => {
                    // Like C's codecommutative: for MUL, if e1 is a numeric
                    // constant, swap operands to try to use K variant.
                    let mut flip = false;
                    if is_mul && !ec.has_jumps()
                        && matches!(ec.kind, ExpKind::Int | ExpKind::Float)
                        && !matches!(e2.exp.kind, ExpKind::Int | ExpKind::Float)
                    {
                        std::mem::swap(&mut ec, &mut e2.exp);
                        flip = true;
                    }
                    // Like C's isSHint/isSCint: only use K variant if e2 has no jumps
                    let k_idx = if !e2.exp.has_jumps() {
                        match &e2.exp.kind {
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
                        }
                    } else {
                        None
                    };
                    if is_idiv {
                        if let Some(k) = k_idx {
                            // IDIVK path: process e1 to get v1
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            // generate IDIVK 0, v1, k + MMBINK v1, k, 12
                            let pc = fs.code_abc(OpCode::IDIVK, 0, v1, k);
                            fs.code_abc_k(OpCode::MMBINK, v1, k, 12, flip);
                            // Free v1's register if it's a temp - like C's freeexps
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        } else {
                            // IDIV path: process e2 first to get v2, then e1 to get v1
                            let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                                let reg = e2.exp.info as i32;
                                if e2.exp.info2 >= 0 {
                                    fs.set_a(e2.exp.info2, reg);
                                }
                                reg
                            } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                                e2.exp.info as i32
                            } else {
                                fs.exp_to_reg(&e2.exp)
                            };
                            let v1 = if ec.has_jumps() {
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
                                if ec.info2 >= 0 {
                                    fs.exp_to_reg(&ec)
                                } else {
                                    ec.info as i32
                                }
                            } else {
                                fs.exp_to_reg(&ec)
                            };
                            // generate IDIV 0, v1, v2 + MMBIN v1, v2, 12
                            let pc = fs.code_abc(OpCode::IDIV, 0, v1, v2);
                            fs.code_abc(OpCode::MMBIN, v1, v2, 12);
                            // Free registers in descending order - like C's freeexps
                            if v1 > v2 {
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            } else {
                                if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                                if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                    fs.free_reg();
                                }
                            }
                            e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                        }
                    } else if let Some(k) = k_idx {
                        // MULK/DIVK/MODK path: process e1 to get v1
                        let v1 = if ec.has_jumps() {
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
                            if ec.info2 >= 0 {
                                fs.exp_to_reg(&ec)
                            } else {
                                ec.info as i32
                            }
                        } else {
                            fs.exp_to_reg(&ec)
                        };
                        // generate op 0, v1, k + MMBINK v1, k, tm
                        let op = if is_mul { OpCode::MULK } else if is_div { OpCode::DIVK } else { OpCode::MODK };
                        let pc = fs.code_abc(op, 0, v1, k);
                        let tm = if is_mul { 8 } else if is_div { 11 } else { 9 };
                        fs.code_abc_k(OpCode::MMBINK, v1, k, tm, flip);
                        // Free v1's register if it's a temp - like C's freeexps
                        if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    } else {
                        // MUL/DIV/MOD path: process e2 first to get v2, then e1 to get v1
                        let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                            let reg = e2.exp.info as i32;
                            if e2.exp.info2 >= 0 {
                                fs.set_a(e2.exp.info2, reg);
                            }
                            reg
                        } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                            e2.exp.info as i32
                        } else {
                            fs.exp_to_reg(&e2.exp)
                        };
                        let v1 = if ec.has_jumps() {
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
                            if ec.info2 >= 0 {
                                fs.exp_to_reg(&ec)
                            } else {
                                ec.info as i32
                            }
                        } else {
                            fs.exp_to_reg(&ec)
                        };
                        // generate op 0, v1, v2 + MMBIN v1, v2, tm
                        let op = if is_mul { OpCode::MUL } else if is_div { OpCode::DIV } else { OpCode::MOD };
                        let pc = fs.code_abc(op, 0, v1, v2);
                        let tm = if is_mul { 8 } else if is_div { 11 } else { 9 };
                        fs.code_abc(OpCode::MMBIN, v1, v2, tm);
                        // Free registers in descending order - like C's freeexps
                        if v1 > v2 {
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        } else {
                            if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                                fs.free_reg();
                            }
                        }
                        e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                    }
                }
            }
            matched = true;
        }
        
        if limit <= PREC_POW && check(fs, &Token::Caret) {
            let mut ec = e.exp.clone();
            // Like C's luaK_infix: if e1 is not a numeral, put it in a register
            if !matches!(ec.kind, ExpKind::Int | ExpKind::Float) {
                let r = fs.exp_to_reg(&ec);
                ec = ExpDesc::new(ExpKind::NonReloc, r as i64);
            }
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_POW);
            let mut pow_no_fold = false;
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let base = ec.info as f64;
                    let exp = e2.exp.info;
                    let result = base.powi(exp as i32);
                    // Like C's constfolding: don't fold if result is NaN or 0.0
                    if !result.is_nan() && result != 0.0 {
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, result.to_bits() as i64) };
                    } else {
                        pow_no_fold = true;
                    }
                }
                (ExpKind::Float, ExpKind::Int) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let base = f64::from_bits(ec.info as u64);
                    let exp = e2.exp.info;
                    let result = base.powi(exp as i32);
                    // Like C's constfolding: don't fold if result is NaN or 0.0
                    if !result.is_nan() && result != 0.0 {
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, result.to_bits() as i64) };
                    } else {
                        pow_no_fold = true;
                    }
                }
                (ExpKind::Int, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let base = ec.info as f64;
                    let exp = f64::from_bits(e2.exp.info as u64);
                    let result = base.powf(exp);
                    // Like C's constfolding: don't fold if result is NaN or 0.0
                    if !result.is_nan() && result != 0.0 {
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, result.to_bits() as i64) };
                    } else {
                        pow_no_fold = true;
                    }
                }
                (ExpKind::Float, ExpKind::Float) if !ec.has_jumps() && !e2.exp.has_jumps() => {
                    let base = f64::from_bits(ec.info as u64);
                    let exp = f64::from_bits(e2.exp.info as u64);
                    let result = base.powf(exp);
                    // Like C's constfolding: don't fold if result is NaN or 0.0
                    if !result.is_nan() && result != 0.0 {
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, result.to_bits() as i64) };
                    } else {
                        pow_no_fold = true;
                    }
                }
                _ => {
                    pow_no_fold = true;
                }
            }
            if pow_no_fold {
                // Like C's isSHint/isSCint: only use K variant if e2 has no jumps
                let k_idx = if !e2.exp.has_jumps() {
                    match &e2.exp.kind {
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
                    }
                } else {
                    None
                };
                if let Some(k) = k_idx {
                    // e2 is a K operand - like C's codebinK
                    // Like C's finishbinexpval: e1 must be in register
                    let v1 = if ec.has_jumps() {
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
                        if ec.info2 >= 0 {
                            fs.exp_to_reg(&ec)
                        } else {
                            ec.info as i32
                        }
                    } else {
                        fs.exp_to_reg(&ec)
                    };
                    let pc = fs.code_abc(OpCode::POWK, 0, v1, k);
                    // Free v1 if it's a temp - like C's freeexps
                    if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc_k(OpCode::MMBINK, v1, k, 10, false);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                } else {
                    // e2 is not a K operand - like C's codebinNoK -> codebinexpval
                    // Like C: process e2 first (luaK_exp2anyreg(fs, e2))
                    let v2 = if matches!(e2.exp.kind, ExpKind::NonReloc) && !e2.exp.has_jumps() {
                        let reg = e2.exp.info as i32;
                        if e2.exp.info2 >= 0 {
                            fs.set_a(e2.exp.info2, reg);
                        }
                        reg
                    } else if matches!(e2.exp.kind, ExpKind::Relocable) && !e2.exp.has_jumps() && e2.exp.info2 < 0 {
                        e2.exp.info as i32
                    } else {
                        fs.exp_to_reg(&e2.exp)
                    };
                    // Like C's finishbinexpval: process e1 (luaK_exp2anyreg(fs, e1))
                    let v1 = if ec.has_jumps() {
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
                        if ec.info2 >= 0 {
                            fs.exp_to_reg(&ec)
                        } else {
                            ec.info as i32
                        }
                    } else {
                        fs.exp_to_reg(&ec)
                    };
                    let pc = fs.code_abc(OpCode::POW, 0, v1, v2);
                    // Free registers in descending order - like C's freeexps
                    if v1 > v2 {
                        if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                    } else {
                        if v2 >= fs.nvarstack() && v2 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        if v1 >= fs.nvarstack() && v1 == fs.freereg - 1 {
                            fs.free_reg();
                        }
                    }
                    // Like C: don't alloc result reg, use VRELOC (info=0, info2=pc)
                    fs.code_abc(OpCode::MMBIN, v1, v2, 10);
                    e = ExprItem { exp: ExpDesc::new_reloc_with_pc(0, pc) };
                }
            }
            matched = true;
        }
        
        if !matched {
            break;
        }
    }
    
    fs.leavelevel();
    e
}

/// ANTLR4: 检查比较运算符 token: `==` | `~=` | `<` | `<=` | `>` | `>=`
fn check_compare(fs: &FuncState) -> bool {
    matches!(fs.ls().token, Token::EqEq | Token::TildeEq | Token::Lt | Token::LtEq | Token::Gt | Token::GtEq)
}

/// Try to convert an expression to an integer constant value.
/// Like C's tonumeral + luaV_tointegerns with LUA_FLOORN2I.
fn to_int_const(e: &ExpDesc) -> Option<i64> {
    if e.has_jumps() { return None; }
    match e.kind {
        ExpKind::Int => Some(e.info),
        ExpKind::Float => {
            let f = f64::from_bits(e.info as u64);
            let i = f as i64;
            if i as f64 == f { Some(i) } else { None }
        }
        _ => None,
    }
}

const OFFSET_SC: i64 = 127;

/// 判断整型常量是否适合 SC 参数编码 (i8 范围内)
fn fits_sc(desc: &ExpDesc) -> bool {
    if let ExpKind::Int = desc.kind {
        let v = desc.info;
        // C's fitsC: range -127 to 128
        v >= -127 && v <= 128
    } else {
        false
    }
}

/// 判断整型常量取负后是否适合 SC 编码
fn fits_sc_neg(v: i64) -> bool {
    // C's fitsC: range -127 to 128, and -v must also fit
    v >= -127 && v <= 128 && -v >= -127 && -v <= 128
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
            // C's fitsC: (unsigned)i + OFFSET_sC <= MAXARG_C, where OFFSET_sC=127, MAXARG_C=255
            // So range is -127 to 128
            if v >= -127 && v <= 128 {
                Some(v)
            } else {
                None
            }
        }
        ExpKind::Float => {
            let f = f64::from_bits(desc.info as u64);
            // Like C's luaV_flttointeger with F2Ieq: floor(f) == f (exact equality)
            let fl = f.floor();
            if fl == f {
                let i = fl as i64;
                // C's fitsC: range -127 to 128
                if i >= -127 && i <= 128 {
                    Some(i)
                } else {
                    None
                }
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
            // Like C's isSCnumber: if float can be exactly converted to integer in sC range,
            // it's an SC number (use EQI), not a K constant
            let fl = f.floor();
            if fl == f {
                let i = fl as i64;
                if i >= -127 && i <= 128 {
                    return None;  // It's an SC number, not a K constant
                }
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

/// Code access to a global variable via _ENV: _ENV[name].
/// Used by parse_simple_exp for both explicit global declarations and implicit globals.
fn code_global_via_env(fs: &mut FuncState, name: &str) -> ExpDesc {
    let k = fs.string_k(name);
    // Like C's singlevar + luaK_indexed: resolve _ENV as local, upvalue, or implicit
    let is_short_str = name.len() <= crate::strings::LUAI_MAXSHORTLEN
        && (k as u32) <= crate::opcodes::MAXINDEXRK;
    let env_local_ex = fs.find_local_ex("_ENV");
    let env_upval = if env_local_ex.is_none() {
        match fs.find_upvalue("_ENV") {
            Some(UpvalueOrCtc::Upvalue(idx)) => Some(idx),
            _ => None,
        }
    } else { None };
    let (r, pc) = if let Some((env_reg, kind)) = env_local_ex {
        let is_vvargvar = kind == RDKVAVAR;
        if is_vvargvar {
            // _ENV 是命名 vararg 参数（VVARGVAR）：键必须在寄存器中。
            // 匹配 C 的 luaK_indexed + VVARGIND discharge：
            // 1. LOADK 加载键到 kr
            // 2. 释放 kr（freeregs）
            // 3. 生成 GETVARG A=0（VRELOC），后续 exp_to_reg 会复用 kr 并 patch A
            // 4. luaK_finish 将 GETVARG 转为 GETTABLE（因为有 PF_VATAB）
            let kr = fs.alloc_reg();
            fs.code_abx(OpCode::LOADK, kr, k);
            // Free key register (like C's freeregs in VVARGIND discharge)
            if kr >= fs.nvarstack() && kr == fs.freereg - 1 {
                fs.free_reg();
            }
            // Generate GETVARG with A=0 (relocatable), like C compiler
            let pc = fs.code_abc(OpCode::GETVARG, 0, env_reg, kr);
            return ExpDesc::new_reloc_with_pc(kr as i64, pc);
        } else if is_short_str {
            // _ENV is a local variable in current function: use GETFIELD
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

/// ANTLR4: `simpleExp: 'nil' | 'false' | 'true' | NUMBER | STRING | '...' | tableconstructor | 'function' funcbody | prefixexp ;` 以及 `unop expr`
fn parse_simple_exp(fs: &mut FuncState) -> ExprItem {
    let mut e = match &fs.ls().token {
        Token::Nil => {
            fs.ls_mut().next();
            return ExprItem { exp: ExpDesc::new(ExpKind::Nil, 0) };
        }
        Token::True => {
            fs.ls_mut().next();
            return ExprItem { exp: ExpDesc::new(ExpKind::Boolean, 1) };
        }
        Token::False => {
            fs.ls_mut().next();
            return ExprItem { exp: ExpDesc::new(ExpKind::Boolean, 0) };
        }
        Token::Int(v) => {
            let val = *v;
            fs.ls_mut().next();
            return ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
        }
        Token::Float(v) => {
            let val = *v;
            fs.ls_mut().next();
            return ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
        }
        Token::String(s) => {
            let s = s.clone();
            fs.ls_mut().next();
            return ExprItem { exp: ExpDesc::new_str(s) };
        }
        Token::DotDotDot => {
            fs.ls_mut().next();
            // Like C: '...' always creates VVARARG, regardless of named vararg params.
            // Named vararg params (RDKVAVAR) are accessed by their name, not by '...'.
            // init_exp(v, VVARARG, luaK_codeABC(fs, OP_VARARG, 0, fs->f->numparams, 1));
            // A=0 (placeholder, set later by setoneret/setreturns), B=numparams, C=1 (multret)
            let numparams = fs.proto.num_params as i32;
            let pc = fs.code_abc(OpCode::VARARG, 0, numparams, 1);
            // info stores PC (like C's u.info), info2 also stores PC for set_c
            ExpDesc { kind: ExpKind::Vararg, info: pc as i64, info2: pc, t: NO_JUMP, f: NO_JUMP, str_val: None }
        }
        Token::LBrace => {
            let (r, _n) = parse_constructor(fs);
            return ExprItem { exp: ExpDesc::new(ExpKind::NonReloc, r as i64) };
        }
        Token::Name(name) => {
            let name = name.clone();
            fs.ls_mut().next();
            // 匹配 C 的 buildvar/searchvar：
            // 1. 查找 local（包括 CTC）
            // 2. 查找具名 global 声明（如 `global a`），优先于 upvalue
            // 3. 查找 upvalue（`global *` 不阻止 upvalue 查找）
            // 4. 未找到则作为全局变量（`global *` 或隐式全局）通过 _ENV[name] 访问
            if let Some(ctc) = fs.find_local_ctc(&name) {
                ctc
            } else if let Some((reg, kind)) = fs.find_local_ex(&name) {
                if kind == RDKVAVAR {
                    ExpDesc::new(ExpKind::VVARGVAR, reg as i64)
                } else {
                    ExpDesc::new(ExpKind::NonReloc, reg as i64)
                }
            } else if let Some(_kind) = fs.find_named_global_decl(&name) {
                // 具名 global 声明（如 `global a`）：优先于 upvalue，通过 _ENV[name] 访问
                code_global_via_env(fs, &name)
            } else if let Some(result) = fs.find_upvalue(&name) {
                match result {
                    UpvalueOrCtc::Upvalue(upval_idx) => {
                        // Like C's singlevar returning VUPVAL: delay GETUPVAL emission.
                        // GETUPVAL is emitted when the value is needed (e.g., in expr_to_reg).
                        ExpDesc { kind: ExpKind::Upval, info: upval_idx as i64, info2: 0, t: NO_JUMP, f: NO_JUMP, str_val: None }
                    }
                    UpvalueOrCtc::CtcConst(ctc) => ctc,
                }
            } else if name == "_ENV" {
                if let Some(env_reg) = fs.find_local("_ENV") {
                    ExpDesc::new(ExpKind::NonReloc, env_reg as i64)
                } else {
                    // _ENV is an upvalue (not a local). Return ExpKind::Upval to delay
                    // GETUPVAL emission, matching C's singlevar returning VUPVAL.
                    // The LBracket/Dot handlers will emit GETUPVAL at the right time.
                    // Use find_upvalue to get the correct upvalue index (not hardcoded 0).
                    let env_idx = match fs.find_upvalue("_ENV") {
                        Some(UpvalueOrCtc::Upvalue(idx)) => idx,
                        _ => 0, // fallback: should not happen for _ENV
                    };
                    ExpDesc { kind: ExpKind::Upval, info: env_idx as i64, info2: 0, t: NO_JUMP, f: NO_JUMP, str_val: None }
                }
            } else {
                // 全局变量（`global *` 或隐式全局）通过 _ENV[name] 访问
                code_global_via_env(fs, &name)
            }
        }
        Token::LParen => {
            fs.ls_mut().next();
            let ei = parse_expr(fs);
            expect(fs, &Token::RParen);
            // Like C's primaryexp: luaK_dischargevars(ls->fs, v);
            // For VCALL/Vararg: setoneret sets C=2 (1 result) and converts to
            // VNONRELOC (Call) or VRELOC (Vararg). This ensures `(...)` returns
            // exactly one value, not multret.
            match ei.exp.kind {
                ExpKind::Call => {
                    let call_pc = ei.exp.info2;
                    if call_pc >= 0 {
                        setarg(&mut fs.proto.code[call_pc as usize], 2, POS_C, SIZE_C);
                    }
                    ExpDesc { kind: ExpKind::NonReloc, info: ei.exp.info, info2: -1, t: NO_JUMP, f: NO_JUMP, str_val: None }
                }
                ExpKind::Vararg => {
                    // Like C's setoneret for VVARARG: SETARG_C(pc, 2), then VRELOC
                    let pc = ei.exp.info2;
                    if pc >= 0 {
                        fs.set_c(pc, 2);
                    }
                    ExpDesc { kind: ExpKind::Relocable, info: ei.exp.info, info2: pc, t: NO_JUMP, f: NO_JUMP, str_val: None }
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
                            // Like C's codenot: discharge2anyreg (no jump resolution),
                            // freeexp, code NOT, set VRELOC, swap t/f, removevalues.
                            let r = fs.discharge_to_any_reg(&ei.exp);
                            // freeexp: free the register if it's a temp at top of stack
                            if r >= fs.nvarstack() && r == fs.freereg - 1 {
                                fs.free_reg();
                            }
                            let pc = fs.code_abc(OpCode::NOT, 0, r, 0);
                            let mut e = ExpDesc::new_reloc_with_pc(0, pc);
                            // Swap t/f (NOT inverts truthiness)
                            e.t = ei.exp.f;
                            e.f = ei.exp.t;
                            fs.remove_values(e.t);
                            fs.remove_values(e.f);
                            e
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
                                ExpDesc::new(ExpKind::Int, ei.exp.info.wrapping_neg())
                            }
                            ExpKind::Float => {
                                let f = f64::from_bits(ei.exp.info as u64);
                                let result = -f;
                                if result.is_nan() || result == 0.0 {
                                    let r = fs.exp_to_reg(&ei.exp);
                                    let pc = fs.code_abc(OpCode::UNM, 0, r, 0);
                                    fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                                    ExpDesc::new_reloc_with_pc(r as i64, pc)
                                } else {
                                    ExpDesc::new(ExpKind::Float, result.to_bits() as i64)
                                }
                            }
                            _ => {
                                let r = fs.exp_to_reg(&ei.exp);
                                let pc = fs.code_abc(OpCode::UNM, 0, r, 0);
                                fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                                ExpDesc::new_reloc_with_pc(r as i64, pc)
                            }
                        }
                    }
                }
                Token::Hash => {
                    // Like C's codeunexpval: exp2anyreg + freeexp + codeABC(A=0) + VRELOC
                    let r = fs.exp_to_reg(&ei.exp);
                    fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                    let pc = fs.code_abc(OpCode::LEN, 0, r, 0);
                    ExpDesc::new_reloc_with_pc(r as i64, pc)
                }
                Token::Tilde => {
                    if ei.exp.has_jumps() {
                        let r = fs.exp_to_reg(&ei.exp);
                        let pc = fs.code_abc(OpCode::BNOT, 0, r, 0);
                        fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                        ExpDesc::new_reloc_with_pc(r as i64, pc)
                    } else {
                        match ei.exp.kind {
                            ExpKind::Int => {
                                ExpDesc::new(ExpKind::Int, !(ei.exp.info))
                            }
                            ExpKind::Float => {
                                // Like C's constfolding: convert float to int, then BNOT
                                let f = f64::from_bits(ei.exp.info as u64);
                                let fi = f as i64;
                                if (fi as f64) == f {
                                    ExpDesc::new(ExpKind::Int, !fi)
                                } else {
                                    let r = fs.exp_to_reg(&ei.exp);
                                    let pc = fs.code_abc(OpCode::BNOT, 0, r, 0);
                                    fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                                    ExpDesc::new_reloc_with_pc(r as i64, pc)
                                }
                            }
                            _ => {
                                let r = fs.exp_to_reg(&ei.exp);
                                let pc = fs.code_abc(OpCode::BNOT, 0, r, 0);
                                fs.free_exp_reg(&ExpDesc::new(ExpKind::NonReloc, r as i64));
                                ExpDesc::new_reloc_with_pc(r as i64, pc)
                            }
                        }
                    }
                }
                _ => {
                    let r = fs.exp_to_reg(&ei.exp);
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
                // Handle VVARGVAR: like C's luaK_indexed + VVARGIND discharge
                if e.kind == ExpKind::VVARGVAR {
                    let base_reg = e.info as i32;
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);  // load key into register
                    // Free key register (like C's freeregs in VVARGIND discharge)
                    if kr >= fs.nvarstack() && kr == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Generate GETVARG with A=0 (relocatable), like C compiler
                    let pc = fs.code_abc(OpCode::GETVARG, 0, base_reg, kr);
                    e = ExpDesc::new_reloc_with_pc(kr as i64, pc);
                    continue;
                }
                let base_reg = fs.exp_to_reg(&e);
                let is_short_str = is_kstr(fs, k);
                // Check if the last instruction is GETUPVAL (any upvalue index, not just _ENV)
                let can_revert_getupval = fs.pc > 0 && is_short_str && {
                    let last_ins = fs.proto.code[fs.pc as usize - 1];
                    get_opcode(last_ins) == OpCode::GETUPVAL && getarg_a(last_ins) == base_reg
                };
                if can_revert_getupval {
                    // Revert: remove the GETUPVAL instruction, free the register, use GETTABUP
                    let last_idx = fs.pc as usize - 1;
                    let last_ins = fs.proto.code[last_idx];
                    let uv_idx = getarg_b(last_ins);
                    fs.proto.code.remove(last_idx);
                    fs.inst_lines.remove(last_idx);
                    fs.pc -= 1;
                    if base_reg >= fs.nvarstack() && base_reg == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    let pc = code_gettabup(fs, 0, uv_idx, k);
                    e = ExpDesc::new_reloc_with_pc(0, pc);
                } else if fs.pc > 0 && {
                    let last_ins = fs.proto.code[fs.pc as usize - 1];
                    get_opcode(last_ins) == OpCode::GETUPVAL && getarg_b(last_ins) == 0
                } && !is_short_str {
                    // _ENV upval with non-short key: keep GETUPVAL, use GETTABLE
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);
                    let inst_pc = fs.code_abc(OpCode::GETTABLE, base_reg, base_reg, kr);
                    fs.free_reg(); // kr
                    e = ExpDesc { kind: ExpKind::NonReloc, info: base_reg as i64, info2: inst_pc, t: NO_JUMP, f: NO_JUMP, str_val: None };
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
                // Handle VVARGVAR: like C's luaK_indexed + VVARGIND discharge
                if e.kind == ExpKind::VVARGVAR {
                    let base_reg = e.info as i32;
                    let ei = parse_expr(fs);
                    expect(fs, &Token::RBracket);
                    // Like C's luaK_exp2anyreg: if key is already in a local register, use it directly
                    let key_reg = if ei.exp.kind == ExpKind::NonReloc && (ei.exp.info as i32) < fs.nvarstack() {
                        ei.exp.info as i32
                    } else {
                        fs.exp_to_next_reg(&ei.exp)
                    };
                    // Free key register (like C's freeregs in VVARGIND discharge)
                    if key_reg >= fs.nvarstack() && key_reg == fs.freereg - 1 {
                        fs.free_reg();
                    }
                    // Generate GETVARG with A=0 (relocatable), like C compiler
                    let pc = fs.code_abc(OpCode::GETVARG, 0, base_reg, key_reg);
                    e = ExpDesc::new_reloc_with_pc(key_reg as i64, pc);
                    continue;
                }
                // Check if table is an upvalue (like C's VUPVAL in luaK_indexed)
                // C compiler flow: yindex(expr + luaK_exp2val) → luaK_indexed
                // luaK_exp2val emits code for comparisons but NOT for simple exprs (VTRUE).
                // So the order depends on key type:
                // - Comparison key: key load code → GETUPVAL (luaK_exp2val emits first)
                // - Simple key (true, nil, etc.): GETUPVAL → key load code (luaK_exp2anyreg emits after)
                // - Short string key: GETTABUP (no GETUPVAL needed)
                let table_is_upvalue = matches!(e.kind, ExpKind::Upval);
                let table_upval_idx = e.info as i32;
                let mut base_reg = if table_is_upvalue {
                    -1  // placeholder: will be set after parsing key expression
                } else if matches!(e.kind, ExpKind::Relocable | ExpKind::NonReloc) && !e.has_jumps() {
                    if e.info2 >= 0 {
                        fs.set_a(e.info2, e.info as i32);
                    }
                    e.info as i32
                } else {
                    fs.exp_to_reg(&e)
                };
                let mut base_is_nonreloc_local = !table_is_upvalue
                    && matches!(e.kind, ExpKind::NonReloc) && (e.info as i32) < fs.nvarstack();
                // Parse the index expression (like C's yindex)
                let ei = parse_expr(fs);
                expect(fs, &Token::RBracket);
                // For upvalue tables: handle like C's suffixedexp [ handler
                // C flow: luaK_exp2anyregup (no-op for VUPVAL) → yindex (parse key + luaK_exp2val)
                // → luaK_indexed (emit GETUPVAL for non-isKstr keys, or use VINDEXUP for isKstr)
                // → later dischargevars: freeregs(t, idx) → emit GETTABLE as VRELOC(A=0)
                if table_is_upvalue {
                    if ei.exp.kind == ExpKind::Str {
                        let k = fs.get_str_k(&ei.exp);
                        if is_kstr(fs, k) {
                            // Short string key: use GETTABUP directly (C's VINDEXUP)
                            let pc = code_gettabup(fs, 0, table_upval_idx, k);
                            e = ExpDesc::new_reloc_with_pc(0, pc);
                            continue;
                        }
                    }
                    // Not a short string key. Order depends on whether luaK_exp2val
                    // would emit code (i.e., key has jumps like a comparison).
                    // VJMP also counts as having jumps because the JMP PC is stored
                    // in info, not in t/f lists.
                    let key_has_jumps = ei.exp.has_jumps() || ei.exp.kind == ExpKind::VJMP;
                    if key_has_jumps {
                        // Comparison: luaK_exp2val emits key load code first,
                        // then luaK_indexed emits GETUPVAL.
                        // So: key load code → GETUPVAL → GETTABLE (as relocatable)
                        let key_reg = fs.exp_to_reg(&ei.exp);
                        let r = fs.alloc_reg();
                        fs.code_abc(OpCode::GETUPVAL, r, table_upval_idx, 0);
                        base_reg = r;
                        // Emit GETTABLE as relocatable (A=0), like C's dischargevars for VINDEXED
                        let inst_pc = fs.code_abc(OpCode::GETTABLE, 0, base_reg, key_reg);
                        // Free both table and key registers (high register first), like C's freeregs
                        // This ensures freereg drops to nactvar, so the relocatable GETTABLE
                        // will be assigned the correct register when discharged
                        let (high_reg, low_reg) = if base_reg > key_reg {
                            (base_reg, key_reg)
                        } else {
                            (key_reg, base_reg)
                        };
                        if high_reg >= fs.nvarstack() && high_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        if low_reg >= fs.nvarstack() && low_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        e = ExpDesc::new_reloc_with_pc(0, inst_pc);
                        continue;
                    }
                    // Simple expression (VTRUE, etc.) or constant: luaK_exp2val doesn't emit code,
                    // so luaK_indexed emits GETUPVAL first, then luaK_exp2anyreg for key.
                    // So: GETUPVAL → key load code → GETTABLE/GETI
                    // Exception: VUPVAL key. C's luaK_exp2val calls dischargevars which
                    // emits a relocatable GETUPVAL (A=0) for the key BEFORE luaK_indexed
                    // emits GETUPVAL for the table. So the instruction order is:
                    //   key GETUPVAL (relocatable) → table GETUPVAL (relocatable)
                    // then register allocation sets A fields: table→reg0, key→reg1.
                    // Handle VUPVAL key specially to match this order.
                    let key_upval_idx = if ei.exp.kind == ExpKind::Upval {
                        Some(ei.exp.info as i32)
                    } else {
                        None
                    };
                    let key_getupval_pc = if let Some(idx) = key_upval_idx {
                        // Emit key GETUPVAL as relocatable (A=0), like C's dischargevars for VUPVAL
                        Some(fs.code_abc(OpCode::GETUPVAL, 0, idx, 0))
                    } else {
                        None
                    };
                    // Emit table GETUPVAL (like C's luaK_indexed for VUPVAL with non-isKstr key)
                    let r = fs.alloc_reg();
                    fs.code_abc(OpCode::GETUPVAL, r, table_upval_idx, 0);
                    base_reg = r;
                    base_is_nonreloc_local = false;
                    // Handle key based on type
                    if ei.exp.kind == ExpKind::Int
                        && ei.exp.info >= 0
                        && ei.exp.info <= ((1u32 << SIZE_C) - 1) as i64
                    {
                        // Int key: GETI (result in base_reg, like C)
                        let inst_pc = fs.code_abc(
                            OpCode::GETI, base_reg, base_reg, ei.exp.info as i32,
                        );
                        e = ExpDesc {
                            kind: ExpKind::NonReloc, info: base_reg as i64,
                            info2: inst_pc, t: NO_JUMP, f: NO_JUMP, str_val: None,
                        };
                        continue;
                    } else if ei.exp.kind == ExpKind::Str {
                        // Long string key: LOADK → GETTABLE (relocatable) → freeregs
                        let k = fs.get_str_k(&ei.exp);
                        let kr = fs.alloc_reg();
                        fs.code_abx(OpCode::LOADK, kr, k);
                        let inst_pc = fs.code_abc(OpCode::GETTABLE, 0, base_reg, kr);
                        // Free both table and key registers (high first)
                        let (high_reg, low_reg) = if base_reg > kr {
                            (base_reg, kr)
                        } else {
                            (kr, base_reg)
                        };
                        if high_reg >= fs.nvarstack() && high_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        if low_reg >= fs.nvarstack() && low_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        e = ExpDesc::new_reloc_with_pc(0, inst_pc);
                        continue;
                    } else if let Some(key_pc) = key_getupval_pc {
                        // VUPVAL key: key GETUPVAL was already emitted (relocatable).
                        // Allocate key register and set A field, like C's luaK_exp2anyreg
                        // for VRELOC: alloc_reg + set_a.
                        let key_reg = fs.alloc_reg();
                        fs.set_a(key_pc, key_reg);
                        let inst_pc = fs.code_abc(OpCode::GETTABLE, 0, base_reg, key_reg);
                        // Free both table and key registers (high first)
                        let (high_reg, low_reg) = if base_reg > key_reg {
                            (base_reg, key_reg)
                        } else {
                            (key_reg, base_reg)
                        };
                        if high_reg >= fs.nvarstack() && high_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        if low_reg >= fs.nvarstack() && low_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        e = ExpDesc::new_reloc_with_pc(0, inst_pc);
                        continue;
                    } else {
                        // Other key (VTRUE, VNIL, etc.): load key → GETTABLE (relocatable) → freeregs
                        let key_reg = fs.exp_to_reg(&ei.exp);
                        let inst_pc = fs.code_abc(OpCode::GETTABLE, 0, base_reg, key_reg);
                        // Free both table and key registers (high first)
                        let (high_reg, low_reg) = if base_reg > key_reg {
                            (base_reg, key_reg)
                        } else {
                            (key_reg, base_reg)
                        };
                        if high_reg >= fs.nvarstack() && high_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        if low_reg >= fs.nvarstack() && low_reg == fs.freereg - 1 {
                            fs.free_reg();
                        }
                        e = ExpDesc::new_reloc_with_pc(0, inst_pc);
                        continue;
                    }
                }
                {
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
                    } else if ei.exp.kind == ExpKind::Str {
                        let k = fs.get_str_k(&ei.exp);
                        if is_kstr(fs, k) {
                            result_reg = if base_is_nonreloc_local {
                                fs.alloc_reg()
                            } else {
                                base_reg
                            };
                            inst_pc = code_getfield(fs, result_reg, base_reg, k);
                        } else {
                            // Long string key for non-upvalue table
                            result_reg = if base_is_nonreloc_local {
                                fs.alloc_reg()
                            } else {
                                base_reg
                            };
                            if result_reg != base_reg {
                                fs.code_abx(OpCode::LOADK, result_reg, k);
                                inst_pc = fs.code_abc(OpCode::GETTABLE, result_reg, base_reg, result_reg);
                            } else {
                                let kr = fs.alloc_reg();
                                fs.code_abx(OpCode::LOADK, kr, k);
                                inst_pc = fs.code_abc(OpCode::GETTABLE, result_reg, base_reg, kr);
                                fs.free_reg(); // kr
                            }
                        }
                    } else {
                        let key_reg = if matches!(ei.exp.kind, ExpKind::Relocable | ExpKind::NonReloc) && !ei.exp.has_jumps() {
                            if ei.exp.info2 >= 0 {
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
                            fs.exp_to_reg(&ei.exp)
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
    // Like C's ifstat: test_then_block (IF) + {test_then_block (ELSEIF)} + [ELSE block] END
    let mut escapelist = NO_JUMP;  // exit list for finished parts

    // First test_then_block (IF cond THEN block)
    fs.ls_mut().next();  // skip IF
    let mut if_jmp = parse_if_cond(fs, entry_freereg);  // cond
    expect(fs, &Token::Then);
    fs.set_freereg(entry_freereg);
    parse_block(fs);  // 'then' part
    // Like C's test_then_block: if followed by 'else'/'elseif', add jump to escapelist
    if check(fs, &Token::Else) || check(fs, &Token::Elseif) {
        let j = fs.jump();
        fs.concat_jump(&mut escapelist, j);
    }
    if if_jmp != NO_JUMP {
        fs.patch_to_here(if_jmp);
    }

    // Subsequent test_then_block (ELSEIF cond THEN block)
    while check(fs, &Token::Elseif) {
        fs.ls_mut().next();  // skip ELSEIF
        if_jmp = parse_if_cond(fs, entry_freereg);  // cond
        expect(fs, &Token::Then);
        fs.set_freereg(entry_freereg);
        parse_block(fs);  // 'then' part
        // Like C's test_then_block: if followed by 'else'/'elseif', add jump to escapelist
        if check(fs, &Token::Else) || check(fs, &Token::Elseif) {
            let j = fs.jump();
            fs.concat_jump(&mut escapelist, j);
        }
        if if_jmp != NO_JUMP {
            fs.patch_to_here(if_jmp);
        }
    }

    if check(fs, &Token::Else) {
        fs.ls_mut().next();
        fs.set_freereg(entry_freereg);
        parse_block(fs);  // 'else' part
    }
    expect(fs, &Token::End);
    // Like C's ifstat: patch escape list to 'if' end
    if escapelist != NO_JUMP {
        fs.patch_to_here(escapelist);
    }
}

/// Helper for parse_if: parse condition and return false-list (condtrue patched to here).
/// Like C's cond() + the condition handling in test_then_block.
fn parse_if_cond(fs: &mut FuncState, entry_freereg: i32) -> i32 {
    let mut ei = parse_expr(fs);
    // Like C's cond: 'falses' are all equal here
    if ei.exp.kind == ExpKind::Nil {
        ei.exp.kind = ExpKind::Boolean;
        ei.exp.info = 0;
    }

    // Like C's luaK_goiftrue: const true (VK/VKFLT/VKINT/VKSTR/VTRUE) → pc=NO_JUMP,
    // but still concat f list and patch t list to here.
    let is_const_true = matches!(ei.exp.kind, ExpKind::Boolean) && ei.exp.info != 0
        || matches!(ei.exp.kind, ExpKind::Int | ExpKind::Float | ExpKind::Str);

    let mut if_jmp = NO_JUMP;

    if is_const_true {
        // Like C's luaK_goiftrue for VTRUE/VK: pc = NO_JUMP (no new jump).
        // concat f with NO_JUMP (no-op), patch t list to here, return f.
        fs.patch_true_jumps(ei.exp.t, fs.pc);
        if_jmp = ei.exp.f;
    } else if ei.exp.kind == ExpKind::VJMP {
        let jmp_pc = ei.exp.info as i32;
        fs.negate_condition(jmp_pc);
        let mut false_list = ei.exp.f;
        fs.concat_jump(&mut false_list, jmp_pc);
        fs.patch_true_jumps(ei.exp.t, fs.pc);
        if_jmp = false_list;
    } else {
        // Like C's jumponcond: check for VRELOC+NOT first
        let is_not_vreloc = ei.exp.info2 >= 0
            && (ei.exp.info2 as usize) < fs.proto.code.len()
            && get_opcode(fs.proto.code[ei.exp.info2 as usize]) == OpCode::NOT
            && matches!(ei.exp.kind, ExpKind::Relocable);

        if is_not_vreloc {
            // VRELOC+NOT: remove NOT, emit TEST with inverted condition
            let not_inst = fs.proto.code[ei.exp.info2 as usize];
            let b = getarg_b(not_inst);
            fs.pc -= 1;
            fs.proto.code.pop();
            fs.inst_lines.pop();
            // goiftrue: cond=0, !cond=1 → k=true
            fs.code_abc_k(OpCode::TEST, b, 0, 0, true);
            let jmp_pc = fs.jump();
            let mut false_list = ei.exp.f;
            fs.concat_jump(&mut false_list, jmp_pc);
            fs.patch_true_jumps(ei.exp.t, fs.pc);
            if_jmp = false_list;
        } else {
            // Like C's jumponcond default: discharge2anyreg + freeexp + TESTSET(NO_REG) + JMP
            let pre_freereg = fs.freereg;
            let r = fs.discharge_to_any_reg(&ei.exp);
            // freeexp: free the register if it's a temp at top of stack
            if r >= fs.nvarstack() && r == fs.freereg - 1 {
                fs.free_reg();
            }
            // goiftrue: cond=0 → k=false
            fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, r, 0, false);
            let jmp_pc = fs.jump();
            let mut false_list = ei.exp.f;
            fs.concat_jump(&mut false_list, jmp_pc);
            fs.patch_true_jumps(ei.exp.t, fs.pc);
            if_jmp = false_list;
            if fs.freereg > pre_freereg {
                fs.free_reg();
            }
        }
    }
    let _ = entry_freereg;
    if_jmp
}

/// ANTLR4: `'while' expr 'do' block 'end' ;`
fn parse_while(fs: &mut FuncState) {
    let entry_freereg = fs.freereg;
    fs.ls_mut().next();
    let loop_start = fs.pc;
    fs.lasttarget = fs.pc;  // mark while start as jump target (like luaK_getlabel)
    let mut ei = parse_expr(fs);
    // Like C's cond: 'falses' are all equal here
    if ei.exp.kind == ExpKind::Nil {
        ei.exp.kind = ExpKind::Boolean;
        ei.exp.info = 0;
    }
    let pre_freereg = fs.freereg;
    
    let is_const_true = matches!(ei.exp.kind, ExpKind::Boolean if ei.exp.info != 0)
        || matches!(ei.exp.kind, ExpKind::Int | ExpKind::Float | ExpKind::Str);

    let condexit = if is_const_true {
        // Like C's luaK_goiftrue: always true; do nothing (NO_JUMP)
        fs.patch_true_jumps(ei.exp.t, fs.pc);
        NO_JUMP
    } else if ei.exp.kind == ExpKind::VJMP {
        // Handle VJMP like luaK_goiftrue in C: negate condition,
        // add JMP to false list, patch true list to here (body start)
        let saved_jmp = ei.exp.info as i32;
        fs.negate_condition(saved_jmp);
        fs.concat_jump(&mut ei.exp.f, saved_jmp);
        let here = fs.pc;
        fs.patch_true_jumps(ei.exp.t, here);
        ei.exp.f
    } else {
        // Like C's jumponcond: check for VRELOC+NOT first
        let is_not_vreloc = ei.exp.info2 >= 0
            && (ei.exp.info2 as usize) < fs.proto.code.len()
            && get_opcode(fs.proto.code[ei.exp.info2 as usize]) == OpCode::NOT
            && matches!(ei.exp.kind, ExpKind::Relocable);

        if is_not_vreloc {
            let not_inst = fs.proto.code[ei.exp.info2 as usize];
            let b = getarg_b(not_inst);
            fs.pc -= 1;
            fs.proto.code.pop();
            fs.inst_lines.pop();
            fs.code_abc_k(OpCode::TEST, b, 0, 0, true);
            let jmp = fs.jump();
            let mut false_list = ei.exp.f;
            fs.concat_jump(&mut false_list, jmp);
            fs.patch_true_jumps(ei.exp.t, fs.pc);
            false_list
        } else {
            let r = fs.discharge_to_any_reg(&ei.exp);
            if r >= fs.nvarstack() && r == fs.freereg - 1 {
                fs.free_reg();
            }
            fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, r, 0, false);
            let jmp = fs.jump();
            let mut false_list = ei.exp.f;
            fs.concat_jump(&mut false_list, jmp);
            fs.patch_true_jumps(ei.exp.t, fs.pc);
            if fs.freereg > pre_freereg {
                fs.free_reg();
            }
            false_list
        }
    };
    expect(fs, &Token::Do);

    let saved_breaklist = fs.break_list;
    fs.break_list = NO_JUMP;

    // Push outer block (while loop block, like C's enterblock with isloop=1)
    let saved_nlocals = fs.locals.len();
    let saved_nlabels = fs.labels.len();
    let saved_ngotos = fs.gotos.len();
    let entry_nactvar = fs.active_nactvar();
    let entry_reglevel = fs.reglevel_for_nactvar(entry_nactvar);
    let parent_insidetbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
    fs.block_stack.push(BlockEntry { saved_nlocals, saved_ngotos, has_upval: false, is_function_body: false, nactvar: entry_nactvar, reglevel: entry_reglevel, insidetbc: parent_insidetbc });
    fs.set_freereg(entry_freereg);

    parse_block(fs);  // inner body block (like C's block(ls))

    // JMP back to loop start (BEFORE outer leaveblock, like C's luaK_jumpto)
    fs.code_sj(OpCode::JMP, loop_start - fs.pc - 1, 0);

    // Leave outer block (while loop block, like C's leaveblock)
    // C's leaveblock order: CLOSE -> freereg -> removevars -> createlabel(break) -> solvegotos
    let has_upval = fs.current_block_has_upval();
    let block_entry = fs.block_stack.pop().unwrap();
    let has_tbc = fs.locals[saved_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
    let close_reg = fs.nvarstack_up_to(saved_nlocals);
    if has_tbc || has_upval {
        fs.code_abc(OpCode::CLOSE, close_reg, 0, 0);
    }
    fs.deactivate_locals_range(saved_nlocals);
    fs.set_freereg(close_reg);

    // Create break label AFTER CLOSE (like C's createlabel after CLOSE in leaveblock)
    fs.labels.push(LabelDesc {
        name: "break".to_string(),
        pc: fs.pc,
        nactvar: block_entry.nactvar,
        nlocals: saved_nlocals,
        reglevel: block_entry.reglevel,
        line: 0,
    });

    solve_gotos_for_block(fs, saved_nlabels, saved_nlocals, block_entry.saved_ngotos, has_tbc || has_upval, block_entry.nactvar, block_entry.reglevel);
    fs.break_list = saved_breaklist;

    // Patch condexit AFTER leaveblock (like C's luaK_patchtohere after leaveblock)
    fs.patch_to_here(condexit);

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
    let bl1_saved_ngotos = fs.gotos.len();
    let bl1_entry_nactvar = fs.active_nactvar();
    let bl1_entry_reglevel = fs.reglevel_for_nactvar(bl1_entry_nactvar);
    let parent_insidetbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
    fs.block_stack.push(BlockEntry { saved_nlocals: bl1_nlocals, saved_ngotos: bl1_saved_ngotos, has_upval: false, is_function_body: false, nactvar: bl1_entry_nactvar, reglevel: bl1_entry_reglevel, insidetbc: parent_insidetbc });

    // Push bl2 (scope block, like C's enterblock with isloop=0)
    let bl2_nlocals = fs.locals.len();
    let bl2_nlabels = fs.labels.len();
    let bl2_saved_ngotos = fs.gotos.len();
    let bl2_entry_nactvar = fs.active_nactvar();
    let bl2_entry_reglevel = fs.reglevel_for_nactvar(bl2_entry_nactvar);
    let parent_insidetbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
    fs.block_stack.push(BlockEntry { saved_nlocals: bl2_nlocals, saved_ngotos: bl2_saved_ngotos, has_upval: false, is_function_body: false, nactvar: bl2_entry_nactvar, reglevel: bl2_entry_reglevel, insidetbc: parent_insidetbc });

    fs.set_freereg(entry_freereg);
    parse_chunk_stmts(fs);  // Like C's statlist(ls)

    expect(fs, &Token::Until);

    // Parse condition INSIDE bl2 (like C's cond(ls) inside scope block)
    let mut ei = parse_expr(fs);
    // Like C's cond: 'falses' are all equal here
    if ei.exp.kind == ExpKind::Nil {
        ei.exp.kind = ExpKind::Boolean;
        ei.exp.info = 0;
    }

    // Handle condition BEFORE leaveblock (like C's cond → luaK_goiftrue)
    // This patches true jumps to current PC, which is where CLOSE will be emitted
    let is_const_true = matches!(ei.exp.kind, ExpKind::Boolean if ei.exp.info != 0)
        || matches!(ei.exp.kind, ExpKind::Int | ExpKind::Float | ExpKind::Str);

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
            // Like C's jumponcond: check for VRELOC+NOT first
            let is_not_vreloc3 = ei.exp.info2 >= 0
                && (ei.exp.info2 as usize) < fs.proto.code.len()
                && get_opcode(fs.proto.code[ei.exp.info2 as usize]) == OpCode::NOT
                && matches!(ei.exp.kind, ExpKind::Relocable);

            if is_not_vreloc3 {
                let not_inst = fs.proto.code[ei.exp.info2 as usize];
                let b = getarg_b(not_inst);
                fs.pc -= 1;
                fs.proto.code.pop();
                fs.inst_lines.pop();
                fs.code_abc_k(OpCode::TEST, b, 0, 0, true);
                let jmp_pc3 = fs.jump();
                let mut false_list3 = ei.exp.f;
                fs.concat_jump(&mut false_list3, jmp_pc3);
                fs.patch_true_jumps(ei.exp.t, fs.pc);
                condexit = false_list3;
            } else {
                let r = fs.discharge_to_any_reg(&ei.exp);
                if r >= fs.nvarstack() && r == fs.freereg - 1 {
                    fs.free_reg();
                }
                fs.code_abc_k(OpCode::TESTSET, NO_REG as i32, r, 0, false);
                let jmp_pc3 = fs.jump();
                let mut false_list3 = ei.exp.f;
                fs.concat_jump(&mut false_list3, jmp_pc3);
                fs.patch_true_jumps(ei.exp.t, fs.pc);
                condexit = false_list3;
                if fs.freereg > pre_freereg {
                    fs.free_reg();
                }
            }
        }
    }

    // Leave bl2 (finish scope, like C's leaveblock)
    // CLOSE will be emitted at current PC, which is where true jumps point
    let bl2_has_upval = fs.current_block_has_upval();
    let bl2_entry = fs.block_stack.pop().unwrap();
    let bl2_has_tbc = fs.locals[bl2_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
    let bl2_close_reg = fs.nvarstack_up_to(bl2_nlocals);
    if bl2_has_tbc || bl2_has_upval {
        fs.code_abc(OpCode::CLOSE, bl2_close_reg, 0, 0);
    }
    fs.deactivate_locals_range(bl2_nlocals);
    fs.set_freereg(bl2_close_reg);
    solve_gotos_for_block(fs, bl2_nlabels, bl2_nlocals, bl2_entry.saved_ngotos, bl2_has_tbc || bl2_has_upval, bl2_entry.nactvar, bl2_entry.reglevel);

    // If bl2 has upvalues, emit CLOSE fix (like C's repeatstat upvalue handling)
    if bl2_has_upval {
        let exit = fs.jump();  // normal exit must jump over fix
        // Patch condexit to here: repetition must close upvalues
        fs.patch_to_here(condexit);
        fs.code_abc(OpCode::CLOSE, bl2_close_reg, 0, 0);
        condexit = fs.jump();  // repeat after closing upvalues
        fs.fix_jump(exit, fs.pc, false);  // normal exit comes to here
    }

    // Patch condexit to loop_start (like C's luaK_patchlist)
    fs.patch_list(condexit, loop_start);

    // Leave bl1 (finish loop, like C's leaveblock)
    // C's leaveblock order: CLOSE -> freereg -> removevars -> createlabel(break) -> solvegotos
    let bl1_has_upval = fs.current_block_has_upval();
    let bl1_entry = fs.block_stack.pop().unwrap();
    let bl1_has_tbc = fs.locals[bl1_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
    let bl1_close_reg = fs.nvarstack_up_to(bl1_nlocals);
    if bl1_has_tbc || bl1_has_upval {
        fs.code_abc(OpCode::CLOSE, bl1_close_reg, 0, 0);
    }
    fs.deactivate_locals_range(bl1_nlocals);
    fs.set_freereg(bl1_close_reg);

    // Create break label AFTER CLOSE (like C's createlabel after CLOSE in leaveblock)
    fs.labels.push(LabelDesc {
        name: "break".to_string(),
        pc: fs.pc,
        nactvar: bl1_entry.nactvar,
        nlocals: bl1_nlocals,
        reglevel: bl1_entry.reglevel,
        line: 0,
    });

    solve_gotos_for_block(fs, bl1_nlabels, bl1_nlocals, bl1_entry.saved_ngotos, bl1_has_tbc || bl1_has_upval, bl1_entry.nactvar, bl1_entry.reglevel);
    fs.break_list = saved_breaklist;
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
        let forstat_saved_ngotos = fs.gotos.len();
        let forstat_entry_nactvar = fs.active_nactvar();
        let forstat_entry_reglevel = fs.reglevel_for_nactvar(forstat_entry_nactvar);
        let parent_insidetbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
        fs.block_stack.push(BlockEntry { saved_nlocals: forstat_nlocals, saved_ngotos: forstat_saved_ngotos, has_upval: false, is_function_body: false, nactvar: forstat_entry_nactvar, reglevel: forstat_entry_reglevel, insidetbc: parent_insidetbc });

        fs.set_freereg(base);
        let ei = parse_expr(fs);
        let init_r = fs.exp_to_reg(&ei.exp);
        if init_r != base {
            fs.code_abc(OpCode::MOVE, base, init_r, 0);
        }
        expect(fs, &Token::Comma);

        fs.set_freereg(base + 1);
        let ei2 = parse_expr(fs);
        let limit_r = fs.exp_to_reg(&ei2.exp);
        if limit_r != base + 1 {
            fs.code_abc(OpCode::MOVE, base + 1, limit_r, 0);
        }

        if check(fs, &Token::Comma) {
            fs.ls_mut().next();
            fs.set_freereg(base + 2);
            let ei3 = parse_expr(fs);
            let step_r = fs.exp_to_reg(&ei3.exp);
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
        let body_saved_ngotos = fs.gotos.len();
        let body_entry_nactvar = fs.active_nactvar();
        let body_entry_reglevel = fs.reglevel_for_nactvar(body_entry_nactvar);
        let parent_insidetbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
        fs.block_stack.push(BlockEntry { saved_nlocals: body_nlocals, saved_ngotos: body_saved_ngotos, has_upval: false, is_function_body: false, nactvar: body_entry_nactvar, reglevel: body_entry_reglevel, insidetbc: parent_insidetbc });

        // Like C's adjustlocalvars(ls, nvars): activate loop variable INSIDE body block
        fs.add_local_kind_reg(&name, fs.pc, RDKCONST, base + 2);

        // parse_block creates the inner block (like C's block() in forbody)
        parse_block(fs);

        // Leave body block (like C's leaveblock for forbody's block)
        let has_body_upval = fs.current_block_has_upval();
        let body_entry = fs.block_stack.pop().unwrap();
        let body_has_tbc = fs.locals[body_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
        let body_close_reg = fs.nvarstack_up_to(body_nlocals);
        if body_has_tbc || has_body_upval {
            fs.code_abc(OpCode::CLOSE, body_close_reg, 0, 0);
        }
        // C's leaveblock order: CLOSE -> freereg -> removevars -> solvegotos
        fs.deactivate_locals_range(body_nlocals);
        fs.set_freereg(body_close_reg);
        solve_gotos_for_block(fs, body_nlabels, body_nlocals, body_entry.saved_ngotos, body_has_tbc || has_body_upval, body_entry.nactvar, body_entry.reglevel);

        fs.fix_jump(prep, fs.pc, false);
        let loop_pc = fs.code_abx(OpCode::FORLOOP, base, 0);
        fs.fix_jump(loop_pc, prep + 1, true);

        // Handle forstat block exit (like C's leaveblock for forstat) [NUMERIC FOR]
        // C order: 1) CLOSE  2) removevars  3) freereg  4) createlabel(break)  5) solvegotos
        let has_forstat_upval = fs.current_block_has_upval();
        let forstat_has_tbc = fs.locals[forstat_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
        let forstat_close_reg = fs.nvarstack_up_to(forstat_nlocals);
        let forstat_entry = fs.block_stack.pop().unwrap();
        if has_forstat_upval || forstat_has_tbc {
            fs.code_abc(OpCode::CLOSE, forstat_close_reg, 0, 0);
        }
        // removevars + freereg (before createlabel and solvegotos, like C)
        fs.deactivate_locals_range(forstat_nlocals);
        fs.set_freereg(forstat_close_reg);

        // Create break label AFTER forstat CLOSE (like C's createlabel after CLOSE)
        fs.labels.push(LabelDesc {
            name: "break".to_string(),
            pc: fs.pc,
            nactvar: forstat_entry.nactvar,
            nlocals: forstat_nlocals,
            reglevel: forstat_entry.reglevel,
            line: 0,
        });

        solve_gotos_for_block(fs, forstat_nlabels, forstat_nlocals, forstat_entry.saved_ngotos, forstat_has_tbc || has_forstat_upval, forstat_entry.nactvar, forstat_entry.reglevel);
        fs.break_list = saved_breaklist;
        expect(fs, &Token::End);
    } else {
        let mut vars = vec![name];
        while check(fs, &Token::Comma) {
            fs.ls_mut().next();
            let var = get_name(fs);
            vars.push(var);
        }
        expect(fs, &Token::In);

        // C: line = ls->linenumber (line of 'in' keyword, used for luaK_fixline in forbody)
        let for_line = fs.ls().lastline;
        let saved_freereg = fs.freereg;
        let base = fs.freereg;

        // Push forstat block (like C's enterblock in forstat)
        let forstat_nlocals = fs.locals.len();
        let forstat_nlabels = fs.labels.len();
        let forstat_saved_ngotos = fs.gotos.len();
        let forstat_entry_nactvar = fs.active_nactvar();
        let forstat_entry_reglevel = fs.reglevel_for_nactvar(forstat_entry_nactvar);
        let parent_insidetbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
        fs.block_stack.push(BlockEntry { saved_nlocals: forstat_nlocals, saved_ngotos: forstat_saved_ngotos, has_upval: false, is_function_body: false, nactvar: forstat_entry_nactvar, reglevel: forstat_entry_reglevel, insidetbc: parent_insidetbc });

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
            let nactvar = fs.active_nactvar();
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
                nactvar,
                pidx: -1,
            });
        }
        // Deactivate internal variables too during expression parsing
        for lv in &mut fs.locals[forstat_nlocals..] {
            lv.active = false;
        }
        
        fs.set_freereg(base);
        let pc_before = fs.pc;
        let mut nexps = 0;
        let mut last_ei = parse_expr(fs);
        nexps += 1;
        while check(fs, &Token::Comma) {
            fs.ls_mut().next();
            // Like C's explist: put previous expression into next register
            fs.exp_to_next_reg(&last_ei.exp);
            last_ei = parse_expr(fs);
            nexps += 1;
        }

        // C: luaK_checkstack(fs, needed) in adjust_assign; needed = 4 - nexps
        fs.checkstack(4 - nexps);

        // Like C's adjust_assign: handle last expression BEFORE adjustlocalvars
        let mut last_is_call = false;
        let is_multret = matches!(last_ei.exp.kind, ExpKind::Call | ExpKind::Vararg);
        if is_multret {
            // Multi-return: adjust the call's C field
            let needed = (6 - nexps).max(1).min(255);
            // Find the CALL instruction and adjust its C field
            if fs.pc > pc_before {
                for i in (pc_before..fs.pc).rev() {
                    if get_opcode(fs.proto.code[i as usize]) == OpCode::CALL {
                        setarg(&mut fs.proto.code[i as usize], needed, POS_C, SIZE_C);
                        break;
                    }
                }
            }
            last_is_call = true;
        } else {
            // Single value: put into next register (like C's luaK_exp2nextreg in adjust_assign)
            fs.exp_to_next_reg(&last_ei.exp);
        }

        if !last_is_call && nexps < 4 {
            fs.code_nil(base + nexps, 4 - nexps);
        }

        // Like C's adjustlocalvars(ls, 3): activate only the 3 internal variables
        // AFTER adjust_assign (like C's order: adjust_assign then adjustlocalvars)
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

        // C: adjust_assign sets freereg = base + 4 (3 internal + 1 control var)
        // User-declared variables (beyond the control var) are activated later in forbody.
        fs.set_freereg(base + 4);
        fs.needclose = true;

        // C: luaK_checkstack(fs, 2);  /* extra space to call iterator */
        fs.checkstack(2);

        expect(fs, &Token::Do);

        let saved_breaklist = fs.break_list;
        fs.break_list = NO_JUMP;

        let prep = fs.code_abx(OpCode::TFORPREP, base, 0);
        fs.lasttarget = fs.pc;  // mark for body start as jump target (like luaK_getlabel)

        // Like C's forbody: enterblock BEFORE activating user-declared variables
        // body_nlocals = forstat_nlocals + 3 (only the 3 internal variables)
        let body_nlocals = fs.locals.len() - vars.len();  // Exclude user-declared vars
        let body_nlabels = fs.labels.len();
        let body_saved_ngotos = fs.gotos.len();
        let body_entry_nactvar = fs.active_nactvar();
        let body_entry_reglevel = fs.reglevel_for_nactvar(body_entry_nactvar);
        let parent_insidetbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
        fs.block_stack.push(BlockEntry { saved_nlocals: body_nlocals, saved_ngotos: body_saved_ngotos, has_upval: false, is_function_body: false, nactvar: body_entry_nactvar, reglevel: body_entry_reglevel, insidetbc: parent_insidetbc });

        // Like C's forbody: fs->freereg-- (TFORPREP removes one register from stack)
        fs.set_freereg(base + 4 - 1);  // base + 3

        // Like C's adjustlocalvars(ls, nvars): activate user-declared variables INSIDE body block
        // Also assign registers to them (like C's luaK_reserveregs)
        // C's adjustlocalvars calls registerlocalvar for each var, registering them to locvars
        for (i, lv) in fs.locals[body_nlocals..].iter_mut().enumerate() {
            lv.active = true;
            lv.reg = base + 3 + i as i32;
            // Register to proto.loc_vars (like C's registerlocalvar in adjustlocalvars)
            if lv.pidx < 0 && lv.kind <= RDKTOCLOSE {
                let p = fs.proto.loc_vars.len() as i32;
                fs.proto.loc_vars.push(LocVar {
                    varname: Some(crate::strings::LuaString::Short(std::sync::Arc::new(
                        crate::strings::ShortString { hash: 0, contents: lv.name.clone() }
                    ))),
                    start_pc: fs.pc,
                    end_pc: 0,
                });
                lv.pidx = p;
            }
        }

        // Like C's luaK_reserveregs(fs, nvars): reserve registers for user variables
        // nvars = ncontrol (number of user-declared variables including control var)
        fs.checkstack(ncontrol);
        fs.set_freereg(base + 3 + ncontrol);

        // parse_block creates the inner block (like C's block() in forbody)
        parse_block(fs);

        // Leave body block (like C's leaveblock for forbody's block) [GENERIC FOR]
        let has_body_upval = fs.current_block_has_upval();
        let body_entry = fs.block_stack.pop().unwrap();
        let body_has_tbc = fs.locals[body_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
        let body_close_reg = fs.nvarstack_up_to(body_nlocals);
        if body_has_tbc || has_body_upval {
            fs.code_abc(OpCode::CLOSE, body_close_reg, 0, 0);
        }
        // C's leaveblock order: CLOSE -> freereg -> removevars -> solvegotos
        fs.deactivate_locals_range(body_nlocals);
        fs.set_freereg(body_close_reg);
        solve_gotos_for_block(fs, body_nlabels, body_nlocals, body_entry.saved_ngotos, body_has_tbc || has_body_upval, body_entry.nactvar, body_entry.reglevel);

        fs.fix_jump(prep, fs.pc, false);
        fs.code_abc(OpCode::TFORCALL, base, 0, ncontrol);
        // Like C's forbody: luaK_fixline(fs, line) after TFORCALL
        fs.fixline(for_line);
        let loop_pc = fs.code_abx(OpCode::TFORLOOP, base, 0);
        fs.fix_jump(loop_pc, prep + 1, true);
        // Like C's forbody: luaK_fixline(fs, line) after FORLOOP
        fs.fixline(for_line);

        // Handle forstat block exit (like C's leaveblock for forstat) [GENERIC FOR]
        // C order: 1) CLOSE  2) removevars  3) freereg  4) createlabel(break)  5) solvegotos
        let has_forstat_upval = fs.current_block_has_upval();
        let forstat_has_tbc = fs.locals[forstat_nlocals..].iter().any(|l| l.kind == RDKTOCLOSE && l.active);
        let forstat_close_reg = fs.nvarstack_up_to(forstat_nlocals);
        let forstat_entry = fs.block_stack.pop().unwrap();
        if has_forstat_upval || forstat_has_tbc {
            fs.code_abc(OpCode::CLOSE, forstat_close_reg, 0, 0);
        }
        // removevars + freereg (before createlabel and solvegotos, like C)
        fs.deactivate_locals_range(forstat_nlocals);
        fs.set_freereg(forstat_close_reg);

        // Create break label AFTER forstat CLOSE (like C's createlabel after CLOSE)
        fs.labels.push(LabelDesc {
            name: "break".to_string(),
            pc: fs.pc,
            nactvar: forstat_entry.nactvar,
            nlocals: forstat_nlocals,
            reglevel: forstat_entry.reglevel,
            line: 0,
        });

        expect(fs, &Token::End);

        solve_gotos_for_block(fs, forstat_nlabels, forstat_nlocals, forstat_entry.saved_ngotos, forstat_has_tbc || has_forstat_upval, forstat_entry.nactvar, forstat_entry.reglevel);
        fs.break_list = saved_breaklist;
    }
}

/// ANTLR4: `'function' funcname funcbody ;`
fn parse_func_stat(fs: &mut FuncState) {
    // Like C's funcstat: save line for luaK_fixline after storevar
    let line = fs.ls().lastline;
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
        // Like C's searchvar: search from back to front, local variables
        // can shadow global declarations (e.g., `local f` shadows `global <const> *`).
        let local_result = fs.find_local_ex(name);
        let global_kind = if local_result.is_none() {
            fs.find_global_decl(name)
        } else {
            None
        };

        if let Some((reg, _kind)) = local_result {
            // Variable is a local (including locals that shadow global declarations):
            // store closure directly into the variable's register with MOVE.
            // No need to add name to constant pool (matches C's VLOCAL path).
            let r = parse_body_ex(fs, false, None);
            fs.code_abc(OpCode::MOVE, reg, r, 0);
            fs.free_reg();
            fs.fixline(line);
            return;
        }

        // Like C's singlevaraux: if not a local, check upvalues before falling
        // back to _ENV. An upvalue assignment generates SETUPVAL (not SETTABUP).
        // Skip upvalue check for names declared as global (global_kind.is_some()).
        if global_kind.is_none() {
            if let Some(UpvalueOrCtc::Upvalue(uv_idx)) = fs.find_upvalue(name) {
                let r = parse_body_ex(fs, false, None);
                fs.code_abc(OpCode::SETUPVAL, uv_idx, r, 0);
                fs.free_reg();
                fs.fixline(line);
                return;
            }
        }

        let k = fs.string_k(name);
        let is_short_str = name.len() <= crate::strings::LUAI_MAXSHORTLEN
            && (k as u32) <= crate::opcodes::MAXINDEXRK;

        // Like C's funcstat: for non-short-string keys, evaluate table and key
        // BEFORE parse_body_ex (which generates CLOSURE), matching C's evaluation
        // order: table -> key -> value (CLOSURE).
        let mut pre_eval: Option<(i32, i32)> = None; // (table_reg, key_reg)
        if !is_short_str {
            if global_kind.is_some() {
                if let Some(env_reg) = fs.find_local("_ENV") {
                    let env_r = fs.alloc_reg();
                    fs.code_abc(OpCode::MOVE, env_r, env_reg, 0);
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);
                    pre_eval = Some((env_r, kr));
                } else {
                    let env_r = fs.alloc_reg();
                    fs.code_abc(OpCode::GETUPVAL, env_r, 0, 0);
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);
                    pre_eval = Some((env_r, kr));
                }
            } else {
                // Like C's funcstat: resolve through _ENV
                if let Some(env_reg) = fs.find_local("_ENV") {
                    let env_r = fs.alloc_reg();
                    fs.code_abc(OpCode::MOVE, env_r, env_reg, 0);
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);
                    pre_eval = Some((env_r, kr));
                } else {
                    let env_r = fs.alloc_reg();
                    fs.code_abc(OpCode::GETUPVAL, env_r, 0, 0);
                    let kr = fs.alloc_reg();
                    fs.code_abx(OpCode::LOADK, kr, k);
                    pre_eval = Some((env_r, kr));
                }
            }
        }

        let r = parse_body_ex(fs, false, None);

        if let Some((table_reg, key_reg)) = pre_eval {
            fs.code_abc(OpCode::SETTABLE, table_reg, key_reg, r);
            fs.free_reg(); // free key_reg
            fs.free_reg(); // free table_reg
        } else if global_kind.is_some() {
            // Variable is declared as global: store through _ENV
            if let Some(env_reg) = fs.find_local("_ENV") {
                if is_short_str {
                    fs.code_abc(OpCode::SETFIELD, env_reg, k, r);
                } else {
                    unreachable!(); // handled by pre_eval
                }
            } else {
                if is_short_str {
                    code_settabup(fs, 0, k, r);
                } else {
                    unreachable!(); // handled by pre_eval
                }
            }
        } else {
            // Like C's funcstat: resolve through _ENV (which may be local or upvalue)
            if let Some(env_reg) = fs.find_local("_ENV") {
                // _ENV is a local variable: use SETFIELD
                if is_short_str {
                    fs.code_abc(OpCode::SETFIELD, env_reg, k, r);
                } else {
                    unreachable!(); // handled by pre_eval
                }
            } else {
                // _ENV is an upvalue: use SETTABUP
                if is_short_str {
                    code_settabup(fs, 0, k, r);
                } else {
                    unreachable!(); // handled by pre_eval
                }
            }
        }
        fs.free_reg();
        fs.fixline(line);
        return;
    }

    let first_name = &chain[0].1;
    // Like C's funcname: build the table expression, then store the closure.
    // C uses delayed expression evaluation (VINDEXSTR), generating GETFIELD lazily.
    // We simulate this by tracking the table register and freeing it before
    // allocating the result register for GETFIELD (so the result reuses the
    // table's register, matching C's behavior).
    let saved_freereg = fs.freereg;
    let mut base_reg = if fs.find_global_decl(first_name).is_some() {
        // Variable is declared as global: load from _ENV
        if let Some(env_reg) = fs.find_local("_ENV") {
            let r = fs.alloc_reg();
            let k = fs.string_k(first_name);
            code_getfield(fs, r, env_reg, k);
            r
        } else {
            let r = fs.alloc_reg();
            let k = fs.string_k(first_name);
            code_gettabup(fs, r, 0, k);
            r
        }
    } else if let Some(reg) = fs.find_local(first_name) {
        // Like C's singlevar for VLOCAL: dischargevars makes it VNONRELOC,
        // then luaK_exp2anyregup returns the register directly.
        // The register will be freed when the first GETFIELD is generated
        // (matching C's freereg in dischargevars for VINDEXSTR).
        reg
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

    // Process intermediate field accesses (like C's fieldsel loop).
    // Each intermediate access generates a GETFIELD instruction.
    // Like C: before generating GETFIELD, free the table register so the
    // result register can reuse it (matching C's dischargevars+exp2nextreg).
    // In C, dischargevars for VINDEXSTR calls freereg(ind.t), which frees
    // the table register if it's a temporary (>= nvarstack). Then exp2nextreg
    // allocates a new register that reuses the just-freed slot.
    let last_idx = chain.len() - 1;
    for i in 1..last_idx {
        let (_col, fname) = &chain[i];
        let k = fs.string_k(fname);
        // Free the table register if it's a temporary (like C's freereg in dischargevars)
        if base_reg >= fs.nvarstack() && base_reg == fs.freereg - 1 {
            fs.free_reg();
        }
        let r = fs.alloc_reg();
        code_getfield(fs, r, base_reg, k);
        base_reg = r;
    }

    // Parse the function body and store the closure.
    // Like C's funcstat: funcname calls luaK_indexed which may LOADK the key
    // (if it's a long string) before body creates the closure.
    let (is_colon, last_name) = &chain[last_idx];
    let fk = fs.string_k(last_name);
    let fk_is_kstr = is_kstr(fs, fk);
    // If key is not a short string, load it into a register now (before parse_body_ex),
    // matching C's luaK_indexed which calls luaK_exp2anyreg for non-Kstr keys.
    let key_reg = if fk_is_kstr {
        -1i32  // sentinel: will use SETFIELD with inline constant
    } else {
        let kr = fs.alloc_reg();
        fs.code_abx(OpCode::LOADK, kr, fk);
        kr
    };
    let freg = parse_body_ex(fs, *is_colon, None);
    if fk_is_kstr {
        // Key is a short string: use SETFIELD with inline constant
        fs.code_abc_k(OpCode::SETFIELD, base_reg, fk, freg, false);
    } else {
        // Key was loaded to key_reg: use SETTABLE
        fs.code_abc_k(OpCode::SETTABLE, base_reg, key_reg, freg, false);
    }
    // Free all temporary registers allocated above (base_reg for non-locals,
    // intermediate GETFIELD results, and freg for the closure).
    fs.set_freereg(saved_freereg);
    // Like C's funcstat: luaK_fixline(fs, line) after storevar
    fs.fixline(line);
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
            let mut last_is_vararg = false;
            let mut last_vararg_pc: i32 = -1;

            loop {
                let ei = parse_expr(fs);
                let target = saved_freereg + n_vals as i32;
                // Check if this is the last expression (no comma follows)
                let is_last = !check(fs, &Token::Comma);
                if is_last && matches!(ei.exp.kind, ExpKind::Vararg) {
                    // Don't discharge Vararg now; handle after loop like C's adjust_assign
                    last_is_vararg = true;
                    last_vararg_pc = ei.exp.info2;
                    last_exp = Some(ei.exp.clone());
                    n_vals += 1;
                    break;
                }
                match ei.exp.kind {
                    ExpKind::NonReloc => {
                        let r = ei.exp.info as i32;
                        if r != target {
                            fs.code_abc(OpCode::MOVE, target, r, 0);
                        }
                        if ei.exp.t != NO_JUMP || ei.exp.f != NO_JUMP {
                            fs.resolve_jumps(&ei.exp, target);
                        }
                        last_exp = Some(ExpDesc::new(ExpKind::NonReloc, target as i64));
                    }
                    ExpKind::Relocable => {
                        if ei.exp.info2 >= 0 {
                            fs.set_a(ei.exp.info2, target);
                            if ei.exp.t != NO_JUMP || ei.exp.f != NO_JUMP {
                                fs.resolve_jumps(&ei.exp, target);
                            }
                            last_exp = Some(ExpDesc::new(ExpKind::NonReloc, target as i64));
                        } else {
                            let r = ei.exp.info as i32;
                            if r != target {
                                fs.code_abc(OpCode::MOVE, target, r, 0);
                            }
                            if ei.exp.t != NO_JUMP || ei.exp.f != NO_JUMP {
                                fs.resolve_jumps(&ei.exp, target);
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

            // C: luaK_checkstack(fs, needed) in adjust_assign; needed = nvars - nexps
            let needed = nvars as i32 - n_vals as i32;
            fs.checkstack(needed);

            let last_is_ctc = n_vals == nvars
                && nvars > 0
                && kinds[nvars - 1] == RDKCONST
                && last_exp.as_ref().map(|e| matches!(e.kind,
                    ExpKind::Int | ExpKind::Float | ExpKind::Str | ExpKind::Boolean | ExpKind::Nil
                )).unwrap_or(false);

            let n_reg = if last_is_ctc { nvars - 1 } else { nvars };

            if last_is_ctc {
                let popped = fs.proto.code.pop();
                fs.inst_lines.pop();
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
            } else if last_is_vararg {
                // Like C's adjust_assign for VVARARG: luaK_setreturns
                // SETARG_C(*pc, nresults + 1); SETARG_A(*pc, fs->freereg); reserveregs(1)
                let needed = n_reg as i32 - n_vals as i32;
                let extra = if needed + 1 > 0 { needed + 1 } else { 0 };
                let c_val = extra + 1;
                fs.set_c(last_vararg_pc, c_val);
                let r = fs.alloc_reg();
                fs.set_a(last_vararg_pc, r);
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
                let nactvar = fs.active_nactvar();
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
                    nactvar,
                    pidx: -1,
                });
            }

            fs.set_freereg(saved_freereg + n_reg as i32);

            if !last_is_call && !last_is_vararg && n_vals < n_reg {
                let remaining = n_reg - n_vals;
                fs.code_nil(saved_freereg + n_vals as i32, remaining as i32);
            }
        } else {
            // C: luaK_checkstack(fs, needed) in adjust_assign; needed = nvars - 0 = nvars
            fs.checkstack(nvars as i32);
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
                    // and insidetbc (inhibits tail calls)
                    if let Some(block) = fs.block_stack.last_mut() {
                        block.has_upval = true;
                        block.insidetbc = true;
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
        // Parse remaining expressions like C's explist: don't discharge last if hasmultret
        let mut last_ei = parse_expr(fs);
        let mut nret = 2;
        while check(fs, &Token::Comma) {
            fs.ls_mut().next();
            fs.exp_to_next_reg(&last_ei.exp);
            last_ei = parse_expr(fs);
            nret += 1;
        }
        // Like C's retstat: if hasmultret, setmultret; else exp2nextreg
        if matches!(last_ei.exp.kind, ExpKind::Call) {
            let call_pc = last_ei.exp.info2 as usize;
            setarg(&mut fs.proto.code[call_pc], 0, POS_C, SIZE_C);
            // Check for tail call
            let has_tbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
            if !has_tbc && nret == 1 {
                SET_OPCODE(&mut fs.proto.code[call_pc], OpCode::TAILCALL);
            }
            fs.return_stat_gen(first, -1);
        } else if matches!(last_ei.exp.kind, ExpKind::Vararg) {
            let pc = last_ei.exp.info2;
            fs.set_c(pc, 0);
            let r = fs.alloc_reg();
            fs.set_a(pc, r);
            fs.return_stat_gen(first, -1);
        } else {
            fs.exp_to_next_reg(&last_ei.exp);
            fs.return_stat_gen(first, nret);
        }
    } else if matches!(ei.exp.kind, ExpKind::Call) {
        // Like C's hasmultret(VCALL): set multret, then check for tail call
        let call_pc = ei.exp.info2 as usize;
        // Like C's luaK_setmultret: set CALL's C to 0 (LUA_MULTRET + 1)
        setarg(&mut fs.proto.code[call_pc], 0, POS_C, SIZE_C);
        // Check for tail call: must not be inside a TBC block (like C's !fs->bl->insidetbc)
        let has_tbc = fs.block_stack.last().map(|b| b.insidetbc).unwrap_or(false);
        if !has_tbc {
            // Convert CALL to TAILCALL (like C's SET_OPCODE)
            SET_OPCODE(&mut fs.proto.code[call_pc], OpCode::TAILCALL);
        }
        // Generate RETURN with LUA_MULTRET (nret = -1, so B = nret+1 = 0)
        fs.return_stat_gen(first, -1);
    } else if matches!(ei.exp.kind, ExpKind::Vararg) {
        // Like C's hasmultret(VVARARG): luaK_setmultret sets C=0 (LUA_MULTRET+1),
        // SETARG_A(pc, fs->freereg), reserveregs(1)
        let pc = ei.exp.info2;
        fs.set_c(pc, 0);  // LUA_MULTRET + 1 = 0
        let r = fs.alloc_reg();
        fs.set_a(pc, r);
        // Generate RETURN with LUA_MULTRET (nret = -1, so B = nret+1 = 0)
        fs.return_stat_gen(first, -1);
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
                        // Flush SETLIST if tostore >= maxtostore (like C's closelistfield)
                        if tostore >= maxtostore {
                            fs.code_abc(OpCode::SETLIST, table_r, tostore, need_array);
                            need_array += tostore;
                            tostore = 0;
                            fs.set_freereg(table_r + 1);
                        }
                    }
                    last_list_exp = Some(parse_expr(fs).exp);
                }
            } else {
                if let Some(prev) = last_list_exp.take() {
                    fs.exp_to_next_reg(&prev);
                    tostore += 1;
                    // Flush SETLIST if tostore >= maxtostore (like C's closelistfield)
                    if tostore >= maxtostore {
                        fs.code_abc(OpCode::SETLIST, table_r, tostore, need_array);
                        need_array += tostore;
                        tostore = 0;
                        fs.set_freereg(table_r + 1);
                    }
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
            // Like C's luaK_setmultret for VVARARG: SETARG_C(pc, 0), SETARG_A(pc, freereg), reserveregs(1)
            let pc = last.info2;
            fs.set_c(pc, 0);  // LUA_MULTRET + 1 = 0
            let r = fs.alloc_reg();
            fs.set_a(pc, r);
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
    // C: new_fs.f->linedefined = line (line is ls->linenumber after skipping FUNCTION)
    let line_defined = fs.ls().linenumber;
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
                    // Add as RDKVAVAR kind local variable (not counted in n_params, like C)
                    param_names.push(name);
                    vararg_named = true;
                } else {
                    // Traditional ... without name (not counted in n_params, like C)
                    param_names.push("(vararg table)".to_string());
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
    new_fs.proto.line_defined = line_defined;
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
        if local.active {
            let is_global = local.kind >= GDKREG;
            let is_ctc = local.kind == RDKCTC;
            let is_vararg = local.kind == RDKVAVAR;
            new_fs.parent_locals.push(ParentVar {
                name: local.name.clone(),
                is_local: true,
                is_global,
                is_ctc,
                is_vararg,
                ctc_kind: local.ctc_kind.clone(),
                ctc_info: local.ctc_info,
                ctc_str: local.ctc_str.clone(),
                reg: local.reg,
                local_idx: i,
                upval_idx: 0,
                is_parent_upval: false,
            });
        }
    }
    // Also add current function's upvalues as parent_locals (is_local=false).
    // In C, _ENV is a local variable, so it naturally appears in parent_locals.
    // In Rust, _ENV is an upvalue, so we need to explicitly add it here
    // so that child functions can find it via find_upvalue.
    for (i, uv) in fs.proto.upvalues.iter().enumerate() {
        if let Some(ref name) = uv.name {
            // Check if this upvalue name already exists in parent_locals
            // (it might have been added as a local above)
            let already_exists = new_fs.parent_locals.iter().any(|p| p.name.as_str() == name.as_str());
            if !already_exists {
                new_fs.parent_locals.push(ParentVar {
                    name: name.to_string(),
                    is_local: false,
                    is_global: false,
                    is_ctc: false,
                    is_vararg: false,
                    ctc_kind: None,
                    ctc_info: None,
                    ctc_str: None,
                    reg: 0,
                    local_idx: 0,
                    upval_idx: i,
                    is_parent_upval: true,
                });
            }
        }
    }
    // Inherit grandparent variables as is_local=false.
    // upval_idx will be resolved lazily in find_upvalue.
    // CTC info is propagated so that grandchild functions can also
    // inline the constant value instead of creating upvalues.
    for gp_var in fs.parent_locals.iter() {
        new_fs.parent_locals.push(ParentVar {
            name: gp_var.name.clone(),
            is_local: false,
            is_global: gp_var.is_global,
            is_ctc: gp_var.is_ctc,
            is_vararg: gp_var.is_vararg,
            ctc_kind: gp_var.ctc_kind.clone(),
            ctc_info: gp_var.ctc_info,
            ctc_str: gp_var.ctc_str.clone(),
            reg: 0,
            local_idx: 0,
            upval_idx: 0,
            is_parent_upval: false,
        });
    }

    parse_chunk(&mut new_fs);
    // C: new_fs.f->lastlinedefined = ls->linenumber (before check_match(END))
    new_fs.proto.last_line_defined = new_fs.ls().linenumber;
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
                // Verify that the local variable at parent_local_idx matches the upvalue name.
                // In Rust, _ENV is not a local variable, so parent_local_idx=0 may point
                // to a different variable. If names don't match, treat as !in_stack.
                let local_name = fs.locals[local_idx].name.as_str();
                let uv_name = uv.name.as_ref().map(|s| s.as_str()).unwrap_or("");
                if local_name == uv_name {
                    // Like C's singlevaraux: if the variable is a vararg parameter (VVARGVAR),
                    // call luaK_vapar2local which sets PF_VATAB on the parent function.
                    if fs.locals[local_idx].kind == RDKVAVAR {
                        fs.proto.flag |= PF_VATAB;
                    }
                    fs.mark_block_upval(local_idx);
                } else {
                    // The upvalue references a variable that is not a local in the parent
                    // (e.g., _ENV which is an upvalue in the parent). Treat as !in_stack.
                    fs.mark_block_for_upval();
                    fs.mark_ancestor_blocks_for_upval(uv.idx as usize);
                }
            }
        } else {
            // in_stack=false: the parent also has an upvalue for this variable.
            // We need to mark the parent's block. Since we don't have a specific
            // local_idx, we mark based on the upvalue chain.
            // In C, singlevaraux calls markupval at each level when recursing.
            // The parent has an upvalue, so its block needs to be marked.
            // However, if the parent's upvalue is _ENV (which is not a local in
            // the parent in Rust), we should NOT mark the current block, because
            // the variable is not declared in the current block scope.
            // We only recursively mark ancestor blocks.
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

/// 检查 float 值是否为整数值且在 i64 范围内。
///
/// 等价于 C 版本 `luaV_flttointeger(n, p, F2Ieq)` 的判断逻辑：
/// 1. n 必须是有限值
/// 2. n 必须是整数值（n == floor(n)）
/// 3. n 必须在 i64 范围内（i64::MIN <= n < 2^63）
fn float_is_integer(k: f64) -> bool {
    if !k.is_finite() {
        return false;
    }
    // 检查是否为整数值（等价于 n != floor(n) 的反向判断）
    if k.trunc() != k {
        return false;
    }
    // 检查是否在 i64 范围内
    // LUA_MININTEGER = i64::MIN = -2^63，可精确表示为 f64
    // -LUA_MININTEGER = 2^63，可精确表示为 f64
    // 条件：k >= i64::MIN && k < 2^63
    k >= (i64::MIN as f64) && k < (1u64 << 63) as f64
}