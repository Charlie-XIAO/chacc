//! A preprocessor for the C programming language.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;

use rustc_hash::FxHashMap;
use smol_str::SmolStr;

use crate::constexpr::ConstValue;
use crate::error::Result;
use crate::parse::Parser;
use crate::source::{SourceMap, SourceSpan};
use crate::tokenize::{Keyword, Token, TokenKind, Tokenizer};
use crate::types::Type;
use crate::utils::VecDequeExt;

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
    span: SourceSpan,
    included: bool,
    ctx: CondFrameContext,
}

/// Preprocessor for a C token stream.
#[derive(Debug)]
pub struct Preprocessor<'a, S: PreprocessorSink> {
    source_map: &'a mut SourceMap,
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
    pub fn new(source_map: &'a mut SourceMap, tokens: Vec<Token>, sink: &'a mut S) -> Self {
        Self {
            source_map,
            input: tokens.into(),
            sink,
            conds: Vec::new(),
            macros: Default::default(),
        }
    }

    /// Emit a token to the sink, with preprocessor-only metadata cleared.
    fn emit(&mut self, mut token: Token) -> Result<()> {
        token.hideset = None;
        self.sink.emit(self.source_map, token)
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
    fn define_macro(&mut self, name: SmolStr, body: Vec<Token>, span: SourceSpan) -> Result<()> {
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
                        if self.source_map.text(old.span) != self.source_map.text(new.span)
                            || old.follows_space != new.follows_space
                        {
                            same = false;
                            break;
                        }
                    }
                } else {
                    same = false;
                }

                if !same {
                    self.source_map.warn(span, "redefinition of macro");
                    entry.insert(body);
                }
            },
        }

        Ok(())
    }

    /// Try to expand the given token as a macro.
    ///
    /// If the given token is not a macro that should be expanded, this returns
    /// `None`.
    fn try_expand_macro(&self, token: &Token) -> Option<Vec<Token>> {
        let name = token.as_ident()?;

        // If this token's hideset already contains this macro name, do not
        // expand it again; this is how C preprocessor prevents repeated
        // recursive re-expansion of the same macro on the same token lineage
        if token
            .hideset
            .as_ref()
            .is_some_and(|hideset| hideset.contains(&name))
        {
            return None;
        }

        let mut body = self.macros.get(&name)?.clone();

        // Shared hideset base for all tokens in the macro body; this includes
        // not only this macro, but also inherits the triggering token's hideset
        // so the output tokens retain the prior macro-expansion history
        let mut base = token.hideset.clone().unwrap_or_default();
        Rc::make_mut(&mut base).insert(name.clone());

        for token in &mut body {
            if let Some(mut hideset) = token.hideset.take() {
                Rc::make_mut(&mut hideset).extend(base.iter().cloned());
                token.hideset = Some(hideset);
            } else {
                token.hideset = Some(base.clone());
            }
        }

        Some(body)
    }

    /// Skip extra tokens in the current logical line, if any.
    ///
    /// Returns the span of the first skipped token, if any. Returns `None` if
    /// no tokens were skipped.
    fn skip_line(&mut self) -> Option<SourceSpan> {
        let token = self.next_if_mol()?;
        while self.next_if_mol().is_some() {}
        Some(token.span)
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

            if directive.is_ident("if")
                || directive.is_ident("ifdef")
                || directive.is_ident("ifndef")
            {
                depth += 1;
                continue;
            }

            if directive.is_ident("elif") {
                if depth == 0 {
                    self.process_elif(token.span)?;
                    return Ok(());
                }
                self.skip_line();
                continue;
            }

            if directive.is_ident("else") {
                if depth == 0 {
                    self.process_else(token.span)?;
                    return Ok(());
                }
                self.skip_line();
                continue;
            }

            if directive.is_ident("endif") {
                if depth == 0 {
                    self.process_endif(token.span)?;
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

    /// Enter a new conditional compilation block.
    fn enter_cond(&mut self, span: SourceSpan, included: bool) -> Result<()> {
        self.conds.push(CondFrame {
            span,
            included,
            ctx: CondFrameContext::Then,
        });

        if !included {
            self.skip_cond()?;
        }
        Ok(())
    }

    /// Preprocess the token stream.
    pub fn preprocess(&mut self, emit_eof: bool) -> Result<()> {
        while let Some(token) = self.input.pop_front() {
            if token.is_eof() {
                if emit_eof {
                    self.emit(token)?;
                }
                break;
            }

            if let Some(body) = self.try_expand_macro(&token) {
                self.input.prepend_compat(body);
                continue;
            }

            if !(token.at_bol && token.is_punct("#")) {
                self.emit(token)?;
                continue;
            }

            let Some(directive) = self.next_if_mol() else {
                continue;
            };

            match directive.as_ident().as_deref() {
                Some("include") => self.process_include(token.span)?,
                Some("define") => self.process_define(token.span)?,
                Some("undef") => self.process_undef(token.span)?,
                Some("if") => self.process_if(token.span)?,
                Some("ifdef") => self.process_ifdef(token.span)?,
                Some("ifndef") => self.process_ifndef(token.span)?,
                Some("elif") => self.process_elif(token.span)?,
                Some("else") => self.process_else(token.span)?,
                Some("endif") => self.process_endif(token.span)?,
                _ => {
                    return Err(self
                        .source_map
                        .error(directive.span, "invalid preprocessor directive"));
                },
            }
        }

        if let Some(frame) = self.conds.last() {
            return Err(self.source_map.error(frame.span, "unterminated #if"));
        }
        Ok(())
    }

    /// Process an "#include" directive.
    fn process_include(&mut self, span: SourceSpan) -> Result<()> {
        let Some(token) = self.next_if_mol() else {
            return Err(self
                .source_map
                .error(span, "bare #include without a filename"));
        };

        let Some(content) = token.as_str() else {
            return Err(self.source_map.error(token.span, "expected a filename"));
        };
        let Some(content) = content.as_ref().strip_suffix(b"\0") else {
            return Err(self.source_map.error(token.span, "expected a filename"));
        };

        let path = std::str::from_utf8(content).map_err(|e| {
            self.source_map
                .error(token.span, format!("invalid filename: {e}"))
        })?;

        let path = self
            .source_map
            .get(span.id)
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path);

        if let Some(span) = self.skip_line() {
            self.source_map.warn(span, "extra tokens after #include");
        }

        let source = self.source_map.push(&path)?;
        let tokens = Tokenizer::new(source).tokenize()?;

        let old_input = std::mem::replace(&mut self.input, tokens.into());
        let old_conds = std::mem::take(&mut self.conds);
        let result = self.preprocess(false);
        self.input = old_input;
        self.conds = old_conds;
        result
    }

    /// Process a macro token.
    fn process_macro(&mut self, span: SourceSpan, directive: &str) -> Result<(SmolStr, Token)> {
        let Some(token) = self.next_if_mol() else {
            return Err(self
                .source_map
                .error(span, format!("no macro name given in #{directive}")));
        };

        let Some(name) = token.as_ident() else {
            return Err(self
                .source_map
                .error(token.span, "macro names must be identifiers"));
        };

        Ok((name, token))
    }

    /// Process a "#define" directive.
    fn process_define(&mut self, span: SourceSpan) -> Result<()> {
        let (name, token) = self.process_macro(span, "define")?;
        let tokens = self.line();
        self.define_macro(name, tokens, token.span)?;
        Ok(())
    }

    /// Process an "#undef" directive.
    fn process_undef(&mut self, span: SourceSpan) -> Result<()> {
        let (name, _) = self.process_macro(span, "undef")?;
        if let Some(span) = self.skip_line() {
            self.source_map.warn(span, "extra tokens after #undef");
        }
        self.macros.remove(&name);
        Ok(())
    }

    /// Process a preprocessor expression and return its value.
    ///
    /// If there is no tokens remaining in the current logical line, this
    /// returns `Ok(None)`.
    fn process_constexpr(&mut self) -> Result<Option<ConstValue>> {
        let mut tokens = Vec::new();

        let mut line = VecDeque::from(self.line());
        while let Some(mut token) = line.pop_front() {
            if let Some(body) = self.try_expand_macro(&token) {
                line.prepend_compat(body);
                continue;
            }

            if token.as_ident().is_some() {
                // All identifiers are undefined in preprocessor expressions,
                // and they are simply evaluated as 0
                token.kind = TokenKind::Num(0, Type::INT);
            }
            tokens.push(token);
        }

        if tokens.is_empty() {
            return Ok(None);
        }

        let last = tokens.last().unwrap().span;
        tokens.push(Token {
            kind: TokenKind::Eof,
            span: SourceSpan {
                id: last.id,
                offset: last.offset + last.len,
                len: 0,
            },
            at_bol: false,
            follows_space: false,
            hideset: None,
        });

        let val = Parser::new(self.source_map, tokens, true).parse_constexpr()?;
        Ok(Some(val))
    }

    /// Process an "#if" directive.
    fn process_if(&mut self, span: SourceSpan) -> Result<()> {
        let Some(val) = self.process_constexpr()? else {
            return Err(self
                .source_map
                .error(span, "bare #if without an expression"));
        };
        self.enter_cond(span, val.into())?;
        Ok(())
    }

    /// Process an "#ifdef" directive.
    fn process_ifdef(&mut self, span: SourceSpan) -> Result<()> {
        let (name, _) = self.process_macro(span, "ifdef")?;
        if let Some(span) = self.skip_line() {
            self.source_map.warn(span, "extra tokens after #ifdef");
        }
        self.enter_cond(span, self.macros.contains_key(&name))?;
        Ok(())
    }

    /// Process an "#ifndef" directive.
    fn process_ifndef(&mut self, span: SourceSpan) -> Result<()> {
        let (name, _) = self.process_macro(span, "ifndef")?;
        if let Some(span) = self.skip_line() {
            self.source_map.warn(span, "extra tokens after #ifndef");
        }
        self.enter_cond(span, !self.macros.contains_key(&name))?;
        Ok(())
    }

    /// Process an "#elif" directive.
    fn process_elif(&mut self, span: SourceSpan) -> Result<()> {
        let Some(mut frame) = self.conds.pop() else {
            return Err(self.source_map.error(span, "#elif without #if"));
        };
        if frame.ctx == CondFrameContext::Else {
            return Err(self.source_map.error(span, "#elif after #else"));
        }

        frame.ctx = CondFrameContext::Elif;
        if frame.included {
            self.conds.push(frame);
            self.skip_cond()?;
            return Ok(());
        }

        let Some(val) = self.process_constexpr()? else {
            return Err(self
                .source_map
                .error(span, "bare #elif without an expression"));
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
    fn process_else(&mut self, span: SourceSpan) -> Result<()> {
        let Some(frame) = self.conds.last_mut() else {
            return Err(self.source_map.error(span, "#else without #if"));
        };
        if frame.ctx == CondFrameContext::Else {
            return Err(self.source_map.error(span, "#else after #else"));
        }

        let included = frame.included;
        frame.included = true;
        frame.ctx = CondFrameContext::Else;

        if let Some(span) = self.skip_line() {
            self.source_map.warn(span, "extra tokens after #else");
        }

        if included {
            self.skip_cond()?;
        }
        Ok(())
    }

    /// Process an "#endif" directive.
    fn process_endif(&mut self, span: SourceSpan) -> Result<()> {
        if self.conds.pop().is_none() {
            return Err(self.source_map.error(span, "#endif without #if"));
        }
        if let Some(span) = self.skip_line() {
            self.source_map.warn(span, "extra tokens after #endif");
        }
        Ok(())
    }
}

/// A sink for preprocessor output tokens.
pub trait PreprocessorSink {
    /// Emit a preprocessed token.
    fn emit(&mut self, source_map: &SourceMap, token: Token) -> Result<()>;
}

/// A preprocessor sink that stores the preprocessed tokens in memory.
#[derive(Default)]
pub struct PreprocessedTokens(Vec<Token>);

impl PreprocessorSink for PreprocessedTokens {
    fn emit(&mut self, _source_map: &SourceMap, token: Token) -> Result<()> {
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
    fn emit(&mut self, source_map: &SourceMap, token: Token) -> Result<()> {
        if !self.first && token.at_bol || token.is_eof() {
            writeln!(self.out)?;
        }
        if !token.at_bol && token.follows_space {
            write!(self.out, " ")?;
        }
        write!(self.out, "{}", source_map.text(token.span))?;
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
