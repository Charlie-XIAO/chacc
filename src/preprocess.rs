//! A preprocessor for the C programming language.

use crate::error::Result;
use crate::source::Source;
use crate::tokenize::{Keyword, Token, TokenKind};

/// Preprocessor for a C token stream.
#[derive(Debug)]
pub struct Preprocessor<'a> {
    source: &'a Source,
    tokens: Vec<Token<'a>>,
}

impl<'a> Preprocessor<'a> {
    /// Create a new preprocessor for the given token stream.
    pub fn new(source: &'a Source, tokens: Vec<Token<'a>>) -> Self {
        Self { source, tokens }
    }

    /// Preprocess the token stream.
    pub fn preprocess(mut self) -> Result<Vec<Token<'a>>> {
        self.process_directives()?;
        self.convert_keywords();
        Ok(self.tokens)
    }

    /// Process preprocessor directives.
    ///
    /// This currently only removes null directives from the token stream.
    fn process_directives(&mut self) -> Result<()> {
        let mut read = 0;
        let mut write = 0;

        while read < self.tokens.len() {
            if self.tokens[read].at_bol && self.tokens[read].is_punct("#") {
                read += 1;
                if read == self.tokens.len()
                    || self.tokens[read].at_bol
                    || self.tokens[read].is_eof()
                {
                    continue;
                }
                return Err(self
                    .source
                    .error_at(self.tokens[read].offset, "invalid preprocessor directive"));
            }

            if write != read {
                self.tokens.swap(write, read);
            }
            write += 1;
            read += 1;
        }

        self.tokens.truncate(write);
        Ok(())
    }

    /// Convert identifiers that are keywords into keyword tokens.
    fn convert_keywords(&mut self) {
        for token in &mut self.tokens {
            let Some(ident) = token.as_ident() else {
                continue;
            };
            let Ok(keyword) = Keyword::try_from(ident) else {
                continue;
            };
            token.kind = TokenKind::Keyword(keyword);
        }
    }
}
