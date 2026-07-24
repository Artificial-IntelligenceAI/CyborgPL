#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    /// A number, always able to hold either whole or decimal values, stored
    /// as an IEEE-754 float of the given bit width (16/32/64/128; 64 is the
    /// default when no `[precision:N]` is given). Different widths count as
    /// different types for the purposes of same-name variable sharing, same
    /// as Num/Bool/Str already do.
    Num(u32),
    /// "Number word": a Num in every runtime respect (same bit-width
    /// options, arithmetic, storage), but a fully separate type for the
    /// purposes of same-name variable sharing -- distinguished only by
    /// accepting an additional literal form, `'1 million'`-style quoted
    /// number-words, alongside the usual numeric expressions Num already
    /// accepts. Carries a width the same way Num does, via `[precision:N]`.
    NumW(u32),
    Bool,
    Str,
    /// Arbitrary-precision decimal (GMP's mpf_t), a fully separate type from
    /// Num -- not a wider float, a different kind of value entirely (heap
    /// allocated, backed by a real library, not IEEE-754 at all). Precision
    /// is in bits and can be any positive value, not restricted to Num's
    /// fixed 16/32/64/128 set.
    BigNum(u32),
    Void,
}

pub const DEFAULT_NUM_PRECISION: u32 = 64;
pub const DEFAULT_BIGNUM_PRECISION: u32 = 256;

/// Renders a type the way it'd actually be written in CyborgPL source,
/// for error messages -- `{:?}` (`Num(64)`) reads like Rust, not CyborgPL.
impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Num(w) => write!(f, "num[precision:{w}]"),
            Type::NumW(w) => write!(f, "numw[precision:{w}]"),
            Type::Bool => write!(f, "bool"),
            Type::Str => write!(f, "str"),
            Type::BigNum(p) => write!(f, "bignum[precision:{p}]"),
            Type::Void => write!(f, "void"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Tetration,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    /// `stch` ("stitch"): text concatenation. Auto-converts a non-`str`
    /// operand to the same display text `print` would give it. Always
    /// produces a fresh, independently-owned `str` value.
    Concat,
}

/// Renders an operator using its actual CyborgPL spelling, for error
/// messages -- `{:?}` (`Mul`, `Ne`) doesn't match anything a user wrote.
impl std::fmt::Display for BinOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "x",
            BinOp::Div => "/",
            BinOp::Pow => "xx",
            BinOp::Tetration => "xxx",
            BinOp::Eq => "==",
            BinOp::Ne => "not=",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::Concat => "stch",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
    /// Postfix `!`: factorial. Unlike `Neg`/`Not`, this attaches after its
    /// operand rather than before it (see `parse_postfix`).
    Factorial,
}

impl std::fmt::Display for UnOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            UnOp::Neg => "-",
            UnOp::Not => "not",
            UnOp::Factorial => "!",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone)]
pub enum Expr {
    /// The parsed value, and its original literal text (empty if this
    /// wasn't a direct digit literal, e.g. numw's computed value). Only
    /// consulted when coercing a bare literal into a `bignum`, which can
    /// hold far more precision than the `f64` alone preserves; every other
    /// use of a Num just reads the parsed value.
    Num(f64, String),
    Bool(bool),
    Str(String),
    /// A variable reference: `ref:var:TYPE 'name'`. Carries the type stated
    /// at the reference site, since a name can now be shared by variables
    /// of different types -- the type is what tells them apart.
    Var(String, Type),
    Unary(UnOp, Box<Expr>),
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Call(String, Vec<Expr>),
}

/// One piece of a `print` argument: literal text outside any parens, or a
/// value to compute and substitute in, written inside parens to mark it as
/// code rather than text (e.g. `print*"You have " ('apples') " apples."*;`).
#[derive(Debug, Clone)]
pub enum PrintSegment {
    Str(String),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    VarDecl(String, Type, Expr),
    /// `input:type 'name';` -- declares 'name' and reads its value from
    /// stdin at runtime, rather than from a compiled expression. Currently
    /// only `Str` and `Num` support this.
    Input(String, Type),
    /// `clock:num 'name';` -- declares 'name' and reads elapsed seconds
    /// since the program started. Currently only `Num` supports this.
    Clock(String, Type),
    Assign(String, Type, Expr),
    Return(Option<Expr>),
    Print(Vec<PrintSegment>),
    ExprStmt(Expr),
    If(Expr, Block, Option<Block>),
    While(Expr, Block),
}

pub type Block = Vec<Stmt>;

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub functions: Vec<Function>,
    /// The statements between `START` and `END` — the program's real entry point.
    pub entry: Block,
}
