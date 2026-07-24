use crate::token::Token;

pub struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    pub line: usize,
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
        self.skip_whitespace_and_comments();
        let line = self.line;

        let c = match self.chars.next() {
            Some(c) => c,
            None => return Ok(Spanned { token: Token::Eof, line }),
        };

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

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            match self.chars.peek() {
                Some('\n') => {
                    self.line += 1;
                    self.chars.next();
                }
                Some(c) if c.is_whitespace() => {
                    self.chars.next();
                }
                Some('/') => {
                    let mut clone = self.chars.clone();
                    clone.next();
                    if clone.peek() == Some(&'/') {
                        // line comment: consume until newline
                        while let Some(&c) = self.chars.peek() {
                            if c == '\n' {
                                break;
                            }
                            self.chars.next();
                        }
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    fn peek_char(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }
}
