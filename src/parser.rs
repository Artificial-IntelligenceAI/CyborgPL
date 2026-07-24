use crate::ast::*;
use crate::lexer::Spanned;
use crate::token::Token;

pub struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
}

type PResult<T> = Result<T, String>;

impl Parser {
    pub fn new(tokens: Vec<Spanned>) -> Self {
        Parser { tokens, pos: 0 }
    }

    pub fn parse_program(&mut self) -> PResult<Program> {
        let mut functions = Vec::new();
        let mut entry: Option<Block> = None;

        while !self.check(&Token::Eof) {
            match self.peek() {
                Token::Fn => functions.push(self.parse_function()?),
                Token::Start => {
                    if entry.is_some() {
                        return Err(format!(
                            "line {}: only one START...END block is allowed",
                            self.tokens[self.pos].line
                        ));
                    }
                    entry = Some(self.parse_entry()?);
                }
                other => {
                    return Err(format!(
                        "line {}: expected 'fn' or 'START', found {:?}",
                        self.tokens[self.pos].line, other
                    ));
                }
            }
        }

        let entry = entry
            .ok_or_else(|| "program is missing a START...END entry point".to_string())?;
        Ok(Program { functions, entry })
    }

    fn parse_entry(&mut self) -> PResult<Block> {
        self.expect(Token::Start)?;
        let mut stmts = Vec::new();
        while !self.check(&Token::End) {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(Token::End)?;
        Ok(stmts)
    }

    fn parse_function(&mut self) -> PResult<Function> {
        self.expect(Token::Fn)?;
        let name = self.expect_quoted_ident()?;
        self.expect(Token::LParen)?;

        let mut params = Vec::new();
        if !self.check(&Token::RParen) {
            loop {
                let pname = self.expect_quoted_ident()?;
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                params.push(Param { name: pname, ty });
                if self.check(&Token::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;

        let return_type = if self.check(&Token::Arrow) {
            self.advance();
            self.parse_type()?
        } else {
            Type::Void
        };

        let body = self.parse_block()?;

        Ok(Function { name, params, return_type, body })
    }

    fn parse_type(&mut self) -> PResult<Type> {
        let name = self.expect_ident()?;
        match name.as_str() {
            "num" => Ok(Type::Num(DEFAULT_NUM_PRECISION)),
            "numw" => Ok(Type::NumW(DEFAULT_NUM_PRECISION)),
            "bool" => Ok(Type::Bool),
            "str" => Ok(Type::Str),
            "bignum" => Ok(Type::BigNum(DEFAULT_BIGNUM_PRECISION)),
            other => Err(format!("unknown type '{other}'")),
        }
    }

    /// Parses an optional `[precision:N]` suffix and applies it to `ty` if
    /// present, returning `ty` unchanged otherwise. Validated per-type:
    /// `num`/`numw` only accept 16/32/64/128 (real hardware float widths);
    /// `bignum` accepts any positive whole number of bits (GMP doesn't
    /// care, it's not tied to a hardware format).
    fn parse_optional_precision(&mut self, ty: Type) -> PResult<Type> {
        if !self.check(&Token::LBracket) {
            return Ok(ty);
        }
        self.advance();
        let line = self.tokens[self.pos].line;
        let word = self.expect_ident()?;
        if word != "precision" {
            return Err(format!("line {line}: expected 'precision', found '{word}'"));
        }
        self.expect(Token::Colon)?;
        let n = match self.peek().clone() {
            Token::Num(n) => {
                self.advance();
                n
            }
            other => return Err(format!("line {line}: expected a precision number, found {other:?}")),
        };
        self.expect(Token::RBracket)?;

        match ty {
            Type::Num(_) => {
                if ![16.0, 32.0, 64.0, 128.0].contains(&n) {
                    return Err(format!("line {line}: num precision must be 16, 32, 64, or 128, found {n}"));
                }
                Ok(Type::Num(n as u32))
            }
            Type::NumW(_) => {
                if ![16.0, 32.0, 64.0, 128.0].contains(&n) {
                    return Err(format!("line {line}: numw precision must be 16, 32, 64, or 128, found {n}"));
                }
                Ok(Type::NumW(n as u32))
            }
            Type::BigNum(_) => {
                if n < 1.0 || n.fract() != 0.0 {
                    return Err(format!(
                        "line {line}: bignum precision must be a positive whole number of bits, found {n}"
                    ));
                }
                Ok(Type::BigNum(n as u32))
            }
            _ => Err(format!("line {line}: [precision:N] only applies to num or bignum, not {ty:?}")),
        }
    }

    /// If `ty` is `numw` and the next token is a quoted number-word literal
    /// (`'1 million'`), consumes it and returns the computed value. Otherwise
    /// leaves the parser untouched and returns `None` -- numw still accepts
    /// any ordinary numeric expression too, this is just an extra literal
    /// form layered on top, not a replacement for the general grammar.
    fn try_parse_numw_literal(&mut self, ty: Type) -> PResult<Option<Expr>> {
        if !matches!(ty, Type::NumW(_)) {
            return Ok(None);
        }
        let line = self.tokens[self.pos].line;
        match self.peek().clone() {
            Token::Quoted(word) => {
                self.advance();
                let value = parse_numw_string(&word).map_err(|e| format!("line {line}: {e}"))?;
                Ok(Some(Expr::Num(value)))
            }
            _ => Ok(None),
        }
    }

    fn parse_block(&mut self) -> PResult<Block> {
        self.expect(Token::LBrace)?;
        let mut stmts = Vec::new();
        while !self.check(&Token::RBrace) {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(Token::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        match self.peek() {
            Token::Var => {
                self.advance();
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                let name = self.expect_quoted_ident()?;
                self.expect(Token::Eq)?;
                let value = match self.try_parse_numw_literal(ty)? {
                    Some(v) => v,
                    None => self.parse_expr()?,
                };
                let ty = self.parse_optional_precision(ty)?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::VarDecl(name, ty, value))
            }
            Token::Ref => {
                // Either a reassignment (`ref:var:TYPE 'name' = expr;`) or a
                // bare variable reference used as a statement on its own.
                // `ref:var:TYPE 'name'` parses as a plain Expr::Var, and `=`
                // isn't part of any expression grammar rule, so parse_expr
                // naturally stops right where we need to check for it --
                // no backtracking required.
                let line = self.tokens[self.pos].line;
                let expr = self.parse_expr()?;
                if self.check(&Token::Eq) {
                    let Expr::Var(name, ty) = expr else {
                        return Err(format!(
                            "line {line}: left side of '=' must be a plain ref:var:TYPE 'name'"
                        ));
                    };
                    self.advance();
                    let value = match self.try_parse_numw_literal(ty)? {
                        Some(v) => v,
                        None => self.parse_expr()?,
                    };
                    self.expect(Token::Semicolon)?;
                    return Ok(Stmt::Assign(name, ty, value));
                }
                self.expect(Token::Semicolon)?;
                Ok(Stmt::ExprStmt(expr))
            }
            Token::Return => {
                self.advance();
                if self.check(&Token::Semicolon) {
                    self.advance();
                    Ok(Stmt::Return(None))
                } else {
                    let value = self.parse_expr()?;
                    self.expect(Token::Semicolon)?;
                    Ok(Stmt::Return(Some(value)))
                }
            }
            Token::Print => {
                self.advance();
                self.expect(Token::Star)?;
                let mut segments = Vec::new();
                loop {
                    match self.peek().clone() {
                        Token::Str(s) => {
                            self.advance();
                            segments.push(PrintSegment::Str(s));
                        }
                        Token::LParen => {
                            self.advance();
                            let value = self.parse_expr()?;
                            self.expect(Token::RParen)?;
                            segments.push(PrintSegment::Expr(value));
                        }
                        Token::Star => break,
                        other => {
                            return Err(format!(
                                "line {}: expected a \"literal\", a (parenthesized value), or the closing '*', found {:?}",
                                self.tokens[self.pos].line, other
                            ));
                        }
                    }
                }
                self.expect(Token::Star)?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Print(segments))
            }
            Token::If => {
                self.advance();
                let cond = self.parse_expr()?;
                let then_block = self.parse_block()?;
                let else_block = if self.check(&Token::Else) {
                    self.advance();
                    Some(self.parse_block()?)
                } else {
                    None
                };
                Ok(Stmt::If(cond, then_block, else_block))
            }
            Token::While => {
                self.advance();
                let cond = self.parse_expr()?;
                let body = self.parse_block()?;
                Ok(Stmt::While(cond, body))
            }
            _ => {
                let expr = self.parse_expr()?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::ExprStmt(expr))
            }
        }
    }

    // Expression parsing, lowest to highest precedence:
    // or -> and -> equality -> comparison -> term -> factor -> power -> unary -> primary
    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        let mut left = self.parse_and()?;
        while self.check(&Token::OrOr) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Binary(Box::new(left), BinOp::Or, Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        let mut left = self.parse_equality()?;
        while self.check(&Token::AndAnd) {
            self.advance();
            let right = self.parse_equality()?;
            left = Expr::Binary(Box::new(left), BinOp::And, Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> PResult<Expr> {
        let mut left = self.parse_comparison()?;
        loop {
            let op = match self.peek() {
                Token::EqEq => BinOp::Eq,
                Token::BangEq => BinOp::Ne,
                _ => break,
            };
            self.advance();
            let right = self.parse_comparison()?;
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> PResult<Expr> {
        let mut left = self.parse_term()?;
        loop {
            let op = match self.peek() {
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::LtEq => BinOp::Le,
                Token::GtEq => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.parse_term()?;
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_term(&mut self) -> PResult<Expr> {
        let mut left = self.parse_factor()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_factor()?;
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_factor(&mut self) -> PResult<Expr> {
        let mut left = self.parse_power()?;
        loop {
            let op = match self.peek() {
                Token::Mul => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let right = self.parse_power()?;
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    /// `xx` (power) and `xxx` (tetration), right-associative and binding
    /// tighter than `x`/`/` but looser than unary `-`/`!`.
    fn parse_power(&mut self) -> PResult<Expr> {
        let left = self.parse_unary()?;
        let op = match self.peek() {
            Token::Pow => BinOp::Pow,
            Token::Tetration => BinOp::Tetration,
            _ => return Ok(left),
        };
        self.advance();
        let right = self.parse_power()?;
        Ok(Expr::Binary(Box::new(left), op, Box::new(right)))
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        match self.peek() {
            Token::Minus => {
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::Unary(UnOp::Neg, Box::new(operand)))
            }
            Token::Bang => {
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::Unary(UnOp::Not, Box::new(operand)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Token::Num(n) => {
                self.advance();
                Ok(Expr::Num(n))
            }
            Token::Str(s) => {
                self.advance();
                Ok(Expr::Str(s))
            }
            Token::True => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            Token::False => {
                self.advance();
                Ok(Expr::Bool(false))
            }
            Token::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            // `ref:var:TYPE 'name'` is the only way to read a variable's
            // value now. The type is which variable named 'name' this is --
            // the same name can be shared by variables of different types.
            Token::Ref => {
                self.advance();
                self.expect(Token::Colon)?;
                self.expect(Token::Var)?;
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                let ty = self.parse_optional_precision(ty)?;
                let name = self.expect_quoted_ident()?;
                Ok(Expr::Var(name, ty))
            }
            // A quoted name is only ever a function name now, and only valid
            // when immediately called with '(' -- plain 'name' alone isn't
            // a variable reference anymore (that's ref:var:TYPE 'name').
            Token::Quoted(name) => {
                let line = self.tokens[self.pos].line;
                self.advance();
                if !self.check(&Token::LParen) {
                    return Err(format!(
                        "line {line}: '{name}' alone isn't a variable reference anymore -- \
                         use ref:var:TYPE '{name}', or '{name}(' to call it as a function"
                    ));
                }
                self.advance();
                let mut args = Vec::new();
                if !self.check(&Token::RParen) {
                    loop {
                        args.push(self.parse_expr()?);
                        if self.check(&Token::Comma) {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(Token::RParen)?;
                Ok(Expr::Call(name, args))
            }
            other => Err(format!(
                "line {}: unexpected token {:?}",
                self.tokens[self.pos].line, other
            )),
        }
    }

    // --- token stream helpers ---

    fn peek(&self) -> &Token {
        &self.tokens[self.pos].token
    }

    fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos].token;
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn check(&self, expected: &Token) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(expected)
    }

    fn expect(&mut self, expected: Token) -> PResult<()> {
        if self.check(&expected) {
            self.advance();
            Ok(())
        } else {
            Err(format!(
                "line {}: expected {:?}, found {:?}",
                self.tokens[self.pos].line, expected, self.peek()
            ))
        }
    }

    fn expect_quoted_ident(&mut self) -> PResult<String> {
        match self.peek().clone() {
            Token::Quoted(name) => {
                self.advance();
                Ok(name)
            }
            other => Err(format!(
                "line {}: expected a quoted name like 'x', found {:?}",
                self.tokens[self.pos].line, other
            )),
        }
    }

    fn expect_ident(&mut self) -> PResult<String> {
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(name)
            }
            other => Err(format!(
                "line {}: expected identifier, found {:?}",
                self.tokens[self.pos].line, other
            )),
        }
    }
}

/// Parses a numw literal's content: an optional leading `-`, a whole or
/// decimal number, and an optional trailing magnitude word (`thousand`,
/// `million`, `billion`, `trillion`, `quadrillion`, `quintillion`, matched
/// case-insensitively). A bare number with no word is just that number
/// (scale of 1) -- the word is additive, not required.
fn parse_numw_string(s: &str) -> Result<f64, String> {
    let s = s.trim();
    let (number_part, word_part) = match s.rsplit_once(char::is_whitespace) {
        Some((n, w)) => (n.trim(), Some(w.trim())),
        None => (s, None),
    };
    let base: f64 = number_part
        .parse()
        .map_err(|_| format!("invalid numw literal '{s}': '{number_part}' isn't a number"))?;
    let scale = match word_part {
        None => 1.0,
        Some(word) => match word.to_lowercase().as_str() {
            "thousand" => 1e3,
            "million" => 1e6,
            "billion" => 1e9,
            "trillion" => 1e12,
            "quadrillion" => 1e15,
            "quintillion" => 1e18,
            other => return Err(format!("invalid numw literal '{s}': unrecognized magnitude word '{other}'")),
        },
    };
    Ok(base * scale)
}
