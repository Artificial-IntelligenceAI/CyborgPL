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
                Token::Func => functions.push(self.parse_function()?),
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
                        "line {}: expected 'func' or 'START', found {:?}",
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
        self.expect(Token::Func)?;
        let name = self.expect_quoted_ident()?;
        self.expect(Token::Star)?;

        let mut params = Vec::new();
        if !self.check(&Token::Star) {
            loop {
                let pname = self.expect_quoted_ident()?;
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                let ty = self.parse_optional_precision(ty)?;
                params.push(Param { name: pname, ty });
                if self.check(&Token::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(Token::Star)?;

        let return_type = if self.check(&Token::Arrow) {
            self.advance();
            let ty = self.parse_type()?;
            self.parse_optional_precision(ty)?
        } else {
            Type::Void
        };

        let body = self.parse_block()?;

        Ok(Function { name, params, return_type, body })
    }

    fn parse_type(&mut self) -> PResult<Type> {
        let line = self.tokens[self.pos].line;
        let name = self.expect_ident()?;
        match name.as_str() {
            "num" => Ok(Type::Num(DEFAULT_NUM_PRECISION)),
            "numw" => Ok(Type::NumW(DEFAULT_NUM_PRECISION)),
            "bool" => Ok(Type::Bool),
            "str" => Ok(Type::Str),
            "bignum" => Ok(Type::BigNum(DEFAULT_BIGNUM_PRECISION)),
            "file" => Ok(Type::File),
            // `array:elem_type`, e.g. `array:num`, `array:str` -- the
            // element type is parsed with the exact same grammar a bare
            // type would be (including its own `[precision:N]`, e.g.
            // `array:num[precision:32]`), just recursively.
            "array" => {
                self.expect(Token::Colon)?;
                let elem_ty = self.parse_type()?;
                let elem_ty = self.parse_optional_precision(elem_ty)?;
                let elem = ElementType::from_type(elem_ty).ok_or_else(|| {
                    format!("line {line}: array elements can't themselves be {elem_ty} (no nested arrays)")
                })?;
                Ok(Type::Array(elem))
            }
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
            Token::Num(n, _) => {
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
                // Not a direct digit literal (it's computed from the
                // word-form text), so there's no "original text" to
                // preserve beyond what the f64 already holds -- numw
                // itself is always f64-bounded regardless.
                Ok(Some(Expr::Num(value, format!("{value}"))))
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

    /// Parses `*segments*` -- the literal/computed text-segment list shared
    /// by `print` and `overwrite`. Consumes both surrounding `*`s.
    fn parse_print_segments(&mut self) -> PResult<Vec<PrintSegment>> {
        self.expect(Token::Star)?;
        let mut segments = Vec::new();
        loop {
            match self.peek().clone() {
                Token::Str(s) => {
                    self.advance();
                    segments.push(PrintSegment::Str(s));
                }
                // A computed segment is recognized just by seeing the
                // start of an expression -- every value now begins with
                // its own required '(' wrap, or a function call begins
                // with `ref:func`, either of which is already enough to
                // tell it apart from a literal Str segment. No separate
                // print-specific marker needed anymore now that the
                // general value grammar itself always starts
                // unambiguously. A call fully consumes its own `*args*`
                // before this loop checks for the closing `*` again, so
                // there's no ambiguity between the two uses of `*`.
                Token::LParen | Token::Ref | Token::Minus | Token::Not => {
                    let value = self.parse_expr()?;
                    segments.push(PrintSegment::Expr(value));
                }
                Token::Star => break,
                other => {
                    return Err(format!(
                        "line {}: expected a \"literal\", a value, or the closing '*', found {:?}",
                        self.tokens[self.pos].line, other
                    ));
                }
            }
        }
        self.expect(Token::Star)?;
        Ok(segments)
    }

    /// Parses an optional `[to*(dest)*]` file-destination clause -- `None`
    /// if there's no `[` here at all. Whether `None` is actually allowed
    /// (as for `print`) or must be rejected (as for `overwrite`, which
    /// requires a destination) is the caller's job.
    /// Parses an optional `[keyword*(expr)*]` clause -- `[to*(dest)*]` for
    /// `print`/`overwrite`, `[from*(source)*]` for `input:`. `None` if
    /// there's no `[` here at all; whether `None` is actually allowed is
    /// each caller's job (optional for `print`/`input:`, required for
    /// `overwrite`).
    fn parse_bracket_clause(&mut self, keyword: Token) -> PResult<Option<Expr>> {
        if !self.check(&Token::LBracket) {
            return Ok(None);
        }
        self.advance();
        self.expect(keyword)?;
        self.expect(Token::Star)?;
        let expr = self.parse_expr()?;
        self.expect(Token::Star)?;
        self.expect(Token::RBracket)?;
        Ok(Some(expr))
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
            // `input:type 'name';` -- no value expression to follow, so
            // (unlike var's trailing-precision placement, which comes after
            // the value) `[precision:N]` sits right after the type here,
            // matching `ref:var:type[precision:N] 'name'`'s own ordering.
            Token::Input => {
                self.advance();
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                let ty = self.parse_optional_precision(ty)?;
                let name = self.expect_quoted_ident()?;
                let source = self.parse_bracket_clause(Token::From)?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Input(name, ty, source))
            }
            // `clock:num 'name';` -- same shape as `input:` above.
            Token::Clock => {
                self.advance();
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                let ty = self.parse_optional_precision(ty)?;
                let name = self.expect_quoted_ident()?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Clock(name, ty))
            }
            // `ref:func 'name'*args*;` (calling a function as a statement,
            // e.g. one that just prints and returns nothing) versus
            // `ref:var:TYPE ...` (a reassignment or a bare variable
            // reference as a statement) -- told apart by looking two
            // tokens past `ref` (past the `:`), same technique as
            // parse_primary's own Ref handling.
            Token::Ref if self.tokens.get(self.pos + 2).map(|t| &t.token) == Some(&Token::Func) => {
                let expr = self.parse_func_call()?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::ExprStmt(expr))
            }
            Token::Ref => {
                // Either a reassignment (`ref:var:TYPE 'name' = expr;`) or a
                // bare variable reference used as a statement on its own.
                // Parsed directly here rather than through the general
                // expression grammar: the target of an assignment is a
                // place, not a value, so it's exempt from the "every value
                // is wrapped in ( )" rule the RHS and everything else now
                // follows.
                let (name, ty) = self.parse_ref_var()?;
                let index = self.parse_optional_array_index()?;
                if self.check(&Token::Eq) {
                    self.advance();
                    let value = match self.try_parse_numw_literal(ty)? {
                        Some(v) => v,
                        None => self.parse_expr()?,
                    };
                    self.expect(Token::Semicolon)?;
                    return match index {
                        Some(index) => Ok(Stmt::ArrayIndexAssign(name, ty, index, value)),
                        None => Ok(Stmt::Assign(name, ty, value)),
                    };
                }
                self.expect(Token::Semicolon)?;
                match index {
                    Some(index) => Ok(Stmt::ExprStmt(Expr::ArrayIndex(name, ty, Box::new(index)))),
                    None => Ok(Stmt::ExprStmt(Expr::Var(name, ty))),
                }
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
                let segments = self.parse_print_segments()?;
                let dest = self.parse_bracket_clause(Token::To)?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Print(segments, dest))
            }
            // Same segment-based text building as `print`, but the
            // `[to*(dest)*]` destination is required -- this only ever
            // writes to a file, never the screen.
            Token::Overwrite => {
                self.advance();
                let segments = self.parse_print_segments()?;
                let dest = match self.parse_bracket_clause(Token::To)? {
                    Some(dest) => dest,
                    None => {
                        return Err(format!(
                            "line {}: overwrite requires a [to*(file)*] destination",
                            self.tokens[self.pos].line
                        ));
                    }
                };
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Overwrite(segments, dest))
            }
            // `append*(array), (value)*;` -- grows an array by one element.
            Token::Append => {
                self.advance();
                self.expect(Token::Star)?;
                let array = self.parse_expr()?;
                self.expect(Token::Comma)?;
                let value = self.parse_expr()?;
                self.expect(Token::Star)?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Append(array, value))
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
        self.parse_concat()
    }

    /// `stch` ("stitch"): text concatenation, the loosest-binding operator
    /// of all -- `("a") stch (1) + (2)` means `("a") stch ((1) + (2))`.
    /// Left-associative/chainable like every other binary level here.
    fn parse_concat(&mut self) -> PResult<Expr> {
        let mut left = self.parse_or()?;
        while self.check(&Token::Stch) {
            self.advance();
            let right = self.parse_or()?;
            left = Expr::Binary(Box::new(left), BinOp::Concat, Box::new(right));
        }
        Ok(left)
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
                Token::NotEq => BinOp::Ne,
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
    /// tighter than `x`/`/` but looser than unary `-`/`not`.
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
            Token::Not => {
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::Unary(UnOp::Not, Box::new(operand)))
            }
            _ => self.parse_postfix(),
        }
    }

    /// Postfix `!` (factorial), binding tighter than anything to its left --
    /// it attaches directly to the atom/call it follows, e.g. `(5)!`, and
    /// can chain (`(5)!!`) the same way prefix `not not (x)` already does.
    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.parse_primary()?;
        while self.check(&Token::Bang) {
            self.advance();
            expr = Expr::Unary(UnOp::Factorial, Box::new(expr));
        }
        Ok(expr)
    }

    /// Every value (a number/string/bool literal, or a `ref:var:TYPE 'name'`
    /// reference) must now be individually wrapped in `( )` to be used in
    /// any expression -- this is the only place that requirement is
    /// enforced, since it's the only place a bare atom used to be reachable.
    /// A chain like `(a) + (b) + (c)` still works exactly like before this
    /// requirement existed: each wrapped atom is just an ordinary operand
    /// to the (unchanged) precedence-climbing chain above, so multi-term
    /// expressions combine without needing to re-wrap any intermediate
    /// result. Function calls (`'name'(...)`) are exempt -- they already
    /// have their own delimiters and don't need an extra wrap around the
    /// whole call, though each of their *arguments* is itself a value and
    /// so still needs its own wrap.
    fn parse_primary(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Token::LParen => {
                self.advance();
                let expr = self.parse_wrapped_atom()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            // `ref:func 'name'*args*` is the only way to call a function
            // now. `ref:var:TYPE 'name'` reaching here (rather than through
            // the required '(' above) means a value wasn't wrapped -- same
            // error as any other bare atom. Looking two tokens past `ref`
            // (past the `:`) tells them apart without consuming anything
            // on the "not a call" path.
            Token::Ref if self.tokens.get(self.pos + 2).map(|t| &t.token) == Some(&Token::Func) => {
                self.parse_func_call()
            }
            // `{(v1), (v2), ...}` -- an array literal, each element
            // individually wrapped like any other value. Exempt from
            // needing an extra outer `( )` wrap, same as a function call.
            Token::LBrace => self.parse_array_literal(),
            other => Err(format!(
                "line {}: every value must be wrapped in parens -- e.g. (5), (ref:var:num 'x') -- found {:?}",
                self.tokens[self.pos].line, other
            )),
        }
    }

    /// Parses exactly one atomic value -- a number/string/bool literal, or
    /// a `ref:var:TYPE 'name'` reference -- the only things allowed
    /// directly inside a required `( )` wrapper.
    fn parse_wrapped_atom(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Token::Num(n, text) => {
                self.advance();
                Ok(Expr::Num(n, text))
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
            // `ref:var:TYPE 'name'` is the only way to read a variable's
            // value now. The type is which variable named 'name' this is --
            // the same name can be shared by variables of different types.
            Token::Ref => {
                let (name, ty) = self.parse_ref_var()?;
                match self.parse_optional_array_index()? {
                    Some(index) => Ok(Expr::ArrayIndex(name, ty, Box::new(index))),
                    None => Ok(Expr::Var(name, ty)),
                }
            }
            // `length*array*` -- still needs its own outer `( )` wrap like
            // any other value (unlike a function call, which doesn't), but
            // the array argument itself doesn't need a *second*, inner
            // wrap -- see parse_length_argument.
            Token::Length => {
                self.advance();
                self.expect(Token::Star)?;
                let array = self.parse_length_argument()?;
                self.expect(Token::Star)?;
                Ok(Expr::Length(Box::new(array)))
            }
            other => Err(format!(
                "line {}: expected a value (a number, string, true/false, or ref:var:TYPE 'name') inside '(', found {:?}",
                self.tokens[self.pos].line, other
            )),
        }
    }

    /// Parses `ref:var:TYPE 'name'` directly. Shared by `parse_wrapped_atom`
    /// (a ref used as a value, gated behind the `( )` requirement above it)
    /// and `parse_stmt`'s reassignment handling (a ref used as an
    /// assignment target, which is exempt from that requirement).
    fn parse_ref_var(&mut self) -> PResult<(String, Type)> {
        self.expect(Token::Ref)?;
        self.expect(Token::Colon)?;
        self.expect(Token::Var)?;
        self.expect(Token::Colon)?;
        let ty = self.parse_type()?;
        let ty = self.parse_optional_precision(ty)?;
        let name = self.expect_quoted_ident()?;
        Ok((name, ty))
    }

    /// Parses an optional `*(index)*` suffix right after a
    /// `ref:var:array:...` reference -- present for single-element access
    /// (`ref:var:array:num 'nums'*(1)*`), absent for a whole-array
    /// reference. Doesn't check the declared type is actually an array;
    /// that's the type checker's job, same division of labor already used
    /// for `input:`/`clock:`'s type restrictions.
    fn parse_optional_array_index(&mut self) -> PResult<Option<Expr>> {
        if !self.check(&Token::Star) {
            return Ok(None);
        }
        self.advance();
        let index = self.parse_expr()?;
        self.expect(Token::Star)?;
        Ok(Some(index))
    }

    /// `{(v1), (v2), ...}` -- an array literal, each element individually
    /// wrapped like any other value. Exempt from needing an extra outer
    /// `( )` wrap, same as a function call. Shared by `parse_primary` (a
    /// literal used as a value) and `parse_length_argument` below.
    fn parse_array_literal(&mut self) -> PResult<Expr> {
        self.expect(Token::LBrace)?;
        let mut elements = Vec::new();
        if !self.check(&Token::RBrace) {
            loop {
                elements.push(self.parse_expr()?);
                if self.check(&Token::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RBrace)?;
        Ok(Expr::ArrayLiteral(elements))
    }

    /// Parses `length`'s single argument -- always an array-typed
    /// expression, so unlike every other value position it doesn't need
    /// its own `( )` wrap. A bare `ref:var:array:TYPE 'name'` reference
    /// can never sensibly take a trailing `*(index)*` *here* anyway
    /// (indexing narrows to one scalar element, and `length` needs a
    /// whole array), so skipping `parse_optional_array_index` for this
    /// one case costs no expressiveness -- and is exactly what removes
    /// the ambiguity a wrap would otherwise be needed to resolve (without
    /// it, `length`'s own closing `*` would be indistinguishable from the
    /// start of an index suffix on the reference). A bare `{...}` array
    /// literal is deliberately not accepted here -- its element type is
    /// only recoverable from a known target type (a `var:array:TYPE`
    /// declaration, a return, a call argument), and `length`'s argument
    /// position isn't one of those, so it would always fail type-checking
    /// anyway; store it in a variable first instead.
    fn parse_length_argument(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Token::Ref if self.tokens.get(self.pos + 2).map(|t| &t.token) == Some(&Token::Func) => {
                self.parse_func_call()
            }
            Token::Ref => {
                let (name, ty) = self.parse_ref_var()?;
                Ok(Expr::Var(name, ty))
            }
            other => Err(format!(
                "line {}: length*...* expects an array reference or call, found {:?}",
                self.tokens[self.pos].line, other
            )),
        }
    }

    /// Parses `ref:func 'name'*arg, arg, ...*` -- the only way to call a
    /// function now. Exempt from the "every value needs its own `( )`"
    /// rule, like every function call: it already has its own delimiters
    /// (`*...*`, mirroring how `print*...*` brackets its own segments).
    fn parse_func_call(&mut self) -> PResult<Expr> {
        self.expect(Token::Ref)?;
        self.expect(Token::Colon)?;
        self.expect(Token::Func)?;
        let name = self.expect_quoted_ident()?;
        self.expect(Token::Star)?;
        let mut args = Vec::new();
        if !self.check(&Token::Star) {
            loop {
                args.push(self.parse_expr()?);
                if self.check(&Token::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(Token::Star)?;
        Ok(Expr::Call(name, args))
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
