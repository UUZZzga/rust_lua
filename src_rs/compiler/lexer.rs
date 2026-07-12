use std::collections::HashMap;

use crate::state::LuaState;
use crate::strings::LuaString;

/// 编译器内部缓冲区的缓存,用于避免每次编译时重复分配 Vec/String/HashMap 的堆内存。
/// 通过 `COMPILER_CACHE` 线程局部变量跨编译调用复用,减少 glibc 堆碎片和 RSS。
struct CompilerCache {
    errors: Vec<String>,
    scanner_strings: HashMap<String, LuaString>,
    token_text: String,
}

impl CompilerCache {
    fn new() -> Self {
        CompilerCache {
            errors: Vec::new(),
            scanner_strings: HashMap::new(),
            token_text: String::new(),
        }
    }
    fn clear(&mut self) {
        self.errors.clear();
        self.scanner_strings.clear();
        self.token_text.clear();
    }
}

thread_local! {
    static COMPILER_CACHE: std::cell::RefCell<Option<Box<CompilerCache>>> = const { std::cell::RefCell::new(None) };
}

/// 词法分析器使用的 EOF 标记字符。
///
/// 对应 C 中的 EOZ (-1)。C 的 `ls->current` 是 int 类型,可以区分 \0 (0) 和 EOZ (-1)。
/// Rust 中 `char` 不能为 -1,因此使用 Unicode 最大码点 U+10FFFF 作为 sentinel。
/// 该字符不会出现在合法的 Lua 源码中 (它不属于任何 Lua 标识符、字符串字面量或
/// 转义序列),因此可安全地用作 EOF 标记,让源码中真正的 \0 字节 (如 reader 函数
/// 模式下读到的字节流) 能被词法分析器正确处理。
pub const EOF_CHAR: char = '\u{10FFFF}';

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

    /// 单字符 token (对应 C llex default 分支返回的非保留字符)
    /// 用于未知字符 (如控制字符 \1), 让解析器报 "syntax error" 或 "unexpected symbol"
    Char(char),

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

    /// 将 token 转换为可读字符串，对应 C 的 `txtToken` + `luaX_token2str`。
    ///
    /// 用于错误消息中显示当前 token (如 `"unexpected symbol near '%s'"`)。
    /// - Name/String/Int/Float: 返回 `"'<value>'"` (带引号)
    /// - Eof: 返回 `"<eof>"`
    /// - 关键字/符号: 返回 `"'<name>'"` (如 `"'and'"`, `"'//'"`)
    pub fn to_display_str(&self) -> String {
        match self {
            Token::Name(s) => format!("'{}'", s),
            Token::String(s) => format!("'{}'", s),
            Token::Int(n) => format!("'{}'", n),
            Token::Float(f) => format!("'{}'", f),
            Token::Eof => "<eof>".to_string(),
            // 关键字
            Token::And => "'and'".to_string(),
            Token::Break => "'break'".to_string(),
            Token::Do => "'do'".to_string(),
            Token::Else => "'else'".to_string(),
            Token::Elseif => "'elseif'".to_string(),
            Token::End => "'end'".to_string(),
            Token::False => "'false'".to_string(),
            Token::For => "'for'".to_string(),
            Token::Function => "'function'".to_string(),
            Token::Goto => "'goto'".to_string(),
            Token::If => "'if'".to_string(),
            Token::In => "'in'".to_string(),
            Token::Local => "'local'".to_string(),
            Token::Nil => "'nil'".to_string(),
            Token::Not => "'not'".to_string(),
            Token::Or => "'or'".to_string(),
            Token::Repeat => "'repeat'".to_string(),
            Token::Return => "'return'".to_string(),
            Token::Then => "'then'".to_string(),
            Token::True => "'true'".to_string(),
            Token::Until => "'until'".to_string(),
            Token::While => "'while'".to_string(),
            // 符号
            Token::Plus => "'+'".to_string(),
            Token::Minus => "'-'".to_string(),
            Token::Star => "'*'".to_string(),
            Token::Slash => "'/'".to_string(),
            Token::Percent => "'%'".to_string(),
            Token::Caret => "'^'".to_string(),
            Token::Hash => "'#'".to_string(),
            Token::Ampersand => "'&'".to_string(),
            Token::Tilde => "'~'".to_string(),
            Token::Pipe => "'|'".to_string(),
            Token::LtLt => "'<<'".to_string(),
            Token::GtGt => "'>>'".to_string(),
            Token::SlashSlash => "'//'".to_string(),
            Token::EqEq => "'=='".to_string(),
            Token::TildeEq => "'~='".to_string(),
            Token::LtEq => "'<='".to_string(),
            Token::GtEq => "'>='".to_string(),
            Token::Lt => "'<'".to_string(),
            Token::Gt => "'>'".to_string(),
            Token::Eq => "'='".to_string(),
            Token::LParen => "'('".to_string(),
            Token::RParen => "')'".to_string(),
            Token::LBrace => "'{'".to_string(),
            Token::RBrace => "'}'".to_string(),
            Token::LBracket => "'['".to_string(),
            Token::RBracket => "']'".to_string(),
            Token::ColonColon => "'::'".to_string(),
            Token::Dot => "'.'".to_string(),
            Token::DotDot => "'..'".to_string(),
            Token::DotDotDot => "'...'".to_string(),
            Token::Comma => "','".to_string(),
            Token::Colon => "':'".to_string(),
            Token::Semi => "';'".to_string(),
            // 对应 C luaX_token2str: 可打印字符 "'c'", 控制字符 "'<\N>'"
            Token::Char(c) => {
                if c.is_ascii_graphic() || *c == ' ' {
                    format!("'{}'", c)
                } else {
                    format!("'<\\{}>'", *c as u32)
                }
            }
        }
    }
}

/// 从字节切片的指定位置安全地读取一个字符。
/// 无效 UTF-8 字节按单字节处理 (作为 Latin-1 字符),对应 C 版本按字节读取源码的行为。
///
/// pos 越界时返回 `EOF_CHAR` (而非 '\0'),以便区分源码中的真实 \0 字节与 EOF。
/// (C 版本用 int 的 -1 表示 EOZ; Rust 中用 U+10FFFF sentinel。)
fn read_char_at(bytes: &[u8], pos: usize) -> char {
    if pos >= bytes.len() {
        return EOF_CHAR;
    }
    if bytes[pos] < 0x80 {
        return bytes[pos] as char;
    }
    match std::str::from_utf8(&bytes[pos..]).ok().and_then(|s| s.chars().next()) {
        Some(ch) => ch,
        None => bytes[pos] as char,  // 无效 UTF-8: 按字节值作为 char
    }
}

/// 对应 C 的 luaO_chunkid：将 chunk_name 格式化为短源标识
/// 用于错误消息中的 source 前缀（`[string "..."]:line:`）
/// LUA_IDSIZE = 60, 输出最多 59 字符（留 1 个给 '\0'）
pub fn format_chunk_id(chunk_name: &str) -> String {
    const LUA_IDSIZE: usize = 60;
    let bytes = chunk_name.as_bytes();
    if bytes.is_empty() {
        return "?".to_string();
    }
    match bytes[0] {
        b'=' => {
            let content = &bytes[1..];
            if content.len() <= LUA_IDSIZE - 1 {
                String::from_utf8_lossy(content).into_owned()
            } else {
                String::from_utf8_lossy(&content[..LUA_IDSIZE - 1]).into_owned()
            }
        }
        b'@' => {
            // C: srclen 含 '@'，bufflen = 60
            //    if srclen <= 60: 显示 content (srclen-1 字符)
            //    else: "..." + 末尾 56 字符 = 59 字符
            let content = &bytes[1..];
            if content.len() <= LUA_IDSIZE - 1 {
                String::from_utf8_lossy(content).into_owned()
            } else {
                let tail_len = LUA_IDSIZE - 3 - 1; // 56
                let start = content.len() - tail_len;
                format!("...{}", String::from_utf8_lossy(&content[start..]))
            }
        }
        _ => {
            // C: PRE = "[string \"" (9), POS = "\"]" (2), RETS = "..." (3)
            //    bufflen = 60 - (9+3+2) - 1 = 45
            const PRE_LEN: usize = 9;
            const POS_LEN: usize = 2;
            const RETS_LEN: usize = 3;
            let bufflen = LUA_IDSIZE - PRE_LEN - POS_LEN - RETS_LEN - 1; // 45

            let nl_pos = bytes.iter().position(|&b| b == b'\n');
            let effective_len = nl_pos.unwrap_or(bytes.len());

            if effective_len < bufflen && nl_pos.is_none() {
                format!("[string \"{}\"]", String::from_utf8_lossy(&bytes[..effective_len]))
            } else {
                let n = effective_len.min(bufflen);
                format!("[string \"{}...\"]", String::from_utf8_lossy(&bytes[..n]))
            }
        }
    }
}

pub struct LexState<'a> {
    pub state: &'a mut LuaState,
    pub source: &'a str,
    pub chunk_name: &'a str,
    pub pos: usize,
    pub current: char,
    pub linenumber: i32,
    pub lastline: i32,
    pub token: Token,
    pub lookahead: Option<Token>,
    pub errors: Vec<String>,
    pub nesting_level: u32,  // recursion depth counter (like C's nCcalls)
    /// Scanner string table — 对应 C 的 `ls->h`。
    /// 锚定长字符串字面量,确保同一源码中的长字符串返回同一 `LuaString` (相同 ptr_id)。
    /// 短字符串已通过全局 `StringTable` 内部化去重,无需在此重复。
    scanner_strings: HashMap<String, LuaString>,
    /// 当前 token 的原始文本 — 对应 C 的 `luaZ_buffer(ls->buff)`。
    /// 用于错误消息中显示数字/字符串的原始文本 (如 "1.000" 而不是 "1")。
    pub token_text: String,
    /// 编译器执行缓存句柄。持有 `CompilerCache` 的 Box,确保其堆内存不被释放。
    /// Drop 时将内部缓冲 (`errors`、`scanner_strings`、`token_text`)回收到线程局部缓存,
    /// 供下一次编译复用,避免每次编译时重新分配堆内存。
    _cache: Option<Box<CompilerCache>>,
}
impl<'a> LexState<'a> {
    pub fn new(state: &'a mut LuaState, source: &'a str, chunk_name: &'a str) -> Self {
        // 从线程局部缓存中获取可重用的内部缓冲,避免每次编译时重新分配堆内存。
        let (errors, scanner_strings, token_text, cache_holder) = COMPILER_CACHE.with(|c| {
            let mut cell = c.borrow_mut();
            let mut boxed = cell.take().unwrap_or_else(|| Box::new(CompilerCache::new()));
            boxed.clear();
            let errors = std::mem::take(&mut boxed.errors);
            let scanner_strings = std::mem::take(&mut boxed.scanner_strings);
            let token_text = std::mem::take(&mut boxed.token_text);
            (errors, scanner_strings, token_text, boxed)
        });
        let first = read_char_at(source.as_bytes(), 0);
        LexState {
            state,
            source,
            chunk_name,
            pos: 0,
            current: first,
            linenumber: 1,
            lastline: 1,
            token: Token::Eof,
            lookahead: None,
            errors,
            nesting_level: 0,
            scanner_strings,
            token_text,
            _cache: Some(cache_holder),
        }
    }

    /// 锚定字符串字面量,对应 C 的 `anchorstr` (llex.cpp)。
    /// 短字符串走全局 `StringTable` 内部化;长字符串走 scanner table 去重,
    /// 确保同一源码字面量跨 proto 返回同一 `LuaString` (相同 `ptr_id`)。
    pub fn anchor_string(&mut self, s: &str) -> LuaString {
        if s.len() <= crate::strings::LUAI_MAXSHORTLEN {
            crate::strings::new_lstr(&self.state.string_table, s)
        } else {
            if let Some(existing) = self.scanner_strings.get(s).cloned() {
                return existing;
            }
            let ls = crate::strings::new_long_str(s);
            self.scanner_strings.insert(s.to_string(), ls.clone());
            ls
        }
    }

    fn next_char(&mut self) {
        let old = self.current;
        if old == '\n' || old == '\r' {
            self.linenumber += 1;
        }
        self.advance_pos();
        // 处理 \n\r 或 \r\n 配对: 两者合为一个换行 (对应 C inclinenumber 的配对逻辑)
        if (old == '\n' || old == '\r')
            && (self.current == '\n' || self.current == '\r')
            && self.current != old
        {
            self.advance_pos();
        }
    }

    /// 仅推进位置指针并更新 current,不处理行号。
    fn advance_pos(&mut self) {
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
                // 对应 C llex 中的空白: ' ', '\f', '\t', '\v' (以及 '\n','\r' 通过 inclinenumber)
                // lispace 表 (lctype.c) 将这 6 个字符均标记为 SPACEBIT
                ' ' | '\t' | '\r' | '\n' | '\u{0B}' | '\u{0C}' => self.next_char(),
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
                    while self.current != '\n' && self.current != '\r' && self.current != EOF_CHAR {
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

    /// 跳过换行序列 (\n, \r, \n\r, \r\n) 并增加行号，对应 C 的 `inclinenumber`。
    ///
    /// Rust 的 `next_char` 只在 `current == '\n'` 时增加行号；对于单独的 '\r'
    /// (旧 Mac 换行) 不会增加。此函数确保任何换行序列都只算一次换行。
    fn inclinenumber(&mut self) {
        let old = self.current;
        let line_before = self.linenumber;
        self.next_char();
        if (self.current == '\n' || self.current == '\r') && self.current != old {
            self.next_char();
        }
        if self.linenumber == line_before {
            self.linenumber += 1;
        }
    }

    fn read_long_comment(&mut self, eqs: usize) {
        self.next_char();
        loop {
            match self.current {
                EOF_CHAR => {
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
        self.errors.push(format!("{}:{}: {}", format_chunk_id(&self.chunk_name), self.linenumber, msg));
    }

    /// 转义序列错误，对应 C 的 `esccheck` + `lexerror(msg, TK_STRING)`。
    ///
    /// 构造 `msg near 'token'` 格式的错误消息，其中 token 包含已读取的字符串内容 `s`
    /// 加上从 `\\` 开始到当前字符的源码片段（对应 C buffer 中保存的内容）。
    fn escape_error(&mut self, s: &str, backslash_pos: usize, msg: &str) {
        let mut token = String::new();
        token.push_str(s);
        let bytes = &self.source.as_bytes()[backslash_pos..self.pos];
        unsafe { token.as_mut_vec().extend_from_slice(bytes); }
        if self.current != EOF_CHAR {
            token.push(self.current);
        }
        self.errors.push(format!("{}:{}: {} near '{}'",
            format_chunk_id(&self.chunk_name), self.linenumber, msg, token));
    }

    pub fn next(&mut self) {
        self.lastline = self.linenumber;
        if let Some(tok) = self.lookahead.take() {
            self.token = tok;
            return;
        }
        self.read_token();
    }

    pub fn lookahead_next(&mut self) -> &Token {
        if self.lookahead.is_none() {
            let saved_token = self.token.clone();
            self.read_token();
            self.lookahead = Some(self.token.clone());
            self.token = saved_token;
        }
        self.lookahead.as_ref().unwrap()
    }

    /// 返回当前 token 的显示字符串, 对应 C 的 `txtToken`。
    ///
    /// 对 Name/String/Int/Float: 优先使用 `token_text` (原始文本, 如 "1.000"),
    ///   回退到 `to_display_str()` (格式化后的值, 如 "1")。
    ///   这对应 C 中 `luaZ_buffer(ls->buff)` 保存的原始扫描文本。
    /// 对其他 token (关键字/符号/Eof): 直接使用 `to_display_str()`。
    pub fn token_display(&self) -> String {
        match &self.token {
            Token::Name(_) | Token::String(_) | Token::Int(_) | Token::Float(_) => {
                if !self.token_text.is_empty() {
                    format!("'{}'", self.token_text)
                } else {
                    self.token.to_display_str()
                }
            }
            _ => self.token.to_display_str(),
        }
    }

    fn read_token(&mut self) {
        self.skip_whitespace();
        // 对应 C: luaZ_resetbuffer(ls->buff) — 每个新 token 开始前清空原始文本缓冲。
        // 只有 read_name/read_number/read_short_string/read_long_string 会填充它,
        // 其他 token (符号/关键字) 保持为空, token_display() 会回退到 to_display_str()。
        self.token_text.clear();
        match self.current {
            EOF_CHAR => self.token = Token::Eof,
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
                let start_pos = self.pos;  // '[' 的位置, 用于长字符串 token_text
                self.next_char();
                let eqs = self.count_equals();
                if self.current == '[' {
                    self.read_long_string(eqs);
                    // 设置 token_text 包含完整的长字符串字面量 (含 [=...[ ... ]=...])
                    // 对应 C: luaZ_buffer(ls->buff) 在 read_long_string 期间累积的所有字符
                    if self.pos > start_pos {
                        self.token_text = std::str::from_utf8(
                            &self.source.as_bytes()[start_pos..self.pos])
                            .unwrap_or("").to_string();
                    }
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
            c if c.is_ascii_alphabetic() || c == '_' => self.read_name(),
            EOF_CHAR => self.token = Token::Eof,
            c => {
                // 对应 C llex default 分支: 非字母数字的单字符直接返回为 token
                // (如控制字符 \1), 解析器在 primaryexp/exprstat 中报 "syntax error"
                // 或 "unexpected symbol". luaX_token2str 对控制字符显示 '<\N>'.
                self.next_char();
                self.token = Token::Char(c);
            }
        }
    }

    fn read_name(&mut self) {
        let start = self.pos;
        while self.current.is_ascii_alphanumeric() || self.current == '_' {
            self.next_char();
        }
        // 源码可能包含非法 UTF-8 字节 (如 string.char(0x80)),str 切片会在
        // continuation byte 处 panic。此处用字节切片绕过边界检查。
        // read_name 只消费 ASCII 字母数字/下划线,内容必为合法 UTF-8。
        let s = std::str::from_utf8(&self.source.as_bytes()[start..self.pos]).unwrap();
        self.token_text = s.to_string();
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

        // 对应 C: if (lislalpha(ls->current)) save_and_next(ls);  /* force an error */
        // 数字后紧跟字母/下划线时,将其包含进 token 以触发 "malformed number" 错误
        if self.current.is_ascii_alphabetic() || self.current == '_' {
            self.next_char();
        }

        let s = std::str::from_utf8(&self.source.as_bytes()[start..self.pos]).unwrap();
        self.token_text = s.to_string();
        if is_float {
            if is_hex {
                match parse_hex_float(s) {
                    Some(v) => self.token = Token::Float(v),
                    None => {
                        // 对应 C: lexerror(ls, "malformed number", TK_FLT)
                        // txtToken(TK_FLT) 返回 buffer 内容 (含 0x 前缀), 带引号
                        self.error(&format!("malformed number near '0x{}'", s));
                        self.token = Token::Eof;
                    }
                }
            } else {
                match s.parse::<f64>() {
                    Ok(v) => self.token = Token::Float(v),
                    Err(_) => {
                        self.error(&format!("malformed number near '{}'", s));
                        self.token = Token::Eof;
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
                        self.error(&format!("malformed number near '{}'", s));
                        self.token = Token::Eof;
                    }
                },
            }
        }
    }

    fn read_short_string(&mut self) {
        let delim = self.current;
        let text_start = self.pos;  // 引号的起始位置
        self.next_char();
        let mut s = String::new();
        loop {
            match self.current {
                EOF_CHAR => {
                    // 对应 C: lexerror(ls, "unfinished string", TK_EOS);
                    self.error("unfinished string near <eof>");
                    break;
                }
                '\n' | '\r' => {
                    // 对应 C: lexerror(ls, "unfinished string", TK_STRING);
                    self.error("unfinished string");
                    break;
                }
                '\\' => {
                    let backslash_pos = self.pos;
                    self.next_char();
                    self.read_escape(&mut s, backslash_pos);
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
        // 设置 token_text 包含原始字面量 (含引号), 对应 C 的 luaZ_buffer(ls->buff)
        self.token_text = std::str::from_utf8(&self.source.as_bytes()[text_start..self.pos])
            .unwrap_or("").to_string();
        self.token = Token::String(s);
    }

    fn read_escape(&mut self, s: &mut String, backslash_pos: usize) {
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
                // 对应 C read_string 中 '\z' 分支: 跳过后续所有空白 (lisspace)
                // 包括 ' ', '\t', '\n', '\r', '\v' (\u{0B}), '\f' (\u{0C}); 换行需更新行号
                while matches!(self.current, ' ' | '\t' | '\r' | '\n' | '\u{0B}' | '\u{0C}') {
                    self.next_char();
                }
            }
            'x' => {
                // 对应 C readhexaesc: 要求恰好 2 个十六进制数字
                self.next_char();  // skip 'x'
                let mut val: u32 = 0;
                for _ in 0..2 {
                    if let Some(d) = self.current.to_digit(16) {
                        val = val * 16 + d;
                        self.next_char();
                    } else {
                        self.escape_error(s, backslash_pos, "hexadecimal digit expected");
                        return;
                    }
                }
                unsafe { s.as_mut_vec().push(val as u8); }
            }
            'u' => {
                self.next_char();  // skip 'u'
                if self.current != '{' {
                    self.escape_error(s, backslash_pos, "missing '{'");
                    return;
                }
                self.next_char();  // skip '{'
                let mut r: u32 = 0;
                let mut has_digit = false;
                while self.current.is_ascii_hexdigit() {
                    has_digit = true;
                    if r > (0x7FFFFFFFu32 >> 4) {
                        self.escape_error(s, backslash_pos, "UTF-8 value too large");
                        return;
                    }
                    r = (r << 4) + (self.current.to_digit(16).unwrap() as u32);
                    self.next_char();
                }
                if !has_digit {
                    self.escape_error(s, backslash_pos, "missing digits");
                    return;
                }
                if self.current != '}' {
                    self.escape_error(s, backslash_pos, "missing '}'");
                    return;
                }
                self.next_char();  // skip '}'
                // 使用 UTF-8 编码（支持 1-6 字节，等价于 C 版本 luaO_utf8esc）
                for b in utf8_encode(r) {
                    unsafe { s.as_mut_vec().push(b); }
                }
            }
            '0'..='9' => {
                // 对应 C readdecesc: 最多 3 位十进制，值不能超过 255
                let mut val: u32 = 0;
                for _ in 0..3 {
                    if self.current.is_ascii_digit() {
                        val = val * 10 + (self.current as u8 - b'0') as u32;
                        self.next_char();
                    } else {
                        break;
                    }
                }
                if val > 0xFF {
                    self.escape_error(s, backslash_pos, "decimal escape too large");
                    return;
                }
                unsafe { s.as_mut_vec().push(val as u8); }
            }
            '\n' | '\r' => {
                // 对应 C: inclinenumber(ls); c = '\n'; goto only_save;
                // 跳过换行序列 (\n, \r, \n\r, \r\n) 并在字符串中存入换行符
                self.inclinenumber();
                s.push('\n');
            }
            EOF_CHAR => {
                // 对应 C: case EOZ: goto no_save; (下一轮报 "unfinished string")
            }
            _ => {
                // 对应 C default: esccheck(lisdigit, "invalid escape sequence")
                self.escape_error(s, backslash_pos, "invalid escape sequence");
            }
        }
    }

    fn read_long_string(&mut self, eqs: usize) {
        self.next_char();  // skip 2nd '['
        // 对应 C: if (currIsNewline(ls)) inclinenumber(ls);  /* skip it */
        if self.current == '\n' || self.current == '\r' {
            self.inclinenumber();
        }
        let mut s = String::new();
        loop {
            match self.current {
                EOF_CHAR => {
                    // 对应 C: lexerror(ls, "unfinished long string (starting at line X)", TK_EOS);
                    self.error("unfinished long string near <eof>");
                    break;
                }
                ']' => {
                    // 对应 C skip_sep: 检查是否为结束分隔符 ]=...]
                    self.next_char();  // skip ']'
                    let actual = self.count_equals();  // count (并跳过) '='
                    if actual == eqs && self.current == ']' {
                        self.next_char();  // skip 2nd ']'
                        self.token = Token::String(s);
                        return;
                    }
                    // 不匹配: ']' 与 '=' 需作为内容保留 (对应 C skip_sep 的 save 行为)
                    s.push(']');
                    for _ in 0..actual {
                        s.push('=');
                    }
                    // current 在 '=' 之后的字符,继续循环处理
                }
                '\n' | '\r' => {
                    // 对应 C: save(ls, '\n'); inclinenumber(ls);
                    s.push('\n');
                    self.inclinenumber();
                }
                c => {
                    // 直接拷贝源字节,以正确处理非 UTF-8 字节
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

    pub fn check(&self, tok: &Token) -> bool {
        std::mem::discriminant(&self.token) == std::mem::discriminant(tok)
    }

    pub fn check_next(&mut self, tok: &Token) -> bool {
        let next_tok = self.lookahead_next().clone();
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

impl<'a> Drop for LexState<'a> {
    /// 将内部缓冲 (`errors`、`scanner_strings`、`token_text`) 回收到线程局部缓存,
    /// 供下一次编译复用。避免 glibc 因频繁分配／释放小对象产生的堆碎片和页缓存膨胀。
    fn drop(&mut self) {
        if let Some(mut boxed) = self._cache.take() {
            boxed.errors = std::mem::take(&mut self.errors);
            boxed.scanner_strings = std::mem::take(&mut self.scanner_strings);
            boxed.token_text = std::mem::take(&mut self.token_text);
            COMPILER_CACHE.with(|c| *c.borrow_mut() = Some(boxed));
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