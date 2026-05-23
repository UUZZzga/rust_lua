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

pub struct LexState {
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
}

impl LexState {
    pub fn new(source: &str, chunk_name: &str) -> Self {
        let src = source.to_string();
        let first = src.chars().next().unwrap_or('\0');
        LexState {
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
        }
    }

    fn next_char(&mut self) {
        if self.current == '\n' {
            self.linenumber += 1;
        }
        if self.pos < self.source.len() {
            let bytes = self.source.as_bytes();
            if bytes[self.pos] < 0x80 {
                self.pos += 1;
            } else {
                let ch = self.source[self.pos..].chars().next().unwrap_or('\0');
                self.pos += ch.len_utf8();
            }
        }
        self.current = if self.pos < self.source.len() {
            self.source[self.pos..].chars().next().unwrap_or('\0')
        } else {
            '\0'
        };
    }

    fn peek(&self) -> char {
        let src = &self.source[self.pos..];
        let mut chars = src.chars();
        chars.next();
        chars.next().unwrap_or('\0')
    }

    fn skip_whitespace(&mut self) {
        loop {
            match self.current {
                ' ' | '\t' | '\r' | '\n' => self.next_char(),
                '-' if self.peek() == '-' => {
                    self.next_char();
                    self.next_char();
                    if self.current == '[' {
                        let start = self.pos;
                        let equals = self.count_equals();
                        if self.current == '[' {
                            self.read_long_comment(equals);
                            continue;
                        } else {
                            self.pos = start;
                            self.current = '-';
                            self.next_char();
                            self.next_char();
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
            let saved_pos = self.pos;
            let saved_ch = self.current;
            self.read_token();
            self.lookahead = Some((self.token.clone(), self.token_info.clone()));
            self.token = saved_token;
            self.token_info = saved_info;
            self.pos = saved_pos;
            self.current = saved_ch;
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
        let start = self.pos;
        let mut is_float = false;
        let mut is_hex = false;

        if self.current == '0' {
            let next = self.peek();
            if next == 'x' || next == 'X' {
                is_hex = true;
                self.next_char();
                self.next_char();
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
            match s.parse::<f64>() {
                Ok(v) => self.token = Token::Float(v),
                Err(_) => {
                    self.error(&format!("malformed number: {}", s));
                    self.token = Token::Float(0.0);
                }
            }
        } else {
            match i64::from_str_radix(s, if is_hex { 16 } else { 10 }) {
                Ok(v) => self.token = Token::Int(v),
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
                    s.push(c);
                    self.next_char();
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
                s.push(val as char);
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
                    s.push(val as u8 as char);
                } else {
                    s.push('?');
                }
            }
            '\n' | '\r' => {
                self.next_char();
                if self.current == '\n' {
                    self.next_char();
                }
            }
            c => { s.push(c); self.next_char(); }
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
                        let s = self.source[start..end].to_string();
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