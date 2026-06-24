use crate::state::LuaState;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    And, Break, Do, Else, Elseif, End, False, For, Function, Goto,
    If, In, Local, Nil, Not, Or, Repeat, Return, Then, True, Until, While,

    // Literals
    Name(String),
    Int(i64),
    Float(f64),
    String(String),

    // Symbols
    Plus, Minus, Star, Slash, Percent, Caret, Hash,
    Ampersand, Tilde, Pipe, LtLt, GtGt, SlashSlash,
    EqEq, TildeEq, LtEq, GtEq, Lt, Gt, Eq,
    LParen, RParen, LBrace, RBrace, LBracket, RBracket,
    ColonColon, Dot, DotDot, DotDotDot, Comma, Colon, Semi,

    Eof,
}

impl Token {
    pub fn is_keyword(s: &str) -> Option<Token> {
        match s {
            "and" => Some(Token::And),
            "break" => Some(Token::Break),
            "do" => Some(Token::Do),
            "else" => Some(Token::Else),
            "elseif" => Some(Token::Elseif),
            "end" => Some(Token::End),
            "false" => Some(Token::False),
            "for" => Some(Token::For),
            "function" => Some(Token::Function),
            "goto" => Some(Token::Goto),
            "if" => Some(Token::If),
            "in" => Some(Token::In),
            "local" => Some(Token::Local),
            "nil" => Some(Token::Nil),
            "not" => Some(Token::Not),
            "or" => Some(Token::Or),
            "repeat" => Some(Token::Repeat),
            "return" => Some(Token::Return),
            "then" => Some(Token::Then),
            "true" => Some(Token::True),
            "until" => Some(Token::Until),
            "while" => Some(Token::While),
            _ => None,
        }
    }
}

/// 从字节切片的指定位置安全地读取一个字符。
/// 无效 UTF-8 字节按单字节处理 (作为 Latin-1 字符),对应 C 版本按字节读取源码的行为。
fn read_char_at(bytes: &[u8], pos: usize) -> char {
    if pos >= bytes.len() {
        return '\0';
    }
    if bytes[pos] < 0x80 {
        return bytes[pos] as char;
    }
    match std::str::from_utf8(&bytes[pos..]).ok().and_then(|s| s.chars().next()) {
        Some(ch) => ch,
        None => bytes[pos] as char,  // 无效 UTF-8: 按字节值作为 char
    }
}

pub struct LexState<'a> {
    pub state: &'a mut LuaState,
    pub source: String,
    pub chunk_name: String,
    pub pos: usize,
    pub current: char,
    pub linenumber: i32,
    pub lastline: i32,
    pub token: Token,
    pub token_info: String,
    pub lookahead: Option<(Token, String)>,
    pub errors: Vec<String>,
    pub nesting_level: u32,  // recursion depth counter (like C's nCcalls)
}

impl<'a> LexState<'a> {
    pub fn new(state: &'a mut LuaState, source: &str, chunk_name: &str) -> Self {
        let src = source.to_string();
        let first = read_char_at(src.as_bytes(), 0);
        LexState {
            state,
            source: src,
            chunk_name: chunk_name.to_string(),
            pos: 0,
            current: first,
            linenumber: 1,
            lastline: 1,
            token: Token::Eof,
            token_info: String::new(),
            lookahead: None,
            errors: Vec::new(),
            nesting_level: 0,
        }
    }

    fn next_char(&mut self) {
        if self.current == '\n' {
            self.linenumber += 1;
        }
        let bytes = self.source.as_bytes();
        if self.pos < bytes.len() {
            if bytes[self.pos] < 0x80 {
                self.pos += 1;
            } else {
                // 尝试解析为 UTF-8;无效字节按单字节处理 (对应 C 按字节读取)
                match std::str::from_utf8(&bytes[self.pos..]).ok().and_then(|s| s.chars().next()) {
                    Some(ch) => self.pos += ch.len_utf8(),
                    None => self.pos += 1,
                }
            }
        }
        self.current = read_char_at(self.source.as_bytes(), self.pos);
    }

    fn peek(&self) -> char {
        let bytes = self.source.as_bytes();
        let mut pos = self.pos;
        // 跳过当前字符
        if pos < bytes.len() {
            if bytes[pos] < 0x80 {
                pos += 1;
            } else {
                match std::str::from_utf8(&bytes[pos..]).ok().and_then(|s| s.chars().next()) {
                    Some(ch) => pos += ch.len_utf8(),
                    None => pos += 1,
                }
            }
        }
        read_char_at(bytes, pos)
    }

    fn skip_whitespace(&mut self) {
        loop {
            match self.current {
                ' ' | '\t' | '\r' | '\n' => self.next_char(),
                '-' if self.peek() == '-' => {
                    self.next_char();
                    self.next_char();
                    if self.current == '[' {
                        self.next_char();
                        let equals = self.count_equals();
                        if self.current == '[' {
                            self.read_long_comment(equals);
                            continue;
                        }
                    }
                    while self.current != '\n' && self.current != '\0' {
                        self.next_char();
                    }
                }
                _ => break,
            }
        }
    }

    fn count_equals(&mut self) -> usize {
        let mut n = 0;
        while self.current == '=' {
            self.next_char();
            n += 1;
        }
        n
    }

    fn read_long_comment(&mut self, eqs: usize) {
        self.next_char();
        loop {
            match self.current {
                '\0' => {
                    self.error("unfinished long comment");
                    return;
                }
                ']' => {
                    self.next_char();
                    let actual = self.count_equals();
                    if actual == eqs && self.current == ']' {
                        self.next_char();
                        return;
                    }
                }
                _ => self.next_char(),
            }
        }
    }

    pub fn error(&mut self, msg: &str) {
        self.errors.push(format!("{}:{}: {}", self.chunk_name, self.linenumber, msg));
    }

    pub fn next(&mut self) {
        self.lastline = self.linenumber;
        if let Some((tok, info)) = self.lookahead.take() {
            self.token = tok;
            self.token_info = info;
            return;
        }
        self.read_token();
    }

    pub fn lookahead_next(&mut self) -> &(Token, String) {
        if self.lookahead.is_none() {
            let saved_token = self.token.clone();
            let saved_info = self.token_info.clone();
            self.read_token();
            self.lookahead = Some((self.token.clone(), self.token_info.clone()));
            self.token = saved_token;
            self.token_info = saved_info;
        }
        self.lookahead.as_ref().unwrap()
    }

    fn read_token(&mut self) {
        self.skip_whitespace();
        match self.current {
            '\0' => self.token = Token::Eof,
            '+' => { self.token = Token::Plus; self.next_char(); }
            '*' => { self.token = Token::Star; self.next_char(); }
            '%' => { self.token = Token::Percent; self.next_char(); }
            '^' => { self.token = Token::Caret; self.next_char(); }
            '#' => { self.token = Token::Hash; self.next_char(); }
            '&' => { self.token = Token::Ampersand; self.next_char(); }
            '|' => { self.token = Token::Pipe; self.next_char(); }
            '(' => { self.token = Token::LParen; self.next_char(); }
            ')' => { self.token = Token::RParen; self.next_char(); }
            '{' => { self.token = Token::LBrace; self.next_char(); }
            '}' => { self.token = Token::RBrace; self.next_char(); }
            ',' => { self.token = Token::Comma; self.next_char(); }
            ';' => { self.token = Token::Semi; self.next_char(); }
            '~' => {
                self.next_char();
                if self.current == '=' {
                    self.token = Token::TildeEq;
                    self.next_char();
                } else {
                    self.token = Token::Tilde;
                }
            }
            '=' => {
                self.next_char();
                if self.current == '=' {
                    self.token = Token::EqEq;
                    self.next_char();
                } else {
                    self.token = Token::Eq;
                }
            }
            '<' => {
                self.next_char();
                match self.current {
                    '=' => { self.token = Token::LtEq; self.next_char(); }
                    '<' => { self.token = Token::LtLt; self.next_char(); }
                    _ => self.token = Token::Lt,
                }
            }
            '>' => {
                self.next_char();
                match self.current {
                    '=' => { self.token = Token::GtEq; self.next_char(); }
                    '>' => { self.token = Token::GtGt; self.next_char(); }
                    _ => self.token = Token::Gt,
                }
            }
            '/' => {
                self.next_char();
                if self.current == '/' {
                    self.token = Token::SlashSlash;
                    self.next_char();
                } else {
                    self.token = Token::Slash;
                }
            }
            ':' => {
                self.next_char();
                if self.current == ':' {
                    self.token = Token::ColonColon;
                    self.next_char();
                } else {
                    self.token = Token::Colon;
                }
            }
            '.' => {
                self.next_char();
                if self.current == '.' {
                    self.next_char();
                    if self.current == '.' {
                        self.token = Token::DotDotDot;
                        self.next_char();
                    } else {
                        self.token = Token::DotDot;
                    }
                } else if self.current.is_ascii_digit() {
                    self.pos -= self.current.len_utf8();
                    self.current = '.';
                    self.read_number();
                } else {
                    self.token = Token::Dot;
                }
            }
            '[' => {
                self.next_char();
                let eqs = self.count_equals();
                if self.current == '[' {
                    self.read_long_string(eqs);
                } else {
                    self.token = Token::LBracket;
                }
            }
            ']' => { self.token = Token::RBracket; self.next_char(); }
            '-' => {
                self.next_char();
                self.token = Token::Minus;
            }
            '\'' | '"' => self.read_short_string(),
            c if c.is_ascii_digit() => self.read_number(),
            c if c.is_alphabetic() || c == '_' => self.read_name(),
            _ => {
                self.error(&format!("unexpected character: '{}'", self.current));
                self.next_char();
                self.token = Token::Eof;
            }
        }
        self.token_info = match &self.token {
            Token::Name(s) => s.clone(),
            Token::String(s) => s.clone(),
            _ => String::new(),
        };
    }

    fn read_name(&mut self) {
        let start = self.pos;
        while self.current.is_alphanumeric() || self.current == '_' {
            self.next_char();
        }
        let s = &self.source[start..self.pos];
        self.token = Token::is_keyword(s).unwrap_or_else(|| Token::Name(s.to_string()));
    }

    fn read_number(&mut self) {
        let mut start = self.pos;
        let mut is_float = false;
        let mut is_hex = false;

        if self.current == '0' {
            let next = self.peek();
            if next == 'x' || next == 'X' {
                is_hex = true;
                self.next_char();
                self.next_char();
                start = self.pos;
            }
        }

        if is_hex {
            while self.current.is_ascii_hexdigit() {
                self.next_char();
            }
            if self.current == '.' {
                is_float = true;
                self.next_char();
                while self.current.is_ascii_hexdigit() {
                    self.next_char();
                }
            }
            if self.current == 'p' || self.current == 'P' {
                is_float = true;
                self.next_char();
                if self.current == '+' || self.current == '-' {
                    self.next_char();
                }
                while self.current.is_ascii_digit() {
                    self.next_char();
                }
            }
        } else {
            while self.current.is_ascii_digit() {
                self.next_char();
            }
            if self.current == '.' {
                is_float = true;
                self.next_char();
                while self.current.is_ascii_digit() {
                    self.next_char();
                }
            }
            if self.current == 'e' || self.current == 'E' {
                is_float = true;
                self.next_char();
                if self.current == '+' || self.current == '-' {
                    self.next_char();
                }
                while self.current.is_ascii_digit() {
                    self.next_char();
                }
            }
        }

        let s = &self.source[start..self.pos];
        if is_float {
            if is_hex {
                match parse_hex_float(s) {
                    Some(v) => self.token = Token::Float(v),
                    None => {
                        self.error(&format!("malformed number: 0x{}", s));
                        self.token = Token::Float(0.0);
                    }
                }
            } else {
                match s.parse::<f64>() {
                    Ok(v) => self.token = Token::Float(v),
                    Err(_) => {
                        self.error(&format!("malformed number: {}", s));
                        self.token = Token::Float(0.0);
                    }
                }
            }
        } else {
            match i64::from_str_radix(s, if is_hex { 16 } else { 10 }) {
                Ok(v) => self.token = Token::Int(v),
                Err(_) if is_hex => {
                    // Hex integers that overflow i64 may fit in u64 (e.g., 0xFFFFFFFFFFFFFFFF = -1)
                    match u64::from_str_radix(s, 16) {
                        Ok(v) => self.token = Token::Int(v as i64),
                        Err(_) => {
                            // Hex integers that overflow u64: use wrapping arithmetic
                            // (与 C 版本的 l_str2int 一致,对十六进制数不做溢出检查)
                            let mut a: u64 = 0;
                            for c in s.chars() {
                                if let Some(d) = c.to_digit(16) {
                                    a = a.wrapping_mul(16).wrapping_add(d as u64);
                                }
                            }
                            self.token = Token::Int(a as i64);
                        }
                    }
                }
                Err(_) => match s.parse::<f64>() {
                    Ok(v) => self.token = Token::Float(v),
                    Err(_) => {
                        self.error(&format!("malformed number: {}", s));
                        self.token = Token::Int(0);
                    }
                },
            }
        }
    }

    fn read_short_string(&mut self) {
        let delim = self.current;
        self.next_char();
        let mut s = String::new();
        loop {
            match self.current {
                '\0' | '\n' => {
                    self.error("unfinished string");
                    break;
                }
                '\\' => {
                    self.next_char();
                    self.read_escape(&mut s);
                }
                c if c == delim => {
                    self.next_char();
                    break;
                }
                c => {
                    // 直接拷贝源字节,以正确处理非 UTF-8 字节 (对应 C 的 save_and_next)
                    let start = self.pos;
                    self.next_char();
                    let bytes = &self.source.as_bytes()[start..self.pos];
                    unsafe { s.as_mut_vec().extend_from_slice(bytes); }
                    let _ = c;
                }
            }
        }
        self.token = Token::String(s);
    }

    fn read_escape(&mut self, s: &mut String) {
        match self.current {
            'a' => { s.push('\x07'); self.next_char(); }
            'b' => { s.push('\x08'); self.next_char(); }
            'f' => { s.push('\x0c'); self.next_char(); }
            'n' => { s.push('\n'); self.next_char(); }
            'r' => { s.push('\r'); self.next_char(); }
            't' => { s.push('\t'); self.next_char(); }
            'v' => { s.push('\x0b'); self.next_char(); }
            '\\' => { s.push('\\'); self.next_char(); }
            '"' => { s.push('"'); self.next_char(); }
            '\'' => { s.push('\''); self.next_char(); }
            'z' => {
                self.next_char();
                while self.current == ' ' || self.current == '\t' || self.current == '\r' || self.current == '\n' {
                    self.next_char();
                }
            }
            'x' => {
                self.next_char();
                let mut hex = String::new();
                for _ in 0..2 {
                    if self.current.is_ascii_hexdigit() {
                        hex.push(self.current);
                        self.next_char();
                    }
                }
                let val = u8::from_str_radix(&hex, 16).unwrap_or(0);
                // 直接 push 原始字节，与 C 版本 save(ls, c) 行为一致
                unsafe { s.as_mut_vec().push(val); }
            }
            'u' => {
                self.next_char();  // skip 'u'
                if self.current != '{' {
                    self.error("missing '{'");
                    return;
                }
                self.next_char();  // skip '{'
                let mut r: u32 = 0;
                let mut has_digit = false;
                while self.current.is_ascii_hexdigit() {
                    has_digit = true;
                    if r > (0x7FFFFFFFu32 >> 4) {
                        self.error("UTF-8 value too large");
                        return;
                    }
                    r = (r << 4) + (self.current.to_digit(16).unwrap() as u32);
                    self.next_char();
                }
                if !has_digit {
                    self.error("missing digits");
                    return;
                }
                if self.current != '}' {
                    self.error("missing '}'");
                    return;
                }
                self.next_char();  // skip '}'
                // 使用 UTF-8 编码（支持 1-6 字节，等价于 C 版本 luaO_utf8esc）
                for b in utf8_encode(r) {
                    unsafe { s.as_mut_vec().push(b); }
                }
            }
            '0'..='9' => {
                let mut digits = String::new();
                for _ in 0..3 {
                    if self.current.is_ascii_digit() {
                        digits.push(self.current);
                        self.next_char();
                    } else {
                        break;
                    }
                }
                let val = u32::from_str_radix(&digits, 10).unwrap_or(0);
                if val <= 0xFF {
                    // 直接 push 原始字节，与 C 版本 save(ls, c) 行为一致
                    unsafe { s.as_mut_vec().push(val as u8); }
                } else {
                    s.push('?');
                }
            }
            '\n' | '\r' => {
                // 对应 C: inclinenumber(ls); c = '\n'; goto only_save;
                // 跳过换行序列 (\n, \r, \n\r, \r\n) 并在字符串中存入换行符
                let old = self.current;
                self.next_char();
                if (self.current == '\n' || self.current == '\r') && self.current != old {
                    self.next_char();
                }
                s.push('\n');
            }
            c => {
                // 直接拷贝源字节,以正确处理非 UTF-8 字节
                let start = self.pos;
                self.next_char();
                let bytes = &self.source.as_bytes()[start..self.pos];
                unsafe { s.as_mut_vec().extend_from_slice(bytes); }
            }
        }
    }

    fn read_long_string(&mut self, eqs: usize) {
        self.next_char();
        if self.current == '\n' {
            self.next_char();
        }
        let start = self.pos;
        loop {
            match self.current {
                '\0' => {
                    self.error("unfinished long string");
                    break;
                }
                ']' => {
                    self.next_char();
                    let actual = self.count_equals();
                    if actual == eqs && self.current == ']' {
                        let end = self.pos - 1 - actual;
                        // 使用原始字节拷贝,以正确处理非 UTF-8 字节
                        let s = unsafe {
                            String::from_utf8_unchecked(self.source.as_bytes()[start..end].to_vec())
                        };
                        self.next_char();
                        self.token = Token::String(s);
                        return;
                    }
                }
                _ => self.next_char(),
            }
        }
        self.token = Token::String(String::new());
    }

    pub fn check(&self, tok: &Token) -> bool {
        std::mem::discriminant(&self.token) == std::mem::discriminant(tok)
    }

    pub fn check_next(&mut self, tok: &Token) -> bool {
        let (next_tok, _) = self.lookahead_next().clone();
        std::mem::discriminant(&next_tok) == std::mem::discriminant(tok)
    }

    pub fn test_next(&mut self, tok: &Token) -> bool {
        if self.check_next(tok) {
            self.next();
            true
        } else {
            false
        }
    }
}

fn parse_hex_float(s: &str) -> Option<f64> {
    // Parse hex float format: HH[.HHH][p[+/-]DD]
    // s does not include "0x" prefix
    let (mantissa_str, exponent_str) = match s.find(|c| c == 'p' || c == 'P') {
        Some(pos) => (&s[..pos], Some(&s[pos + 1..])),
        None => (s, None),
    };

    let (int_str, frac_str) = match mantissa_str.find('.') {
        Some(pos) => (&mantissa_str[..pos], Some(&mantissa_str[pos + 1..])),
        None => (mantissa_str, None),
    };

    if int_str.is_empty() && frac_str.map_or(true, |f| f.is_empty()) {
        return None;
    }

    let int_part = if int_str.is_empty() {
        0.0
    } else {
        u64::from_str_radix(int_str, 16).ok()? as f64
    };

    let frac_part = if let Some(frac) = frac_str {
        if frac.is_empty() {
            0.0
        } else {
            let frac_val = u64::from_str_radix(frac, 16).ok()? as f64;
            frac_val / 16f64.powi(frac.len() as i32)
        }
    } else {
        0.0
    };

    let exponent = if let Some(exp) = exponent_str {
        if exp.is_empty() {
            return None;
        }
        i32::from_str_radix(exp, 10).ok()?
    } else {
        0
    };

    Some((int_part + frac_part) * 2f64.powi(exponent))
}

/// UTF-8 编码：等价于 C 版本的 `luaO_utf8esc`，支持 1-6 字节序列。
///
/// 与标准 Rust `char::encode_utf8` 不同，此函数支持超出 Unicode 范围的码点
/// （0x110000 到 0x7FFFFFFF），生成 5-6 字节的"扩展 UTF-8"序列，
/// 与 Lua C 实现保持一致。
fn utf8_encode(x: u32) -> Vec<u8> {
    const UTF8BUFFSZ: usize = 8;
    let mut buff = [0u8; UTF8BUFFSZ];
    let mut n = 1usize;
    if x < 0x80 {
        // ASCII: 单字节序列
        buff[UTF8BUFFSZ - 1] = x as u8;
    } else {
        // 多字节序列：从后向前填充续字节，最后写首字节
        let mut x = x;
        let mut mfb: u32 = 0x3f;  // 首字节可用的最大有效位
        loop {
            buff[UTF8BUFFSZ - n] = (0x80 | (x & 0x3f)) as u8;
            n += 1;
            x >>= 6;
            mfb >>= 1;
            if x <= mfb {
                break;
            }
        }
        buff[UTF8BUFFSZ - n] = (((!mfb) << 1) | x) as u8;
    }
    buff[UTF8BUFFSZ - n..UTF8BUFFSZ].to_vec()
}