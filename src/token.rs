#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals and identifiers
    Int(i64),
    Ident(String),

    // Keywords
    Fn,
    Let,
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
            "let" => Some(Token::Let),
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
