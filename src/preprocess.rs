//! A preprocessor for the C programming language.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::{SmolStr, format_smolstr};

use crate::constexpr::ConstValue;
use crate::error::{Error, Result};
use crate::parse::Parser;
use crate::source::{SourceMap, SourceSpan};
use crate::tokenize::{PreToken, PreTokenKind, PreTokenResolver, Token, Tokenizer};

/// A consumable and prependable stream of tokens.
#[derive(Debug)]
struct TokenStream(VecDeque<PreToken>);

impl TokenStream {
    /// Create a new token stream from the given tokens.
    fn new(mut tokens: Vec<PreToken>) -> Self {
        let last = tokens.last().expect("token stream must not be empty");
        if !last.is_eof() {
            tokens.push(PreToken {
                kind: PreTokenKind::Eof,
                span: SourceSpan {
                    id: last.span.id,
                    offset: last.span.offset + last.span.len,
                    len: 0,
                },
                at_bol: false,
                follows_space: false,
                hideset: None,
                synthetic: None,
            });
        }

        Self(tokens.into())
    }

    /// Return the current token.
    fn current(&self) -> &PreToken {
        self.0
            .front()
            .expect("token stream is in broken state: moving out of bounds")
    }

    /// Consume the next token.
    fn next(&mut self) -> Option<PreToken> {
        self.0.pop_front()
    }

    /// Consume the next token if it satisfies the given predicate.
    fn next_if(&mut self, f: impl FnOnce(&mut PreToken) -> bool) -> Option<PreToken> {
        self.0.pop_front_if(f)
    }

    /// Consume the next token if it is in the middle of a logical line.
    fn next_if_mol(&mut self) -> Option<PreToken> {
        self.0
            .pop_front_if(|token| !token.at_bol && !token.is_eof())
    }

    /// Prepend tokens to the front of the stream.
    ///
    /// TODO: Remove once [`VecDeque::prepend`] is stabilized.
    fn prepend(&mut self, tokens: Vec<PreToken>) {
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
    body: Vec<PreToken>,
    /// The parameters of a function-like macro.
    ///
    /// The macro is object-like if this is `None`.
    params: Option<Rc<[SmolStr]>>,
}

/// Helper struct for macro expansion utilities.
struct MacroExpander<'a> {
    source_map: &'a mut SourceMap,
    macros: &'a FxHashMap<SmolStr, Macro>,
}

impl<'a> MacroExpander<'a> {
    /// Create a new macro expander.
    fn new(source_map: &'a mut SourceMap, macros: &'a FxHashMap<SmolStr, Macro>) -> Self {
        Self { source_map, macros }
    }

    /// Read an argument of a function-like macro call.
    ///
    /// This always stops at a "," or ")" token. The provided `span` should be
    /// the span of the triggering token of the macro call.
    fn read_arg(&self, input: &mut TokenStream, span: SourceSpan) -> Result<Vec<PreToken>> {
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

        Ok(arg)
    }

    /// Stringize a macro argument for the "#" operator.
    ///
    /// Returns a string literal token joined form the given argument tokens.
    /// The provided `hash` should be the triggering "#" token.
    fn stringize(&self, hash: &PreToken, arg: &[PreToken]) -> PreToken {
        let resolver = PreTokenResolver::new(self.source_map);
        let mut content = String::new();
        content.push('"');

        for (i, token) in arg.iter().enumerate() {
            if i > 0 && token.follows_space {
                content.push(' ');
            }

            let spelling = resolver.spelling(token);
            for ch in spelling.chars() {
                match ch {
                    '\\' => content.push_str("\\\\"),
                    '"' => content.push_str("\\\""),
                    ch => content.push(ch),
                }
            }
        }

        content.push('"');

        PreToken {
            kind: PreTokenKind::StrLit,
            span: hash.span,
            at_bol: hash.at_bol,
            follows_space: hash.follows_space,
            hideset: None,
            synthetic: Some(content.into()),
        }
    }

    /// Paste two tokens together for the "##" operator.
    ///
    /// The provided `span` should be the span of the "##" token that triggers
    /// this pasting.
    fn paste(&mut self, lhs: PreToken, rhs: PreToken, span: SourceSpan) -> Result<PreToken> {
        let resolver = PreTokenResolver::new(self.source_map);
        let pasted = format_smolstr!("{}{}", resolver.spelling(&lhs), resolver.spelling(&rhs));

        let source = self.source_map.push_virtual(pasted.clone());
        let source_name = source.name.clone();
        let tokens = Tokenizer::new(source).tokenize(false).map_err(|_| {
            self.source_map.error(
                span,
                format!(
                    "pasting formed an invalid preprocessing token: '{pasted}'; see {source_name} \
                     for more details"
                ),
            )
        })?;

        let [token, eof] = tokens.as_slice() else {
            return Err(self.source_map.error(
                span,
                format!("pasting formed an invalid preprocessing token: '{pasted}'"),
            ));
        };
        debug_assert!(eof.is_eof(), "tokenizer did not produce eof");

        Ok(PreToken {
            kind: token.kind.clone(),
            span: lhs.span,
            at_bol: lhs.at_bol,
            follows_space: lhs.follows_space,
            hideset: None,
            synthetic: Some(pasted),
        })
    }

    /// Substitute one replacement-list token if it names a macro parameter.
    ///
    /// `args` should be `None` for object-like macros, and the token will be
    /// returned unchanged. Otherwise, a non-parameter token will be returned
    /// unchanged, and a parameter token will be replaced with the corresponding
    /// argument. If `expand_arg` is true, the argument will be fully expanded
    /// via [`Self::expand_all`].
    fn subst_param(
        &mut self,
        token: PreToken,
        args: Option<&FxHashMap<SmolStr, Vec<PreToken>>>,
        expand_arg: bool,
    ) -> Result<Vec<PreToken>> {
        let Some(args) = args else {
            return Ok(vec![token]);
        };

        let PreTokenKind::Ident(name) = &token.kind else {
            return Ok(vec![token]);
        };
        let Some(arg) = args.get(name) else {
            return Ok(vec![token]);
        };

        if arg.is_empty() || !expand_arg {
            Ok(arg.clone())
        } else {
            self.expand_all(arg.clone())
        }
    }

    /// Expand this macro's replacement list in place.
    ///
    /// `args` should be `None` for object-like macros.
    fn expand_replacement(
        &mut self,
        body: &mut Vec<PreToken>,
        args: Option<&FxHashMap<SmolStr, Vec<PreToken>>>,
    ) -> Result<()> {
        let mut iter = std::mem::take(body).into_iter().peekable();

        while let Some(token) = iter.next() {
            let mut current = if token.is_punct("#")
                && let Some(args) = args
            {
                // Stringize operator is only allowed for function-like macros,
                // and we consume its next token as well
                let Some(next) = iter.next() else {
                    return Err(self
                        .source_map
                        .error(token.span, "'#' is not followed by a macro parameter"));
                };
                let Some(arg) = next.as_ident().and_then(|name| args.get(&name)) else {
                    return Err(self
                        .source_map
                        .error(next.span, "'#' is not followed by a macro parameter"));
                };
                vec![self.stringize(&token, arg)]
            } else if token.is_punct("##") {
                // All "##"s will be consumed in a subsequent loop, so if
                // we reach here that loop must have not been reached yet,
                // which means this "##" is the first token in the macro
                return Err(self
                    .source_map
                    .error(token.span, "'##' cannot appear at start of macro expansion"));
            } else {
                // Substitute parameters in a normal token; note that parameter
                // immediately followed by "##" must be substituted as is (not
                // expanded) as it participates in token pasting later
                let has_pending_paste = iter.peek().is_some_and(|tok| tok.is_punct("##"));
                self.subst_param(token, args, !has_pending_paste)?
            };

            // Consume and fold the whole "a ## b ## ..." chain, if any
            while let Some(token) = iter.next_if(|tok| tok.is_punct("##")) {
                let Some(next) = iter.next() else {
                    return Err(self
                        .source_map
                        .error(token.span, "'##' cannot appear at end of macro expansion"));
                };
                let mut rhs = self.subst_param(next, args, false)?;

                // If either side of "##" is empty, the result is simply the
                // other side
                if current.is_empty() {
                    current = rhs;
                    continue;
                }
                if rhs.is_empty() {
                    continue;
                }

                // Paste the boundary tokens, e.g., "[a,b]" ## "[c,d]"
                // becomes "[a, paste(b,c), d]"
                let last = current.pop().unwrap();
                let first = rhs.remove(0);
                let pasted = self.paste(last, first, token.span)?;
                current.push(pasted);
                current.extend(rhs);
            }

            body.extend(current);
        }

        Ok(())
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
    fn try_expand_in(&mut self, token: &PreToken, input: &mut TokenStream) -> Result<bool> {
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

                args.insert(param.clone(), arg);
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

            self.expand_replacement(&mut body, Some(&args))?;
        } else {
            // Object-like macro, inherit only the triggering token's hideset
            self.expand_replacement(&mut body, None)?;
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
    fn expand_all(&mut self, tokens: Vec<PreToken>) -> Result<Vec<PreToken>> {
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
    pub fn new(source_map: &'a mut SourceMap, tokens: Vec<PreToken>, sink: &'a mut S) -> Self {
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
    fn line(&mut self) -> Vec<PreToken> {
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
        body: Vec<PreToken>,
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

                if old_macro.body.len() != body.len() || old_macro.params != params {
                    same = false;
                } else {
                    for (old, new) in old_macro.body.iter().zip(&body) {
                        if self.source_map.text(old.span) != self.source_map.text(new.span)
                            || old.follows_space != new.follows_space
                        {
                            same = false;
                            break;
                        }
                    }
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

            let mut expander = MacroExpander::new(self.source_map, &self.macros);
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

        if !token.is_str_lit() {
            return Err(self.error(token.span, "expected a filename"));
        }

        let resolver = PreTokenResolver::new(self.source_map);
        let spelling = resolver.spelling(&token);
        let Some(path) = spelling
            .strip_prefix('"')
            .and_then(|spelling| spelling.strip_suffix('"'))
        else {
            return Err(self.error(token.span, "expected a filename"));
        };

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
        let tokens = Tokenizer::new(source).tokenize(true)?;

        let old_input = std::mem::replace(&mut self.input, TokenStream::new(tokens));
        let old_conds = std::mem::take(&mut self.conds);
        let result = self.preprocess(false);
        self.input = old_input;
        self.conds = old_conds;
        result
    }

    /// Process a macro token.
    fn process_macro(&mut self, span: SourceSpan, directive: &str) -> Result<(SmolStr, PreToken)> {
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

        let mut expander = MacroExpander::new(self.source_map, &self.macros);
        let tokens = expander.expand_all(line)?;
        if tokens.is_empty() {
            return Ok(None);
        }

        let resolver = PreTokenResolver::new(self.source_map);
        let tokens = tokens
            .into_iter()
            .map(|tok| resolver.lower(tok, true))
            .collect::<Result<_>>()?;

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
    fn emit(&mut self, source_map: &SourceMap, token: PreToken) -> Result<()>;
}

/// A preprocessor sink that stores the preprocessed tokens in memory.
#[derive(Default)]
pub struct PreprocessedTokens(Vec<PreToken>);

impl PreprocessorSink for PreprocessedTokens {
    fn emit(&mut self, _source_map: &SourceMap, token: PreToken) -> Result<()> {
        self.0.push(token);
        Ok(())
    }
}

impl PreprocessedTokens {
    /// Lower the collected preprocessed tokens into regular tokens.
    pub fn lower(self, source_map: &SourceMap) -> Result<Vec<Token>> {
        let resolver = PreTokenResolver::new(source_map);
        self.0
            .into_iter()
            .map(|tok| resolver.lower(tok, false))
            .collect()
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
    fn emit(&mut self, source_map: &SourceMap, token: PreToken) -> Result<()> {
        if !self.first && token.at_bol || token.is_eof() {
            writeln!(self.out)?;
        }
        if !token.at_bol && token.follows_space {
            write!(self.out, " ")?;
        }
        let resolver = PreTokenResolver::new(source_map);
        write!(self.out, "{}", resolver.spelling(&token))?;
        self.first = false;
        Ok(())
    }
}
