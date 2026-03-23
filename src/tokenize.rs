//! Tokenizer and diagnostic helpers.

/// Reserved keywords recognized by the tokenizer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Keyword {
    Return,
    If,
    Else,
    For,
    While,
    Int,
    Sizeof,
}

impl std::fmt::Display for Keyword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Return => "return",
            Self::If => "if",
            Self::Else => "else",
            Self::For => "for",
            Self::While => "while",
            Self::Int => "int",
            Self::Sizeof => "sizeof",
        };
        write!(f, "{s}")
    }
}

impl std::convert::TryFrom<&str> for Keyword {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "return" => Ok(Self::Return),
            "if" => Ok(Self::If),
            "else" => Ok(Self::Else),
            "for" => Ok(Self::For),
            "while" => Ok(Self::While),
            "int" => Ok(Self::Int),
            "sizeof" => Ok(Self::Sizeof),
            _ => Err(()),
        }
    }
}

/// Token kinds recognized by the tokenizer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenKind<'a> {
    Ident(&'a str),
    Keyword(Keyword),
    Punct(&'a str),
    Num(i64),
    /// A sentinel token representing the end of the input.
    Eof,
}

/// A token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Token<'a> {
    pub kind: TokenKind<'a>,
    /// The byte offset of the token in the input string.
    pub offset: usize,
}

/// Convert the input string into a sequence of tokens.
pub fn tokenize(input: &str) -> Result<Vec<Token<'_>>, String> {
    let mut tokens = Vec::new();
    let mut rest = input;
    let mut offset = 0;

    while !rest.is_empty() {
        let ch = rest.as_bytes()[0];

        // Skip whitespace characters
        if ch.is_ascii_whitespace() {
            rest = &rest[1..];
            offset += 1;
            continue;
        }

        // Numeric literal
        if ch.is_ascii_digit() {
            let len = rest.bytes().take_while(u8::is_ascii_digit).count();
            let lexeme = &rest[..len];
            tokens.push(Token::num(offset, lexeme.parse().unwrap()));
            rest = &rest[len..];
            offset += len;
            continue;
        }

        // Identifier or keyword
        if is_ident1(ch) {
            let len = rest.bytes().take_while(|byte| is_ident2(*byte)).count();
            let lexeme = &rest[..len];
            let token = if let Ok(keyword) = Keyword::try_from(lexeme) {
                Token::keyword(offset, keyword)
            } else {
                Token::ident(offset, lexeme)
            };
            tokens.push(token);
            rest = &rest[len..];
            offset += len;
            continue;
        }

        // Punctuator
        let punct_len = read_punct(rest);
        if punct_len != 0 {
            tokens.push(Token::punct(offset, &rest[..punct_len]));
            rest = &rest[punct_len..];
            offset += punct_len;
            continue;
        }

        return Err(format_error_at(input, offset, "invalid token"));
    }

    tokens.push(Token::eof(offset));
    Ok(tokens)
}

impl<'a> Token<'a> {
    /// Construct an identifier token.
    pub fn ident(offset: usize, lexeme: &'a str) -> Self {
        Self {
            offset,
            kind: TokenKind::Ident(lexeme),
        }
    }

    /// Construct a keyword token.
    pub fn keyword(offset: usize, keyword: Keyword) -> Self {
        Self {
            offset,
            kind: TokenKind::Keyword(keyword),
        }
    }

    /// Construct a punctuation token.
    pub fn punct(offset: usize, lexeme: &'a str) -> Self {
        Self {
            offset,
            kind: TokenKind::Punct(lexeme),
        }
    }

    /// Construct a numeric token.
    pub fn num(offset: usize, value: i64) -> Self {
        Self {
            offset,
            kind: TokenKind::Num(value),
        }
    }

    /// Construct the EOF sentinel.
    pub fn eof(offset: usize) -> Self {
        Self {
            offset,
            kind: TokenKind::Eof,
        }
    }

    /// Return whether this token is a punctuator.
    pub fn is_punct(self, expected: &str) -> bool {
        self.kind == TokenKind::Punct(expected)
    }

    /// Return whether this token is a keyword.
    pub fn is_keyword(self, expected: Keyword) -> bool {
        self.kind == TokenKind::Keyword(expected)
    }

    /// Return the lexeme if this is an identifier token.
    pub fn as_ident(self) -> Option<&'a str> {
        match self.kind {
            TokenKind::Ident(name) => Some(name),
            _ => None,
        }
    }

    /// Return the value if this is a numeric token.
    pub fn as_num(self) -> Option<i64> {
        match self.kind {
            TokenKind::Num(value) => Some(value),
            _ => None,
        }
    }

    /// Return whether this token is the EOF sentinel.
    pub fn is_eof(self) -> bool {
        self.kind == TokenKind::Eof
    }
}

/// Return whether the byte is valid at the start of an identifier.
///
/// Identifiers must start with an ASCII letter or underscore.
fn is_ident1(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Return whether the byte is valid after the first identifier byte.
///
/// Identifiers can contain ASCII letters, digits, or underscores.
fn is_ident2(byte: u8) -> bool {
    is_ident1(byte) || byte.is_ascii_digit()
}

/// Read a punctuator token and return its length.
fn read_punct(input: &str) -> usize {
    if ["==", "!=", "<=", ">="]
        .into_iter()
        .any(|prefix| input.starts_with(prefix))
    {
        return 2;
    }

    usize::from(
        input
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_punctuation),
    )
}

/// Format an error with a caret pointing at the given byte offset.
pub fn format_error_at(input: &str, offset: usize, message: &str) -> String {
    format!("{input}\n{}^ {message}", " ".repeat(offset))
}
