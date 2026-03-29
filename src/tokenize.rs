//! Tokenizer and diagnostic helpers.

use std::rc::Rc;

use crate::error::{Error, Result};
use crate::source::Source;

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
    /// A string literal with the given content, including the null terminator.
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
    source: &'a Source,
    pos: usize,
    tokens: Vec<Token<'a>>,
}

impl<'a> Tokenizer<'a> {
    /// Create a new tokenizer for the given source.
    pub fn new(source: &'a Source) -> Self {
        Self {
            source,
            pos: 0,
            tokens: Vec::new(),
        }
    }

    fn error_current(&self, message: &str) -> Error {
        self.source.error_at(self.pos, message)
    }

    /// Tokenize the entire source into a flat token list.
    pub fn tokenize(mut self) -> Result<Vec<Token<'a>>> {
        let content = self.source.content();

        while self.pos < content.len() {
            let ch = content.as_bytes()[self.pos];

            if self.read_comment()? {
                continue;
            }

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

            if is_ident1(&ch) {
                self.read_ident_or_keyword();
                continue;
            }

            if self.read_punct() {
                continue;
            }

            return Err(self.error_current("invalid token"));
        }

        self.tokens.push(Token::eof(self.pos));
        Ok(self.tokens)
    }

    /// Read an inline or block comment, returning whether there is one.
    fn read_comment(&mut self) -> Result<bool> {
        let offset = self.pos;
        let content = self.source.content();
        let bytes = content.as_bytes();
        let rest = &content[offset..];

        if rest.starts_with("//") {
            self.pos += 2;
            while self.pos < content.len() && bytes[self.pos] != b'\n' {
                self.pos += 1;
            }
            return Ok(true);
        }

        if rest.starts_with("/*") {
            self.pos += 2;
            while self.pos + 1 < content.len() {
                if bytes[self.pos] == b'*' && bytes[self.pos + 1] == b'/' {
                    self.pos += 2;
                    return Ok(true);
                }
                self.pos += 1;
            }
            return Err(self.source.error_at(offset, "unclosed block comment"));
        }

        Ok(false)
    }

    /// Read a numeric literal token.
    fn read_number(&mut self) {
        let offset = self.pos;
        let content = self.source.content();

        let len = content[self.pos..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();

        let num = content[offset..offset + len]
            .parse()
            .expect("failed to parse number");

        self.tokens.push(Token::num(offset, num));
        self.pos += len;
    }

    /// Read a string literal token.
    fn read_string_literal(&mut self) -> Result<()> {
        let bytes = self.source.content().as_bytes();
        let mut i = self.pos + 1; // Skip opening quote
        let mut content = Vec::new();

        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    content.push(b'\0');
                    self.tokens.push(Token::str(self.pos, content));
                    self.pos = i + 1; // Skip past closing quote
                    return Ok(());
                },
                b'\\' => {
                    i += 1;
                    if i >= bytes.len() || matches!(bytes[i], b'\n' | b'\0') {
                        break;
                    }
                    let (escaped, len) = self.read_escape_char(i)?;
                    content.push(escaped);
                    i += len;
                },
                b'\n' | b'\0' => break,
                byte => {
                    content.push(byte);
                    i += 1;
                },
            }
        }

        Err(self.error_current("unclosed string literal"))
    }

    /// Read an escape sequence starting at the first byte after the backslash.
    ///
    /// Returns the decoded byte and the number of bytes consumed.
    fn read_escape_char(&self, start: usize) -> Result<(u8, usize)> {
        let bytes = self.source.content().as_bytes();
        let first = bytes[start];

        // Octal escape sequence (up to three octal digits)
        if (first as char).is_digit(8) {
            let mut octal_value = first - b'0';
            let mut len = 1;
            for &byte in bytes.iter().skip(start + 1).take(2) {
                if (byte as char).is_digit(8) {
                    octal_value = (octal_value << 3) + (byte - b'0');
                    len += 1;
                } else {
                    break;
                }
            }
            return Ok((octal_value, len));
        }

        // Hexadecimal escape sequence
        if first == b'x' {
            let mut pos = start + 1;
            if pos >= bytes.len() || !bytes[pos].is_ascii_hexdigit() {
                return Err(self.source.error_at(pos, "invalid hex escape sequence"));
            }

            let mut hex_value = 0u8;
            let mut has_warned_overflow = false;

            while pos < bytes.len() && bytes[pos].is_ascii_hexdigit() {
                let digit = (bytes[pos] as char).to_digit(16).unwrap() as u8;
                if !has_warned_overflow {
                    if let Some(next) = hex_value.checked_mul(16).and_then(|v| v.checked_add(digit))
                    {
                        hex_value = next;
                    } else {
                        has_warned_overflow = true;
                        self.source.warn_at(pos, "hex escape sequence out of range");
                        hex_value = hex_value.wrapping_mul(16).wrapping_add(digit);
                    }
                } else {
                    hex_value = hex_value.wrapping_mul(16).wrapping_add(digit);
                }
                pos += 1;
            }

            return Ok((hex_value, pos - start));
        }

        // Standard single-character escapes.
        let decoded = match first {
            b'a' => b'\x07',
            b'b' => b'\x08',
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => b'\x0b',
            b'f' => b'\x0c',
            b'r' => b'\r',
            b'e' => 27, // GNU C extension for the ASCII escape character
            _ => {
                self.source.warn_at(
                    start,
                    &format!("unknown escape sequence '\\{}'", first as char),
                );
                first
            },
        };
        Ok((decoded, 1))
    }

    /// Read an identifier or keyword token.
    fn read_ident_or_keyword(&mut self) {
        let offset = self.pos;
        let content = self.source.content();

        let len = content[self.pos..].bytes().take_while(is_ident2).count();
        let lexeme = &content[offset..offset + len];

        let token = if let Ok(keyword) = Keyword::try_from(lexeme) {
            Token::keyword(offset, keyword)
        } else {
            Token::ident(offset, lexeme)
        };

        self.tokens.push(token);
        self.pos += len;
    }

    /// Read a punctuator token, returning whether there is one.
    fn read_punct(&mut self) -> bool {
        let offset = self.pos;
        let rest = &self.source.content()[offset..];

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

        self.tokens.push(Token::punct(offset, &rest[..punct_len]));
        self.pos += punct_len;
        true
    }
}

/// Return whether the byte is valid at the start of an identifier.
fn is_ident1(byte: &u8) -> bool {
    byte.is_ascii_alphabetic() || *byte == b'_'
}

/// Return whether the byte is valid after the first identifier byte.
fn is_ident2(byte: &u8) -> bool {
    is_ident1(byte) || byte.is_ascii_digit()
}
