//! A preprocessor for the C programming language.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::{SmolStr, format_smolstr};

use crate::constexpr::ConstValue;
use crate::error::{Error, Result};
use crate::parse::Parser;
use crate::source::{SourceMap, SourceSpan};
use crate::tokenize::{PreToken, PreTokenKind, PreTokenResolver, Token, TokenKind, Tokenizer};
use crate::utils::datetime;

/// A consumable and prependable stream of tokens.
#[derive(Debug)]
struct TokenStream(VecDeque<PreToken>);

impl TokenStream {
    /// Create a new token stream from the given tokens.
    fn new(tokens: impl Into<VecDeque<PreToken>>) -> Self {
        let mut tokens = tokens.into();

        let last = tokens.back().expect("token stream must not be empty");
        if !last.is_eof() {
            tokens.push_back(PreToken {
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
                origin: None,
            });
        }

        Self(tokens)
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

/// An "#include" filename.
#[derive(Debug)]
struct IncludeFilename {
    name: String,
    span: SourceSpan,
    is_quoted: bool,
}

/// Handler for builtin macros that cannot be represented as simple replacement.
#[derive(Debug, Clone, Copy)]
enum MacroHandler {
    File,
    Line,
}

impl MacroHandler {
    /// Call this macro handler on the given token.
    fn call(self, token: &PreToken, source_map: &SourceMap) -> Vec<PreToken> {
        match self {
            Self::File => {
                let mut filename = String::new();
                filename.push('"');
                let span = token.origin.unwrap_or(token.span);
                for ch in source_map.get(span.id).name.chars() {
                    match ch {
                        '\\' => filename.push_str("\\\\"),
                        '"' => filename.push_str("\\\""),
                        ch => filename.push(ch),
                    }
                }
                filename.push('"');
                vec![PreToken::synthetic(
                    PreTokenKind::StrLit,
                    span,
                    token.at_bol,
                    token.follows_space,
                    filename,
                )]
            },
            Self::Line => {
                let span = token.origin.unwrap_or(token.span);
                let (_, line, _) = source_map.file_line_col(span);
                vec![PreToken::synthetic(
                    PreTokenKind::NumLit,
                    span,
                    token.at_bol,
                    token.follows_space,
                    format_smolstr!("{line}"),
                )]
            },
        }
    }
}

/// A macro definition.
#[derive(Debug, Clone)]
struct Macro {
    body: Vec<PreToken>,
    /// The parameters of a function-like macro.
    ///
    /// The macro is object-like if this is `None`.
    params: Option<Rc<[SmolStr]>>,
    /// Whether a function-like macro is variadic.
    is_variadic: bool,
    /// A [`MacroHandler`], if applicable.
    ///
    /// If this is given, the other fields are ignored and the macro is expanded
    /// by calling the handler rather than going through normal replacement.
    handler: Option<MacroHandler>,
}

impl Macro {
    /// Create an object-like macro from the given spelling.
    fn obj(spelling: &str, source_map: &mut SourceMap) -> Self {
        let source = source_map.push_virtual(spelling.into(), Some("<built-in>".into()));
        let body = Tokenizer::new(source)
            .tokenize(false, false)
            .expect("built-in macro has invalid spelling");

        Self {
            body,
            params: None,
            is_variadic: false,
            handler: None,
        }
    }

    /// Create a macro with the given handler.
    fn handler(handler: MacroHandler) -> Self {
        Self {
            body: Vec::new(),
            params: None,
            is_variadic: false,
            handler: Some(handler),
        }
    }
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
    /// This always stops at a "," or ")" token. If `read_all` is true, this
    /// only stops at ")", i.e., it reads all remaining arguments. The provided
    /// `span` should be the span of the triggering token of the macro call.
    fn read_arg(
        &self,
        input: &mut TokenStream,
        read_all: bool,
        span: SourceSpan,
    ) -> Result<Vec<PreToken>> {
        let mut arg = Vec::new();
        let mut depth = 0;

        loop {
            if depth == 0 && input.current().is_punct(")") {
                break;
            }
            if depth == 0 && !read_all && input.current().is_punct(",") {
                break;
            }

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

        PreToken::synthetic(
            PreTokenKind::StrLit,
            hash.span,
            hash.at_bol,
            hash.follows_space,
            content,
        )
    }

    /// Paste two tokens together for the "##" operator.
    ///
    /// The provided `span` should be the span of the "##" token that triggers
    /// this pasting.
    fn paste(&mut self, lhs: PreToken, rhs: PreToken, span: SourceSpan) -> Result<PreToken> {
        let resolver = PreTokenResolver::new(self.source_map);
        let pasted = format_smolstr!("{}{}", resolver.spelling(&lhs), resolver.spelling(&rhs));

        let source = self.source_map.push_virtual(pasted.clone(), None);
        let source_name = source.name.clone();
        let tokens = Tokenizer::new(source).tokenize(false, false).map_err(|_| {
            self.source_map.error(
                span,
                format!(
                    "pasting formed an invalid preprocessing token: '{pasted}'; see {source_name} \
                     for more details"
                ),
            )
        })?;

        if tokens.len() != 1 {
            return Err(self.source_map.error(
                span,
                format!("pasting formed multiple preprocessing tokens: '{pasted}'"),
            ));
        }

        Ok(PreToken::synthetic(
            tokens[0].kind.clone(),
            lhs.span,
            lhs.at_bol,
            lhs.follows_space,
            pasted,
        ))
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

        let mut tokens = if arg.is_empty() || !expand_arg {
            arg.clone()
        } else {
            self.expand_all(arg.clone())?
        };

        if let Some(first) = tokens.first_mut() {
            first.at_bol = token.at_bol;
            first.follows_space = token.follows_space;
        }
        Ok(tokens)
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
            let current = if token.is_punct("#")
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

            let mut current = VecDeque::from(current);

            // Consume and fold the whole "a ## b ## ..." chain, if any
            while let Some(token) = iter.next_if(|tok| tok.is_punct("##")) {
                let Some(next) = iter.next() else {
                    return Err(self
                        .source_map
                        .error(token.span, "'##' cannot appear at end of macro expansion"));
                };
                let mut rhs = VecDeque::from(self.subst_param(next, args, false)?);

                // Paste the boundary tokens, e.g., "[a,b]" ## "[c,d]" becomes
                // "[a, paste(b,c), d]"
                let Some(left) = current.pop_back() else {
                    current = rhs;
                    continue;
                };
                let Some(right) = rhs.pop_front() else {
                    current.push_back(left);
                    continue;
                };
                let pasted = self.paste(left, right, token.span)?;
                current.push_back(pasted);
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

        let Some(Macro {
            mut body,
            params,
            is_variadic,
            handler,
        }) = self.macros.get(&name).cloned()
        else {
            return Ok(false);
        };

        if let Some(handler) = handler {
            let expanded = handler.call(token, self.source_map);
            input.prepend(expanded);
            return Ok(true);
        }

        let mut base_hideset = FxHashSet::default();

        if let Some(params) = params {
            if !input.current().is_punct("(") {
                return Ok(false);
            }
            input.next();

            let n_params = params.len();
            let mut args = FxHashMap::default();

            for (index, param) in params.iter().enumerate() {
                if index > 0 {
                    if input.current().is_punct(")") {
                        return Err(self.source_map.error(
                            input.current().span,
                            format!("too few arguments for macro call (expected {n_params})"),
                        ));
                    }
                    debug_assert!(input.current().is_punct(","));
                    input.next();
                }
                let arg = self.read_arg(input, false, token.span)?;
                args.insert(param.clone(), arg);
            }

            if is_variadic {
                if input.current().is_punct(")") {
                    args.insert("__VA_ARGS__".into(), Vec::new());
                } else {
                    if n_params > 0 {
                        debug_assert!(input.current().is_punct(","));
                        input.next();
                    }
                    let arg = self.read_arg(input, true, token.span)?;
                    args.insert("__VA_ARGS__".into(), arg);
                }
            } else if !input.current().is_punct(")") {
                return Err(self.source_map.error(
                    input.current().span,
                    format!("too many arguments for macro call (expected {n_params})"),
                ));
            }

            debug_assert!(input.current().is_punct(")"));
            input.next();

            self.expand_replacement(&mut body, Some(&args))?;

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
        } else {
            self.expand_replacement(&mut body, None)?;

            // Object-like macros only inherit the triggering token's hideset
            base_hideset = token.hideset.as_deref().cloned().unwrap_or_default();
        }

        if let Some(first) = body.first_mut() {
            first.at_bol = token.at_bol;
            first.follows_space = token.follows_space;
        }

        let origin = token.origin.unwrap_or(token.span);
        for token in &mut body {
            token.origin = Some(origin);
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
    fn expand_all(&mut self, tokens: impl Into<VecDeque<PreToken>>) -> Result<Vec<PreToken>> {
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
    includes: &'a [PathBuf],
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
    pub fn new(
        source_map: &'a mut SourceMap,
        includes: &'a [PathBuf],
        macro_ops: &'a [(SmolStr, bool)],
        tokens: Vec<PreToken>,
        sink: &'a mut S,
    ) -> Result<Self> {
        let (now_date, now_time) = datetime();

        let mut macros = FxHashMap::default();

        for (name, replacement) in [
            (
                "__VERSION__",
                concat!("\"", env!("CARGO_PKG_VERSION"), "\""),
            ),
            ("_LP64", "1"),
            ("__C99_MACRO_WITH_VA_ARGS", "1"),
            ("__ELF__", "1"),
            ("__LP64__", "1"),
            ("__SIZEOF_DOUBLE__", "8"),
            ("__SIZEOF_FLOAT__", "4"),
            ("__SIZEOF_INT__", "4"),
            ("__SIZEOF_LONG_DOUBLE__", "8"),
            ("__SIZEOF_LONG_LONG__", "8"),
            ("__SIZEOF_LONG__", "8"),
            ("__SIZEOF_POINTER__", "8"),
            ("__SIZEOF_PTRDIFF_T__", "8"),
            ("__SIZEOF_SHORT__", "2"),
            ("__SIZEOF_SIZE_T__", "8"),
            ("__SIZE_TYPE__", "unsigned long"),
            ("__STDC_HOSTED__", "1"),
            ("__STDC_NO_ATOMICS__", "1"),
            ("__STDC_NO_COMPLEX__", "1"),
            ("__STDC_NO_THREADS__", "1"),
            ("__STDC_NO_VLA__", "1"),
            ("__STDC_VERSION__", "201112L"),
            ("__STDC__", "1"),
            ("__USER_LABEL_PREFIX__", "\"\""),
            ("__alignof__", "_Alignof"),
            ("__amd64", "1"),
            ("__amd64__", "1"),
            ("__chacc__", "1"),
            ("__const__", "const"),
            ("__gnu_linux__", "1"),
            ("__inline__", "inline"),
            ("linux", "1"),
            ("__linux", "1"),
            ("__linux__", "1"),
            ("__signed__", "signed"),
            ("__typeof__", "typeof"),
            ("unix", "1"),
            ("__unix", "1"),
            ("__unix__", "1"),
            ("__volatile__", "volatile"),
            ("__x86_64", "1"),
            ("__x86_64__", "1"),
            ("__DATE__", &format!("\"{now_date}\"")),
            ("__TIME__", &format!("\"{now_time}\"")),
        ] {
            macros.insert(name.into(), Macro::obj(replacement, source_map));
        }

        for (name, handler) in [
            ("__FILE__", MacroHandler::File),
            ("__LINE__", MacroHandler::Line),
        ] {
            macros.insert(name.into(), Macro::handler(handler));
        }

        let mut input = TokenStream::new(tokens);

        for (content, is_def) in macro_ops.iter().rev() {
            let content = if *is_def {
                let (macro_, replacement) = content.split_once('=').unwrap_or((content, "1"));
                format_smolstr!("#define {macro_} {replacement}")
            } else {
                format_smolstr!("#undef {content}")
            };
            let source = source_map.push_virtual(content, Some("<command-line>".into()));
            let tokens = Tokenizer::new(source).tokenize(true, false)?;
            input.prepend(tokens);
        }

        Ok(Self {
            source_map,
            includes,
            input,
            sink,
            conds: Vec::new(),
            macros,
        })
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
        is_variadic: bool,
        span: SourceSpan,
    ) -> Result<()> {
        use std::collections::hash_map::Entry;

        let redefined = match self.macros.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(Macro {
                    body,
                    params,
                    is_variadic,
                    handler: None,
                });
                false
            },
            Entry::Occupied(mut entry) => {
                let mut same = true;
                let old_macro = entry.get();

                if old_macro.handler.is_some()
                    || old_macro.is_variadic != is_variadic
                    || old_macro.body.len() != body.len()
                    || old_macro.params != params
                {
                    same = false;
                } else {
                    for (old, new) in old_macro.body.iter().zip(&body) {
                        if old.follows_space != new.follows_space
                            || self.source_map.text(old.span) != self.source_map.text(new.span)
                        {
                            same = false;
                            break;
                        }
                    }
                }

                if !same {
                    entry.insert(Macro {
                        body,
                        params,
                        is_variadic,
                        handler: None,
                    });
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
                Some("error") => self.process_warning_error(directive.span, true)?,
                Some("warning") => self.process_warning_error(directive.span, false)?,
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

    /// Process the filename after an "#include" directive.
    ///
    /// The span should be the span of the "#include" directive. The tokens
    /// should be remaining tokens in the same logical line after the "#include"
    /// directive. If `expand` is true, this will try to expand the tokens once
    /// as a macro before processing them as a filename. The resolved filename
    /// and its span will be returned on success.
    fn process_include_filename(
        &mut self,
        span: SourceSpan,
        tokens: Vec<PreToken>,
        expand: bool,
    ) -> Result<IncludeFilename> {
        let mut tokens = VecDeque::from(tokens);
        let Some(first) = tokens.pop_front() else {
            return Err(self.error(span, "bare #include without a filename"));
        };

        let resolver = PreTokenResolver::new(self.source_map);

        if first.is_str_lit() {
            let spelling = resolver.spelling(&first);
            let filename = spelling
                .strip_prefix('"')
                .and_then(|spelling| spelling.strip_suffix('"'))
                .ok_or_else(|| self.error(first.span, "expected a filename"))?;
            if let Some(front) = tokens.front() {
                self.warn(front.span, "extra tokens after #include");
            }
            return Ok(IncludeFilename {
                name: filename.to_string(),
                span: first.span,
                is_quoted: true,
            });
        }

        if first.is_punct("<") {
            let mut filename = String::new();

            for token in tokens {
                if token.is_punct(">") {
                    if filename.is_empty() {
                        return Err(self.error(first.span, "empty filename"));
                    }
                    return Ok(IncludeFilename {
                        name: filename,
                        span: first.span,
                        is_quoted: false,
                    });
                }
                if !filename.is_empty() && token.follows_space {
                    filename.push(' ');
                }
                let spelling = resolver.spelling(&token);
                filename.push_str(&spelling);
            }

            return Err(self.error(first.span, "expected '>'"));
        }

        if expand {
            tokens.push_front(first);
            let mut expander = MacroExpander::new(self.source_map, &self.macros);
            let tokens = expander.expand_all(tokens)?;
            return self.process_include_filename(span, tokens, false);
        }

        Err(self.error(first.span, "expected a filename"))
    }

    /// Process an "#include" directive.
    fn process_include(&mut self, span: SourceSpan) -> Result<()> {
        let line = self.line();
        let IncludeFilename {
            name,
            span,
            is_quoted,
        } = self.process_include_filename(span, line, true)?;

        if name.is_empty() {
            return Err(self.error(span, "empty filename"));
        }

        // The source file directory is searched only for quoted includes
        let root = is_quoted.then(|| {
            self.source_map
                .get(span.id)
                .path
                .parent()
                .unwrap_or_else(|| Path::new("."))
        });

        let Some(path) = root
            .into_iter()
            .chain(self.includes.iter().map(AsRef::as_ref))
            .map(|base| base.join(&name))
            .find(|path| path.is_file())
        else {
            return Err(self.error(span, "file not found"));
        };

        let source = self.source_map.push(&path)?;
        let tokens = Tokenizer::new(source).tokenize(true, true)?;

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
        let mut is_variadic = false;

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

                if self.input.current().is_punct("...") {
                    is_variadic = true;
                    self.input.next();
                    break;
                }

                let Some(ident) = self.input.current().as_ident() else {
                    return Err(self.error_current("expected an identifier"));
                };
                params.push(ident);
                self.input.next();
            }

            if !self.input.current().is_punct(")") {
                return Err(self.error_current("expected ')'"));
            }
            self.input.next();
            Some(params)
        } else {
            None
        };

        let tokens = self.line();
        self.define_macro(
            name,
            tokens,
            params.map(Into::into),
            is_variadic,
            token.span,
        )?;
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
        let mut line = self.line();
        if line.is_empty() {
            return Ok(None);
        }

        let mut iter = std::mem::take(&mut line).into_iter().peekable();
        while let Some(token) = iter.next() {
            if token.as_ident().as_deref() != Some("defined") {
                line.push(token);
                continue;
            }

            let Some(next) = iter.next() else {
                return Err(self.error(token.span, "expected an identifier after 'defined'"));
            };

            // Either "defined MACRO" or "defined(MACRO)"
            let name = if next.is_punct("(") {
                let Some(ident) = iter.next() else {
                    return Err(self.error(next.span, "expected an identifier after 'defined'"));
                };
                let Some(name) = ident.as_ident() else {
                    return Err(self.error(ident.span, "expected an identifier after 'defined'"));
                };
                let Some(rparen) = iter.next() else {
                    return Err(self.error(ident.span, "missing ')' after 'defined'"));
                };
                if !rparen.is_punct(")") {
                    return Err(self.error(rparen.span, "missing ')' after 'defined'"));
                }
                name
            } else if let Some(name) = next.as_ident() {
                name
            } else {
                return Err(self.error(next.span, "expected an identifier after 'defined'"));
            };

            let value = self.macros.contains_key(&name) as u8; // 0/1
            line.push(PreToken::synthetic(
                PreTokenKind::NumLit,
                token.span,
                token.at_bol,
                token.follows_space,
                format_smolstr!("{value}"),
            ));
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

    /// Process a "#warning" or "#error" directive.
    fn process_warning_error(&mut self, span: SourceSpan, is_error: bool) -> Result<()> {
        let line = self.line();
        let mut message = String::new();
        let resolver = PreTokenResolver::new(self.source_map);

        for token in &line {
            if !message.is_empty() && token.follows_space {
                message.push(' ');
            }
            message.push_str(&resolver.spelling(token));
        }

        if is_error {
            Err(self.error(span, message))
        } else {
            self.warn(span, message);
            Ok(())
        }
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
        let mut tokens: Vec<Token> = Vec::with_capacity(self.0.len());

        for token in self.0 {
            let token = resolver.lower(token, false)?;

            // Fold adjacent string literals into one.
            if let TokenKind::Str(current) = &token.kind
                && let Some(Token {
                    kind: TokenKind::Str(next),
                    ..
                }) = tokens.last_mut()
            {
                debug_assert_eq!(next.last(), Some(&b'\0'));
                let mut content = Vec::with_capacity(next.len() + current.len() - 1);
                content.extend_from_slice(&next[..next.len() - 1]);
                content.extend_from_slice(current);
                *next = content.into();
                continue;
            }

            tokens.push(token);
        }

        Ok(tokens)
    }
}

/// A preprocessor sink that directly writes out the preprocessed tokens.
pub struct PreprocessedWriter {
    out: Vec<u8>,
    first: bool,
}

impl PreprocessedWriter {
    /// Create a new preprocessor writer.
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            first: true,
        }
    }
}

impl PreprocessorSink for PreprocessedWriter {
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

impl From<PreprocessedWriter> for Vec<u8> {
    fn from(val: PreprocessedWriter) -> Self {
        val.out
    }
}
