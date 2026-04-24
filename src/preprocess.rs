//! A preprocessor for the C programming language.

use crate::error::Result;
use crate::tokenize::{Keyword, Token, TokenKind};

/// Preprocessor for a C token stream.
#[derive(Debug)]
pub struct Preprocessor<'a> {
    tokens: Vec<Token<'a>>,
}

impl<'a> Preprocessor<'a> {
    /// Create a new preprocessor for the given token stream.
    pub fn new(tokens: Vec<Token<'a>>) -> Self {
        Self { tokens }
    }

    /// Preprocess the token stream.
    pub fn preprocess(mut self) -> Result<Vec<Token<'a>>> {
        for token in &mut self.tokens {
            let Some(ident) = token.as_ident() else {
                continue;
            };
            let Ok(keyword) = Keyword::try_from(ident) else {
                continue;
            };
            token.kind = TokenKind::Keyword(keyword);
        }

        Ok(self.tokens)
    }
}
