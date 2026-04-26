//! A preprocessor for the C programming language.

use std::io::Write;
use std::iter::Peekable;
use std::path::Path;
use std::vec::IntoIter;

use smol_str::format_smolstr;

use crate::error::Result;
use crate::parse::Parser;
use crate::source::Source;
use crate::tokenize::{Keyword, Token, TokenKind, Tokenizer};
use crate::types::Type;

/// Preprocessor for a C token stream.
#[derive(Debug)]
pub struct Preprocessor<'a, S: PreprocessorSink> {
    source: &'a Source,
    input: Peekable<IntoIter<Token>>,
    sink: &'a mut S,
    /// Offsets of the stack of currently active "#if" inclusions.
    if_stack: Vec<usize>,
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
            if_stack: Vec::new(),
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

    /// Skip extra tokens in the current logical line, if any.
    ///
    /// Returns the offset of the first skipped token, if any. Returns `None` if
    /// no tokens were skipped.
    fn skip_line(&mut self) -> Option<usize> {
        let token = self.next_if_mol()?;
        while self.next_if_mol().is_some() {}
        Some(token.offset)
    }

    /// Skip tokens until the matching "#endif".
    fn skip_until_endif(&mut self) -> Result<()> {
        let mut depth = 0;

        while let Some(token) = self.input.next() {
            if token.is_eof() {
                return Ok(());
            }

            if !(token.at_bol && token.is_punct("#")) {
                continue;
            }

            let Some(directive) = self.next_if_mol() else {
                continue;
            };

            if directive.is_ident("if") {
                depth += 1;
                continue;
            }

            if directive.is_ident("endif") {
                if depth == 0 {
                    self.process_endif(token.offset)?;
                    return Ok(());
                }
                depth -= 1;
                self.skip_line();
                continue;
            }

            self.skip_line();
        }

        Ok(())
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

            if !(token.at_bol && token.is_punct("#")) {
                self.emit(token)?;
                continue;
            }

            let Some(directive) = self.next_if_mol() else {
                continue;
            };

            if directive.is_ident("include") {
                self.process_include(token.offset)?;
                continue;
            }

            if directive.is_ident("if") {
                self.if_stack.push(token.offset);
                if !self.process_if(token.offset)? {
                    self.skip_until_endif()?;
                }
                continue;
            }

            if directive.is_ident("endif") {
                self.process_endif(token.offset)?;
                continue;
            }

            return Err(self
                .source
                .error_at(token.offset, "invalid preprocessor directive"));
        }

        if let Some(offset) = self.if_stack.last().copied() {
            return Err(self.source.error_at(offset, "unterminated #if"));
        }
        Ok(())
    }

    /// Process an "#if" directive.
    fn process_if(&mut self, offset: usize) -> Result<bool> {
        let mut tokens = Vec::new();

        while let Some(mut token) = self.next_if_mol() {
            if token.as_ident().is_some() {
                // All identifiers are undefined in preprocessor expressions,
                // and they are simply evaluated as 0
                token.kind = TokenKind::Num(0, Type::INT);
            }
            tokens.push(token);
        }

        let Some(token) = tokens.last() else {
            return Err(self
                .source
                .error_at(offset, "bare #if without an expression"));
        };

        tokens.push(Token {
            kind: TokenKind::Eof,
            len: 0,
            offset: token.offset + token.len,
            at_bol: false,
            follows_space: false,
        });

        Ok(Parser::new(self.source, tokens, true)
            .parse_constexpr()?
            .into())
    }

    /// Process an "#endif" directive.
    fn process_endif(&mut self, offset: usize) -> Result<()> {
        if self.if_stack.pop().is_none() {
            return Err(self.source.error_at(offset, "#endif without #if"));
        }
        if let Some(offset) = self.skip_line() {
            self.source.warn_at(offset, "extra tokens after #endif");
        }
        Ok(())
    }

    /// Process an "#include" directive.
    fn process_include(&mut self, offset: usize) -> Result<()> {
        let Some(token) = self.next_if_mol() else {
            return Err(self
                .source
                .error_at(offset, "bare #include without a filename"));
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
            self.source.warn_at(offset, "extra tokens after #include");
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
        convert_keywords(&mut self.0);
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

fn convert_keywords(tokens: &mut [Token]) {
    for token in tokens {
        let Some(ident) = token.as_ident() else {
            continue;
        };
        let Ok(keyword) = Keyword::try_from(ident.as_str()) else {
            continue;
        };
        token.kind = TokenKind::Keyword(keyword);
    }
}
