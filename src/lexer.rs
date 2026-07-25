use crate::token::Token;

pub struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    pub line: usize,
    /// Whether every character seen since the last newline (or the start
    /// of the file) has been whitespace -- i.e. whether we're currently at
    /// the first non-whitespace position on a line. `#`/`#N` comments are
    /// only recognized there, never trailing after real code.
    at_line_start: bool,
}

#[derive(Debug, Clone)]
pub struct Spanned {
    pub token: Token,
    pub line: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Lexer {
            chars: source.chars().peekable(),
            line: 1,
            at_line_start: true,
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<Spanned>, String> {
        let mut tokens = Vec::new();
        loop {
            let spanned = self.next_token()?;
            let is_eof = spanned.token == Token::Eof;
            tokens.push(spanned);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }

    fn next_token(&mut self) -> Result<Spanned, String> {
        self.skip_whitespace_and_comments()?;
        let line = self.line;

        let c = match self.chars.next() {
            Some(c) => c,
            None => return Ok(Spanned { token: Token::Eof, line }),
        };
        self.at_line_start = false;

        let token = match c {
            '(' => Token::LParen,
            ')' => Token::RParen,
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            ',' => Token::Comma,
            ':' => Token::Colon,
            ';' => Token::Semicolon,
            '+' => Token::Plus,
            '*' => Token::Star,
            '/' => Token::Slash,
            '-' => {
                if self.peek_char() == Some('>') {
                    self.chars.next();
                    Token::Arrow
                } else {
                    Token::Minus
                }
            }
            '=' => {
                if self.peek_char() == Some('=') {
                    self.chars.next();
                    Token::EqEq
                } else {
                    Token::Eq
                }
            }
            '!' => Token::Bang,
            '<' => {
                if self.peek_char() == Some('=') {
                    self.chars.next();
                    Token::LtEq
                } else {
                    Token::Lt
                }
            }
            '>' => {
                if self.peek_char() == Some('=') {
                    self.chars.next();
                    Token::GtEq
                } else if self.peek_char() == Some('>') {
                    self.chars.next();
                    Token::Activate
                } else {
                    Token::Gt
                }
            }
            '&' => {
                if self.peek_char() == Some('&') {
                    self.chars.next();
                    Token::AndAnd
                } else {
                    return Err(format!("line {}: unexpected character '&'", line));
                }
            }
            '|' => {
                if self.peek_char() == Some('|') {
                    self.chars.next();
                    Token::OrOr
                } else {
                    return Err(format!("line {}: unexpected character '|'", line));
                }
            }
            '0'..='9' => self.lex_number(c),
            c if c.is_alphabetic() || c == '_' => self.lex_ident(c),
            '\'' => self.lex_quoted_ident(line)?,
            '"' => self.lex_string(line)?,
            other => return Err(format!("line {}: unexpected character '{}'", line, other)),
        };

        Ok(Spanned { token, line })
    }

    fn lex_number(&mut self, first: char) -> Token {
        let mut s = String::new();
        s.push(first);
        while let Some(&c) = self.chars.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.chars.next();
            } else {
                break;
            }
        }

        // Only consume a '.' as a decimal point if it's followed by a digit,
        // so a bare trailing '.' is left alone rather than swallowed.
        if self.peek_char() == Some('.') {
            let mut lookahead = self.chars.clone();
            lookahead.next();
            if matches!(lookahead.peek(), Some(d) if d.is_ascii_digit()) {
                s.push('.');
                self.chars.next();
                while let Some(&c) = self.chars.peek() {
                    if c.is_ascii_digit() {
                        s.push(c);
                        self.chars.next();
                    } else {
                        break;
                    }
                }
            }
        }

        let value = s.parse().expect("validated numeral must parse as f64");
        Token::Num(value, s)
    }

    /// Any character is allowed between the quotes (a deliberate choice --
    /// there's no character-set restriction on names anymore), except a
    /// literal `'`, which always closes it.
    fn lex_quoted_ident(&mut self, line: usize) -> Result<Token, String> {
        let mut s = String::new();
        loop {
            match self.chars.next() {
                Some('\'') => break,
                Some(ch) => {
                    if ch == '\n' {
                        self.line += 1;
                    }
                    s.push(ch);
                }
                None => return Err(format!("line {}: unterminated quoted identifier", line)),
            }
        }
        Ok(Token::Quoted(s))
    }

    fn lex_string(&mut self, line: usize) -> Result<Token, String> {
        let mut s = String::new();
        loop {
            match self.chars.next() {
                Some('"') => break,
                Some('\\') => match self.chars.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some(other) => {
                        if other == '\n' {
                            self.line += 1;
                        }
                        s.push(other);
                    }
                    None => return Err(format!("line {}: unterminated string literal", line)),
                },
                Some('\n') => {
                    self.line += 1;
                    s.push('\n');
                }
                Some(ch) => s.push(ch),
                None => return Err(format!("line {}: unterminated string literal", line)),
            }
        }
        Ok(Token::Str(s))
    }

    fn lex_ident(&mut self, first: char) -> Token {
        let mut s = String::new();
        s.push(first);
        while let Some(&c) = self.chars.peek() {
            if c.is_alphanumeric() || c == '_' {
                s.push(c);
                self.chars.next();
            } else {
                break;
            }
        }
        let token = Token::keyword_from_str(&s).unwrap_or(Token::Ident(s));
        if token == Token::Not && self.peek_char() == Some('=') {
            self.chars.next();
            Token::NotEq
        } else {
            token
        }
    }

    /// `#` (or `#N`, e.g. `#5`) comments out whole lines -- only recognized
    /// as the first non-whitespace thing on a line, never trailing after
    /// real code. Bare `#` comments out just that one line, same as `#1`.
    /// `#N` comments out N total lines starting with that one (`#5`
    /// comments out its own line and 4 more below it). `N` beyond the
    /// remaining lines in the file just comments out whatever's left.
    fn skip_whitespace_and_comments(&mut self) -> Result<(), String> {
        loop {
            match self.chars.peek() {
                Some('\n') => {
                    self.line += 1;
                    self.chars.next();
                    self.at_line_start = true;
                }
                Some(c) if c.is_whitespace() => {
                    self.chars.next();
                }
                Some('#') => {
                    if !self.at_line_start {
                        return Err(format!(
                            "line {}: '#' comments must start at the beginning of a line",
                            self.line
                        ));
                    }
                    self.chars.next();
                    let mut digits = String::new();
                    while let Some(&c) = self.chars.peek() {
                        if c.is_ascii_digit() {
                            digits.push(c);
                            self.chars.next();
                        } else {
                            break;
                        }
                    }
                    let line_count: usize = if digits.is_empty() { 1 } else { digits.parse().unwrap() };

                    for _ in 0..line_count {
                        while let Some(&c) = self.chars.peek() {
                            if c == '\n' {
                                break;
                            }
                            self.chars.next();
                        }
                        if self.chars.peek() == Some(&'\n') {
                            self.chars.next();
                            self.line += 1;
                            self.at_line_start = true;
                        } else {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(())
    }

    fn peek_char(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }
}
