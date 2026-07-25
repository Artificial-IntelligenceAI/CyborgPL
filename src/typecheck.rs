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
    Bool,
    Str,
    BigNum,
    /// Genuinely distinct from `BigNum`, even though codegen happens to
    /// reuse bignum's exact `{ptr}`-wrapped struct to represent arrays at
    /// the LLVM level (both are `StructValue` there). This type checker is
    /// the *only* thing standing between that reuse and a bignum/array
    /// value getting silently confused with each other, so the two must
    /// never share a shape here.
    Array,
    /// A genuine whole-number type -- also `IntValue` at the LLVM level,
    /// same as `Bool`, but a real (64-bit-wide) integer rather than a
    /// single bit, and with none of `Bool`'s boolean-logic operators.
    /// Deliberately its own shape rather than reusing `Bool`'s, the same
    /// reasoning as `Array` above: two genuinely different things must
    /// never share a shape just because codegen happens to represent them
    /// with the same underlying LLVM value kind.
    Int,
}

fn shape_of(ty: Type) -> Shape {
    match ty {
        Type::Num(_) | Type::NumW(_) => Shape::Float,
        Type::Bool => Shape::Bool,
        Type::Str | Type::File => Shape::Str,
        Type::BigNum(_) => Shape::BigNum,
        Type::Array(_) => Shape::Array,
        Type::Int(_) => Shape::Int,
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
                if let Some(expr_ty) = self.check_expr_for(expr, *ty) {
                    if !coercible(expr_ty, *ty) {
                        self.error(format!(
                            "cannot assign {expr_ty} to variable '{name}' declared as {ty}"
                        ));
                    }
                }
                self.declare(name.clone(), *ty);
            }
            Stmt::Input(name, ty, source) => {
                // The parser only ever produces Str/Num here today, but
                // checking anyway costs nothing and stays correct if that
                // ever changes.
                if !matches!(ty, Type::Str | Type::Num(_)) {
                    self.error(format!("input: doesn't support {ty} yet (only str and num)"));
                }
                if let Some(source_expr) = source {
                    self.check_source(source_expr);
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
                if let Some(expr_ty) = self.check_expr_for(expr, *ty) {
                    if !coercible(expr_ty, *ty) {
                        self.error(format!(
                            "cannot assign {expr_ty} to variable '{name}' declared as {ty}"
                        ));
                    }
                }
            }
            Stmt::ArrayIndexAssign(name, ty, index, value) => {
                let declared_ok = self.check_var_ref(name, *ty);
                if let Some(ity) = self.check_expr(index) {
                    if shape_of(ity) != Shape::Float {
                        self.error(format!("array index must be num, got {ity}"));
                    }
                }
                match ty {
                    Type::Array(elem) if declared_ok => {
                        let elem_ty = elem.as_type();
                        if let Some(vty) = self.check_expr_for(value, elem_ty) {
                            if !coercible(vty, elem_ty) {
                                self.error(format!("cannot assign {vty} to array element of type {elem_ty}"));
                            }
                        }
                    }
                    Type::Array(_) => {
                        self.check_expr(value);
                    }
                    other => {
                        self.error(format!("'{name}' can't be indexed -- declared as {other}, not an array"));
                        self.check_expr(value);
                    }
                }
            }
            Stmt::Append(array_expr, value_expr) => match self.check_expr(array_expr) {
                Some(Type::Array(elem)) => {
                    let elem_ty = elem.as_type();
                    if let Some(vty) = self.check_expr_for(value_expr, elem_ty) {
                        if !coercible(vty, elem_ty) {
                            self.error(format!("cannot append {vty} to an array of {elem_ty}"));
                        }
                    }
                }
                Some(other) => {
                    self.error(format!("append*...*'s first argument must be an array, got {other}"));
                    self.check_expr(value_expr);
                }
                None => {
                    self.check_expr(value_expr);
                }
            },
            Stmt::Return(expr) => match expr {
                Some(e) => {
                    let return_ty = self.current_return_type;
                    if let Some(ety) = self.check_expr_for(e, return_ty) {
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
            Stmt::Print(segments, dest) => {
                self.check_print_segments(segments);
                if let Some(dest_expr) = dest {
                    self.check_dest(dest_expr);
                }
            }
            Stmt::Overwrite(segments, dest) => {
                self.check_print_segments(segments);
                self.check_dest(dest);
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

    /// Shared by `print` and `overwrite`: checks each computed segment's
    /// own expression.
    fn check_print_segments(&mut self, segments: &[PrintSegment]) {
        for seg in segments {
            if let PrintSegment::Expr(e) = seg {
                if let Some(ty) = self.check_expr(e) {
                    if matches!(ty, Type::Array(_)) {
                        self.error(format!("can't print an array value directly, got {ty}"));
                    }
                }
            }
        }
    }

    /// Checks a `[to*(dest)*]` clause's destination: must be `str` or
    /// `file`, matching `compile_write_to_file`'s codegen (which just
    /// reads the pointer, so any bare-pointer-shaped type would technically
    /// "work," but only these two are meant to be used this way).
    fn check_dest(&mut self, dest: &Expr) {
        self.check_file_clause(dest, "to");
    }

    /// Checks a `[from*(source)*]` clause's source -- same rule as
    /// `check_dest`, just the other bracket keyword for a clearer message.
    fn check_source(&mut self, source: &Expr) {
        self.check_file_clause(source, "from");
    }

    /// Shared by `check_dest`/`check_source`: must be `str` or `file`,
    /// matching what codegen's `compile_write_to_file`/`cyborg_read_file_or_die`
    /// call sites actually read the pointer as.
    fn check_file_clause(&mut self, expr: &Expr, keyword: &str) {
        if let Some(ty) = self.check_expr(expr) {
            if !matches!(ty, Type::Str | Type::File) {
                self.error(format!("[{keyword}*...*] must be str or file, got {ty}"));
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
                if self.check_var_ref(name, *ty) {
                    Some(*ty)
                } else {
                    None
                }
            }
            Expr::ArrayLiteral(_) => {
                // Reached only when an array literal shows up somewhere
                // with no known target type (e.g. a bare
                // `print*({1,2,3})*;`) -- check_expr_for handles the
                // context-aware case (var decl, assign, return, call
                // argument) before ever falling through to here.
                self.error("array literal needs a known target type here (e.g. a var:array:TYPE declaration, argument, or return)".to_string());
                None
            }
            Expr::ArrayIndex(name, ty, index) => {
                let elem = match ty {
                    Type::Array(elem) => Some(*elem),
                    other => {
                        self.error(format!("'{name}' can't be indexed -- declared as {other}, not an array"));
                        None
                    }
                };
                let declared_ok = self.check_var_ref(name, *ty);
                if let Some(ity) = self.check_expr(index) {
                    if shape_of(ity) != Shape::Float {
                        self.error(format!("array index must be num, got {ity}"));
                    }
                }
                if declared_ok { elem.map(|e| e.as_type()) } else { None }
            }
            Expr::Length(array_expr) => match self.check_expr(array_expr) {
                Some(Type::Array(_)) => Some(Type::Num(DEFAULT_NUM_PRECISION)),
                Some(other) => {
                    self.error(format!("length*...* expects an array, got {other}"));
                    None
                }
                None => None,
            },
            Expr::Unary(op, inner) => {
                let ity = self.check_expr(inner)?;
                match (op, ity) {
                    (UnOp::Neg, Type::Num(_) | Type::NumW(_)) => Some(ity),
                    // Negation preserves the operand's own precision/width --
                    // unlike factorial, it doesn't change the value's
                    // magnitude category, so there's no reason to force a
                    // different one.
                    (UnOp::Neg, Type::BigNum(_)) => Some(ity),
                    (UnOp::Neg, Type::Int(w)) => Some(Type::Int(w)),
                    (UnOp::Not, Type::Bool) => Some(Type::Bool),
                    // Forced to a fixed result type/width regardless of the
                    // operand's own precision -- matches compile_factorial
                    // (always 64-bit) / compile_bignum_factorial (always
                    // default precision) exactly -- factorial results grow
                    // fast enough that starting from the widest available
                    // precision/width gives the most headroom before
                    // overflowing/losing precision.
                    (UnOp::Factorial, Type::Num(_) | Type::NumW(_)) => Some(Type::Num(DEFAULT_NUM_PRECISION)),
                    (UnOp::Factorial, Type::BigNum(_)) => Some(Type::BigNum(DEFAULT_BIGNUM_PRECISION)),
                    (UnOp::Factorial, Type::Int(_)) => Some(Type::Int(DEFAULT_INT_PRECISION)),
                    (op, ity) => {
                        self.error(format!("{op} not supported on {ity}"));
                        None
                    }
                }
            }
            Expr::Binary(lhs, op, rhs) => {
                // A bare whole-number literal paired with an `int`
                // operand is itself treated as `int` -- matching how a
                // literal already "just works" mixed with num/numw (same
                // runtime representation there, so no resolution is
                // needed). `int` has a genuinely different
                // representation, so this has to be resolved here,
                // structurally, before the generic check_expr below
                // (which always gives a bare `Expr::Num` type
                // `Num(DEFAULT)`, with no way to know afterward it could
                // have been `int` instead). Both sides being bare
                // literals (`(1) + (2)`) is unaffected -- still defaults
                // to `num`, since neither side is independently known as
                // `int` without the other.
                let (lty, rty) = match (lhs.as_ref(), rhs.as_ref()) {
                    (Expr::Num(n, _), other) if !matches!(other, Expr::Num(_, _)) => {
                        let rty = self.check_expr(rhs);
                        let lty = match rty {
                            Some(Type::Int(w)) => self.check_whole_number_literal(*n, w),
                            _ => self.check_expr(lhs),
                        };
                        (lty, rty)
                    }
                    (other, Expr::Num(n, _)) if !matches!(other, Expr::Num(_, _)) => {
                        let lty = self.check_expr(lhs);
                        let rty = match lty {
                            Some(Type::Int(w)) => self.check_whole_number_literal(*n, w),
                            _ => self.check_expr(rhs),
                        };
                        (lty, rty)
                    }
                    _ => (self.check_expr(lhs), self.check_expr(rhs)),
                };
                let (lty, rty) = (lty?, rty?);
                if *op == BinOp::Concat {
                    // Accepts any shape on either side, auto-converting to
                    // display text like print -- except an array, which
                    // codegen's value_fmt has no way to render (and can't
                    // tell apart from a bignum at the LLVM level besides).
                    if matches!(lty, Type::Array(_)) || matches!(rty, Type::Array(_)) {
                        self.error(format!("stch doesn't support array operands ({lty} / {rty})"));
                        return None;
                    }
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
            (Shape::Bool, Shape::Bool) => match op {
                BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or => Some(Type::Bool),
                _ => {
                    self.error(format!("{op} not supported on bool operands"));
                    None
                }
            },
            // Widens to the larger of the two operand widths, mirroring
            // `match_int_widths` in codegen (arithmetic always happens at
            // a full i64 there; the declared result width here just
            // needs to be *at least* as wide as either operand, so no
            // information is lost before the caller's own storage
            // boundary narrows it back down if needed).
            (Shape::Int, Shape::Int) => {
                let (Type::Int(lw), Type::Int(rw)) = (lty, rty) else {
                    unreachable!("shape_of guarantees Type::Int for Shape::Int")
                };
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow | BinOp::Tetration => {
                        Some(Type::Int(lw.max(rw)))
                    }
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => Some(Type::Bool),
                    BinOp::And | BinOp::Or => {
                        self.error(format!("{op} requires bool operands, not int"));
                        None
                    }
                    BinOp::Concat => unreachable!("Concat is handled before check_binary is called"),
                }
            }
            // Widens to the larger of the two operand precisions,
            // mirroring `Shape::Int` above and codegen's identical
            // `bignum_precision_of_expr` computation. The shape remap
            // above means one (or both) side's `Type` here might
            // actually still be a plain `Num`/`NumW` (promoted to match
            // the bignum side) rather than genuinely `BigNum` -- mirrors
            // codegen's `coerce_to_bignum`, where a promoted float takes
            // on the bignum side's own precision, not some default, so
            // the result is simply whichever precision the real bignum
            // side(s) have.
            (Shape::BigNum, Shape::BigNum) => {
                let bignum_precision = match (lty, rty) {
                    (Type::BigNum(lp), Type::BigNum(rp)) => lp.max(rp),
                    (Type::BigNum(p), _) | (_, Type::BigNum(p)) => p,
                    _ => unreachable!("shape remap guarantees at least one side is BigNum"),
                };
                match op {
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => Some(Type::Bool),
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow | BinOp::Tetration => {
                        Some(Type::BigNum(bignum_precision))
                    }
                    BinOp::And | BinOp::Or => {
                        self.error(format!("{op} not supported on bignum yet"));
                        None
                    }
                    BinOp::Concat => unreachable!("Concat is handled before check_binary is called"),
                }
            }
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
            match self.check_expr_for(a, pty) {
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

    /// Whether `(name, ty)` refers to an actually-declared variable,
    /// reporting the appropriate error otherwise. Shared by `Expr::Var`
    /// and `Expr::ArrayIndex`/`Stmt::ArrayIndexAssign`, which all resolve
    /// a named variable the same way before doing their own extra work.
    fn check_var_ref(&mut self, name: &str, ty: Type) -> bool {
        let key = (name.to_string(), ty);
        if self.vars.contains(&key) {
            true
        } else if let Some((_, actual_ty)) = self.vars.iter().find(|(n, _)| n == name) {
            self.error(format!(
                "'{name}' is declared as {actual_ty}, not {ty} -- check the type stated at this reference"
            ));
            false
        } else {
            self.error(format!("undefined variable '{name}'"));
            false
        }
    }

    /// Infers `expr`'s type when it's about to be checked against a known
    /// `target` type (a var decl's declared type, a reassignment, a
    /// return, a call argument's parameter type). Needed because an array
    /// literal's element type is only recoverable from such a target --
    /// an empty `{}` has nothing to infer from -- so `Expr::ArrayLiteral`
    /// can't go through the generic, context-free `check_expr` the way
    /// every other expression does (mirrors codegen's
    /// compile_and_coerce/compile_array_literal split). Every element is
    /// itself checked against `target`'s element type, recursively -- so
    /// e.g. a bare bignum literal inside an array of `bignum` is still
    /// fine. Any other expression is unaffected, falling straight through
    /// to `check_expr`.
    fn check_expr_for(&mut self, expr: &Expr, target: Type) -> Option<Type> {
        if let (Expr::Num(n, _), Type::Int(width)) = (expr, target) {
            return self.check_whole_number_literal(*n, width);
        }
        // Propagate an `int` target into a binary/unary expression's own
        // operands too -- not just a bare literal directly assigned.
        // `var:int 'c' = (2) xx (10);` has *neither* operand already
        // known as `int` on its own (no variable to anchor
        // `Expr::Binary`'s own literal-pairing check against), but the
        // assignment target still makes the intent unambiguous. A
        // non-literal operand (a variable, a call) is unaffected --
        // `check_expr_for` only special-cases bare literals, so anything
        // else still resolves to its own real type regardless of what's
        // propagated, and a genuine mismatch (e.g. pairing a `bignum`
        // where an `int` was expected) is still caught by `check_binary`.
        if let (Expr::Binary(lhs, op, rhs), Type::Int(_)) = (expr, target) {
            if *op != BinOp::Concat {
                let lty = self.check_expr_for(lhs, target);
                let rty = self.check_expr_for(rhs, target);
                return self.check_binary(*op, lty?, rty?);
            }
        }
        if let (Expr::Unary(op, inner), Type::Int(_)) = (expr, target) {
            let ity = self.check_expr_for(inner, target)?;
            return match op {
                // Negation preserves whatever width the operand resolved
                // to; factorial always forces the default width, same as
                // the direct (non-propagated) check_expr arms above.
                UnOp::Neg => Some(ity),
                UnOp::Factorial => Some(Type::Int(DEFAULT_INT_PRECISION)),
                UnOp::Not => {
                    self.error(format!("{op} not supported on {ity}"));
                    None
                }
            };
        }
        let (Expr::ArrayLiteral(elements), Type::Array(elem)) = (expr, target) else {
            return self.check_expr(expr);
        };
        let elem_ty = elem.as_type();
        let mut ok = true;
        for e in elements {
            match self.check_expr_for(e, elem_ty) {
                Some(ety) if coercible(ety, elem_ty) => {}
                Some(ety) => {
                    self.error(format!("cannot use {ety} as an element of {target}"));
                    ok = false;
                }
                None => ok = false,
            }
        }
        if ok { Some(target) } else { None }
    }

    /// A bare numeric literal being treated as `int` -- from a known
    /// target type (`check_expr_for`) or paired with an already-`int`
    /// operand in a binary op (`check_expr`'s `Expr::Binary` arm) -- must
    /// actually be a whole number. `int` exists specifically to rule out
    /// a fractional value ever ending up in one, so this is checked at
    /// every point a literal could become `int`, not just storage.
    fn check_whole_number_literal(&mut self, n: f64, width: u32) -> Option<Type> {
        if n.fract() == 0.0 {
            Some(Type::Int(width))
        } else {
            self.error(format!("{n} is not a whole number, can't use it as int"));
            None
        }
    }
}

/// Whether a value of type `from` can be stored/passed/returned as `to` --
/// a direct mirror of codegen's `coerce_to_type`/`coerce_to_bignum`:
/// - `num`/`numw` freely interconvert at any precision (both are plain
///   floats at the LLVM level; codegen doesn't even distinguish them).
/// - Anything except `bool` coerces into `bignum` (float-shaped values via
///   `bignum_set_d`, `str` via `bignum_set_str` for numeric-literal text,
///   another `bignum` via a copy).
/// - `str` and `file` freely interconvert with each other (`file` is just
///   a typed path string), but neither coerces from anything else --
///   turning another type into text needs an explicit `stch`, which
///   bypasses this rule entirely.
/// - `bool` only coerces from `bool`.
fn coercible(from: Type, to: Type) -> bool {
    match (from, to) {
        (Type::Num(_) | Type::NumW(_), Type::Num(_) | Type::NumW(_)) => true,
        (Type::Num(_) | Type::NumW(_) | Type::Str | Type::BigNum(_), Type::BigNum(_)) => true,
        // `file` is just a typed path string -- freely interchangeable
        // with `str`, the same relationship `num`/`numw` already have.
        (Type::Str | Type::File, Type::Str | Type::File) => true,
        (Type::Bool, Type::Bool) => true,
        // No coercion from num/numw/bignum into int -- a fractional
        // value ever ending up in an int is exactly what the type exists
        // to rule out, so (unlike bignum, which happily accepts any
        // numeric-shaped source) int only ever accepts int itself.
        // Unlike bool, though, int freely coerces between its *own*
        // different widths (mirroring num/numw's "any precision"
        // convention) -- purely a type-level allowance; codegen enforces
        // actual safety at runtime (widening is always safe, narrowing
        // is overflow-checked and crashes if the value doesn't fit).
        (Type::Int(_), Type::Int(_)) => true,
        // No cross-element-type coercion -- an array:num can't quietly
        // become an array:str the way a bare num can become a str-typed
        // display via stch. Element types must match exactly.
        (Type::Array(a), Type::Array(b)) => a == b,
        _ => false,
    }
}
