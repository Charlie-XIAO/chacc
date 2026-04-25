//! A preprocessor for the C programming language.

use std::iter::Peekable;
use std::path::Path;
use std::vec::IntoIter;

use smol_str::format_smolstr;

use crate::error::Result;
use crate::source::Source;
use crate::tokenize::{Keyword, Token, TokenKind, Tokenizer};

/// Preprocessor for a C token stream.
#[derive(Debug)]
pub struct Preprocessor<'a> {
    source: &'a Source,
    input: Peekable<IntoIter<Token>>,
    output: Vec<Token>,
}

impl<'a> Preprocessor<'a> {
    /// Create a new preprocessor for the given token stream.
    pub fn new(source: &'a Source, tokens: Vec<Token>) -> Self {
        Self {
            source,
            input: tokens.into_iter().peekable(),
            output: Vec::new(),
        }
    }

    /// Consume the next token if it is in the middle of a logical line.
    fn next_if_mol(&mut self) -> Option<Token> {
        self.input.next_if(|token| !token.at_bol && !token.is_eof())
    }

    /// Skip extra tokens in the same logical line, if any.
    ///
    /// Returns the offset of the first skipped token, if any. Returns `None` if
    /// no tokens were skipped.
    fn skip_line(&mut self) -> Option<usize> {
        let Some(token) = self.next_if_mol() else {
            return None;
        };
        while self.next_if_mol().is_some() {}
        Some(token.offset)
    }

    /// Preprocess the token stream.
    pub fn preprocess(mut self) -> Result<Vec<Token>> {
        self.process_directives()?;
        self.convert_keywords();
        Ok(self.output)
    }

    /// Process preprocessor directives in the token stream.
    fn process_directives(&mut self) -> Result<()> {
        while let Some(token) = self.input.next() {
            if token.is_eof() {
                self.output.push(token);
                break;
            }

            // Not a preprocessor directive
            if !(token.at_bol && token.is_punct("#")) {
                self.output.push(token);
                continue;
            }

            let Some(directive) = self.next_if_mol() else {
                continue;
            };

            if directive.is_ident("include") {
                self.process_include(directive.offset)?;
                continue;
            }

            return Err(self
                .source
                .error_at(directive.offset, "invalid preprocessor directive"));
        }

        Ok(())
    }

    /// Process an "#include" directive.
    fn process_include(&mut self, offset: usize) -> Result<()> {
        let Some(token) = self.input.next() else {
            return Err(self.source.error_at(offset, "expected a filename"));
        };

        let Some(content) = token.as_str() else {
            return Err(self.source.error_at(token.offset, "expected a filename"));
        };
        let Some(content) = content.as_ref().strip_suffix(b"\0") else {
            return Err(self.source.error_at(token.offset, "expected a filename"));
        };

        let path = std::str::from_utf8(content).map_err(|e| {
            self.source
                .error_at(token.offset, format_smolstr!("invalid filename: {e}"))
        })?;

        let path = self
            .source
            .path()
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path);

        let source = Source::new(&path)?;
        let tokens = Tokenizer::new(&source).tokenize()?;
        let mut tokens = Preprocessor::new(&source, tokens).preprocess()?;
        tokens.pop_if(|token| token.is_eof());
        self.output.extend(tokens);

        if let Some(offset) = self.skip_line() {
            self.source
                .warn_at(offset, "extra tokens at end of #include directive");
        }
        Ok(())
    }

    /// Convert identifiers that are keywords into keyword tokens.
    fn convert_keywords(&mut self) {
        for token in &mut self.output {
            let Some(ident) = token.as_ident() else {
                continue;
            };
            let Ok(keyword) = Keyword::try_from(ident.as_str()) else {
                continue;
            };
            token.kind = TokenKind::Keyword(keyword);
        }
    }
}
