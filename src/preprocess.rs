//! A preprocessor for the C programming language.

use std::io::Write;
use std::iter::Peekable;
use std::path::Path;
use std::vec::IntoIter;

use smol_str::format_smolstr;

use crate::error::Result;
use crate::source::Source;
use crate::tokenize::{Keyword, Token, TokenKind, Tokenizer};

/// Preprocessor for a C token stream.
#[derive(Debug)]
pub struct Preprocessor<'a, S: PreprocessorSink> {
    source: &'a Source,
    input: Peekable<IntoIter<Token>>,
    sink: &'a mut S,
}

impl<'a, S> Preprocessor<'a, S>
where
    S: PreprocessorSink,
{
    /// Create a new preprocessor for the given token stream.
    pub fn new(source: &'a Source, tokens: Vec<Token>, sink: &'a mut S) -> Self {
        Self {
            source,
            input: tokens.into_iter().peekable(),
            sink,
        }
    }

    /// Convenience method to emit a token from the original source.
    fn emit(&mut self, token: Token) -> Result<()> {
        self.sink.emit(self.source, token)
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
        let token = self.next_if_mol()?;
        while self.next_if_mol().is_some() {}
        Some(token.offset)
    }

    /// Preprocess the token stream.
    pub fn preprocess(mut self, emit_eof: bool) -> Result<()> {
        while let Some(token) = self.input.next() {
            if token.is_eof() {
                if emit_eof {
                    self.emit(token)?;
                }
                break;
            }

            // Not a preprocessor directive
            if !(token.at_bol && token.is_punct("#")) {
                self.emit(token)?;
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
        Preprocessor::new(&source, tokens, self.sink).preprocess(false)?;

        if let Some(offset) = self.skip_line() {
            self.source
                .warn_at(offset, "extra tokens at end of #include directive");
        }
        Ok(())
    }
}

/// A sink for preprocessor output tokens.
pub trait PreprocessorSink {
    /// Emit a preprocessed token.
    fn emit(&mut self, source: &Source, token: Token) -> Result<()>;
}

/// A preprocessor sink that stores the preprocessed tokens in memory.
#[derive(Default)]
pub struct PreprocessedTokens(Vec<Token>);

impl PreprocessorSink for PreprocessedTokens {
    fn emit(&mut self, _source: &Source, token: Token) -> Result<()> {
        self.0.push(token);
        Ok(())
    }
}

impl PreprocessedTokens {
    /// Finalize the preprocessor output into a token stream ready for parsing.
    pub fn into_parser_tokens(mut self) -> Vec<Token> {
        for token in &mut self.0 {
            let Some(ident) = token.as_ident() else {
                continue;
            };
            let Ok(keyword) = Keyword::try_from(ident.as_str()) else {
                continue;
            };
            token.kind = TokenKind::Keyword(keyword);
        }

        self.0
    }
}

/// A preprocessor sink that directly writes out the preprocessed tokens.
pub struct PreprocessedWriter<'a, W: Write> {
    out: &'a mut W,
    first: bool,
}

impl<'a, W: Write> PreprocessedWriter<'a, W> {
    /// Create a new preprocessor writer that writes to the given output.
    pub fn new(out: &'a mut W) -> Self {
        Self { out, first: true }
    }
}

impl<'a, W: Write> PreprocessorSink for PreprocessedWriter<'a, W> {
    fn emit(&mut self, source: &Source, token: Token) -> Result<()> {
        if !self.first && token.at_bol || token.is_eof() {
            writeln!(self.out)?;
        }
        if !token.at_bol && token.follows_space {
            write!(self.out, " ")?;
        }
        write!(self.out, "{}", source.slice(token.offset, token.len)?)?;
        self.first = false;
        Ok(())
    }
}
