#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals and identifiers
    Num(f64),
    Str(String),
    Ident(String),
    /// A single-quoted identifier, e.g. 'name'. Required when declaring a
    /// variable; also accepted (interchangeably with a bare Ident) wherever
    /// an identifier is referenced, pending a decision on whether quoting
    /// should be required everywhere.
    Quoted(String),

    // Keywords
    Fn,
    Var,
    If,
    Else,
    While,
    Return,
    True,
    False,
    Print,

    // Symbols
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Semicolon,
    Arrow, // ->

    // Operators
    Plus,
    Minus,
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
            "if" => Some(Token::If),
            "else" => Some(Token::Else),
            "while" => Some(Token::While),
            "return" => Some(Token::Return),
            "true" => Some(Token::True),
            "false" => Some(Token::False),
            "print" => Some(Token::Print),
            _ => None,
        }
    }
}
