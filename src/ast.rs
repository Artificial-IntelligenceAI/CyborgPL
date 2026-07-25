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
    /// A file path. Behaves exactly like `Str` at runtime -- same
    /// automatic memory management, same bare-pointer shape -- it's its
    /// own type purely for clarity at the type-system level, the same
    /// relationship `NumW` has to `Num`.
    File,
    /// A growable, homogeneous array of `ElementType`. Flat (not `Box`ed
    /// or recursive) specifically so `Type` can stay `Copy` -- no arrays
    /// of arrays for this first version, only scalar element types.
    Array(ElementType),
    Void,
}

/// The scalar types an `Array` can hold. A separate, deliberately
/// non-recursive enum (rather than letting `Type::Array` hold a boxed
/// `Type` directly) so nested arrays are simply inexpressible for now,
/// and so `Type` itself never needs to stop being `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ElementType {
    Num(u32),
    NumW(u32),
    Bool,
    Str,
    BigNum(u32),
    File,
}

impl ElementType {
    pub fn as_type(self) -> Type {
        match self {
            ElementType::Num(w) => Type::Num(w),
            ElementType::NumW(w) => Type::NumW(w),
            ElementType::Bool => Type::Bool,
            ElementType::Str => Type::Str,
            ElementType::BigNum(p) => Type::BigNum(p),
            ElementType::File => Type::File,
        }
    }

    /// `None` for `Type::Array(_)` (no nested arrays) and `Type::Void`
    /// (never a valid element type) -- every other `Type` has a direct
    /// `ElementType` counterpart.
    pub fn from_type(ty: Type) -> Option<ElementType> {
        match ty {
            Type::Num(w) => Some(ElementType::Num(w)),
            Type::NumW(w) => Some(ElementType::NumW(w)),
            Type::Bool => Some(ElementType::Bool),
            Type::Str => Some(ElementType::Str),
            Type::BigNum(p) => Some(ElementType::BigNum(p)),
            Type::File => Some(ElementType::File),
            Type::Array(_) | Type::Void => None,
        }
    }
}

impl std::fmt::Display for ElementType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_type())
    }
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
            Type::File => write!(f, "file"),
            Type::Array(elem) => write!(f, "array:{elem}"),
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
    /// `{(v1), (v2), ...}` -- an array literal. The element type isn't
    /// carried here; it comes from wherever this literal is being stored
    /// (a `var:array:TYPE` declaration), the same way a bare `Num` literal
    /// doesn't know its own target type either.
    ArrayLiteral(Vec<Expr>),
    /// `ref:var:array:TYPE 'name'*(index)*` -- reads a single element (`1`
    /// is the first element, not `0`). `Type` is the *array's* type
    /// (`Array(ElementType)`), matching how `Var` carries the type stated
    /// at the reference site for the same (name, type) key lookup.
    ArrayIndex(String, Type, Box<Expr>),
    /// `(length*(array)*)` -- element count, as a `num`.
    Length(Box<Expr>),
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
    /// `input:type 'name' [from*(source)*];` -- declares 'name' and reads
    /// its value from stdin (source is `None`) or from a file's whole
    /// content (source is `Some`, must be `Str`/`File`), rather than from
    /// a compiled expression. Currently only `Str` and `Num` support this.
    Input(String, Type, Option<Expr>),
    /// `clock:num 'name';` -- declares 'name' and reads elapsed seconds
    /// since the program started. Currently only `Num` supports this.
    Clock(String, Type),
    Assign(String, Type, Expr),
    /// `ref:var:array:TYPE 'name'*(index)* = value;` -- writes a single
    /// element (`1` is the first element). `Type` is the array's type.
    ArrayIndexAssign(String, Type, Expr, Expr),
    /// `append*(array), (value)*;` -- grows the array by one element.
    Append(Expr, Expr),
    Return(Option<Expr>),
    /// The optional `[to*(dest)*]` clause -- `None` prints to the screen
    /// as always; `Some(dest)` redirects this call to that file instead
    /// (`dest` must be `Str` or `File`).
    Print(Vec<PrintSegment>, Option<Expr>),
    /// `overwrite*segments* [to*(dest)*];` -- same segment-based text
    /// building as `Print`, but `dest` is required: this only ever writes
    /// to a file (replacing its entire content), never the screen.
    Overwrite(Vec<PrintSegment>, Expr),
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
