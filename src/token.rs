#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals and identifiers
    /// The parsed value, and the original digit text as written (no sign --
    /// unary minus is handled at the parser level). The raw text matters
    /// for `bignum`, which can represent far more precision than `f64`
    /// holds; every other consumer just uses the parsed value.
    Num(f64, String),
    Str(String),
    Ident(String),
    /// A single-quoted name, e.g. 'name'. Any character is allowed inside
    /// (except a literal `'`, which always closes it). Used for variable
    /// names, function names, and parameter names alike.
    Quoted(String),

    // Keywords
    Var,
    /// Starts a variable reference (`ref:var:TYPE 'name'`) or a function
    /// call (`ref:func 'name'*args*`).
    Ref,
    /// Both a top-level function definition (`func 'name'*params* -> type
    /// { ... }`) and the other thing `ref:` can lead to, alongside `var`
    /// (`ref:func 'name'*args*`) -- same keyword, two different positions.
    Func,
    If,
    Else,
    While,
    Return,
    True,
    False,
    Print,
    /// `input:type 'name';` -- reads a line from stdin into a new variable.
    Input,
    /// `clock:num 'name';` -- reads elapsed time since the program started
    /// (in seconds) into a new variable.
    Clock,
    /// `overwrite*segments* [to*(dest)*];` -- always writes to a file
    /// (replacing its entire content), never the screen. Same
    /// segment-based text building `print` uses.
    Overwrite,
    /// `[to*(dest)*]` -- the file-destination clause `print` (optionally)
    /// and `overwrite` (always) can take.
    To,
    /// `[from*(dest)*]` -- the file-source clause `input:` (optionally)
    /// can take, reading its whole content instead of a line from stdin.
    From,
    /// `append*(array), (value)*;` -- grows an array by one element.
    Append,
    /// `length*(array)*` -- an array's element count, as a value.
    Length,
    /// Marks the start of the program's entry point block (replaces `fn main`).
    Start,
    End,
    /// `x`: multiply. Replaces `*`, which is reserved for print's `*expr*` wrapper.
    Mul,
    /// `xx`: power/exponentiation.
    Pow,
    /// `xxx`: tetration (repeated exponentiation).
    Tetration,
    /// `stch` ("stitch"): text concatenation.
    Stch,

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
    /// `not` (prefix boolean negation).
    Not,
    /// `not=` (not-equal comparison), written as one word with no space.
    NotEq,
    /// Postfix `!`: factorial. Freed up for this once `not`/`not=` replaced
    /// `!`/`!=` as the boolean operators.
    Bang,
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
            "input" => Some(Token::Input),
            "clock" => Some(Token::Clock),
            "overwrite" => Some(Token::Overwrite),
            "to" => Some(Token::To),
            "from" => Some(Token::From),
            "append" => Some(Token::Append),
            "length" => Some(Token::Length),
            "not" => Some(Token::Not),
            "START" => Some(Token::Start),
            "END" => Some(Token::End),
            "x" => Some(Token::Mul),
            "xx" => Some(Token::Pow),
            "xxx" => Some(Token::Tetration),
            "stch" => Some(Token::Stch),
            _ => None,
        }
    }
}
