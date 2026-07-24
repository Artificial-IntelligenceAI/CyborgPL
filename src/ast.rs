#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    /// A number, always able to hold either whole or decimal values, stored
    /// as an IEEE-754 float of the given bit width (16/32/64/128; 64 is the
    /// default when no `[precision:N]` is given). Different widths count as
    /// different types for the purposes of same-name variable sharing, same
    /// as Num/Bool/Str already do.
    Num(u32),
    /// "Number word": a Num in every runtime respect (always 64-bit, same
    /// arithmetic/storage), but a fully separate type for the purposes of
    /// same-name variable sharing -- distinguished only by accepting an
    /// additional literal form, `'1 million'`-style quoted number-words,
    /// alongside the usual numeric expressions Num already accepts.
    NumW,
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
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
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
