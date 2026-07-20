#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Type {
    Int,
    Bool,
    Void,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
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
    Int(i64),
    Bool(bool),
    Var(String),
    Unary(UnOp, Box<Expr>),
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let(String, Type, Expr),
    Assign(String, Expr),
    Return(Option<Expr>),
    Print(Expr),
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
}
