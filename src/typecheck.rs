// A static type-checking pass, run between parsing and codegen. Its one job
// is to turn what currently happens as a confusing Rust panic deep in
// codegen.rs (or, worse, an LLVM-level type mismatch inkwell panics on)
// into a clear, CyborgPL-level error message pointing at the actual
// mistake -- nothing about what programs are *accepted* changes. Every
// coercion rule here is a deliberate mirror of codegen.rs's own
// coerce_to_type/coerce_to_bignum/Expr::Binary dispatch; if codegen's rules
// ever change, this file needs the matching update or the two will drift
// out of sync and this checker will start rejecting (or wrongly accepting)
// valid programs.

use std::collections::HashMap;

use crate::ast::*;

/// The four runtime "shapes" a value can have in codegen (FloatValue,
/// IntValue, PointerValue, StructValue) -- coercion and binary-op validity
/// are actually decided by shape, not by the exact declared type (e.g. any
/// two float-shaped types, `num` or `numw` at any precision, freely mix).
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Float,
    Int,
    Str,
    BigNum,
}

fn shape_of(ty: Type) -> Shape {
    match ty {
        Type::Num(_) | Type::NumW(_) => Shape::Float,
        Type::Bool => Shape::Int,
        Type::Str => Shape::Str,
        Type::BigNum(_) => Shape::BigNum,
        Type::Void => panic!("Void has no runtime shape; should never reach type-checking"),
    }
}

pub struct TypeChecker {
    function_sigs: HashMap<String, (Vec<Type>, Type)>,
    /// Currently-visible (name, type) pairs -- mirrors codegen's
    /// `variables` map, minus the LLVM values (we only need to know
    /// whether a reference is valid and what type it resolves to).
    vars: std::collections::HashSet<(String, Type)>,
    /// Mirrors codegen's `scopes` stack, but only remembering which keys
    /// were *newly* declared in each block (so they can be removed when it
    /// ends) -- a key that already existed before a block doesn't need
    /// restoring, since re-declaring the same (name, type) inside is
    /// indistinguishable from the outer one for checking purposes alone
    /// (no values to tell apart, unlike codegen's real shadowing).
    scopes: Vec<Vec<(String, Type)>>,
    current_return_type: Type,
    errors: Vec<String>,
}

impl TypeChecker {
    /// Checks every function and the entry block. `Ok(())` if nothing was
    /// wrong; otherwise every problem found (not just the first), so a
    /// program with several mistakes doesn't need to be fixed one error at
    /// a time across repeated runs.
    pub fn check_program(program: &Program) -> Result<(), Vec<String>> {
        let mut tc = TypeChecker {
            function_sigs: HashMap::new(),
            vars: std::collections::HashSet::new(),
            scopes: Vec::new(),
            current_return_type: Type::Void,
            errors: Vec::new(),
        };

        for f in &program.functions {
            let param_types = f.params.iter().map(|p| p.ty).collect();
            tc.function_sigs.insert(f.name.clone(), (param_types, f.return_type));
        }

        for f in &program.functions {
            tc.check_function(f);
        }
        tc.check_entry(&program.entry);

        if tc.errors.is_empty() {
            Ok(())
        } else {
            Err(tc.errors)
        }
    }

    fn error(&mut self, message: String) {
        self.errors.push(message);
    }

    fn check_function(&mut self, f: &Function) {
        self.vars.clear();
        self.scopes.clear();
        self.current_return_type = f.return_type;
        // One frame for the params, mirroring compile_function wrapping
        // params + the whole body in a single scope -- compile_block below
        // pushes its own separate, nested frame for the body itself.
        self.scopes.push(Vec::new());
        for p in &f.params {
            self.declare(p.name.clone(), p.ty);
        }
        self.check_block(&f.body);
        self.pop_scope();
    }

    fn check_entry(&mut self, entry: &Block) {
        self.vars.clear();
        self.scopes.clear();
        self.current_return_type = Type::Void;
        self.check_block(entry);
    }

    fn declare(&mut self, name: String, ty: Type) {
        let key = (name, ty);
        if !self.vars.contains(&key) {
            self.vars.insert(key.clone());
            self.scopes.last_mut().expect("declare called outside any block").push(key);
        }
    }

    fn pop_scope(&mut self) {
        for key in self.scopes.pop().expect("pop_scope with no open scope") {
            self.vars.remove(&key);
        }
    }

    fn check_block(&mut self, block: &Block) {
        self.scopes.push(Vec::new());
        for stmt in block {
            self.check_stmt(stmt);
        }
        self.pop_scope();
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::VarDecl(name, ty, expr) => {
                if let Some(expr_ty) = self.check_expr(expr) {
                    if !coercible(expr_ty, *ty) {
                        self.error(format!(
                            "cannot assign {expr_ty} to variable '{name}' declared as {ty}"
                        ));
                    }
                }
                self.declare(name.clone(), *ty);
            }
            Stmt::Input(name, ty) => {
                // The parser only ever produces Str/Num here today, but
                // checking anyway costs nothing and stays correct if that
                // ever changes.
                if !matches!(ty, Type::Str | Type::Num(_)) {
                    self.error(format!("input: doesn't support {ty} yet (only str and num)"));
                }
                self.declare(name.clone(), *ty);
            }
            Stmt::Clock(name, ty) => {
                if !matches!(ty, Type::Num(_)) {
                    self.error(format!("clock: doesn't support {ty} yet (only num)"));
                }
                self.declare(name.clone(), *ty);
            }
            Stmt::Assign(name, ty, expr) => {
                let key = (name.clone(), *ty);
                if !self.vars.contains(&key) {
                    self.error(format!("assignment to undeclared variable '{name}' of type {ty}"));
                    self.check_expr(expr);
                    return;
                }
                if let Some(expr_ty) = self.check_expr(expr) {
                    if !coercible(expr_ty, *ty) {
                        self.error(format!(
                            "cannot assign {expr_ty} to variable '{name}' declared as {ty}"
                        ));
                    }
                }
            }
            Stmt::Return(expr) => match expr {
                Some(e) => {
                    if let Some(ety) = self.check_expr(e) {
                        if !coercible(ety, self.current_return_type) {
                            self.error(format!(
                                "returning {ety} but the function is declared to return {}",
                                self.current_return_type
                            ));
                        }
                    }
                }
                None => {
                    if self.current_return_type != Type::Void {
                        self.error(format!(
                            "bare return in a function declared to return {}",
                            self.current_return_type
                        ));
                    }
                }
            },
            Stmt::Print(segments) => {
                for seg in segments {
                    if let PrintSegment::Expr(e) = seg {
                        self.check_expr(e);
                    }
                }
            }
            Stmt::ExprStmt(expr) => match expr {
                // A call as a bare statement discards its result (even a
                // non-void one), matching Stmt::ExprStmt's codegen -- so
                // unlike a call used as a value, Void is fine here.
                Expr::Call(name, args) => {
                    self.check_call(name, args);
                }
                other => {
                    self.check_expr(other);
                }
            },
            Stmt::If(cond, then_block, else_block) => {
                self.check_condition(cond);
                self.check_block(then_block);
                if let Some(else_block) = else_block {
                    self.check_block(else_block);
                }
            }
            Stmt::While(cond, body) => {
                self.check_condition(cond);
                self.check_block(body);
            }
        }
    }

    fn check_condition(&mut self, cond: &Expr) {
        if let Some(cty) = self.check_expr(cond) {
            if cty != Type::Bool {
                self.error(format!("condition must be bool, got {cty}"));
            }
        }
    }

    /// Infers `expr`'s type, reporting an error and returning `None` if
    /// something inside it is already wrong -- callers that need a type to
    /// keep checking (e.g. a binary op's other operand) short-circuit via
    /// `?` rather than cascading a second, confusing error on top of the
    /// real one.
    fn check_expr(&mut self, expr: &Expr) -> Option<Type> {
        match expr {
            Expr::Num(_, _) => Some(Type::Num(DEFAULT_NUM_PRECISION)),
            Expr::Bool(_) => Some(Type::Bool),
            Expr::Str(_) => Some(Type::Str),
            Expr::Var(name, ty) => {
                let key = (name.clone(), *ty);
                if self.vars.contains(&key) {
                    Some(*ty)
                } else if let Some((_, actual_ty)) = self.vars.iter().find(|(n, _)| n == name) {
                    self.error(format!(
                        "'{name}' is declared as {actual_ty}, not {ty} -- check the type stated at this reference"
                    ));
                    None
                } else {
                    self.error(format!("undefined variable '{name}'"));
                    None
                }
            }
            Expr::Unary(op, inner) => {
                let ity = self.check_expr(inner)?;
                match (op, ity) {
                    (UnOp::Neg, Type::Num(_) | Type::NumW(_)) => Some(ity),
                    (UnOp::Neg, Type::BigNum(_)) => Some(Type::BigNum(DEFAULT_BIGNUM_PRECISION)),
                    (UnOp::Not, Type::Bool) => Some(Type::Bool),
                    // Both forced to a fixed result type regardless of the
                    // operand's own precision -- matches compile_factorial
                    // (always 64-bit) / compile_bignum_factorial (always
                    // default precision) exactly.
                    (UnOp::Factorial, Type::Num(_) | Type::NumW(_)) => Some(Type::Num(DEFAULT_NUM_PRECISION)),
                    (UnOp::Factorial, Type::BigNum(_)) => Some(Type::BigNum(DEFAULT_BIGNUM_PRECISION)),
                    (op, ity) => {
                        self.error(format!("{op} not supported on {ity}"));
                        None
                    }
                }
            }
            Expr::Binary(lhs, op, rhs) => {
                let lty = self.check_expr(lhs);
                let rty = self.check_expr(rhs);
                let (lty, rty) = (lty?, rty?);
                if *op == BinOp::Concat {
                    // Accepts any shape on either side, auto-converting to
                    // display text like print -- no restriction at all.
                    return Some(Type::Str);
                }
                self.check_binary(*op, lty, rty)
            }
            Expr::Call(name, args) => {
                let ty = self.check_call(name, args)?;
                if ty == Type::Void {
                    self.error(format!("function '{name}' returns nothing, can't be used as a value"));
                    return None;
                }
                Some(ty)
            }
        }
    }

    /// Mirrors `Expr::Binary`'s codegen dispatch exactly: the bignum<->float
    /// promotion (a lone float-shaped operand paired with a bignum-shaped
    /// one is allowed, promoted to bignum-vs-bignum) happens first, then
    /// the result is decided per matched shape-pair, with the *same* set
    /// of operators each shape-pair actually supports in codegen -- not
    /// just "these are both numbers so anything goes".
    fn check_binary(&mut self, op: BinOp, lty: Type, rty: Type) -> Option<Type> {
        let (ls, rs) = (shape_of(lty), shape_of(rty));
        let (ls, rs) = match (ls, rs) {
            (Shape::BigNum, Shape::Float) | (Shape::Float, Shape::BigNum) => (Shape::BigNum, Shape::BigNum),
            other => other,
        };
        match (ls, rs) {
            (Shape::Float, Shape::Float) => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow | BinOp::Tetration => {
                    Some(Type::Num(DEFAULT_NUM_PRECISION))
                }
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => Some(Type::Bool),
                BinOp::And | BinOp::Or => {
                    self.error(format!("{op} requires bool operands, not num"));
                    None
                }
                BinOp::Concat => unreachable!("Concat is handled before check_binary is called"),
            },
            (Shape::Int, Shape::Int) => match op {
                BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or => Some(Type::Bool),
                _ => {
                    self.error(format!("{op} not supported on bool operands"));
                    None
                }
            },
            (Shape::BigNum, Shape::BigNum) => match op {
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => Some(Type::Bool),
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow | BinOp::Tetration => {
                    Some(Type::BigNum(DEFAULT_BIGNUM_PRECISION))
                }
                BinOp::And | BinOp::Or => {
                    self.error(format!("{op} not supported on bignum yet"));
                    None
                }
                BinOp::Concat => unreachable!("Concat is handled before check_binary is called"),
            },
            _ => {
                self.error(format!("{op} used with mismatched operand types {lty} / {rty}"));
                None
            }
        }
    }

    /// Checks a call's target/argument count/argument types, returning the
    /// callee's declared return type (which may be `Void` -- fine for a
    /// bare-statement call, checked separately by `Expr::Call`'s own
    /// caller in `check_expr` for the value-position case).
    fn check_call(&mut self, name: &str, args: &[Expr]) -> Option<Type> {
        let Some((param_types, return_type)) = self.function_sigs.get(name).cloned() else {
            self.error(format!("undefined function '{name}'"));
            for a in args {
                self.check_expr(a);
            }
            return None;
        };
        if args.len() != param_types.len() {
            self.error(format!(
                "function '{name}' expects {} argument(s), got {}",
                param_types.len(),
                args.len()
            ));
            for a in args {
                self.check_expr(a);
            }
            return None;
        }
        let mut ok = true;
        for (a, &pty) in args.iter().zip(param_types.iter()) {
            match self.check_expr(a) {
                Some(aty) if coercible(aty, pty) => {}
                Some(aty) => {
                    self.error(format!("function '{name}' argument type mismatch: expected {pty}, got {aty}"));
                    ok = false;
                }
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        Some(return_type)
    }
}

/// Whether a value of type `from` can be stored/passed/returned as `to` --
/// a direct mirror of codegen's `coerce_to_type`/`coerce_to_bignum`:
/// - `num`/`numw` freely interconvert at any precision (both are plain
///   floats at the LLVM level; codegen doesn't even distinguish them).
/// - Anything except `bool` coerces into `bignum` (float-shaped values via
///   `bignum_set_d`, `str` via `bignum_set_str` for numeric-literal text,
///   another `bignum` via a copy).
/// - `str` only coerces from `str` (turning another type into text needs
///   an explicit `stch`, which bypasses this rule entirely).
/// - `bool` only coerces from `bool`.
fn coercible(from: Type, to: Type) -> bool {
    match (from, to) {
        (Type::Num(_) | Type::NumW(_), Type::Num(_) | Type::NumW(_)) => true,
        (Type::Num(_) | Type::NumW(_) | Type::Str | Type::BigNum(_), Type::BigNum(_)) => true,
        (Type::Str, Type::Str) => true,
        (Type::Bool, Type::Bool) => true,
        _ => false,
    }
}
