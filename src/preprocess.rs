//! A preprocessor for the C programming language.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;

use rustc_hash::FxHashMap;
use smol_str::{SmolStr, format_smolstr};

use crate::constexpr::ConstValue;
use crate::error::Result;
use crate::parse::Parser;
use crate::source::Source;
use crate::tokenize::{Keyword, Token, TokenKind, Tokenizer};
use crate::types::Type;

/// The context of a conditional compilation block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CondFrameContext {
    Then,
    Elif,
    Else,
}

/// A frame for conditional compilation.
#[derive(Debug)]
struct CondFrame {
    offset: usize,
    included: bool,
    ctx: CondFrameContext,
}

/// Preprocessor for a C token stream.
#[derive(Debug)]
pub struct Preprocessor<'a, S: PreprocessorSink> {
    source: &'a Source,
    input: VecDeque<Token>,
    sink: &'a mut S,
    conds: Vec<CondFrame>,
    macros: FxHashMap<SmolStr, Vec<Token>>,
}

impl<'a, S> Preprocessor<'a, S>
where
    S: PreprocessorSink,
{
    /// Create a new preprocessor for the given token stream.
    pub fn new(source: &'a Source, tokens: Vec<Token>, sink: &'a mut S) -> Self {
        Self {
            source,
            input: tokens.into(),
            sink,
            conds: Vec::new(),
            macros: Default::default(),
        }
    }

    /// Consume the next token if it is in the middle of a logical line.
    fn next_if_mol(&mut self) -> Option<Token> {
        self.input
            .pop_front_if(|token| !token.at_bol && !token.is_eof())
    }

    /// Consume the rest of the current logical line.
    fn line(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        while let Some(token) = self.next_if_mol() {
            tokens.push(token);
        }
        tokens
    }

    /// Define a macro.
    fn define_macro(&mut self, name: SmolStr, body: Vec<Token>, offset: usize) -> Result<()> {
        use std::collections::hash_map::Entry;

        match self.macros.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(body);
            },
            Entry::Occupied(mut entry) => {
                let mut same = true;

                let old_body = entry.get();
                if old_body.len() == body.len() {
                    for (old, new) in old_body.iter().zip(&body) {
                        if self.source.slice(old.offset, old.len)?
                            != self.source.slice(new.offset, new.len)?
                        {
                            same = false;
                            break;
                        }
                    }
                } else {
                    same = false;
                }

                if !same {
                    self.source.warn_at(offset, "redefinition of macro");
                    entry.insert(body);
                }
            },
        }

        Ok(())
    }

    /// Expand a macro at the current position.
    ///
    /// Returns whether the given token indeed corresponds to a defined macro.
    /// If not, this returns `false` and does nothing.
    fn expand_macro(&mut self, token: &Token) -> bool {
        let Some(name) = token.as_ident() else {
            return false;
        };
        let Some(body) = self.macros.get(&name).cloned() else {
            return false;
        };

        // TODO: Replace with VecDeque::prepend once it is stable
        self.input.reserve(body.len());
        for token in body.into_iter().rev() {
            self.input.push_front(token);
        }
        true
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

    /// Skip tokens until the next matching "#else", "#elif", or "#endif".
    fn skip_cond(&mut self) -> Result<()> {
        let mut depth = 0;

        while let Some(token) = self.input.pop_front() {
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

            if directive.is_ident("elif") {
                if depth == 0 {
                    self.process_elif(token.offset)?;
                    return Ok(());
                }
                self.skip_line();
                continue;
            }

            if directive.is_ident("else") {
                if depth == 0 {
                    self.process_else(token.offset)?;
                    return Ok(());
                }
                self.skip_line();
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
        while let Some(token) = self.input.pop_front() {
            if token.is_eof() {
                if emit_eof {
                    self.sink.emit(self.source, token)?;
                }
                break;
            }

            if self.expand_macro(&token) {
                continue;
            }

            if !(token.at_bol && token.is_punct("#")) {
                self.sink.emit(self.source, token)?;
                continue;
            }

            let Some(directive) = self.next_if_mol() else {
                continue;
            };

            if directive.is_ident("include") {
                self.process_include(token.offset)?;
                continue;
            }

            if directive.is_ident("define") {
                self.process_define(token.offset)?;
                continue;
            }

            if directive.is_ident("if") {
                self.process_if(token.offset)?;
                continue;
            }

            if directive.is_ident("elif") {
                self.process_elif(token.offset)?;
                continue;
            }

            if directive.is_ident("else") {
                self.process_else(token.offset)?;
                continue;
            }

            if directive.is_ident("endif") {
                self.process_endif(token.offset)?;
                continue;
            }

            return Err(self
                .source
                .error_at(directive.offset, "invalid preprocessor directive"));
        }

        if let Some(frame) = self.conds.last() {
            return Err(self.source.error_at(frame.offset, "unterminated #if"));
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

        if let Some(offset) = self.skip_line() {
            self.source.warn_at(offset, "extra tokens after #include");
        }

        // TODO: Make parent macros visible inside includes
        let source = Source::new(&path)?;
        let tokens = Tokenizer::new(&source).tokenize()?;
        Preprocessor::new(&source, tokens, self.sink).preprocess(false)?;

        Ok(())
    }

    /// Process a "#define" directive.
    fn process_define(&mut self, offset: usize) -> Result<()> {
        let Some(token) = self.next_if_mol() else {
            return Err(self
                .source
                .error_at(offset, "no macro name given in #define"));
        };

        let Some(name) = token.as_ident() else {
            return Err(self
                .source
                .error_at(token.offset, "macro names must be identifiers"));
        };

        let tokens = self.line();
        self.define_macro(name, tokens, token.offset)?;
        Ok(())
    }

    /// Process a preprocessor expression and return its value.
    ///
    /// If there is no tokens remaining in the current logical line, this
    /// returns `Ok(None)`.
    fn process_constexpr(&mut self) -> Result<Option<ConstValue>> {
        let mut tokens = self.line();
        if tokens.is_empty() {
            return Ok(None);
        }

        for token in &mut tokens {
            if token.as_ident().is_some() {
                // All identifiers are undefined in preprocessor expressions,
                // and they are simply evaluated as 0
                token.kind = TokenKind::Num(0, Type::INT);
            }
        }

        let last = tokens.last().unwrap();
        tokens.push(Token {
            kind: TokenKind::Eof,
            len: 0,
            offset: last.offset + last.len,
            at_bol: false,
            follows_space: false,
        });

        let val = Parser::new(self.source, tokens, true).parse_constexpr()?;
        Ok(Some(val))
    }

    /// Process an "#if" directive.
    fn process_if(&mut self, offset: usize) -> Result<()> {
        let Some(val) = self.process_constexpr()? else {
            return Err(self
                .source
                .error_at(offset, "bare #if without an expression"));
        };

        let included = val.into();
        self.conds.push(CondFrame {
            offset,
            included,
            ctx: CondFrameContext::Then,
        });

        if !included {
            self.skip_cond()?;
        }
        Ok(())
    }

    /// Process an "#elif" directive.
    fn process_elif(&mut self, offset: usize) -> Result<()> {
        let Some(mut frame) = self.conds.pop() else {
            return Err(self.source.error_at(offset, "#elif without #if"));
        };
        if frame.ctx == CondFrameContext::Else {
            return Err(self.source.error_at(offset, "#elif after #else"));
        }

        frame.ctx = CondFrameContext::Elif;
        if frame.included {
            self.conds.push(frame);
            self.skip_cond()?;
            return Ok(());
        }

        let Some(val) = self.process_constexpr()? else {
            return Err(self
                .source
                .error_at(offset, "bare #elif without an expression"));
        };

        let included = val.into();
        frame.included = included;
        self.conds.push(frame);

        if !included {
            self.skip_cond()?;
        }
        Ok(())
    }

    /// Process an "#else" directive.
    fn process_else(&mut self, offset: usize) -> Result<()> {
        let Some(frame) = self.conds.last_mut() else {
            return Err(self.source.error_at(offset, "#else without #if"));
        };
        if frame.ctx == CondFrameContext::Else {
            return Err(self.source.error_at(offset, "#else after #else"));
        }

        let included = frame.included;
        frame.included = true;
        frame.ctx = CondFrameContext::Else;

        if let Some(offset) = self.skip_line() {
            self.source.warn_at(offset, "extra tokens after #else");
        }

        if included {
            self.skip_cond()?;
        }
        Ok(())
    }

    /// Process an "#endif" directive.
    fn process_endif(&mut self, offset: usize) -> Result<()> {
        if self.conds.pop().is_none() {
            return Err(self.source.error_at(offset, "#endif without #if"));
        }
        if let Some(offset) = self.skip_line() {
            self.source.warn_at(offset, "extra tokens after #endif");
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
