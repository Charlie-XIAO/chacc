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

/// A decoded escape sequence.
#[derive(Debug, Clone, Copy)]
enum EscapeSeq {
    Byte(u8),
    Num(u32),
    Codepoint(char),
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
    /// The origin span, if this token needs to be expanded elsewhere.
    pub origin: Option<SourceSpan>,
}

impl PreToken {
    /// Create a synthetic preprocessor token.
    pub fn synthetic(
        kind: PreTokenKind,
        span: SourceSpan,
        at_bol: bool,
        follows_space: bool,
        spelling: impl Into<SmolStr>,
    ) -> Self {
        Self {
            kind,
            span,
            at_bol,
            follows_space,
            hideset: None,
            synthetic: Some(spelling.into()),
            origin: None,
        }
    }

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
    /// A string literal.
    ///
    /// The first element is the string content, with null terminator preserved.
    /// The second element is the base element type of the literal.
    Str(Rc<[u32]>, Type),
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
    pub fn as_str(&self) -> Option<(Rc<[u32]>, Type)> {
        match self.kind {
            TokenKind::Str(ref content, base_ty) => Some((content.clone(), base_ty)),
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
            origin: None,
        });
        self.at_bol = false;
        self.follows_space = false;
    }

    /// Tokenize the entire source into a flat token list.
    ///
    /// If `allow_comment` is false, comments will be treated as invalid tokens
    /// instead of being skipped. If `emit_eof` is false, no EOF sentinel will
    /// be emitted at the end of the token stream.
    pub fn tokenize(mut self, allow_comment: bool, emit_eof: bool) -> Result<Vec<PreToken>> {
        let content = &self.source.content;
        let bytes = content.as_bytes();

        while self.pos < content.len() {
            if allow_comment && self.read_comment()? {
                self.follows_space = true;
                continue;
            }

            match &bytes[self.pos..] {
                [b'\n', ..] => {
                    self.pos += 1;
                    self.at_bol = true;
                    self.follows_space = false;
                },
                [b, ..] if b.is_ascii_whitespace() => {
                    self.pos += 1;
                    self.follows_space = true;
                },
                [b, ..] if b.is_ascii_digit() => self.read_numeric_literal(),
                [b'.', next, ..] if next.is_ascii_digit() => self.read_numeric_literal(),
                [b'"', ..] => self.read_string_char_literal(false, 0)?,
                [b'u' | b'U' | b'L', b'"', ..] => self.read_string_char_literal(false, 1)?,
                [b'u', b'8', b'"', ..] => self.read_string_char_literal(false, 2)?,
                [b'\'', ..] => self.read_string_char_literal(true, 0)?,
                [b'u' | b'U' | b'L', b'\'', ..] => self.read_string_char_literal(true, 1)?,
                _ if self.read_ident() => {},
                _ if self.read_punct() => {},
                _ => return Err(self.source.error(self.pos, 1, "invalid token")),
            }
        }

        if emit_eof {
            self.push(PreTokenKind::Eof, self.pos, 0);
        }
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
    fn read_string_char_literal(&mut self, is_char: bool, prefix_len: usize) -> Result<()> {
        let bytes = self.source.content.as_bytes();
        let mut i = self.pos + prefix_len + 1; // Skip prefix and opening quote

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
    fn read_ident(&mut self) -> bool {
        let offset = self.pos;
        let content = &self.source.content;
        let rest = &content[offset..];

        /// Whether the codepoint can start an identifier.
        fn is_ident_start(code: u32) -> bool {
            matches!(
                code,
                0x5F // _
                    | 0x61..=0x7A // a-z
                    | 0x41..=0x5A // A-Z
                    | 0x00A8 // https://www.sigbus.info/n1570#D.1
                    | 0x00AA
                    | 0x00AD
                    | 0x00AF
                    | 0x00B2..=0x00B5
                    | 0x00B7..=0x00BA
                    | 0x00BC..=0x00BE
                    | 0x00C0..=0x00D6
                    | 0x00D8..=0x00F6
                    | 0x00F8..=0x00FF
                    | 0x0100..=0x02FF
                    | 0x0370..=0x167F
                    | 0x1681..=0x180D
                    | 0x180F..=0x1DBF
                    | 0x1E00..=0x1FFF
                    | 0x200B..=0x200D
                    | 0x202A..=0x202E
                    | 0x203F..=0x2040
                    | 0x2054
                    | 0x2060..=0x206F
                    | 0x2070..=0x20CF
                    | 0x2100..=0x218F
                    | 0x2460..=0x24FF
                    | 0x2776..=0x2793
                    | 0x2C00..=0x2DFF
                    | 0x2E80..=0x2FFF
                    | 0x3004..=0x3007
                    | 0x3021..=0x302F
                    | 0x3031..=0x303F
                    | 0x3040..=0xD7FF
                    | 0xF900..=0xFD3D
                    | 0xFD40..=0xFDCF
                    | 0xFDF0..=0xFE1F
                    | 0xFE30..=0xFE44
                    | 0xFE47..=0xFFFD
                    | 0x10000..=0x1FFFD
                    | 0x20000..=0x2FFFD
                    | 0x30000..=0x3FFFD
                    | 0x40000..=0x4FFFD
                    | 0x50000..=0x5FFFD
                    | 0x60000..=0x6FFFD
                    | 0x70000..=0x7FFFD
                    | 0x80000..=0x8FFFD
                    | 0x90000..=0x9FFFD
                    | 0xA0000..=0xAFFFD
                    | 0xB0000..=0xBFFFD
                    | 0xC0000..=0xCFFFD
                    | 0xD0000..=0xDFFFD
                    | 0xE0000..=0xEFFFD
            )
        }

        /// Return whether the codepoint can continue an identifier after start.
        fn is_ident_cont(code: u32) -> bool {
            is_ident_start(code)
                || matches!(
                    code,
                    0x30..=0x39 // 0-9
                        | 0x0300..=0x036F // https://www.sigbus.info/n1570#D.2
                        | 0x1DC0..=0x1DFF
                        | 0x20D0..=0x20FF
                        | 0xFE20..=0xFE2F
                )
        }

        let Some(first) = rest.chars().next() else {
            return false;
        };

        if !is_ident_start(first as u32) {
            return false;
        }

        let len = rest
            .char_indices()
            .skip(1)
            .find(|(_, ch)| !is_ident_cont(*ch as u32))
            .map_or(rest.len(), |(idx, _)| idx);

        let lexeme = &content[offset..offset + len];
        self.push(PreTokenKind::Ident(lexeme.into()), offset, len);
        self.pos += len;
        true
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
            PreTokenKind::StrLit => self.lower_string_literal(&token)?,
            PreTokenKind::CharLit => self.lower_char_literal(&token)?,
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

    /// Lower a string literal.
    fn lower_string_literal(&self, token: &PreToken) -> Result<TokenKind> {
        let spelling = self.spelling(token);
        let bytes = spelling.as_bytes();

        enum StrLitKind {
            Utf8,
            Utf16,
            Utf32,
            Wide,
        }

        let (kind, mut i) = match bytes {
            [b'"', .., b'"'] => (StrLitKind::Utf8, 1),
            [b'u', b'8', b'"', .., b'"'] => (StrLitKind::Utf8, 3),
            [b'u', b'"', .., b'"'] => (StrLitKind::Utf16, 2),
            [b'U', b'"', .., b'"'] => (StrLitKind::Utf32, 2),
            [b'L', b'"', .., b'"'] => (StrLitKind::Wide, 2),
            _ => return Err(self.0.error(token.span, "invalid string literal")),
        };

        let len = bytes.len() - 1; // Exclude closing quote
        let mut content = Vec::new();

        fn push_utf8(content: &mut Vec<u32>, ch: char) {
            let mut buf = [0; 4];
            let buf = ch.encode_utf8(&mut buf).bytes();
            content.extend(buf.map(|b| b as u32));
        }

        fn push_utf16(content: &mut Vec<u32>, ch: char) {
            let mut buf = [0; 2];
            let buf = ch.encode_utf16(&mut buf);
            content.extend(buf.iter().map(|&b| b as u32));
        }

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
                    let (escape, len) = self.decode_escape_seq(
                        token,
                        bytes,
                        i,
                        len,
                        matches!(kind, StrLitKind::Utf8),
                    )?;
                    match escape {
                        EscapeSeq::Byte(byte) => content.push(byte as _),
                        EscapeSeq::Num(val) => match kind {
                            StrLitKind::Utf8 => content.push(val as u8 as _),
                            StrLitKind::Utf16 => content.push(val as u16 as _),
                            StrLitKind::Utf32 | StrLitKind::Wide => content.push(val),
                        },
                        EscapeSeq::Codepoint(ch) => match kind {
                            StrLitKind::Utf8 => push_utf8(&mut content, ch),
                            StrLitKind::Utf16 => push_utf16(&mut content, ch),
                            StrLitKind::Utf32 | StrLitKind::Wide => content.push(ch as _),
                        },
                    }
                    i += len;
                },
                byte if matches!(kind, StrLitKind::Utf8) => {
                    content.push(byte as _);
                    i += 1;
                },
                _ => {
                    let ch = spelling[i..len]
                        .chars()
                        .next()
                        .expect("non-empty string literal tail");
                    match kind {
                        StrLitKind::Utf8 => unreachable!(),
                        StrLitKind::Utf16 => push_utf16(&mut content, ch),
                        StrLitKind::Utf32 | StrLitKind::Wide => content.push(ch as _),
                    }
                    i += ch.len_utf8();
                },
            }
        }

        content.push(0);

        Ok(TokenKind::Str(
            content.into(),
            match kind {
                StrLitKind::Utf8 => Type::CHAR,
                StrLitKind::Utf16 => Type::USHORT,
                StrLitKind::Utf32 => Type::UINT,
                StrLitKind::Wide => Type::INT,
            },
        ))
    }

    /// Lower a character literal.
    fn lower_char_literal(&self, token: &PreToken) -> Result<TokenKind> {
        let spelling = self.spelling(token);
        let bytes = spelling.as_bytes();

        enum CharLitKind {
            Normal,
            Utf16,
            Utf32,
            Wide,
        }

        let (kind, start) = match bytes {
            [b'\'', .., b'\''] => (CharLitKind::Normal, 1),
            [b'u', b'\'', .., b'\''] => (CharLitKind::Utf16, 2),
            [b'U', b'\'', .., b'\''] => (CharLitKind::Utf32, 2),
            [b'L', b'\'', .., b'\''] => (CharLitKind::Wide, 2),
            _ => return Err(self.0.error(token.span, "invalid char literal")),
        };

        let end = bytes.len() - 1; // Exclude closing quote
        if start >= end {
            return Err(self.0.error(token.span, "empty char constant"));
        }

        let val = if bytes[start] == b'\\' {
            let (escape, len) = self.decode_escape_seq(
                token,
                bytes,
                start + 1,
                end,
                matches!(kind, CharLitKind::Normal),
            )?;
            if start + 1 + len < end {
                return Err(self.0.error(token.span, "multi-character char constant"));
            }
            match escape {
                EscapeSeq::Byte(byte) => byte as _,
                EscapeSeq::Num(val) => val,
                EscapeSeq::Codepoint(ch) => ch.into(),
            }
        } else {
            if matches!(kind, CharLitKind::Normal) && start + 1 != end {
                return Err(self.0.error(token.span, "multi-character char constant"));
            }
            let mut chars = spelling[start..end].chars();
            let Some(ch) = chars.next() else {
                return Err(self.0.error(token.span, "empty char constant"));
            };
            if chars.next().is_some() {
                return Err(self.0.error(token.span, "multi-character char constant"));
            }
            ch as u32
        };

        let (val, ty) = match kind {
            // Normal one-byte character constants are interpreted using signed-
            // char semantics, e.g., '\x80' becomes -128 (wrapped around)
            CharLitKind::Normal => (val as u8 as i8 as u64, Type::INT),
            CharLitKind::Utf16 => ((val & 0xffff) as u64, Type::USHORT),
            CharLitKind::Utf32 => (val as u64, Type::UINT),
            CharLitKind::Wide => (val as u64, Type::INT),
        };
        Ok(TokenKind::Num(val, ty))
    }

    /// Decode an escape sequence in a string or character literal.
    ///
    /// The `bytes` must correspond to the spelling of the given token. This
    /// will decode `bytes[start..end]` and return the escape sequence together
    /// with the number of bytes consumed from `bytes`.
    ///
    /// If `check_byte_range` is true, this emits a warning if the octal or hex
    /// escape sequence goes out of the one-byte range.
    fn decode_escape_seq(
        &self,
        token: &PreToken,
        bytes: &[u8],
        start: usize,
        end: usize,
        check_byte_range: bool,
    ) -> Result<(EscapeSeq, usize)> {
        let first = bytes[start];

        // Octal escape sequence (up to three octal digits)
        if (first as char).is_digit(8) {
            let mut octal_value = (first - b'0') as u32;
            let mut len = 1;

            if let Some(seq) = bytes.get(start + 1..end.min(start + 3)) {
                for &byte in seq {
                    if (byte as char).is_digit(8) {
                        octal_value = (octal_value << 3) + (byte - b'0') as u32;
                        len += 1;
                    } else {
                        break;
                    }
                }
            }

            if check_byte_range && octal_value > u8::MAX as _ {
                self.0.warn(
                    token.span_at(start, len),
                    "octal escape sequence out of range",
                );
            }
            return Ok((EscapeSeq::Num(octal_value), len));
        }

        // Hexadecimal escape sequence
        if first == b'x' {
            let mut pos = start + 1;
            if pos >= end || !bytes[pos].is_ascii_hexdigit() {
                return Err(self
                    .0
                    .error(token.span_at(pos, 1), "invalid hex escape sequence"));
            }

            let mut hex_value = 0;
            let mut overflowed = false;
            while pos < end
                && let Some(digit) = (bytes[pos] as char).to_digit(16)
            {
                overflowed |= hex_value > (u8::MAX as u32).saturating_sub(digit) / 16;
                hex_value = hex_value.wrapping_mul(16).wrapping_add(digit);
                pos += 1;
            }

            if check_byte_range && overflowed {
                self.0.warn(
                    token.span_at(start, pos - start),
                    "hex escape sequence out of range",
                );
            }
            return Ok((EscapeSeq::Num(hex_value), pos - start));
        }

        // Universal character name
        if first == b'u' || first == b'U' {
            let hex_len = if first == b'u' { 4 } else { 8 };
            let mut code = 0;
            let mut bytes_processed = 0;

            for &byte in bytes.iter().skip(start + 1).take(hex_len) {
                let Some(digit) = (byte as char).to_digit(16) else {
                    break; // Not a hex digit
                };
                code = (code << 4) | digit;
                bytes_processed += 1;
            }

            if bytes_processed != hex_len {
                return Err(self.0.error(
                    token.span_at(start, bytes_processed + 1),
                    "incomplete universal character name",
                ));
            }

            let Some(ch) = char::from_u32(code) else {
                return Err(self.0.error(
                    token.span_at(start, hex_len + 1),
                    "invalid universal character name",
                ));
            };
            return Ok((EscapeSeq::Codepoint(ch), hex_len + 1));
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
        Ok((EscapeSeq::Byte(decoded), 1))
    }
}
