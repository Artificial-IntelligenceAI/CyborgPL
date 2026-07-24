#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals and identifiers
    Num(f64),
    Str(String),
    Ident(String),
    /// A single-quoted name, e.g. 'name'. Any character is allowed inside
    /// (except a literal `'`, which always closes it). Used for variable
    /// names, function names, and parameter names alike.
    Quoted(String),

    // Keywords
    Fn,
    Var,
    /// Starts a variable reference (`ref:var:TYPE 'name'`) or a function
    /// call (`ref:func 'name'*args*`).
    Ref,
    /// The other thing `ref:` can lead to, alongside `var`: `ref:func 'name'*args*`.
    Func,
    If,
    Else,
    While,
    Return,
    True,
    False,
    Print,
    /// Marks the start of the program's entry point block (replaces `fn main`).
    Start,
    End,
    /// `x`: multiply. Replaces `*`, which is reserved for print's `*expr*` wrapper.
    Mul,
    /// `xx`: power/exponentiation.
    Pow,
    /// `xxx`: tetration (repeated exponentiation).
    Tetration,

    // Symbols
    LParen,
    RParen,
    LBrace,
    RBrace,
    /// Only used for `[precision:N]` right now.
    LBracket,
    RBracket,
    Comma,
    Colon,
    Semicolon,
    Arrow, // ->

    // Operators
    Plus,
    Minus,
    /// Only print's `*expr*` wrapper now; no longer multiply (see Mul).
    Star,
    Slash,
    Eq,      // =
    EqEq,    // ==
    Bang,    // !
    BangEq,  // !=
    Lt,      // <
    Gt,      // >
    LtEq,    // <=
    GtEq,    // >=
    AndAnd,  // &&
    OrOr,    // ||

    Eof,
}

impl Token {
    pub fn keyword_from_str(s: &str) -> Option<Token> {
        match s {
            "fn" => Some(Token::Fn),
            "var" => Some(Token::Var),
            "ref" => Some(Token::Ref),
            "func" => Some(Token::Func),
            "if" => Some(Token::If),
            "else" => Some(Token::Else),
            "while" => Some(Token::While),
            "return" => Some(Token::Return),
            "true" => Some(Token::True),
            "false" => Some(Token::False),
            "print" => Some(Token::Print),
            "START" => Some(Token::Start),
            "END" => Some(Token::End),
            "x" => Some(Token::Mul),
            "xx" => Some(Token::Pow),
            "xxx" => Some(Token::Tetration),
            _ => None,
        }
    }
}
