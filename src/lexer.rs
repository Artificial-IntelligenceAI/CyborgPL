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
        let mut tokens: Vec<Spanned> = Vec::new();
        loop {
            // Right after `print*`, a `"` starts a raw literal: read via
            // lex_print_literal instead of the normal escaped-string rule,
            // so quotes inside print's own argument are just text.
            let after_print_star = tokens.len() >= 2
                && tokens[tokens.len() - 2].token == Token::Print
                && tokens[tokens.len() - 1].token == Token::Star;

            if after_print_star {
                self.skip_whitespace_and_comments();
                if self.peek_char() == Some('"') {
                    let line = self.line;
                    self.chars.next(); // consume opening '"'
                    let token = self.lex_print_literal(line)?;
                    tokens.push(Spanned { token, line });
                    continue;
                }
            }

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
            '!' => {
                if self.peek_char() == Some('=') {
                    self.chars.next();
                    Token::BangEq
                } else {
                    Token::Bang
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

        Token::Num(s.parse().expect("validated numeral must parse as f64"))
    }

    fn lex_quoted_ident(&mut self, line: usize) -> Result<Token, String> {
        let mut s = String::new();
        loop {
            match self.chars.next() {
                Some('\'') => break,
                Some(ch) => s.push(ch),
                None => return Err(format!("line {}: unterminated quoted identifier", line)),
            }
        }
        let valid = matches!(s.chars().next(), Some(c) if c.is_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_alphanumeric() || c == '_');
        if !valid {
            return Err(format!("line {}: '{}' is not a valid identifier", line, s));
        }
        Ok(Token::Quoted(s))
    }

    /// Reads print's `"..."` argument as one raw literal: the opening `"`
    /// is whatever the caller already consumed, and the *last* `"` seen
    /// before the next `*` is treated as the closing one -- any quotes in
    /// between are just literal characters, unlike a normal string literal
    /// where the first `"` always closes it. No escape processing either;
    /// everything between the two delimiting quotes prints exactly as
    /// written. Mixing this with other syntax (e.g. `stch`) inside the same
    /// print argument isn't supported -- it either gets swallowed into the
    /// literal or reported as a trailing-content error.
    fn lex_print_literal(&mut self, line: usize) -> Result<Token, String> {
        let mut raw: Vec<char> = Vec::new();
        loop {
            match self.chars.peek().copied() {
                Some('*') => break,
                Some(c) => {
                    if c == '\n' {
                        self.line += 1;
                    }
                    raw.push(c);
                    self.chars.next();
                }
                None => {
                    return Err(format!("line {}: unterminated print literal (missing closing '*')", line));
                }
            }
        }

        let close_idx = raw
            .iter()
            .rposition(|&c| c == '"')
            .ok_or_else(|| format!("line {}: print literal is missing a closing '\"'", line))?;

        if !raw[close_idx + 1..].iter().all(|c| c.is_whitespace()) {
            return Err(format!(
                "line {}: mixing a raw print literal with other syntax after the closing '\"' isn't supported yet",
                line
            ));
        }

        Ok(Token::Str(raw[..close_idx].iter().collect()))
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
                    Some(other) => s.push(other),
                    None => return Err(format!("line {}: unterminated string literal", line)),
                },
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
        Token::keyword_from_str(&s).unwrap_or(Token::Ident(s))
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
