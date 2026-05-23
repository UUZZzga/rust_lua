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
// Expression descriptor
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum ExpKind {
    Void, Nil, Boolean, Int, Float, Str,
    NonReloc, Relocable, Call, Vararg,
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
    pub locals: Vec<LocalVar>,
    pub errors: Vec<String>,
    ls: *mut LexState,
}

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
    proto.max_stack_size = (fs.freereg + 2) as u8;
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
            locals: Vec::new(),
            errors: Vec::new(),
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
    fn emit(&mut self, ins: Instruction) -> i32 {
        self.proto.code.push(ins);
        let cur = self.pc;
        self.pc += 1;
        cur
    }

    fn code_abc(&mut self, op: OpCode, a: i32, b: i32, c: i32) -> i32 {
        self.emit(create_abck(op, a, b, c, 0))
    }

    fn code_abc_k(&mut self, op: OpCode, a: i32, b: i32, c: i32, k: bool) -> i32 {
        self.emit(create_abck(op, a, b, c, if k { 1 } else { 0 }))
    }

    fn code_abx(&mut self, op: OpCode, a: i32, bx: i32) -> i32 {
        let ins = ((op as u32) << POS_OP) | ((a as u32) << POS_A) | ((bx as u32) << POS_BX);
        self.emit(ins)
    }

    fn code_asbx(&mut self, op: OpCode, a: i32, sbx: i32) -> i32 {
        let ins = ((op as u32) << POS_OP)
            | ((a as u32) << POS_A)
            | ((((sbx + OFFSET_SBX) as u32) & mask1(SIZE_BX, 0)) << POS_BX);
        self.emit(ins)
    }

    fn code_sj(&mut self, op: OpCode, sj: i32, k: i32) -> i32 {
        let ins = ((op as u32) << POS_OP)
            | ((((sj + OFFSET_sJ) as u32) & mask1(SIZE_sJ, 0)) << POS_SJ)
            | (((k & 1) as u32) << POS_K);
        self.emit(ins)
    }

    fn code_ax(&mut self, op: OpCode, ax: i32) -> i32 {
        let ins = ((op as u32) << POS_OP) | ((ax as u32) << POS_A);
        self.emit(ins)
    }

    fn fix_jump(&mut self, pc: i32, dest: i32) {
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
                let offset = (dest - pc - 1) as i32;
                setarg(i, offset, POS_BX, SIZE_BX);
            }
            OpMode::IAsBx => {
                let offset = dest - pc - 1;
                setarg(i, offset, POS_BX, SIZE_BX);
            }
            OpMode::IsJ => {
                setarg(i, (dest - pc - 1) + OFFSET_sJ, POS_SJ, SIZE_sJ);
            }
            _ => {}
        }
    }

    fn jump(&mut self) -> i32 {
        self.code_sj(OpCode::JMP, NO_JUMP, 0)
    }

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

    fn string_k(&mut self, s: &str) -> i32 {
        let t = crate::strings::StringTable::new();
        let ls = crate::strings::new_lstr(&t, s);
        self.const_k(TValue::Str(ls))
    }

    fn int_k(&mut self, i: i64) -> i32 {
        self.const_k(TValue::Integer(i))
    }

    fn float_k(&mut self, f: f64) -> i32 {
        self.const_k(TValue::Float(f))
    }

    fn alloc_reg(&mut self) -> i32 {
        let r = self.freereg;
        self.freereg += 1;
        r
    }

    fn free_reg(&mut self) {
        if self.freereg > 0 {
            self.freereg -= 1;
        }
    }

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

    fn find_local(&self, name: &str) -> Option<i32> {
        for lv in self.locals.iter().rev() {
            if lv.active && lv.name == name {
                return Some(lv.reg);
            }
        }
        None
    }

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
            ExpKind::NonReloc => e.info as i32,
            ExpKind::Relocable | ExpKind::Call | ExpKind::Vararg => {
                if e.info as i32 == self.freereg - 1 {
                    e.info as i32
                } else {
                    let r = self.alloc_reg();
                    self.code_abc(OpCode::MOVE, r, e.info as i32, 0);
                    r
                }
            }
        }
    }

    fn discharge_to_any_reg(&mut self, e: &ExpDesc) -> (i32, ExpDesc) {
        let r = self.expr_to_reg(e);
        let mut ne = ExpDesc::new(ExpKind::NonReloc, r as i64);
        ne.info2 = e.info2;
        (r, ne)
    }

    fn return_stat_gen(&mut self, first: i32, nret: i32) {
        let is_vararg = (self.proto.flag & PF_VAHID) != 0;
        let c = if is_vararg { self.proto.num_params as i32 + 1 } else { 0 };
        match nret {
            0 => {
                if is_vararg {
                    self.code_abc(OpCode::RETURN, first, 1, c);
                } else {
                    self.code_abc(OpCode::RETURN0, first, 1, 0);
                }
            }
            1 => {
                if is_vararg {
                    self.code_abc(OpCode::RETURN, first, 2, c);
                } else {
                    self.code_abc(OpCode::RETURN1, first, 2, 0);
                }
            }
            _ => { self.code_abc(OpCode::RETURN, first, nret + 1, c); }
        }
    }

    fn nvarstack(&self) -> i32 {
        self.proto.num_params as i32
    }
}

// ============================================================================
// Token utilities
// ============================================================================

fn check(fs: &FuncState, t: &Token) -> bool {
    std::mem::discriminant(&fs.ls().token) == std::mem::discriminant(t)
}

fn test_next(fs: &mut FuncState, t: &Token) -> bool {
    let l = fs.ls_mut();
    if std::mem::discriminant(&l.lookahead_next().0) == std::mem::discriminant(t) {
        l.next();
        true
    } else {
        false
    }
}

fn expect(fs: &mut FuncState, t: &Token) {
    if !check(fs, t) {
        fs.error(&format!("expected {:?}, got {:?}", t, fs.ls().token));
    } else {
        fs.ls_mut().next();
    }
}

fn block_follow(fs: &FuncState, with_until: bool) -> bool {
    match &fs.ls().token {
        Token::Else | Token::Elseif | Token::End | Token::Eof => true,
        Token::Until if with_until => true,
        _ => false,
    }
}

fn get_name(fs: &mut FuncState) -> String {
    match &fs.ls().token {
        Token::Name(s) => {
            let name = s.clone();
            fs.ls_mut().next();
            name
        }
        _ => {
            fs.error("expected identifier");
            String::new()
        }
    }
}

// ============================================================================
// Parser entry
// ============================================================================

fn parse_chunk(fs: &mut FuncState) {
    let is_last = block_follow(fs, true);
    if !is_last {
        parse_block(fs);
    }
    let nvarstack = fs.nvarstack();
    fs.return_stat_gen(nvarstack, 0);
}

fn parse_block(fs: &mut FuncState) {
    while !block_follow(fs, true) {
        if check(fs, &Token::Return) {
            parse_statement(fs);
            return;
        }
        parse_statement(fs);
    }
}

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

fn checkglobal(fs: &mut FuncState, varname: &str, _line: i32) {
    let r = fs.alloc_reg();
    let k = fs.string_k(varname);
    fs.code_abc(OpCode::GETTABUP, r, 0, k);
    let k_bx = if k >= 256 { 0 } else { k + 1 };
    fs.code_abx(OpCode::ERRNNIL, r, k_bx);
    fs.free_reg();
}

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

fn globalstat(fs: &mut FuncState) {
    let defkind = getglobalattribute(fs, GDKREG);
    if !test_next(fs, &Token::Star) {
        globalnames(fs, defkind);
    } else {
        fs.add_local_kind("(global *)", fs.pc, defkind);
    }
}

fn globalfunc(fs: &mut FuncState, _line: i32) {
    let fname = get_name(fs);
    fs.add_local_kind(&fname, fs.pc, GDKREG);
    let r = parse_body(fs);
    let k = fs.string_k(&fname);
    fs.code_abc(OpCode::SETTABUP, 0, k, r);
    checkglobal(fs, &fname, _line);
}

fn globalstatfunc(fs: &mut FuncState, line: i32) {
    fs.ls_mut().next();
    if test_next(fs, &Token::Function) {
        globalfunc(fs, line);
    } else {
        globalstat(fs);
    }
}

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

fn parse_assign_or_call(fs: &mut FuncState) {
    let first = parse_prefix_exp(fs);
    
    if check(fs, &Token::LParen) || check(fs, &Token::Colon) || check(fs, &Token::Dot) || check(fs, &Token::LBrace) || matches!(&fs.ls().token, Token::String(..)) {
        let freg = load_func(fs, &first);
        parse_func_args(fs, freg);
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
                if let Some(ref name) = v.var_name {
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

fn load_func(fs: &mut FuncState, p: &PrefixResult) -> i32 {
    if let Some(reg) = p.local_idx {
        reg
    } else if let Some(key) = p.key {
        let r = fs.alloc_reg();
        fs.code_abc(OpCode::GETTABUP, r, 0, key);
        r
    } else {
        // Should not happen, but handle gracefully
        fs.alloc_reg()
    }
}

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
        fs.code_abc(OpCode::CALL, freg, 2, 1);
        return;
    }
    
    if check(fs, &Token::LBrace) {
        let (tr, _n) = parse_constructor(fs);
        fs.code_abc(OpCode::MOVE, freg + 1, tr, 0);
        fs.code_abc(OpCode::CALL, freg, 2, 1);
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
            fs.code_abc(OpCode::CALL, freg, na + 1, 1);
        }
        return;
    }
    
    if check(fs, &Token::LParen) {
        fs.ls_mut().next();
        let nparams = parse_args(fs);
        expect(fs, &Token::RParen);
        fs.code_abc(OpCode::CALL, freg, nparams + 1, 1);
        return;
    }
}

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
}

fn parse_prefix_exp(fs: &mut FuncState) -> PrefixResult {
    match &fs.ls().token {
        Token::Name(name) => {
            let name = name.clone();
            fs.ls_mut().next();
            
            if let Some(reg) = fs.find_local(&name) {
                PrefixResult { var_name: None, local_idx: Some(reg), key: None, reg: Some(reg) }
            } else {
                let k = fs.string_k(&name);
                PrefixResult { var_name: Some(name), local_idx: None, key: Some(k), reg: None }
            }
        }
        Token::LParen => {
            fs.ls_mut().next();
            let e = parse_expr(fs);
            expect(fs, &Token::RParen);
            let r = fs.expr_to_reg(&e.exp);
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r) }
        }
        _ => {
            let e = parse_simple_exp(fs);
            let r = fs.expr_to_reg(&e.exp);
            PrefixResult { var_name: None, local_idx: None, key: None, reg: Some(r) }
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

fn parse_expr(fs: &mut FuncState) -> ExprItem {
    parse_subexpr(fs, 0)
}

const PREC_OR: i32 = 1;
const PREC_AND: i32 = 2;
const PREC_COMP: i32 = 3;
const PREC_CONCAT: i32 = 5;
const PREC_ADD: i32 = 7;
const PREC_MUL: i32 = 9;
const PREC_UNARY: i32 = 11;
const PREC_POW: i32 = 13;

fn parse_subexpr(fs: &mut FuncState, limit: i32) -> ExprItem {
    let mut e = parse_simple_exp(fs);
    
    loop {
        let mut matched = false;
        
        if limit <= PREC_OR && (check(fs, &Token::Or) || check(fs, &Token::And)) {
            let ec = e.exp.clone();
            let r = fs.expr_to_reg(&ec);
            let is_and = check(fs, &Token::And);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, if is_and { PREC_AND + 1 } else { PREC_OR + 1 });
            let r2 = fs.expr_to_reg(&e2.exp);
            let lhs_int = ec.info;
            let lhs_kind = ec.kind.clone();
            match (lhs_kind.clone(), e2.exp.kind.clone()) {
                (ExpKind::Int, _) | (ExpKind::Float, _) => {
                    let op = if is_and { OpCode::BANDK } else { OpCode::BORK };
                    let k = if matches!(lhs_kind, ExpKind::Int) { fs.int_k(lhs_int) } else { fs.float_k(f64::from_bits(lhs_int as u64)) };
                    fs.code_abc(op, r, k, r2);
                }
                _ => {
                    let op = if is_and { OpCode::BAND } else { OpCode::BOR };
                    fs.code_abc(op, r, r, r2);
                }
            }
            e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
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
                let r_result = fs.alloc_reg();
                fs.code_abc(OpCode::LFALSESKIP, r_result, 0, 0);
                let load_true_pc = fs.code_abc(OpCode::LOADTRUE, r_result, 0, 0);
                fs.fix_jump(jmp_pc, load_true_pc);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r_result as i64) };
            } else {
                let r = fs.expr_to_reg(&ec);
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
                let r_result = fs.alloc_reg();
                fs.code_abc(OpCode::LFALSESKIP, r_result, 0, 0);
                let load_true_pc = fs.code_abc(OpCode::LOADTRUE, r_result, 0, 0);
                fs.fix_jump(jmp_pc, load_true_pc);
                e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r_result as i64) };
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
                    fs.code_abc(if is_add { OpCode::ADDI } else { OpCode::SUBK }, r, r2, ec.info as i32);
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                }
                _ => {
                    let r = fs.expr_to_reg(&ec);
                    let r2 = fs.expr_to_reg(&e2.exp);
                    let op = if is_add { OpCode::ADD } else { OpCode::SUB };
                    fs.code_abc(op, r, r, r2);
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
                }
            }
            matched = true;
        }
        
        if limit <= PREC_MUL && check_mulop(fs) {
            let ec = e.exp.clone();
            let is_mul = check(fs, &Token::Star);
            let is_div = check(fs, &Token::Slash);
            fs.ls_mut().next();
            let e2 = parse_subexpr(fs, PREC_MUL + 1);
            match (&ec.kind, &e2.exp.kind) {
                (ExpKind::Int, ExpKind::Int) => {
                    if is_div {
                        let val = ec.info as f64 / e2.exp.info as f64;
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Float, val.to_bits() as i64) };
                    } else {
                        let val = if is_mul { ec.info * e2.exp.info } else { ec.info % e2.exp.info };
                        e = ExprItem { exp: ExpDesc::new(ExpKind::Int, val) };
                    }
                }
                _ => {
                    let r = fs.expr_to_reg(&ec);
                    let r2 = fs.expr_to_reg(&e2.exp);
                    let op = if is_mul { OpCode::MUL } else if is_div { OpCode::DIV } else { OpCode::MOD };
                    fs.code_abc(op, r, r, r2);
                    e = ExprItem { exp: ExpDesc::new(ExpKind::Relocable, r as i64) };
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

fn check_compare(fs: &FuncState) -> bool {
    matches!(fs.ls().token, Token::EqEq | Token::TildeEq | Token::Lt | Token::LtEq | Token::Gt | Token::GtEq)
}

const OFFSET_SC: i64 = 127;

fn fits_sc(desc: &ExpDesc) -> bool {
    if let ExpKind::Int = desc.kind {
        let v = desc.info;
        (v as i8 as i64) == v
    } else {
        false
    }
}

fn int_to_sc(v: i64) -> i32 {
    ((v as u64).wrapping_add(OFFSET_SC as u64)) as i32
}

fn fits_sbx(v: i64) -> bool {
    v >= -(OFFSET_SBX as i64) && v <= (OFFSET_SBX as i64) + 1
}

fn check_addop(fs: &FuncState) -> bool {
    matches!(fs.ls().token, Token::Plus | Token::Minus)
}

fn check_mulop(fs: &FuncState) -> bool {
    matches!(fs.ls().token, Token::Star | Token::Slash | Token::Percent)
}

fn parse_simple_exp(fs: &mut FuncState) -> ExprItem {
    let e = match &fs.ls().token {
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
            let r = fs.expr_to_reg(&ei.exp);
            ExpDesc::new(ExpKind::Relocable, r as i64)
        }
        Token::Not | Token::Minus | Token::Hash | Token::Tilde => {
            let op_tok = fs.ls().token.clone();
            fs.ls_mut().next();
            let ei = parse_subexpr(fs, PREC_UNARY);
            let r = fs.expr_to_reg(&ei.exp);
            let nr = fs.alloc_reg();
            match op_tok {
                Token::Not => { fs.code_abc(OpCode::NOT, nr, r, 0); }
                Token::Minus => { fs.code_abc(OpCode::UNM, nr, r, 0); }
                Token::Hash => { fs.code_abc(OpCode::LEN, nr, r, 0); }
                Token::Tilde => { fs.code_abc(OpCode::BNOT, nr, r, 0); }
                _ => {}
            }
            ExpDesc::new(ExpKind::Relocable, nr as i64)
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
    ExprItem { exp: e }
}

// ============================================================================
// Statements
// ============================================================================

fn parse_if(fs: &mut FuncState) {
    fs.ls_mut().next();
    let ei = parse_expr(fs);
    let cond_reg = fs.expr_to_reg(&ei.exp);
    let jmp = fs.code_abc(OpCode::TEST, cond_reg, 0, 0);
    expect(fs, &Token::Then);
    parse_block(fs);
    let mut exit_jumps = Vec::new();

    while check(fs, &Token::Elseif) {
        let j = fs.jump();
        exit_jumps.push(j);
        fs.fix_jump(jmp, fs.pc);
        fs.ls_mut().next();
        let ei2 = parse_expr(fs);
        let cr2 = fs.expr_to_reg(&ei2.exp);
        let _jmp2 = fs.code_abc(OpCode::TEST, cr2, 0, 0);
        expect(fs, &Token::Then);
        parse_block(fs);
    }

    if check(fs, &Token::Else) {
        let j = fs.jump();
        exit_jumps.push(j);
        fs.fix_jump(jmp, fs.pc);
        fs.ls_mut().next();
        parse_block(fs);
    } else {
        fs.fix_jump(jmp, fs.pc);
    }
    expect(fs, &Token::End);

    for j in exit_jumps {
        fs.fix_jump(j, fs.pc);
    }
}

fn parse_while(fs: &mut FuncState) {
    fs.ls_mut().next();
    let loop_start = fs.pc;
    let ei = parse_expr(fs);
    let r = fs.expr_to_reg(&ei.exp);
    let jmp = fs.code_abc(OpCode::TEST, r, 0, 0);
    expect(fs, &Token::Do);
    parse_block(fs);
    fs.code_asbx(OpCode::JMP, 0, loop_start - fs.pc - 1);
    fs.fix_jump(jmp, fs.pc);
    expect(fs, &Token::End);
}

fn parse_do(fs: &mut FuncState) {
    fs.ls_mut().next();
    parse_block(fs);
    expect(fs, &Token::End);
}

fn parse_repeat(fs: &mut FuncState) {
    fs.ls_mut().next();
    let loop_start = fs.pc;
    parse_block(fs);
    expect(fs, &Token::Until);
    let ei = parse_expr(fs);
    let r = fs.expr_to_reg(&ei.exp);
    fs.code_abc(OpCode::EQ, r, 0, 0);
    fs.fix_jump(fs.pc - 1, loop_start);
}

fn parse_for(fs: &mut FuncState) {
    fs.ls_mut().next();
    let name = get_name(fs);
    
    if check(fs, &Token::Eq) {
        fs.ls_mut().next();
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
        
        expect(fs, &Token::Do);
        let idx = fs.add_local(&name, fs.pc);
        fs.code_abc(OpCode::MOVE, idx, init_r, 0);
        
        let prep = fs.code_asbx(OpCode::FORPREP, idx, 0);
        parse_block(fs);
        fs.code_asbx(OpCode::FORLOOP, idx, 0);
        fs.fix_jump(prep, fs.pc);
        expect(fs, &Token::End);
    } else {
        fs.error("generic for not yet supported");
        fs.ls_mut().next();
        expect(fs, &Token::End);
    }
}

fn parse_func_stat(fs: &mut FuncState) {
    fs.ls_mut().next();
    let name = get_name(fs);
    let r = parse_body(fs);
    if let Some(reg) = fs.find_local(&name) {
        fs.code_abc(OpCode::MOVE, reg, r, 0);
    } else {
        let k = fs.string_k(&name);
        fs.code_abc(OpCode::SETTABUP, 0, k, r);
    }
}

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
        let mut exps: Vec<ExpDesc> = Vec::new();
        let n_vals: i32;

        if has_init {
            fs.ls_mut().next();
            loop {
                let ei = parse_expr(fs);
                exps.push(ei.exp);
                if !check(fs, &Token::Comma) { break; }
                fs.ls_mut().next();
            }
            n_vals = exps.len() as i32;
        } else {
            n_vals = 0;
        }

        let last_is_ctc = n_vals as usize == nvars
            && nvars > 0
            && kinds[nvars - 1] == RDKCONST
            && exps.last().map(|e| matches!(e.kind,
                ExpKind::Int | ExpKind::Float | ExpKind::Str | ExpKind::Boolean | ExpKind::Nil
            )).unwrap_or(false);

        let n_reg = if last_is_ctc { nvars as i32 - 1 } else { nvars as i32 };
        let n_val_regs = if last_is_ctc { n_vals - 1 } else { n_vals };

        let mut regs = Vec::new();
        for i in 0..n_reg as usize {
            let reg = fs.add_local_kind(&names[i], fs.pc, kinds[i]);
            regs.push(reg);
        }
        if last_is_ctc {
            fs.add_local_kind(&names[nvars - 1], fs.pc, RDKCTC);
        }

        if has_init {
            for i in 0..n_val_regs as usize {
                let _r = fs.expr_to_reg(&exps[i]);
            }
            let base_val = fs.freereg - n_val_regs;
            for (i, &reg) in regs.iter().enumerate() {
                if (i as i32) < n_val_regs {
                    let val_r = base_val + i as i32;
                    if reg != val_r {
                        fs.code_abc(OpCode::MOVE, reg, val_r, 0);
                    }
                }
            }
            for _ in 0..n_val_regs {
                fs.free_reg();
            }
            for i in n_val_regs as usize..n_reg as usize {
                fs.code_abc(OpCode::LOADNIL, regs[i], 0, 0);
            }
        } else {
            for &reg in &regs {
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
            } else if let Token::Name(s) = &fs.ls().token {
                let name = s.clone();
                fs.ls_mut().next();
                expect(fs, &Token::Eq);
                let ev = parse_expr(fs);
                let v_r = fs.expr_to_reg(&ev.exp);
                let k = fs.string_k(&name);
                fs.code_abc(OpCode::SETI, table_r, k, v_r);
            } else {
                n_arr += 1;
                let ev = parse_expr(fs);
                let v_r = fs.expr_to_reg(&ev.exp);
                fs.code_abc(OpCode::SETI, table_r, n_arr, v_r);
            }
            
            if !check(fs, &Token::Comma) && !check(fs, &Token::Semi) { break; }
            fs.ls_mut().next();
            if check(fs, &Token::RBrace) { break; }
        }
    }
    expect(fs, &Token::RBrace);
    
    let r = fs.alloc_reg();
    fs.code_abc(OpCode::MOVE, r, table_r, 0);
    (r, n_arr)
}

fn parse_body(fs: &mut FuncState) -> i32 {
    expect(fs, &Token::LParen);
    let has_params = !check(fs, &Token::RParen);
    let mut is_vararg = false;
    let mut n_params: u8 = 0;
    
    let mut param_names = Vec::new();
    if has_params {
        let mut first = true;
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
            first = false;
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