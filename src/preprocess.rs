//! A preprocessor for the C programming language.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

use crate::constexpr::ConstValue;
use crate::error::{Error, Result};
use crate::parse::Parser;
use crate::source::{SourceMap, SourceSpan};
use crate::tokenize::{Keyword, Token, TokenKind, Tokenizer, ensure_eof};
use crate::types::Type;

/// A consumable and prependable stream of tokens.
#[derive(Debug)]
struct TokenStream(VecDeque<Token>);

impl TokenStream {
    /// Create a new token stream from the given tokens.
    fn new(tokens: Vec<Token>) -> Self {
        Self(ensure_eof(tokens).into())
    }

    /// Return the current token.
    fn current(&self) -> &Token {
        self.0
            .front()
            .expect("token stream is in broken state: moving out of bounds")
    }

    /// Consume the next token.
    fn next(&mut self) -> Option<Token> {
        self.0.pop_front()
    }

    /// Consume the next token if it satisfies the given predicate.
    fn next_if(&mut self, f: impl FnOnce(&mut Token) -> bool) -> Option<Token> {
        self.0.pop_front_if(f)
    }

    /// Consume the next token if it is in the middle of a logical line.
    fn next_if_mol(&mut self) -> Option<Token> {
        self.0
            .pop_front_if(|token| !token.at_bol && !token.is_eof())
    }

    /// Prepend tokens to the front of the stream.
    ///
    /// TODO: Remove once [`VecDeque::prepend`] is stabilized.
    fn prepend(&mut self, tokens: Vec<Token>) {
        self.0.reserve(tokens.len());
        for token in tokens.into_iter().rev() {
            self.0.push_front(token);
        }
    }
}

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
    /// The name of the directive that opened this frame.
    name: &'static str,
    span: SourceSpan,
    included: bool,
    ctx: CondFrameContext,
}

/// A macro definition.
#[derive(Debug, Clone)]
struct Macro {
    body: Vec<Token>,
    /// The parameters of a function-like macro.
    ///
    /// The macro is object-like if this is `None`.
    params: Option<Rc<[SmolStr]>>,
}

/// Helper struct for macro expansion utilities.
struct MacroExpander<'a> {
    source_map: &'a SourceMap,
    macros: &'a FxHashMap<SmolStr, Macro>,
}

impl<'a> MacroExpander<'a> {
    /// Read an argument of a function-like macro call.
    ///
    /// This always stops at a "," or ")" token. The returned argument tokens
    /// will be fully macro-expanded. The provided `span` should be the span of
    /// the triggering token of the macro call.
    fn read_arg(&self, input: &mut TokenStream, span: SourceSpan) -> Result<Vec<Token>> {
        let mut arg = Vec::new();
        let mut depth = 0;

        while depth > 0 || (!input.current().is_punct(",") && !input.current().is_punct(")")) {
            let token = input.next().unwrap();
            if token.is_eof() {
                return Err(self.source_map.error(span, "unterminated macro call"));
            }

            if token.is_punct("(") {
                depth += 1;
            } else if token.is_punct(")") {
                depth -= 1;
            }

            arg.push(token);
        }

        Ok(if arg.is_empty() {
            arg
        } else {
            self.expand_all(arg)?
        })
    }

    /// Try to expand the given token as a macro.
    ///
    /// The token should be the one that precedes the given input stream, as
    /// function-like macro expansion needs to look ahead into the input stream
    /// to parse the arguments.
    ///
    /// This returns whether the token was successfully expanded. If so, the
    /// expanded tokens are automatically prepended to the given input stream.
    ///
    /// The implementation is based on [X3J11/86-196 Complete macro expansion
    /// algorithm](https://www.spinellis.gr/blog/20060626/x3J11-86-196.pdf).
    fn try_expand_in(&self, token: &Token, input: &mut TokenStream) -> Result<bool> {
        let Some(name) = token.as_ident() else {
            return Ok(false);
        };

        // If this token's hideset already contains this macro name, do not
        // expand it again; this is how C preprocessor prevents repeated
        // recursive re-expansion of the same macro on the same token lineage
        if token
            .hideset
            .as_ref()
            .is_some_and(|hideset| hideset.contains(&name))
        {
            return Ok(false);
        }

        let Some(Macro { mut body, params }) = self.macros.get(&name).cloned() else {
            return Ok(false);
        };

        let mut base_hideset = FxHashSet::default();

        if let Some(params) = params {
            if !input.current().is_punct("(") {
                return Ok(false);
            }
            input.next();

            let n_params = params.len();
            let mut params = params.iter();
            let mut args = FxHashMap::default();
            let mut last_span = input.current().span;

            while !input.current().is_punct(")") {
                if !args.is_empty() {
                    debug_assert!(input.current().is_punct(","));
                    input.next();
                }

                let span = input.current().span;
                let arg = self.read_arg(input, token.span)?;

                let Some(param) = params.next() else {
                    return Err(self.source_map.error(
                        span,
                        format!("too many arguments provided to macro call (expected {n_params})"),
                    ));
                };

                args.insert(param, arg);
                last_span = input.current().span;
            }

            // For function-like macros, the invocation may have been assembled
            // from tokens with different expansion lineages, so we inherit the
            // intersection of the hidesets on the macro name token and the
            // matching ")" token; that keeps only macro names that were already
            // blocked on the invocation as a whole
            if let Some(token_hideset) = &token.hideset
                && let Some(rparen_hideset) = &input.current().hideset
            {
                base_hideset = token_hideset
                    .intersection(rparen_hideset)
                    .cloned()
                    .collect();
            }

            input.next();

            if params.next().is_some() {
                return Err(self.source_map.error(
                    last_span,
                    format!("too few arguments provided to macro call (expected {n_params})"),
                ));
            }

            for token in std::mem::take(&mut body) {
                if let TokenKind::Ident(name) = &token.kind
                    && let Some(arg) = args.get(name)
                {
                    body.extend(arg.clone());
                    continue;
                }
                body.push(token);
            }
        } else {
            // Object-like macro, inherit only the triggering token's hideset
            base_hideset = token.hideset.as_deref().cloned().unwrap_or_default();
        }

        // Add this macro's own name to prevent the replacement from immediately
        // expanding the same macro again
        base_hideset.insert(name.clone());
        let base_hideset = Rc::new(base_hideset);

        for token in &mut body {
            if let Some(mut hideset) = token.hideset.take() {
                Rc::make_mut(&mut hideset).extend(base_hideset.iter().cloned());
                token.hideset = Some(hideset);
            } else {
                token.hideset = Some(base_hideset.clone());
            }
        }

        input.prepend(body);
        Ok(true)
    }

    /// Expand all macros in the given token stream.
    fn expand_all(&self, tokens: Vec<Token>) -> Result<Vec<Token>> {
        let mut input = TokenStream::new(tokens);
        let mut out = Vec::new();

        while let Some(token) = input.next() {
            if token.is_eof() {
                break;
            }
            if self.try_expand_in(&token, &mut input)? {
                continue;
            }
            out.push(token);
        }

        Ok(out)
    }
}

/// Preprocessor for a C token stream.
pub struct Preprocessor<'a, S: PreprocessorSink> {
    source_map: &'a mut SourceMap,
    input: TokenStream,
    sink: &'a mut S,
    conds: Vec<CondFrame>,
    macros: FxHashMap<SmolStr, Macro>,
}

impl<'a, S> Preprocessor<'a, S>
where
    S: PreprocessorSink,
{
    /// Create a new preprocessor for the given token stream.
    pub fn new(source_map: &'a mut SourceMap, tokens: Vec<Token>, sink: &'a mut S) -> Self {
        Self {
            source_map,
            input: TokenStream::new(tokens),
            sink,
            conds: Vec::new(),
            macros: Default::default(),
        }
    }

    /// Dispatch of [`SourceMap::error`].
    fn error(&self, span: SourceSpan, message: impl Into<String>) -> Error {
        self.source_map.error(span, message)
    }

    /// [`Self::error`] using the current token's span.
    fn error_current(&self, message: impl Into<String>) -> Error {
        self.error(self.input.current().span, message)
    }

    /// Dispatch of [`SourceMap::warn`].
    fn warn(&self, span: SourceSpan, message: impl Into<String>) {
        self.source_map.warn(span, message);
    }

    /// Consume the rest of the current logical line.
    fn line(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        while let Some(token) = self.input.next_if_mol() {
            tokens.push(token);
        }
        tokens
    }

    /// Define a macro.
    fn define_macro(
        &mut self,
        name: SmolStr,
        body: Vec<Token>,
        params: Option<Rc<[SmolStr]>>,
        span: SourceSpan,
    ) -> Result<()> {
        use std::collections::hash_map::Entry;

        let redefined = match self.macros.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(Macro { body, params });
                false
            },
            Entry::Occupied(mut entry) => {
                let mut same = true;

                let old_macro = entry.get();
                if old_macro.body.len() == body.len() {
                    for (old, new) in old_macro.body.iter().zip(&body) {
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
                    entry.insert(Macro { body, params });
                }
                !same
            },
        };

        if redefined {
            self.warn(span, "redefinition of macro");
        }
        Ok(())
    }

    /// Skip extra tokens in the current logical line, if any.
    ///
    /// Returns the span of the first skipped token, if any. Returns `None` if
    /// no tokens were skipped.
    fn skip_line(&mut self) -> Option<SourceSpan> {
        let token = self.input.next_if_mol()?;
        while self.input.next_if_mol().is_some() {}
        Some(token.span)
    }

    /// Skip tokens until the next matching "#else", "#elif", or "#endif".
    fn skip_cond(&mut self) -> Result<()> {
        let mut depth = 0;

        while let Some(token) = self.input.next() {
            if token.is_eof() {
                return Ok(());
            }

            if !(token.at_bol && token.is_punct("#")) {
                continue;
            }

            let Some(directive) = self.input.next_if_mol() else {
                continue;
            };

            match directive.as_ident().as_deref() {
                Some("if" | "ifdef" | "ifndef") => depth += 1,
                Some("elif") if depth == 0 => {
                    self.process_elif(token.span)?;
                    return Ok(());
                },
                Some("else") if depth == 0 => {
                    self.process_else(token.span)?;
                    return Ok(());
                },
                Some("endif") if depth == 0 => {
                    self.process_endif(token.span)?;
                    return Ok(());
                },
                Some("endif") => depth -= 1,
                _ => {},
            }

            self.skip_line();
        }

        Ok(())
    }

    /// Enter a new conditional compilation block.
    fn enter_cond(&mut self, name: &'static str, span: SourceSpan, included: bool) -> Result<()> {
        self.conds.push(CondFrame {
            name,
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
        while let Some(token) = self.input.next() {
            if token.is_eof() {
                if emit_eof {
                    self.sink.emit(self.source_map, token)?;
                }
                break;
            }

            let expander = MacroExpander {
                source_map: self.source_map,
                macros: &self.macros,
            };
            if expander.try_expand_in(&token, &mut self.input)? {
                continue;
            }

            if !(token.at_bol && token.is_punct("#")) {
                self.sink.emit(self.source_map, token)?;
                continue;
            }

            let Some(directive) = self.input.next_if_mol() else {
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
                    return Err(self.error(directive.span, "invalid preprocessor directive"));
                },
            }
        }

        if let Some(frame) = self.conds.last() {
            return Err(self.error(frame.span, format!("unterminated #{}", frame.name)));
        }
        Ok(())
    }

    /// Process an "#include" directive.
    fn process_include(&mut self, span: SourceSpan) -> Result<()> {
        let Some(token) = self.input.next_if_mol() else {
            return Err(self.error(span, "bare #include without a filename"));
        };

        let Some(content) = token.as_str() else {
            return Err(self.error(token.span, "expected a filename"));
        };
        let Some(content) = content.as_ref().strip_suffix(b"\0") else {
            return Err(self.error(token.span, "expected a filename"));
        };

        let path = std::str::from_utf8(content)
            .map_err(|e| self.error(token.span, format!("invalid filename: {e}")))?;

        let path = self
            .source_map
            .get(span.id)
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path);

        if let Some(span) = self.skip_line() {
            self.warn(span, "extra tokens after #include");
        }

        let source = self.source_map.push(&path)?;
        let tokens = Tokenizer::new(source).tokenize()?;

        let old_input = std::mem::replace(&mut self.input, TokenStream::new(tokens));
        let old_conds = std::mem::take(&mut self.conds);
        let result = self.preprocess(false);
        self.input = old_input;
        self.conds = old_conds;
        result
    }

    /// Process a macro token.
    fn process_macro(&mut self, span: SourceSpan, directive: &str) -> Result<(SmolStr, Token)> {
        let Some(token) = self.input.next_if_mol() else {
            return Err(self.error(span, format!("no macro name given in #{directive}")));
        };
        let Some(name) = token.as_ident() else {
            return Err(self.error(token.span, "macro names must be identifiers"));
        };
        Ok((name, token))
    }

    /// Process a "#define" directive.
    fn process_define(&mut self, span: SourceSpan) -> Result<()> {
        let (name, token) = self.process_macro(span, "define")?;

        let params = if self
            .input
            .next_if(|tok| !tok.follows_space && !tok.at_bol && tok.is_punct("("))
            .is_some()
        {
            let mut params = Vec::new();

            while !self.input.current().is_punct(")") {
                if !params.is_empty() && self.input.next_if(|tok| tok.is_punct(",")).is_none() {
                    return Err(self.error_current("expected ','"));
                }
                let Some(ident) = self.input.current().as_ident() else {
                    return Err(self.error_current("expected an identifier"));
                };
                params.push(ident);
                self.input.next();
            }

            self.input.next();
            Some(params)
        } else {
            None
        };

        let tokens = self.line();
        self.define_macro(name, tokens, params.map(Into::into), token.span)?;
        Ok(())
    }

    /// Process an "#undef" directive.
    fn process_undef(&mut self, span: SourceSpan) -> Result<()> {
        let (name, _) = self.process_macro(span, "undef")?;
        if let Some(span) = self.skip_line() {
            self.warn(span, "extra tokens after #undef");
        }
        self.macros.remove(&name);
        Ok(())
    }

    /// Process a preprocessor expression and return its value.
    ///
    /// If there is no tokens remaining in the current logical line, this
    /// returns `Ok(None)`.
    fn process_constexpr(&mut self) -> Result<Option<ConstValue>> {
        let line = self.line();
        if line.is_empty() {
            return Ok(None);
        }

        let expander = MacroExpander {
            source_map: self.source_map,
            macros: &self.macros,
        };
        let mut tokens = expander.expand_all(line)?;
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

        let val = Parser::new(self.source_map, tokens, true).parse_constexpr()?;
        Ok(Some(val))
    }

    /// Process an "#if" directive.
    fn process_if(&mut self, span: SourceSpan) -> Result<()> {
        let Some(val) = self.process_constexpr()? else {
            return Err(self.error(span, "bare #if without an expression"));
        };
        self.enter_cond("if", span, val.into())?;
        Ok(())
    }

    /// Process an "#ifdef" directive.
    fn process_ifdef(&mut self, span: SourceSpan) -> Result<()> {
        let (name, _) = self.process_macro(span, "ifdef")?;
        if let Some(span) = self.skip_line() {
            self.warn(span, "extra tokens after #ifdef");
        }
        self.enter_cond("ifdef", span, self.macros.contains_key(&name))?;
        Ok(())
    }

    /// Process an "#ifndef" directive.
    fn process_ifndef(&mut self, span: SourceSpan) -> Result<()> {
        let (name, _) = self.process_macro(span, "ifndef")?;
        if let Some(span) = self.skip_line() {
            self.warn(span, "extra tokens after #ifndef");
        }
        self.enter_cond("ifndef", span, !self.macros.contains_key(&name))?;
        Ok(())
    }

    /// Process an "#elif" directive.
    fn process_elif(&mut self, span: SourceSpan) -> Result<()> {
        let Some(mut frame) = self.conds.pop() else {
            return Err(self.error(span, "#elif without #if"));
        };
        if frame.ctx == CondFrameContext::Else {
            return Err(self.error(span, "#elif after #else"));
        }

        frame.ctx = CondFrameContext::Elif;
        if frame.included {
            self.conds.push(frame);
            self.skip_cond()?;
            return Ok(());
        }

        let Some(val) = self.process_constexpr()? else {
            return Err(self.error(span, "bare #elif without an expression"));
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
            return Err(self.error(span, "#else without #if"));
        };
        if frame.ctx == CondFrameContext::Else {
            return Err(self.error(span, "#else after #else"));
        }

        let included = frame.included;
        frame.included = true;
        frame.ctx = CondFrameContext::Else;

        if let Some(span) = self.skip_line() {
            self.warn(span, "extra tokens after #else");
        }

        if included {
            self.skip_cond()?;
        }
        Ok(())
    }

    /// Process an "#endif" directive.
    fn process_endif(&mut self, span: SourceSpan) -> Result<()> {
        if self.conds.pop().is_none() {
            return Err(self.error(span, "#endif without #if"));
        }
        if let Some(span) = self.skip_line() {
            self.warn(span, "extra tokens after #endif");
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
        for token in &mut self.0 {
            token.hideset = None;

            if let Some(ident) = token.as_ident()
                && let Ok(keyword) = Keyword::try_from(ident.as_str())
            {
                token.kind = TokenKind::Keyword(keyword);
            }
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
