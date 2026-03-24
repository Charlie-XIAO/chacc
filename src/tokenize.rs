//! Tokenizer and diagnostic helpers.

use std::rc::Rc;

/// Reserved keywords recognized by the tokenizer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Keyword {
    Return,
    If,
    Else,
    For,
    While,
    Char,
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
            Self::Char => "char",
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
            "char" => Ok(Self::Char),
            "int" => Ok(Self::Int),
            "sizeof" => Ok(Self::Sizeof),
            _ => Err(()),
        }
    }
}

/// Token kinds recognized by the tokenizer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenKind<'a> {
    /// An identifier with the given lexeme.
    Ident(&'a str),
    /// A reserved keyword.
    Keyword(Keyword),
    /// A punctuator with the given lexeme.
    Punct(&'a str),
    /// A numeric literal with the given value.
    Num(i64),
    /// A string literal with the given content.
    Str(Rc<[u8]>),
    /// A sentinel token representing the end of the input.
    Eof,
}

/// A token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token<'a> {
    pub kind: TokenKind<'a>,
    /// The byte offset of the token in the input string.
    pub offset: usize,
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

    /// Construct a numeric literal token.
    pub fn num(offset: usize, value: i64) -> Self {
        Self {
            offset,
            kind: TokenKind::Num(value),
        }
    }

    /// Construct a string literal token.
    pub fn str(offset: usize, content: impl Into<Rc<[u8]>>) -> Self {
        Self {
            offset,
            kind: TokenKind::Str(content.into()),
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
    pub fn is_punct(&self, expected: &str) -> bool {
        self.kind == TokenKind::Punct(expected)
    }

    /// Return whether this token is a keyword.
    pub fn is_keyword(&self, expected: Keyword) -> bool {
        self.kind == TokenKind::Keyword(expected)
    }

    /// Return whether this token is a type name keyword.
    pub fn is_typename_keyword(&self) -> bool {
        matches!(self.kind, TokenKind::Keyword(Keyword::Char | Keyword::Int))
    }

    /// Return the lexeme if this is an identifier token.
    pub fn as_ident(&self) -> Option<&'a str> {
        match self.kind {
            TokenKind::Ident(name) => Some(name),
            _ => None,
        }
    }

    /// Return the value if this is a numeric token.
    pub fn as_num(&self) -> Option<i64> {
        match self.kind {
            TokenKind::Num(value) => Some(value),
            _ => None,
        }
    }

    /// Return the content if this is a string literal token.
    pub fn as_str(&self) -> Option<Rc<[u8]>> {
        match self.kind {
            TokenKind::Str(ref content) => Some(content.clone()),
            _ => None,
        }
    }

    /// Return whether this token is the EOF sentinel.
    pub fn is_eof(&self) -> bool {
        self.kind == TokenKind::Eof
    }
}

pub struct Tokenizer<'a> {
    input: &'a str,
    pos: usize,
    tokens: Vec<Token<'a>>,
}

impl<'a> Tokenizer<'a> {
    /// Create a new tokenizer for the given input string.
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            tokens: Vec::new(),
        }
    }

    fn error_current(&self, message: &str) -> String {
        format_error_at(self.input, self.pos, message)
    }

    /// Tokenize the entire input into a flat token list.
    pub fn tokenize(mut self) -> Result<Vec<Token<'a>>, String> {
        while self.pos < self.input.len() {
            let ch = self.input.as_bytes()[self.pos];

            if ch.is_ascii_whitespace() {
                self.pos += 1;
                continue;
            }

            if ch.is_ascii_digit() {
                self.read_number();
                continue;
            }

            if ch == b'"' {
                self.read_string_literal()?;
                continue;
            }

            if is_ident1(ch) {
                self.read_ident_or_keyword();
                continue;
            }

            if self.try_read_punct() {
                continue;
            }

            return Err(self.error_current("invalid token"));
        }

        self.tokens.push(Token::eof(self.pos));
        Ok(self.tokens)
    }

    /// Read a numeric literal token.
    fn read_number(&mut self) {
        let offset = self.pos;
        let len = self.input[self.pos..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();

        let lexeme = &self.input[offset..offset + len];
        let num = lexeme.parse().expect("failed to parse number");

        self.tokens.push(Token::num(offset, num));
        self.pos += len;
    }

    /// Read a string literal token.
    fn read_string_literal(&mut self) -> Result<(), String> {
        let offset = self.pos + 1; // Skip opening quote
        let rest = &self.input.as_bytes()[offset..];

        for (i, &byte) in rest.iter().enumerate() {
            match byte {
                b'"' => {
                    let mut content = Vec::with_capacity(i + 1);
                    content.extend_from_slice(&rest[..i]);
                    content.push(b'\0');

                    self.tokens.push(Token::str(offset, content));
                    self.pos += i + 2; // Skip past closing quote
                    return Ok(());
                },
                b'\n' | b'\0' => break,
                _ => {},
            }
        }

        Err(self.error_current("unclosed string literal"))
    }

    /// Read an identifier or keyword token.
    fn read_ident_or_keyword(&mut self) {
        let offset = self.pos;
        let len = self.input[self.pos..]
            .bytes()
            .take_while(|byte| is_ident2(*byte))
            .count();

        let lexeme = &self.input[offset..offset + len];
        let token = if let Ok(keyword) = Keyword::try_from(lexeme) {
            Token::keyword(offset, keyword)
        } else {
            Token::ident(offset, lexeme)
        };

        self.tokens.push(token);
        self.pos += len;
    }

    /// Try to read a punctuator token, returning whether there is one.
    fn try_read_punct(&mut self) -> bool {
        let offset = self.pos;
        let rest = &self.input[offset..];

        const PUNCTUATORS: &[&str] = &["==", "!=", "<=", ">="];

        let punct_len =
            if let Some(punct) = PUNCTUATORS.iter().find(|prefix| rest.starts_with(*prefix)) {
                punct.len()
            } else if rest
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_punctuation)
            {
                1
            } else {
                0
            };

        if punct_len == 0 {
            return false;
        }

        let lexeme = &self.input[offset..offset + punct_len];
        self.tokens.push(Token::punct(offset, lexeme));
        self.pos += punct_len;
        true
    }
}

/// Return whether the byte is valid at the start of an identifier.
fn is_ident1(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Return whether the byte is valid after the first identifier byte.
fn is_ident2(byte: u8) -> bool {
    is_ident1(byte) || byte.is_ascii_digit()
}

/// Format an error with a caret pointing at the given byte offset.
pub fn format_error_at(input: &str, offset: usize, message: &str) -> String {
    format!("{input}\n{}^ {message}", " ".repeat(offset))
}
