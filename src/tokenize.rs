//! Tokenizer and diagnostic helpers.

/// Token categories used by the parser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenKind {
    Punct,
    Num,
    Eof,
}

/// A token with its source slice and byte offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Token<'a> {
    pub(crate) kind: TokenKind,
    pub(crate) lexeme: &'a str,
    pub(crate) value: i64,
    pub(crate) offset: usize,
}

/// Convert the input string into tokens.
pub(crate) fn tokenize(input: &str) -> Result<Vec<Token<'_>>, String> {
    let mut tokens = Vec::new();
    let mut rest = input;
    let mut offset = 0;

    while !rest.is_empty() {
        let ch = rest.as_bytes()[0];

        if ch.is_ascii_whitespace() {
            rest = &rest[1..];
            offset += 1;
            continue;
        }

        let punct_len = read_punct(rest);
        if punct_len != 0 {
            tokens.push(Token {
                kind: TokenKind::Punct,
                lexeme: &rest[..punct_len],
                value: 0,
                offset,
            });
            rest = &rest[punct_len..];
            offset += punct_len;
            continue;
        }

        if ch.is_ascii_digit() {
            let len = rest.bytes().take_while(u8::is_ascii_digit).count();
            let lexeme = &rest[..len];
            tokens.push(Token {
                kind: TokenKind::Num,
                lexeme,
                value: lexeme.parse().unwrap(),
                offset,
            });
            rest = &rest[len..];
            offset += len;
            continue;
        }

        return Err(format_error_at(input, offset, "invalid token"));
    }

    // EOF sentinel
    tokens.push(Token {
        kind: TokenKind::Eof,
        lexeme: "",
        value: 0,
        offset,
    });
    Ok(tokens)
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
pub(crate) fn format_error_at(input: &str, offset: usize, message: &str) -> String {
    format!("{input}\n{}^ {message}", " ".repeat(offset))
}
