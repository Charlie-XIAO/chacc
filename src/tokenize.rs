//! Tokenize C source code into a flat token stream.

use std::rc::Rc;

use rustc_hash::FxHashSet;
use smol_str::SmolStr;

use crate::error::Result;
use crate::source::{Source, SourceSpan};
use crate::types::Type;

/// Reserved keywords recognized by the tokenizer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display, strum::EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum Keyword {
    Return,
    If,
    Else,
    For,
    While,
    Do,
    Goto,
    Break,
    Continue,
    Switch,
    Case,
    Default,
    Void,
    #[strum(serialize = "_Bool")]
    Bool,
    Char,
    Short,
    Int,
    Long,
    Float,
    Double,
    Signed,
    Unsigned,
    Sizeof,
    Struct,
    Union,
    Enum,
    Const,
    Volatile,
    Typedef,
    Static,
    Extern,
    Auto,
    Register,
    #[strum(
        serialize = "restrict",
        serialize = "__restrict",
        serialize = "__restrict__"
    )]
    Restrict,
    #[strum(serialize = "_Noreturn")]
    Noreturn,
    #[strum(serialize = "_Alignof")]
    Alignof,
    #[strum(serialize = "_Alignas")]
    Alignas,
}

/// Token kinds recognized by the tokenizer.
#[derive(Clone, Debug)]
pub enum TokenKind {
    Ident(SmolStr),
    Keyword(Keyword),
    Punct(SmolStr),
    Num(u64, Type),
    Flonum(f64, Type),
    /// A string literal, with null terminator preserved.
    Str(Rc<[u8]>),
    Eof,
}

/// A token.
#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: SourceSpan,
    /// Whether this token begins a logical source line.
    pub at_bol: bool,
    /// Whether this token follows a space.
    pub follows_space: bool,
    /// Macro names suppressed for this token during preprocessing.
    ///
    /// This is used by the preprocessor to prevent recursive re-expansion of
    /// macros. It should be cleared to free up memory when the preprocessing
    /// stage completes.
    pub hideset: Option<Rc<FxHashSet<SmolStr>>>,
}

impl Token {
    /// Return whether this token is a punctuator.
    pub fn is_punct(&self, expected: &str) -> bool {
        matches!(self.kind, TokenKind::Punct(ref p) if p == expected)
    }

    /// Return whether this token is a certain keyword.
    pub fn is_keyword(&self, expected: Keyword) -> bool {
        matches!(self.kind, TokenKind::Keyword(p) if p == expected)
    }

    /// Return whether this token is the EOF sentinel.
    pub fn is_eof(&self) -> bool {
        matches!(self.kind, TokenKind::Eof)
    }

    /// Return whether this token is a typename keyword.
    pub fn is_typename_keyword(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::Keyword(
                Keyword::Void
                    | Keyword::Bool
                    | Keyword::Char
                    | Keyword::Short
                    | Keyword::Int
                    | Keyword::Long
                    | Keyword::Float
                    | Keyword::Double
                    | Keyword::Signed
                    | Keyword::Unsigned
                    | Keyword::Struct
                    | Keyword::Union
                    | Keyword::Enum
                    | Keyword::Const
                    | Keyword::Volatile
                    | Keyword::Typedef
                    | Keyword::Static
                    | Keyword::Extern
                    | Keyword::Auto
                    | Keyword::Register
                    | Keyword::Restrict
                    | Keyword::Noreturn
                    | Keyword::Alignas
            )
        )
    }

    /// Return the keyword if this is a keyword token.
    pub fn as_keyword(&self) -> Option<Keyword> {
        match self.kind {
            TokenKind::Keyword(keyword) => Some(keyword),
            _ => None,
        }
    }

    /// Return the lexeme if this is an identifier token.
    pub fn as_ident(&self) -> Option<SmolStr> {
        match self.kind {
            TokenKind::Ident(ref name) => Some(name.clone()),
            _ => None,
        }
    }

    /// Return the value if this is an integer numeric token.
    pub fn as_num(&self) -> Option<(u64, Type)> {
        match self.kind {
            TokenKind::Num(value, ty) => Some((value, ty)),
            _ => None,
        }
    }

    /// Return the value if this is a floating-point numeric token.
    pub fn as_flonum(&self) -> Option<(f64, Type)> {
        match self.kind {
            TokenKind::Flonum(value, ty) => Some((value, ty)),
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
}

/// Tokenizer for C source code.
pub struct Tokenizer<'a> {
    source: &'a Source,
    pos: usize,
    at_bol: bool,
    follows_space: bool,
    tokens: Vec<Token>,
}

impl<'a> Tokenizer<'a> {
    /// Create a new tokenizer for the given source.
    pub fn new(source: &'a Source) -> Self {
        Self {
            source,
            pos: 0,
            at_bol: true,
            follows_space: false,
            tokens: Vec::new(),
        }
    }

    /// Push a token with the given kind.
    fn push(&mut self, kind: TokenKind, offset: usize, len: usize) {
        self.tokens.push(Token {
            kind,
            span: SourceSpan {
                id: self.source.id,
                offset,
                len,
            },
            at_bol: self.at_bol,
            follows_space: self.follows_space,
            hideset: None,
        });
        self.at_bol = false;
        self.follows_space = false;
    }

    /// Tokenize the entire source into a flat token list.
    pub fn tokenize(mut self) -> Result<Vec<Token>> {
        let content = &self.source.content;

        while self.pos < content.len() {
            let ch = content.as_bytes()[self.pos];

            if self.read_comment()? {
                self.follows_space = true;
                continue;
            }

            if ch == b'\n' {
                self.pos += 1;
                self.at_bol = true;
                self.follows_space = false;
                continue;
            }

            if ch.is_ascii_whitespace() {
                self.pos += 1;
                self.follows_space = true;
                continue;
            }

            if ch.is_ascii_digit()
                || (ch == b'.'
                    && content
                        .as_bytes()
                        .get(self.pos + 1)
                        .is_some_and(u8::is_ascii_digit))
            {
                self.read_numeric_literal()?;
                continue;
            }

            if ch == b'"' {
                self.read_string_literal()?;
                continue;
            }

            if ch == b'\'' {
                self.read_char_literal()?;
                continue;
            }

            if is_ident1(&ch) {
                self.read_ident();
                continue;
            }

            if self.read_punct() {
                continue;
            }

            return Err(self.source.error(self.pos, 1, "invalid token"));
        }

        self.push(TokenKind::Eof, self.pos, 0);
        Ok(self.tokens)
    }

    /// Read an inline or block comment, returning whether there is one.
    fn read_comment(&mut self) -> Result<bool> {
        let offset = self.pos;
        let content = &self.source.content;
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
            return Err(self.source.error(offset, 2, "unclosed block comment"));
        }

        Ok(false)
    }

    /// Read a numeric literal token.
    fn read_numeric_literal(&mut self) -> Result<()> {
        let content = &self.source.content;
        let bytes = content.as_bytes();
        let offset = self.pos;

        // Scan as far as we can while the characters are valid in some kind of
        // numeric literal, which might not yet be valid
        let mut end = offset + 1;
        while end < bytes.len() {
            let ch = bytes[end];
            if ch.is_ascii_alphanumeric()
                || ch == b'.'
                || matches!(
                    (ch, bytes[end - 1]),
                    (b'+' | b'-', b'e' | b'E' | b'p' | b'P')
                )
            {
                end += 1;
            } else {
                break;
            }
        }

        let lexeme = &content[offset..end];
        let bytes = lexeme.as_bytes();
        let starts_hex = bytes.starts_with(b"0x") || bytes.starts_with(b"0X");
        let starts_binary = bytes.starts_with(b"0b") || bytes.starts_with(b"0B");

        let is_hex_flonum =
            starts_hex && bytes[2..].iter().any(|b| matches!(b, b'.' | b'p' | b'P'));

        let is_dec_flonum = !starts_hex
            && !starts_binary
            && (bytes.first() == Some(&b'.')
                || bytes.iter().any(|b| matches!(b, b'.' | b'e' | b'E')));

        if is_hex_flonum || is_dec_flonum {
            let (num, ty) = parse_flonum_literal(lexeme, is_hex_flonum).ok_or_else(|| {
                self.source
                    .error(self.pos, lexeme.len(), "invalid floating-point literal")
            })?;
            self.push(TokenKind::Flonum(num, ty), offset, lexeme.len());
        } else {
            let (num, ty) = parse_integer_literal(lexeme).ok_or_else(|| {
                self.source
                    .error(self.pos, lexeme.len(), "invalid integer literal")
            })?;
            self.push(TokenKind::Num(num, ty), offset, lexeme.len());
        }

        self.pos = end;
        Ok(())
    }

    /// Read a string literal token.
    fn read_string_literal(&mut self) -> Result<()> {
        let bytes = self.source.content.as_bytes();
        let mut i = self.pos + 1; // Skip opening quote
        let mut content = Vec::new();

        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    content.push(b'\0');
                    self.push(TokenKind::Str(content.into()), self.pos, i - self.pos + 1);
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

        Err(self
            .source
            .error(self.pos, i - self.pos, "unclosed string literal"))
    }

    /// Read a character literal token.
    fn read_char_literal(&mut self) -> Result<()> {
        let bytes = self.source.content.as_bytes();
        let mut i = self.pos + 1; // Skip opening quote

        let byte = *bytes
            .get(i)
            .ok_or_else(|| self.source.error(self.pos, 1, "unclosed char literal"))?;

        if matches!(byte, b'\n' | b'\0') {
            return Err(self.source.error(self.pos, 1, "unclosed char literal"));
        }

        let ch = if byte == b'\\' {
            let (escaped, len) = self.read_escape_char(i + 1)?;
            i += 1 + len;
            escaped
        } else {
            i += 1;
            byte
        };

        // Interpret one-byte character constant using signed-char semantics,
        // e.g., '\x80' becomes -128 (wrapped around)
        let ch = ch as i8;

        let len = i - self.pos + 1;
        match bytes[i..]
            .iter()
            .position(|&b| matches!(b, b'\'' | b'\n' | b'\0'))
            .filter(|&pos| bytes[i + pos] == b'\'')
        {
            Some(0) => {
                self.push(TokenKind::Num(ch as _, Type::INT), self.pos, len);
                self.pos = i + 1; // Skip past closing quote
                Ok(())
            },
            Some(j) => Err(self
                .source
                .error(self.pos, len + j, "multi-charcter char constant")),
            None => Err(self.source.error(self.pos, 1, "unclosed char literal")),
        }
    }

    /// Read an escape sequence starting at the first byte after the backslash.
    ///
    /// Returns the decoded byte and the number of bytes consumed.
    fn read_escape_char(&self, start: usize) -> Result<(u8, usize)> {
        let bytes = self.source.content.as_bytes();
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
                return Err(self.source.error(pos, 1, "invalid hex escape sequence"));
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
                        self.source.warn(pos, 1, "hex escape sequence out of range");
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
            b'"' | b'\'' | b'\\' | b'?' => first,
            _ => {
                self.source.warn(start, 1, "unknown escape sequence");
                first
            },
        };
        Ok((decoded, 1))
    }

    /// Read an identifier token.
    fn read_ident(&mut self) {
        let offset = self.pos;
        let content = &self.source.content;

        let len = content[self.pos..].bytes().take_while(is_ident2).count();
        let lexeme = &content[offset..offset + len];
        self.push(TokenKind::Ident(lexeme.into()), offset, len);
        self.pos += len;
    }

    /// Read a punctuator token, returning whether there is one.
    fn read_punct(&mut self) -> bool {
        let offset = self.pos;
        let rest = &self.source.content[offset..];

        const PUNCTUATORS: &[&str] = &[
            "<<=", ">>=", "...", "==", "!=", "<=", ">=", "<<", ">>", "->", "+=", "-=", "*=", "/=",
            "%=", "&=", "|=", "^=", "++", "--", "&&", "||",
        ];

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

        self.push(
            TokenKind::Punct(rest[..punct_len].into()),
            offset,
            punct_len,
        );
        self.pos += punct_len;
        true
    }
}

/// Make the token stream EOF-terminated.
///
/// This requires the given token stream to be non-empty. It is no-op if the
/// given token stream is already EOF-terminated.
pub fn ensure_eof(mut tokens: Vec<Token>) -> Vec<Token> {
    let last = tokens.last().expect("token stream must not be empty");
    if last.is_eof() {
        return tokens;
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        span: SourceSpan {
            id: last.span.id,
            offset: last.span.offset + last.span.len,
            len: 0,
        },
        at_bol: false,
        follows_space: false,
        hideset: None,
    });
    tokens
}

/// Return whether the byte is valid at the start of an identifier.
fn is_ident1(byte: &u8) -> bool {
    byte.is_ascii_alphabetic() || *byte == b'_'
}

/// Return whether the byte is valid after the first identifier byte.
fn is_ident2(byte: &u8) -> bool {
    is_ident1(byte) || byte.is_ascii_digit()
}

/// Parse an integer literal from the given lexeme.
///
/// Returns the parsed value and its type, or `None` if the lexeme is not a
/// valid integer literal.
fn parse_integer_literal(lexeme: &str) -> Option<(u64, Type)> {
    let bytes = lexeme.as_bytes();

    let (radix, start) = match bytes {
        [b'0', b'x' | b'X', ..] => (16, 2),
        [b'0', b'b' | b'B', ..] => (2, 2),
        // A leading 0 followed by more digits is an octal literal, e.g.,
        // "08" is an invalid octal rather than a valid decimal; also we do
        // not strip the leading 0 because it does not affect the parsed
        // value, and that we want to accept a single "0"
        [b'0', ..] => (8, 0),
        _ => (10, 0),
    };

    let (suffix_len, l, u) = match bytes[start..] {
        [.., b'L', b'L', b'U']
        | [.., b'L', b'L', b'u']
        | [.., b'l', b'l', b'U']
        | [.., b'l', b'l', b'u']
        | [.., b'U', b'L', b'L']
        | [.., b'U', b'l', b'l']
        | [.., b'u', b'L', b'L']
        | [.., b'u', b'l', b'l'] => (3, true, true),
        [.., b'L', b'U']
        | [.., b'L', b'u']
        | [.., b'l', b'U']
        | [.., b'l', b'u']
        | [.., b'U', b'L']
        | [.., b'U', b'l']
        | [.., b'u', b'L']
        | [.., b'u', b'l'] => (2, true, true),
        [.., b'L', b'L'] | [.., b'l', b'l'] => (2, true, false),
        [.., b'L'] | [.., b'l'] => (1, true, false),
        [.., b'U'] | [.., b'u'] => (1, false, true),
        _ => (0, false, false),
    };

    let body = &lexeme[start..bytes.len() - suffix_len];
    let num = u64::from_str_radix(body, radix).ok()?;

    let ty = if radix == 10 {
        if l && u {
            Type::ULONG
        } else if l {
            Type::LONG
        } else if u {
            if num > u32::MAX as _ {
                Type::ULONG
            } else {
                Type::UINT
            }
        } else if num > i32::MAX as _ {
            Type::LONG
        } else {
            Type::INT
        }
    } else if l && u {
        Type::ULONG
    } else if l {
        if num > i64::MAX as _ {
            Type::ULONG
        } else {
            Type::LONG
        }
    } else if u {
        if num > u32::MAX as _ {
            Type::ULONG
        } else {
            Type::UINT
        }
    } else if num > i64::MAX as _ {
        Type::ULONG
    } else if num > u32::MAX as _ {
        Type::LONG
    } else if num > i32::MAX as _ {
        Type::UINT
    } else {
        Type::INT
    };

    Some((num, ty))
}

/// Parse a floating-point literal from the given lexeme.
///
/// Returns the parsed value and its type, or `None` if the lexeme is not a
/// valid floating-point literal.
fn parse_flonum_literal(lexeme: &str, is_hex: bool) -> Option<(f64, Type)> {
    let (suffix_len, ty) = match lexeme.as_bytes().last() {
        Some(b'f' | b'F') => (1, Type::FLOAT),
        Some(b'l' | b'L') => (1, Type::DOUBLE),
        _ => (0, Type::DOUBLE),
    };

    let body = &lexeme[..lexeme.len() - suffix_len];
    let num = if is_hex {
        hexf_parse::parse_hexf64(body, false).ok()?
    } else {
        body.parse::<f64>().ok()?
    };

    Some((num, ty))
}
