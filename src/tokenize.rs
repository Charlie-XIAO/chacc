//! Tokenize C source code into a flat token stream.

use std::borrow::Cow;
use std::rc::Rc;

use rustc_hash::FxHashSet;
use smol_str::SmolStr;

use crate::error::Result;
use crate::source::{Source, SourceMap, SourceSpan};
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

/// Preprocessor token kinds for [`PreToken`].
#[derive(Clone, Debug)]
pub enum PreTokenKind {
    Ident(SmolStr),
    Punct(SmolStr),
    NumLit,
    StrLit,
    CharLit,
    Eof,
}

/// A preprocessor token.
///
/// This is the direct output of tokenization consumed by the preprocessor,
/// which needs to be lowered into [`Token`] before C level parsing.
#[derive(Clone, Debug)]
pub struct PreToken {
    pub kind: PreTokenKind,
    pub span: SourceSpan,
    /// Whether this token begins a logical source line.
    pub at_bol: bool,
    /// Whether this token follows a space.
    pub follows_space: bool,
    /// Macro names suppressed for this token during preprocessing.
    pub hideset: Option<Rc<FxHashSet<SmolStr>>>,
    /// The spelling if this is a synthetic token.
    ///
    /// If this is `None`, this token must originate from the source content,
    /// and its spelling can be obtained from the source using its `span`.
    pub synthetic: Option<SmolStr>,
}

impl PreToken {
    /// Return whether this token is a punctuator.
    pub fn is_punct(&self, expected: &str) -> bool {
        matches!(self.kind, PreTokenKind::Punct(ref p) if p == expected)
    }

    /// Return whether this token is a string literal.
    pub fn is_str_lit(&self) -> bool {
        matches!(self.kind, PreTokenKind::StrLit)
    }

    /// Return whether this token is the EOF sentinel.
    pub fn is_eof(&self) -> bool {
        matches!(self.kind, PreTokenKind::Eof)
    }

    /// Return the lexeme if this is an identifier token.
    pub fn as_ident(&self) -> Option<SmolStr> {
        match self.kind {
            PreTokenKind::Ident(ref name) => Some(name.clone()),
            _ => None,
        }
    }

    /// Return a source span for a substring of this token.
    fn span_at(&self, offset: usize, len: usize) -> SourceSpan {
        if self.synthetic.is_some() {
            self.span // Not meaningful, return as is
        } else {
            SourceSpan {
                id: self.span.id,
                offset: self.span.offset + offset,
                len,
            }
        }
    }
}

/// Token kinds for [`Token`].
#[derive(Clone, Debug)]
pub enum TokenKind {
    Ident(SmolStr),
    Keyword(Keyword),
    Punct(SmolStr),
    Num(u64, Type),
    Flonum(f64, Type),
    /// The bytes of a string literal, with null terminator preserved.
    Str(Rc<[u8]>),
    Eof,
}

/// A token.
#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: SourceSpan,
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
    tokens: Vec<PreToken>,
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
    fn push(&mut self, kind: PreTokenKind, offset: usize, len: usize) {
        self.tokens.push(PreToken {
            kind,
            span: SourceSpan {
                id: self.source.id,
                offset,
                len,
            },
            at_bol: self.at_bol,
            follows_space: self.follows_space,
            hideset: None,
            synthetic: None,
        });
        self.at_bol = false;
        self.follows_space = false;
    }

    /// Tokenize the entire source into a flat token list.
    ///
    /// If `allow_comment` is false, comments will be treated as invalid tokens
    /// instead of being skipped.
    pub fn tokenize(mut self, allow_comment: bool) -> Result<Vec<PreToken>> {
        let content = &self.source.content;

        while self.pos < content.len() {
            let ch = content.as_bytes()[self.pos];

            if allow_comment && self.read_comment()? {
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
                self.read_numeric_literal();
                continue;
            }

            if ch == b'"' {
                self.read_string_char_literal(false)?;
                continue;
            }

            if ch == b'\'' {
                self.read_string_char_literal(true)?;
                continue;
            }

            if is_ident_start(&ch) {
                self.read_ident();
                continue;
            }

            if self.read_punct() {
                continue;
            }

            return Err(self.source.error(self.pos, 1, "invalid token"));
        }

        self.push(PreTokenKind::Eof, self.pos, 0);
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
    fn read_numeric_literal(&mut self) {
        let content = &self.source.content;
        let bytes = content.as_bytes();
        let offset = self.pos;

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

        self.push(PreTokenKind::NumLit, offset, end - offset);
        self.pos = end;
    }

    /// Read a string or char literal token.
    fn read_string_char_literal(&mut self, is_char: bool) -> Result<()> {
        let bytes = self.source.content.as_bytes();
        let mut i = self.pos + 1; // Skip opening quote

        while i < bytes.len() {
            match bytes[i] {
                b'\'' if is_char => {
                    self.push(PreTokenKind::CharLit, self.pos, i - self.pos + 1);
                    self.pos = i + 1; // Skip past closing quote
                    return Ok(());
                },
                b'"' if !is_char => {
                    self.push(PreTokenKind::StrLit, self.pos, i - self.pos + 1);
                    self.pos = i + 1; // Skip past closing quote
                    return Ok(());
                },
                b'\\' => {
                    i += 1;
                    if i >= bytes.len() || matches!(bytes[i], b'\n' | b'\0') {
                        break;
                    }
                    i += 1;
                },
                b'\n' | b'\0' => break,
                _ => i += 1,
            }
        }

        Err(self.source.error(
            self.pos,
            i - self.pos,
            format!(
                "unclosed {} literal",
                if is_char { "char" } else { "string" }
            ),
        ))
    }

    /// Read an identifier token.
    fn read_ident(&mut self) {
        let offset = self.pos;
        let content = &self.source.content;

        let len = content[self.pos..]
            .bytes()
            .take_while(|b| is_ident_start(b) || b.is_ascii_digit())
            .count();
        let lexeme = &content[offset..offset + len];
        self.push(PreTokenKind::Ident(lexeme.into()), offset, len);
        self.pos += len;
    }

    /// Read a punctuator token, returning whether there is one.
    fn read_punct(&mut self) -> bool {
        let offset = self.pos;
        let rest = &self.source.content[offset..];

        const PUNCTUATORS: &[&str] = &[
            "<<=", ">>=", "...", "==", "!=", "<=", ">=", "<<", ">>", "->", "+=", "-=", "*=", "/=",
            "%=", "&=", "|=", "^=", "++", "--", "&&", "||", "##",
        ];

        let len = if let Some(punct) = PUNCTUATORS.iter().find(|&pfx| rest.starts_with(pfx)) {
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

        if len == 0 {
            return false;
        }

        self.push(PreTokenKind::Punct(rest[..len].into()), offset, len);
        self.pos += len;
        true
    }
}

/// Helper for interpreting preprocessor tokens.
pub struct PreTokenResolver<'a>(&'a SourceMap);

impl<'a> PreTokenResolver<'a> {
    /// Create a new preprocess token resolver.
    pub fn new(source_map: &'a SourceMap) -> Self {
        Self(source_map)
    }

    /// Return the spelling of a preprocessor token.
    pub fn spelling<'t>(&'t self, token: &'t PreToken) -> Cow<'t, str> {
        if let Some(spelling) = &token.synthetic {
            spelling.as_str().into()
        } else {
            self.0.text(token.span).into()
        }
    }

    /// Lower a preprocessor token into a regular token.
    ///
    /// If `for_pp_expr` is true, preprocessor expression rules will be applied.
    /// In particular,
    ///
    /// - All identifiers are treated as 0;
    /// - String literals are rejected;
    /// - Floating-point literals are rejected.
    pub fn lower(&self, token: PreToken, for_pp_expr: bool) -> Result<Token> {
        let kind = match token.kind {
            PreTokenKind::Ident(_) if for_pp_expr => TokenKind::Num(0, Type::INT),
            PreTokenKind::Ident(ident) if let Ok(keyword) = Keyword::try_from(ident.as_str()) => {
                TokenKind::Keyword(keyword)
            },
            PreTokenKind::Ident(ident) => TokenKind::Ident(ident),
            PreTokenKind::Punct(punct) => TokenKind::Punct(punct),
            PreTokenKind::NumLit => self.lower_numeric_literal(&token, for_pp_expr)?,
            PreTokenKind::StrLit if for_pp_expr => {
                return Err(self.0.error(
                    token.span,
                    "string literal is not valid in preprocessor expressions",
                ));
            },
            PreTokenKind::StrLit => self.lower_string_char_literal(&token, false)?,
            PreTokenKind::CharLit => self.lower_string_char_literal(&token, true)?,
            PreTokenKind::Eof => TokenKind::Eof,
        };

        Ok(Token {
            kind,
            span: token.span,
        })
    }

    /// Lower a numeric literal.
    ///
    /// This will output either an integer or floating-point number token, if
    /// valid. The type will be determined by the suffix or otherwise inferred
    /// from the value and radix according to the C standard rules.
    ///
    /// If `for_pp_expr` is true, floating-point literals will be rejected as
    /// they are not allowed in C preprocessor expressions.
    fn lower_numeric_literal(&self, token: &PreToken, for_pp_expr: bool) -> Result<TokenKind> {
        let spelling = self.spelling(token);
        let bytes = spelling.as_bytes();

        let starts_hex = bytes.starts_with(b"0x") || bytes.starts_with(b"0X");
        let starts_binary = bytes.starts_with(b"0b") || bytes.starts_with(b"0B");

        let is_hex_flonum =
            starts_hex && bytes[2..].iter().any(|b| matches!(b, b'.' | b'p' | b'P'));

        let is_dec_flonum = !starts_hex
            && !starts_binary
            && (bytes.first() == Some(&b'.')
                || bytes.iter().any(|b| matches!(b, b'.' | b'e' | b'E')));

        // Floating-point literal
        if is_hex_flonum || is_dec_flonum {
            if for_pp_expr {
                return Err(self.0.error(
                    token.span,
                    "floating-point literal is not valid in preprocessor expressions",
                ));
            }

            let (suffix_len, ty) = match bytes.last() {
                Some(b'f' | b'F') => (1, Type::FLOAT),
                Some(b'l' | b'L') => (1, Type::DOUBLE),
                _ => (0, Type::DOUBLE),
            };
            let body = &spelling[..spelling.len() - suffix_len];
            let val = if is_hex_flonum {
                hexf_parse::parse_hexf64(body, false).ok()
            } else {
                body.parse::<f64>().ok()
            };
            let val =
                val.ok_or_else(|| self.0.error(token.span, "invalid floating-point literal"))?;
            return Ok(TokenKind::Flonum(val, ty));
        }

        // Integer literal
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

        let body = &spelling[start..bytes.len() - suffix_len];
        let val = u64::from_str_radix(body, radix)
            .map_err(|_| self.0.error(token.span, "invalid integer literal"))?;

        let ty = if radix == 10 {
            if l && u {
                Type::ULONG
            } else if l {
                Type::LONG
            } else if u {
                if val > u32::MAX as _ {
                    Type::ULONG
                } else {
                    Type::UINT
                }
            } else if val > i32::MAX as _ {
                Type::LONG
            } else {
                Type::INT
            }
        } else if l && u {
            Type::ULONG
        } else if l {
            if val > i64::MAX as _ {
                Type::ULONG
            } else {
                Type::LONG
            }
        } else if u {
            if val > u32::MAX as _ {
                Type::ULONG
            } else {
                Type::UINT
            }
        } else if val > i64::MAX as _ {
            Type::ULONG
        } else if val > u32::MAX as _ {
            Type::LONG
        } else if val > i32::MAX as _ {
            Type::UINT
        } else {
            Type::INT
        };

        Ok(TokenKind::Num(val, ty))
    }

    /// Lower a string or character literal.
    fn lower_string_char_literal(&self, token: &PreToken, is_char: bool) -> Result<TokenKind> {
        let spelling = self.spelling(token);
        let bytes = spelling.as_bytes();

        if is_char && !matches!(bytes, [b'\'', .., b'\'']) {
            return Err(self.0.error(token.span, "invalid char literal"));
        }
        if !is_char && !matches!(bytes, [b'"', .., b'"']) {
            return Err(self.0.error(token.span, "invalid string literal"));
        }

        let mut i = 1; // Skip opening quote
        let len = bytes.len() - 1; // Exclude closing quote
        let mut content = Vec::new();

        while i < len {
            match bytes[i] {
                b'\n' | b'\0' => {
                    return Err(self
                        .0
                        .error(token.span_at(i, 1), "invalid character in literal"));
                },
                b'\\' => {
                    if i + 1 >= len {
                        return Err(self.0.error(token.span_at(i, 1), "invalid escape sequence"));
                    }
                    i += 1;
                    let (escaped, len) = self.decode_escape_seq(token, bytes, i, len)?;
                    content.push(escaped);
                    i += len;
                },
                byte => {
                    content.push(byte);
                    i += 1;
                },
            }
        }

        if is_char {
            let [ch] = content.as_slice() else {
                return Err(self.0.error(token.span, "multi-character char constant"));
            };
            // Interpret one-byte character constant using signed-char semantics,
            // e.g., '\x80' becomes -128 (wrapped around)
            return Ok(TokenKind::Num(*ch as i8 as _, Type::INT));
        }

        content.push(b'\0');
        Ok(TokenKind::Str(content.into()))
    }

    /// Decode an escape sequence in a string or character literal.
    ///
    /// The `bytes` must correspond to the spelling of the given token. This
    /// will decode `bytes[start..end]` and return the decoded byte and the
    /// number of bytes consumed.
    fn decode_escape_seq(
        &self,
        token: &PreToken,
        bytes: &[u8],
        start: usize,
        end: usize,
    ) -> Result<(u8, usize)> {
        let first = bytes[start];

        // Octal escape sequence (up to three octal digits)
        if (first as char).is_digit(8) {
            let mut octal_value = first - b'0';
            let mut len = 1;
            if let Some(seq) = bytes.get(start + 1..end.min(start + 3)) {
                for &byte in seq {
                    if (byte as char).is_digit(8) {
                        octal_value = octal_value.wrapping_shl(3).wrapping_add(byte - b'0');
                        len += 1;
                    } else {
                        break;
                    }
                }
            }
            return Ok((octal_value, len));
        }

        // Hexadecimal escape sequence
        if first == b'x' {
            let mut pos = start + 1;
            if pos >= end || !bytes[pos].is_ascii_hexdigit() {
                return Err(self
                    .0
                    .error(token.span_at(pos, 1), "invalid hex escape sequence"));
            }

            let mut hex_value = 0u8;
            let mut has_warned_overflow = false;

            while pos < end && bytes[pos].is_ascii_hexdigit() {
                let digit = (bytes[pos] as char).to_digit(16).unwrap() as u8;
                if !has_warned_overflow {
                    if let Some(next) = hex_value.checked_mul(16).and_then(|v| v.checked_add(digit))
                    {
                        hex_value = next;
                    } else {
                        has_warned_overflow = true;
                        self.0
                            .warn(token.span_at(pos, 1), "hex escape sequence out of range");
                        hex_value = hex_value.wrapping_mul(16).wrapping_add(digit);
                    }
                } else {
                    hex_value = hex_value.wrapping_mul(16).wrapping_add(digit);
                }
                pos += 1;
            }

            return Ok((hex_value, pos - start));
        }

        // Standard single-character escapes
        let decoded = match first {
            b'a' => b'\x07',
            b'b' => b'\x08',
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => b'\x0b',
            b'f' => b'\x0c',
            b'r' => b'\r',
            b'e' => b'\x1b', // GNU C extension
            b'"' | b'\'' | b'\\' | b'?' => first,
            _ => {
                self.0
                    .warn(token.span_at(start, 1), "unknown escape sequence");
                first
            },
        };
        Ok((decoded, 1))
    }
}

/// Return whether the byte is valid at the start of an identifier.
fn is_ident_start(byte: &u8) -> bool {
    byte.is_ascii_alphabetic() || *byte == b'_'
}
