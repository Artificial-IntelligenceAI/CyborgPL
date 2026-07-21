#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Type {
    /// A number, always able to hold either whole or decimal values.
    Num,
    Bool,
    Str,
    Void,
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
    Var(String),
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
    Assign(String, Expr),
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
