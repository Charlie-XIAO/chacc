//! A [recursive-descent parser][1] for the C programming language.
//!
//! [1]: https://en.wikipedia.org/wiki/Recursive_descent_parser

use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::{SmolStr, ToSmolStr, format_smolstr};

use crate::ast::{
    BinaryOp, EntityRef, Function, GlobalInitData, GlobalStorage, GlobalVar, LocalVar, Node,
    NodeKind, Program, Relocation, Stmt, StmtKind,
};
use crate::constexpr::ConstValue;
use crate::error::{Error, Result};
use crate::source::Source;
use crate::tokenize::{Keyword, Token};
use crate::types::{ArrayTypeData, ConstType, Member, StructOrUnionTypeData, Type, TypeStore};
use crate::utils::{MAX_FUNC_PARAMS, VA_AREA_SIZE};

/// Declaration of a function parameter.
struct Parameter {
    name: Option<SmolStr>,
    ty: Type,
    offset: usize,
}

/// An object declarator.
struct Declarator {
    name: Option<SmolStr>,
    ty: Type,
    /// The byte offset of the declarator in the source code.
    offset: usize,
    /// The parameter declarations for a function declarator.
    ///
    /// This keeps parameter names alongside the semantic function type in `ty`.
    /// Non-function declarators leave it empty and it is necessary to check
    /// that `ty` is a function type before using this field.
    params: Vec<Parameter>,
}

/// A variable initializer.
struct Initializer {
    ty: Type,
    kind: InitializerKind,
}

/// One step in the path from a local object to a nested initializer target.
enum InitializerStep {
    Index(usize),
    Member(Member),
}

/// The compile-time value model for global initializers.
enum GlobalInitValue {
    Num(ConstValue),
    /// Relocation against the given label with the given addend/offset.
    Reloc(SmolStr, i64),
}

/// The specific initializer form carried by [`Initializer`].
enum InitializerKind {
    /// The initialization expression for non-aggregate types.
    Expr(Node),
    /// Nested initializers for aggregate types, e.g., array, struct.
    Aggregate(Vec<Initializer>),
}

/// A storage class specifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
enum StorageClass {
    Typedef,
    Static,
    Extern,
    Auto,
    Register,
}

/// The parsing context for declaration specifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
enum DeclspecContext {
    #[strum(serialize = "file-scope declaration")]
    FileScopeDecl,
    #[strum(serialize = "block-scope declaration")]
    BlockScopeDecl,
    #[strum(serialize = "typename")]
    Typename,
    #[strum(serialize = "parameter declaration")]
    ParameterDecl,
    #[strum(serialize = "for loop initializer")]
    ForLoopInitializer,
    #[strum(serialize = "{0} member declaration")]
    MemberDecl(&'static str),
}

/// A declaration specifier.
struct Declspec {
    ty: Type,
    align: Option<u64>,
    storage_class: Option<StorageClass>,
    noreturn: bool,
}

/// An ordinary identifier.
#[derive(Debug, Copy, Clone)]
enum OrdinaryIdent {
    Local(usize),
    /// A global variable.
    ///
    /// The second argument represents whether it has linkage, i.e., whether it
    /// can be declared via "extern".
    Global(usize, bool),
    Function(usize),
    Typedef(Type),
    Enumerator(ConstValue),
}

impl OrdinaryIdent {
    fn into_typedef(self) -> Option<Type> {
        match self {
            OrdinaryIdent::Typedef(ty) => Some(ty),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct SwitchContext {
    ty: ConstType,
    cases: Vec<(ConstValue, SmolStr)>,
    default: Option<SmolStr>,
}

/// A scope frame.
#[derive(Debug, Default)]
struct ScopeFrame {
    /// The namespace of ordinary identifiers.
    idents: FxHashMap<SmolStr, OrdinaryIdent>,
    /// The namespace of struct, union, and enum tags.
    tags: FxHashMap<SmolStr, Type>,
}

/// Stateful parser over the token stream during parsing.
pub struct Parser<'a> {
    source: &'a Source,
    tokens: Vec<Token>,
    /// Whether to follow preprocessor rules for certain constructs.
    preprocess: bool,

    // Mutable states
    pos: usize,
    types: TypeStore,
    locals: Vec<LocalVar>,
    functions: Vec<Function>,
    /// The index of the function currently being parsed.
    active_function: Option<usize>,
    active_brk_label: Option<SmolStr>,
    active_cont_label: Option<SmolStr>,
    active_switch: Option<SwitchContext>,
    globals: Vec<GlobalVar>,
    scopes: Vec<ScopeFrame>,
    next_unique_label: usize,
    speculate_depth: usize,
}

impl<'a> Parser<'a> {
    /// Create a parser over a token stream.
    pub fn new(source: &'a Source, tokens: Vec<Token>, preprocess: bool) -> Self {
        debug_assert!(
            tokens.last().is_some_and(|t| t.is_eof()),
            "token stream must end with an EOF sentinel",
        );

        Self {
            source,
            tokens,
            preprocess,
            pos: 0,
            types: TypeStore::default(),
            locals: Vec::new(),
            functions: Vec::new(),
            active_function: None,
            active_brk_label: None,
            active_cont_label: None,
            active_switch: None,
            globals: Vec::new(),
            scopes: vec![ScopeFrame::default()],
            next_unique_label: 0,
            speculate_depth: 0,
        }
    }

    /// Advance to the next token.
    fn advance(&mut self) {
        self.pos += 1;
    }

    /// Return the current token.
    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    /// Return the token at the given lookahead distance.
    fn peek(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.pos + offset)
    }

    /// Return an error diagnostic at the current token.
    fn error_current(&self, message: impl Into<SmolStr>) -> Error {
        self.source.error_at(self.current().offset, message)
    }

    /// Emit a warning message at the given byte offset.
    fn warn_at(&self, offset: usize, message: impl Into<SmolStr>) {
        if self.speculate_depth == 0 {
            self.source.warn_at(offset, message);
        }
    }

    /// Emit a warning message at the current token.
    fn warn_current(&self, message: impl Into<SmolStr>) {
        self.warn_at(self.current().offset, message);
    }

    /// Generate a unique label.
    fn unique_label(&mut self) -> SmolStr {
        let label = format_smolstr!(".L..{}", self.next_unique_label);
        self.next_unique_label += 1;
        label
    }

    /// Assume and skip a specific punctuator.
    fn skip_punct(&mut self, expected: &str) -> Result<()> {
        if !self.current().is_punct(expected) {
            return Err(self.error_current(format_smolstr!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// Assume and skip a specific keyword.
    fn skip_keyword(&mut self, expected: Keyword) -> Result<()> {
        if !self.current().is_keyword(expected) {
            return Err(self.error_current(format_smolstr!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// Assume and consume an identifier.
    fn consume_ident(&mut self) -> Result<SmolStr> {
        let Some(ident) = self.current().as_ident() else {
            return Err(self.error_current("expected an identifier"));
        };
        self.advance();
        Ok(ident)
    }

    /// Maybe skip the end of a braced list.
    ///
    /// A braced list ends with either "}" or "," + "}". If we are not at such
    /// an end sequence, this returns false and does nothing. Otherwise, this
    /// returns true, and optionally skips over that sequence if `skip` is true.
    fn maybe_skip_list_end(&mut self, skip: bool) -> bool {
        if self.current().is_punct("}") {
            if skip {
                self.advance();
            }
            return true;
        }

        if self.current().is_punct(",") && self.peek(1).is_some_and(|tok| tok.is_punct("}")) {
            if skip {
                self.advance();
                self.advance();
            }
            return true;
        }

        false
    }

    /// Assume and skip the end of a braced list.
    ///
    /// This is similar to [`maybe_skip_list_end`], but it assumes that we are
    /// at such an end sequence and always skips it, erroring if we are not.
    ///
    /// [`maybe_skip_list_end`]: Self::maybe_skip_list_end
    fn skip_list_end(&mut self) -> Result<()> {
        if !self.maybe_skip_list_end(true) {
            return Err(self.error_current("expected '}'"));
        }
        Ok(())
    }

    /// Return whether the current token can be interpreted as a typename.
    fn at_typename(&self) -> bool {
        if self.current().is_typename_keyword() {
            return true;
        }
        let Some(name) = self.current().as_ident() else {
            return false;
        };
        self.find_ident(&name)
            .and_then(OrdinaryIdent::into_typedef)
            .is_some()
    }

    /// Run a parser operation speculatively.
    ///
    /// All read operations on the parser states allowed. Mutation is only
    /// allowed for:
    ///
    /// - Mutating position;
    /// - Appending transient types to the type store;
    /// - Mutating the outermost scope frame, or appending new frames;
    ///
    /// The parser state will be rolled back when the callback completes. The
    /// rollback is valid only if the rules above are respected. Returns both
    /// the operation result and the token position that was reached before the
    /// checkpoint was restored.
    fn speculate<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<(T, usize)> {
        // Push an extra top scope frame to hold e.g. transient tags introduced
        // during speculative parsing, while preserving access to outer scopes
        self.scopes.push(ScopeFrame::default());

        let saved_pos = self.pos;
        let saved_types_len = self.types.len();
        let saved_locals_len = self.locals.len();
        let saved_functions_len = self.functions.len();
        let saved_globals_len = self.globals.len();
        let saved_scope_depth = self.scopes.len();

        #[cfg(debug_assertions)]
        let (saved_active_function, saved_active_brk_label, saved_active_cont_label) = (
            self.active_function,
            self.active_brk_label.clone(),
            self.active_cont_label.clone(),
        );

        self.speculate_depth += 1;

        let result = f(self).map(|value| (value, self.pos));

        debug_assert!(self.speculate_depth > 0, "speculation state is broken");
        debug_assert!(
            self.types.len() >= saved_types_len,
            "cannot remove pre-existing types during speculation",
        );
        debug_assert!(
            self.locals.len() >= saved_locals_len,
            "cannot remove pre-existing locals during speculation",
        );
        debug_assert!(
            self.functions.len() >= saved_functions_len,
            "cannot remove pre-existing functions during speculation",
        );
        debug_assert!(
            self.globals.len() >= saved_globals_len,
            "cannot remove pre-existing globals during speculation",
        );
        debug_assert!(
            self.scopes.len() >= saved_scope_depth,
            "cannot pop more scope frames than appended during speculation",
        );

        #[cfg(debug_assertions)]
        {
            debug_assert!(
                self.active_function == saved_active_function,
                "cannot change active function during speculation",
            );
            debug_assert!(
                self.active_brk_label == saved_active_brk_label,
                "cannot change active break label during speculation",
            );
            debug_assert!(
                self.active_cont_label == saved_active_cont_label,
                "cannot change active continue label during speculation",
            );
        }

        self.pos = saved_pos;
        self.types.truncate(saved_types_len);
        self.locals.truncate(saved_locals_len);
        self.functions.truncate(saved_functions_len);
        self.globals.truncate(saved_globals_len);
        self.scopes.truncate(saved_scope_depth);

        #[cfg(debug_assertions)]
        {
            self.active_function = saved_active_function;
            self.active_brk_label = saved_active_brk_label;
            self.active_cont_label = saved_active_cont_label;
        }

        self.speculate_depth -= 1;

        self.scopes.pop(); // Pop the extra frame we inserted
        result
    }

    fn disallow_speculation(&self) {
        debug_assert_eq!(
            self.speculate_depth, 0,
            "this operation is not allowed during parser speculation"
        );
    }

    /// ```bnf
    /// <declspec> ::= <declspec-atom>+
    /// <declspec-atom> ::=
    ///   "typedef"
    ///   | "static"
    ///   | "extern"
    ///   | "auto"
    ///   | "register"
    ///   | "void"
    ///   | "_Bool"
    ///   | "char"
    ///   | "short"
    ///   | "int"
    ///   | "long"
    ///   | "float"
    ///   | "double"
    ///   | "signed"
    ///   | "unsigned"
    ///   | "const"
    ///   | "volatile"
    ///   | "restrict"
    ///   | "_Noreturn"
    ///   | "_Alignas" "(" (<typename> | <constexpr>) ")"
    ///   | <struct-or-union-decl>
    ///   | <typedef-name>
    ///   | <enum-specifier>
    /// ```
    ///
    /// As per C language specification, type specifiers are order-insensitive,
    /// but only certain combinations are legal.
    fn parse_declspec(&mut self, context: DeclspecContext) -> Result<Declspec> {
        #[derive(Debug)]
        enum TypeSpec {
            Void,
            Bool,
            Char,
            Short,
            Int,
            Long,
            Float,
            Double,
            Other(Type),
        }

        let mut spec = None;
        let mut long_count = 0;
        let mut signed = None;
        let mut storage_class = None;
        let mut align = None;
        let mut noreturn = false;
        let mut defaults_to_int = false;

        while self.at_typename() {
            let offset = self.current().offset;
            let keyword = self.current().as_keyword();
            let ident = self.current().as_ident();
            let typedef_ty = ident
                .and_then(|ident| self.find_ident(&ident))
                .and_then(OrdinaryIdent::into_typedef);

            if spec.is_some() && typedef_ty.is_some() {
                // There is already a type specifier, so another ident, even if
                // it can be interpreted as a typedef name, we should not treat
                // it as part of the declspec but rather break before advance to
                // let other parsing logic handle it
                break;
            }

            self.advance();

            macro_rules! bail_multiple_types {
                () => {
                    return Err(self
                        .source
                        .error_at(offset, "multiple types in declaration specifiers"))
                };
            }

            let Some(keyword) = keyword else {
                if let Some(ty) = typedef_ty {
                    if spec.is_some() || signed.is_some() {
                        bail_multiple_types!();
                    }
                    spec = Some(TypeSpec::Other(ty));
                }
                continue;
            };

            match keyword {
                Keyword::Void => match spec {
                    None if signed.is_none() => spec = Some(TypeSpec::Void),
                    _ => bail_multiple_types!(),
                },
                Keyword::Bool => match spec {
                    None if signed.is_none() => spec = Some(TypeSpec::Bool),
                    _ => bail_multiple_types!(),
                },
                Keyword::Char => match spec {
                    None => spec = Some(TypeSpec::Char),
                    _ => bail_multiple_types!(),
                },
                Keyword::Short => match spec {
                    None | Some(TypeSpec::Int) => spec = Some(TypeSpec::Short),
                    _ => bail_multiple_types!(),
                },
                Keyword::Int => match spec {
                    None => spec = Some(TypeSpec::Int),
                    Some(TypeSpec::Short | TypeSpec::Long) => {},
                    _ => bail_multiple_types!(),
                },
                Keyword::Long => match spec {
                    None | Some(TypeSpec::Int) | Some(TypeSpec::Long) if long_count < 2 => {
                        spec = Some(TypeSpec::Long);
                        long_count += 1;
                    },
                    _ => bail_multiple_types!(),
                },
                Keyword::Float => match spec {
                    None if signed.is_none() => spec = Some(TypeSpec::Float),
                    _ => bail_multiple_types!(),
                },
                Keyword::Double => match spec {
                    None if signed.is_none() => spec = Some(TypeSpec::Double),
                    Some(TypeSpec::Long) if signed.is_none() => spec = Some(TypeSpec::Double),
                    _ => bail_multiple_types!(),
                },
                Keyword::Struct => match spec {
                    None if signed.is_none() => {
                        spec = Some(TypeSpec::Other(
                            self.parse_struct_or_union_decl(true, context)?,
                        ))
                    },
                    _ => bail_multiple_types!(),
                },
                Keyword::Union => match spec {
                    None if signed.is_none() => {
                        spec = Some(TypeSpec::Other(
                            self.parse_struct_or_union_decl(false, context)?,
                        ))
                    },
                    _ => bail_multiple_types!(),
                },
                Keyword::Enum => match spec {
                    None if signed.is_none() => {
                        spec = Some(TypeSpec::Other(self.parse_enum_specifier()?))
                    },
                    _ => bail_multiple_types!(),
                },
                Keyword::Signed => {
                    if signed.is_some() {
                        bail_multiple_types!();
                    }
                    signed = Some(true);
                    defaults_to_int = true;
                },
                Keyword::Unsigned => {
                    if signed.is_some() {
                        bail_multiple_types!();
                    }
                    signed = Some(false);
                    defaults_to_int = true;
                },
                Keyword::Typedef
                | Keyword::Static
                | Keyword::Extern
                | Keyword::Auto
                | Keyword::Register => {
                    let allowed = matches!(
                        (keyword, context),
                        (
                            Keyword::Typedef | Keyword::Static | Keyword::Extern,
                            DeclspecContext::FileScopeDecl | DeclspecContext::BlockScopeDecl,
                        ) | (
                            Keyword::Auto,
                            DeclspecContext::BlockScopeDecl | DeclspecContext::ForLoopInitializer,
                        ) | (
                            Keyword::Register,
                            DeclspecContext::BlockScopeDecl
                                | DeclspecContext::ForLoopInitializer
                                | DeclspecContext::ParameterDecl,
                        )
                    );
                    if !allowed {
                        return Err(self.source.error_at(
                            offset,
                            format_smolstr!("storage class specifier is not allowed in {context}"),
                        ));
                    }
                    if storage_class.is_some() {
                        return Err(self.source.error_at(
                            offset,
                            "multiple storage classes in declaration specifiers",
                        ));
                    }
                    storage_class = Some(match keyword {
                        Keyword::Typedef => StorageClass::Typedef,
                        Keyword::Static => StorageClass::Static,
                        Keyword::Extern => StorageClass::Extern,
                        Keyword::Auto => StorageClass::Auto,
                        Keyword::Register => StorageClass::Register,
                        _ => unreachable!(),
                    });
                    defaults_to_int = true;
                },
                Keyword::Noreturn => {
                    if !matches!(
                        context,
                        DeclspecContext::FileScopeDecl
                            | DeclspecContext::BlockScopeDecl
                            | DeclspecContext::ParameterDecl
                            | DeclspecContext::ForLoopInitializer
                    ) {
                        return Err(self.source.error_at(
                            offset,
                            format_smolstr!("'_Noreturn' is not allowed in {context}"),
                        ));
                    }
                    noreturn = true;
                    defaults_to_int = true;
                },
                Keyword::Alignas => {
                    if !matches!(
                        context,
                        DeclspecContext::FileScopeDecl
                            | DeclspecContext::BlockScopeDecl
                            | DeclspecContext::MemberDecl(_)
                    ) {
                        return Err(self.source.error_at(
                            offset,
                            format_smolstr!("'_Alignas' is not allowed in {context}"),
                        ));
                    }
                    self.skip_punct("(")?;
                    align = align.max(Some(if self.at_typename() {
                        let ty = self.parse_typename()?;
                        self.types.align(ty)
                    } else {
                        u64::try_from(self.parse_constexpr()?).map_err(|_| {
                            self.error_current("constant expression is out of range")
                        })?
                    }));
                    self.skip_punct(")")?;
                },
                Keyword::Const | Keyword::Volatile | Keyword::Restrict => {
                    defaults_to_int = true;
                },
                _ => unreachable!("all keyword tokens should have been handled"),
            }
        }

        if spec.is_none() && defaults_to_int {
            if signed.is_none() {
                // "signed/unsigned x" is valid without warning
                self.warn_current("missing type specifier, defaults to 'int'");
            }
            spec = Some(TypeSpec::Int);
        }

        let Some(spec) = spec else {
            return Err(self.error_current("expected a typename"));
        };

        let ty = match (spec, signed) {
            (TypeSpec::Void, _) => Type::Void,
            (TypeSpec::Bool, _) => Type::BOOL,
            (TypeSpec::Char, Some(false)) => Type::UCHAR,
            (TypeSpec::Char, _) => Type::CHAR,
            (TypeSpec::Short, Some(false)) => Type::USHORT,
            (TypeSpec::Short, _) => Type::SHORT,
            (TypeSpec::Int, Some(false)) => Type::UINT,
            (TypeSpec::Int, _) => Type::INT,
            (TypeSpec::Long, Some(false)) => Type::ULONG,
            (TypeSpec::Long, _) => Type::LONG,
            (TypeSpec::Float, _) => Type::FLOAT,
            (TypeSpec::Double, _) => Type::DOUBLE,
            (TypeSpec::Other(ty), _) => ty,
        };

        Ok(Declspec {
            ty,
            align,
            storage_class,
            noreturn,
        })
    }

    /// ```bnf
    /// <declarator> ::= <pointers> (<ident>? | "(" <declarator> ")") <type-suffix>
    /// ```
    fn parse_declarator(&mut self, mut ty: Type, in_param: bool) -> Result<Declarator> {
        ty = self.parse_pointers(ty);

        if self.current().is_punct("(") {
            self.advance();
            let inner_pos = self.pos; // After "("

            // Try to parse the inner declarator to find where it ends, i.e.,
            // the matching ")"
            let (_, next_pos) = self.speculate(|parser| {
                parser.parse_declarator(Default::default(), in_param)?;
                parser.skip_punct(")")?;
                Ok(())
            })?;

            // Parse the type suffix after ")"
            self.pos = next_pos;
            let (ty, params) = self.parse_type_suffix(ty, in_param)?;
            let next_pos = self.pos;

            // Rewind to parse the inner declarator again, this time with the
            // real type; we don't go through the type suffix again but rather
            // directly take its params
            self.pos = inner_pos;
            let mut declarator = self.parse_declarator(ty, in_param)?;
            if !self.types.is_func(declarator.ty) {
                declarator.params = params;
            }
            self.pos = next_pos;
            return Ok(declarator);
        }

        let offset = self.current().offset;
        let name = self.current().as_ident();
        if name.is_some() {
            self.advance();
        }
        let (ty, params) = self.parse_type_suffix(ty, in_param)?;

        Ok(Declarator {
            name,
            ty,
            offset,
            params,
        })
    }

    /// ```bnf
    /// <abstract-declarator> ::=
    ///   <pointers> ("(" <abstract-declarator> ")")? <type-suffix>
    /// ```
    fn parse_abstract_declarator(&mut self, mut ty: Type, in_param: bool) -> Result<Type> {
        ty = self.parse_pointers(ty);

        // The following part of logic is analogous to "parse_declarator"
        if self.current().is_punct("(") {
            self.advance();
            let inner_pos = self.pos;

            let (_, next_pos) = self.speculate(|parser| {
                parser.parse_abstract_declarator(Default::default(), in_param)?;
                parser.skip_punct(")")?;
                Ok(())
            })?;

            self.pos = next_pos;
            let (ty, _) = self.parse_type_suffix(ty, in_param)?;
            let next_pos = self.pos;

            self.pos = inner_pos;
            let ty = self.parse_abstract_declarator(ty, in_param)?;
            self.pos = next_pos;
            return Ok(ty);
        }

        let (ty, _) = self.parse_type_suffix(ty, in_param)?;
        Ok(ty)
    }

    /// ```bnf
    /// <pointers> ::= ("*" ("const" | "volatile" | "restrict")*)*
    /// ```
    fn parse_pointers(&mut self, mut ty: Type) -> Type {
        while self.current().is_punct("*") {
            self.advance();
            ty = self.types.ptr(ty);

            while self.current().as_keyword().is_some_and(|kw| {
                matches!(kw, Keyword::Const | Keyword::Volatile | Keyword::Restrict)
            }) {
                self.advance();
            }
        }
        ty
    }

    /// ```bnf
    /// <typename> ::= <declspec> <abstract-declarator>
    /// ```
    fn parse_typename(&mut self) -> Result<Type> {
        let declspec = self.parse_declspec(DeclspecContext::Typename)?;
        self.parse_abstract_declarator(declspec.ty, false)
    }

    /// ```bnf
    /// <type-suffix> ::= "(" <func-params> | <array-dimensions>
    /// ```
    fn parse_type_suffix(&mut self, ty: Type, in_param: bool) -> Result<(Type, Vec<Parameter>)> {
        if self.current().is_punct("(") {
            self.advance();
            return self.parse_func_params(ty);
        }

        let ty = self.parse_array_dimensions(ty, in_param)?;
        Ok((ty, Vec::new()))
    }

    /// ```bnf
    /// <func-params> ::= ("void" | <param> ("," <param>)* ("," "...")?)? ")"
    /// <param> ::= <declspec> <declarator>
    /// ```
    fn parse_func_params(&mut self, return_ty: Type) -> Result<(Type, Vec<Parameter>)> {
        let mut params = Vec::new();
        let mut param_names = FxHashSet::default();
        let mut is_variadic = false;

        while !self.current().is_punct(")") {
            if !params.is_empty() {
                self.skip_punct(",")?;
            }

            if self.current().is_punct("...") {
                is_variadic = true;
                self.advance();
                break;
            }

            let offset = self.current().offset;
            let declspec = self.parse_declspec(DeclspecContext::ParameterDecl)?;

            if params.is_empty() && declspec.ty == Type::Void {
                if self.current().is_punct(")") {
                    self.advance();
                    return Ok((self.types.func(return_ty, Vec::new(), false), Vec::new()));
                }
                if self.current().is_punct(",") {
                    return Err(self
                        .source
                        .error_at(offset, "'void' must be the only parameter"));
                }
            }

            let offset = self.current().offset;
            let declarator = self.parse_declarator(declspec.ty, true)?;
            if declspec.noreturn {
                self.warn_at(declarator.offset, "parameter declared '_Noreturn'");
            }

            let ty = if let Some(array) = self.types.as_array(declarator.ty) {
                // Array decay will convert "array of T" to "pointer to T" in
                // parameter declarations; e.g., "*argv[]" being converted to
                // "**argv" is because of this rule
                self.types.ptr(array.base)
            } else if self.types.is_func(declarator.ty) {
                // Likewise, a function in a parameter would be decayed to a
                // pointer to that function
                self.types.ptr(declarator.ty)
            } else {
                declarator.ty
            };

            if self.types.is_incomplete(ty) {
                return Err(self
                    .source
                    .error_at(offset, "parameter has incomplete type"));
            }

            if let Some(name) = &declarator.name
                && !param_names.insert(name.clone())
            {
                return Err(self.source.error_at(offset, "redefinition of parameter"));
            }

            params.push(Parameter {
                name: declarator.name,
                ty,
                offset: declarator.offset,
            });

            if params.len() > MAX_FUNC_PARAMS {
                return Err(self
                    .source
                    .error_at(declarator.offset, "too many parameters"));
            }
        }

        self.skip_punct(")")?;
        let param_tys = params.iter().map(|param| param.ty).collect();
        Ok((self.types.func(return_ty, param_tys, is_variadic), params))
    }

    /// ```bnf
    /// <array-dimensions> ::= ("[" <array-qualifier>* <constexpr>? "]")*
    /// <array-qualifier> ::= "static" | "const" | "volatile" | "restrict"
    /// ```
    fn parse_array_dimensions(&mut self, ty: Type, in_param: bool) -> Result<Type> {
        if !self.current().is_punct("[") {
            return Ok(ty);
        }

        let offset = self.current().offset;
        self.advance();

        let mut static_offset = None;
        while let Some(keyword) = self.current().as_keyword() {
            if !matches!(
                keyword,
                Keyword::Static | Keyword::Const | Keyword::Volatile | Keyword::Restrict
            ) {
                break;
            }

            if !in_param {
                return Err(
                    self.error_current("'static' or qualifiers in non-parameter array declarator")
                );
            }

            if keyword == Keyword::Static {
                if static_offset.is_some() {
                    return Err(
                        self.error_current("duplicate 'static' in array parameter declarator")
                    );
                }
                static_offset = Some(self.current().offset);
            }
            self.advance();
        }

        let len = if self.current().is_punct("]") {
            if let Some(offset) = static_offset {
                return Err(self.source.error_at(
                    offset,
                    "array parameter declared 'static' but bound is missing",
                ));
            }
            None
        } else {
            let len = self.parse_constexpr()?;
            let Ok(len) = usize::try_from(len) else {
                return Err(self.error_current("array size is negative or out of range"));
            };
            Some(len)
        };

        self.skip_punct("]")?;

        let ty = self.parse_array_dimensions(ty, in_param)?;
        if self.types.is_incomplete(ty) {
            return Err(self
                .source
                .error_at(offset, "array element type is incomplete"));
        }
        if self.types.is_func(ty) {
            return Err(self
                .source
                .error_at(offset, "array element type cannot be function"));
        }

        Ok(self.types.array(ty, len))
    }

    /// ```bnf
    /// <struct-or-union-decl> ::= <ident> | <ident>? "{" <members-decl>
    /// ```
    fn parse_struct_or_union_decl(
        &mut self,
        is_struct: bool,
        context: DeclspecContext,
    ) -> Result<Type> {
        let offset = self.current().offset;
        let tag = self.current().as_ident();

        let repr = || if is_struct { "struct" } else { "union" };
        let member_context = DeclspecContext::MemberDecl(repr());

        if let Some(tag) = tag {
            self.advance();

            if !self.current().is_punct("{") {
                if self.current().is_punct(";")
                    && matches!(
                        context,
                        DeclspecContext::FileScopeDecl | DeclspecContext::BlockScopeDecl
                    )
                {
                    // "struct T;" in file/block scope, which is a forward
                    // declaration within the same scope
                    let Some(ty) = self.find_tag_current(&tag) else {
                        let ty = self.types.struct_or_union(is_struct, None);
                        self.push_scope_tag(tag, ty);
                        return Ok(ty);
                    };

                    if let Some(sou) = self.types.as_struct_or_union(ty)
                        && sou.is_struct == is_struct
                    {
                        return Ok(ty);
                    }

                    return Err(self
                        .source
                        .error_at(offset, format_smolstr!("defined as wrong kind of tag")));
                }

                // "struct T" not followed by "{" or ";"; which is a tag use
                // rather than a declaration, so we should look it up not only
                // in the current scope; only if it is not found in any visible
                // scope should we introduce a new incomplete type
                let Some(ty) = self.find_tag(&tag) else {
                    let ty = self.types.struct_or_union(is_struct, None);
                    self.push_scope_tag(tag, ty);
                    return Ok(ty);
                };

                if let Some(sou) = self.types.as_struct_or_union(ty)
                    && sou.is_struct == is_struct
                {
                    return Ok(ty);
                }

                return Err(self
                    .source
                    .error_at(offset, format_smolstr!("defined as wrong kind of tag")));
            }

            // "struct T {...}", which is a concrete definition
            let Some(ty) = self.find_tag_current(&tag) else {
                // Note: We have to create an incomplete type first, then parse
                // the members and complete the type; this is to handle self-
                // referential structs, so members can see that this type is
                // already declared and will not create a separate declaration
                let ty = self.types.struct_or_union(is_struct, None);
                self.push_scope_tag(tag, ty);
                self.advance();
                let members = self.parse_members_decl(is_struct, member_context)?;
                self.types.complete_struct_or_union(is_struct, members, ty);
                return Ok(ty);
            };

            if let Some(sou) = self.types.as_struct_or_union(ty)
                && sou.is_struct == is_struct
            {
                if !self.types.is_incomplete(ty) {
                    return Err(self
                        .source
                        .error_at(offset, format_smolstr!("redefinition of {} tag", repr())));
                }

                self.advance();
                let members = self.parse_members_decl(is_struct, member_context)?;
                self.types.complete_struct_or_union(is_struct, members, ty);
                return Ok(ty);
            }

            return Err(self
                .source
                .error_at(offset, format_smolstr!("defined as wrong kind of tag")));
        }

        // "struct {...}", which is an anonymous definition
        self.skip_punct("{")?;
        let members = self.parse_members_decl(is_struct, member_context)?;
        let ty = self.types.struct_or_union(is_struct, Some(members));
        Ok(ty)
    }

    /// ```bnf
    /// <members-decl> ::= <member-decl>* "}"
    /// <member-decl> ::= <declspec> <declarator> ("," <declarator>)* ";"
    /// ```
    fn parse_members_decl(
        &mut self,
        is_struct: bool,
        context: DeclspecContext,
    ) -> Result<Vec<Member>> {
        let mut members = Vec::new();

        // (is_flexible_array, offset)
        let mut pending_incomplete = None::<(bool, usize)>;

        while !self.current().is_punct("}") {
            let declspec = self.parse_declspec(context)?;
            if let Some((is_flexible_array, offset)) = pending_incomplete.take() {
                if !is_flexible_array {
                    return Err(self.source.error_at(offset, "field has incomplete type"));
                }
                if !is_struct {
                    return Err(self
                        .source
                        .error_at(offset, "flexible array member in union"));
                }
                return Err(self
                    .source
                    .error_at(offset, "flexible array member not at end of struct"));
            }

            if self.current().is_punct(";") {
                self.warn_current("declaration does not declare anything");
                self.advance();
                continue;
            }
            if self.current().is_punct("}") {
                return Err(self.error_current("expected ';'"));
            }

            loop {
                let declarator = self.parse_declarator(declspec.ty, false)?;
                if self.types.is_incomplete(declarator.ty) {
                    pending_incomplete = Some((
                        self.types.as_array(declarator.ty).is_some(),
                        declarator.offset,
                    ));
                }

                let Some(name) = declarator.name else {
                    return Err(self
                        .source
                        .error_at(declarator.offset, "missing member name"));
                };

                members.push(Member {
                    name,
                    ty: declarator.ty,
                    align: declspec.align,
                    offset: 0, // union requires 0; struct fills in later
                });

                if self.current().is_punct(",") {
                    self.advance();
                    continue;
                }
                if self.current().is_punct(";") {
                    self.advance();
                    break;
                }
                if self.current().is_punct("}") {
                    return Err(self.error_current("expected ';'"));
                }
                return Err(self.error_current("expected ',' or ';'"));
            }
        }

        self.advance();

        if let Some((is_flexible_array, offset)) = pending_incomplete.take() {
            if !is_flexible_array {
                return Err(self.source.error_at(offset, "field has incomplete type"));
            }
            if !is_struct {
                return Err(self
                    .source
                    .error_at(offset, "flexible array member in union"));
            }
            if members.len() <= 1 {
                return Err(self.source.error_at(
                    offset,
                    "flexible array member in a struct with no named members",
                ));
            }
        }

        Ok(members)
    }

    /// ```bnf
    /// <enum-specifier> ::= <ident>? "{" <enum-list>? "}" | <ident>
    /// <enum-list> ::=
    ///   <ident> ("=" <constexpr>)? ("," <ident> ("=" <constexpr>)?)* ","?
    /// ```
    fn parse_enum_specifier(&mut self) -> Result<Type> {
        let offset = self.current().offset;
        let tag = self.current().as_ident();

        if let Some(ref tag) = tag {
            self.advance();
            if !self.current().is_punct("{") {
                let Some(ty) = self.find_tag(tag) else {
                    return Err(self.source.error_at(offset, "unknown enum type"))?;
                };
                if ty != Type::Enum {
                    return Err(self.source.error_at(offset, "not an enum tag"));
                }
                return Ok(ty);
            }

            if let Some(ty) = self.find_tag_current(tag) {
                if ty == Type::Enum {
                    return Err(self.source.error_at(offset, "redeclaration of enum tag"));
                }
                return Err(self.source.error_at(offset, "defined as wrong kind of tag"));
            }
        }

        self.skip_punct("{")?;

        let mut first = true;
        let mut val = ConstValue::int(0, ConstType::INT);
        while !self.maybe_skip_list_end(true) {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let offset = self.current().offset;
            let name = self.consume_ident()?;
            if self.current().is_punct("=") {
                self.advance();
                val = self.parse_constexpr()?;
            }

            if let Some(ident) = self.find_ident_current(&name) {
                return match ident {
                    OrdinaryIdent::Enumerator(_) => {
                        Err(self.source.error_at(offset, "redeclaration of enumerator"))
                    },
                    _ => Err(self
                        .source
                        .error_at(offset, "redeclared as a different kind of symbol")),
                };
            }

            self.push_scope_ident(name.to_smolstr(), OrdinaryIdent::Enumerator(val));
            val = val.add(ConstValue::int(1, val.ty), val.ty);
        }

        let ty = Type::Enum;
        if let Some(tag) = tag {
            self.push_scope_tag(tag, ty);
        }
        Ok(ty)
    }

    /// ```bnf
    /// <expr> ::= <assign> ("," <expr>)?
    /// ```
    fn parse_expr(&mut self) -> Result<Node> {
        let node = self.parse_assign()?;

        if self.current().is_punct(",") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::comma(node, self.parse_expr()?, offset));
        }

        Ok(node)
    }

    /// ```bnf
    /// <program> ::= (<typedef> | <function> | <global-variable>)* <eof>
    /// ```
    pub fn parse_program(&mut self) -> Result<Program> {
        self.disallow_speculation();

        while !self.current().is_eof() {
            let declspec = self.parse_declspec(DeclspecContext::FileScopeDecl)?;
            if declspec.storage_class == Some(StorageClass::Typedef) {
                self.parse_typedef_tail(declspec.ty, declspec.noreturn)?;
                continue;
            }

            if self.at_function()? {
                self.parse_function(
                    declspec.ty,
                    declspec.storage_class == Some(StorageClass::Static),
                    declspec.noreturn,
                )?;
                continue;
            }

            self.parse_global_variable(declspec)?;
        }

        Ok(Program {
            types: std::mem::take(&mut self.types),
            functions: std::mem::take(&mut self.functions),
            globals: std::mem::take(&mut self.globals),
        })
    }

    /// Lookahead to determine whether we are at a [`<function>`].
    ///
    /// [`<function>`]: Self::parse_function
    fn at_function(&mut self) -> Result<bool> {
        if self.current().is_punct(";") {
            return Ok(false);
        }

        let (result, _) = self.speculate(|parser| {
            let declarator = parser.parse_declarator(Default::default(), false)?;
            Ok(parser.types.is_func(declarator.ty))
        })?;
        Ok(result)
    }

    /// ```bnf
    /// <function> ::= <declarator> (";" | "{" <compound-stmt>)
    /// ```
    fn parse_function(&mut self, return_ty: Type, is_static: bool, noreturn: bool) -> Result<()> {
        self.disallow_speculation();

        let declarator = self.parse_declarator(return_ty, false)?;
        let Some(name) = declarator.name.clone() else {
            return Err(self
                .source
                .error_at(declarator.offset, "missing function name"));
        };
        let Some(func_ty) = self.types.as_func(declarator.ty, false) else {
            return Err(self.error_current("expected a function"));
        };
        let is_variadic = func_ty.is_variadic;

        let func_id = self.declare_function(name, declarator.ty, noreturn, declarator.offset)?;
        if self.current().is_punct(";") {
            self.advance();
            return Ok(());
        }
        if self.functions[func_id].body.is_some() {
            return Err(self
                .source
                .error_at(declarator.offset, "redefinition of function"));
        }

        self.active_function = Some(func_id);

        let body_offset = self.current().offset;
        self.locals.clear();
        self.enter_scope();

        let param_locals = self.create_param_locals(declarator.params)?;

        let va_area_local = if is_variadic {
            let ty = self.types.array(Type::CHAR, Some(VA_AREA_SIZE));
            Some(self.create_local("__va_area__", ty, None))
        } else {
            None
        };

        self.skip_punct("{")?;
        let mut body = Stmt::block(self.parse_compound_stmt()?, body_offset);

        self.leave_scope();

        {
            let mut labels = FxHashMap::default();
            self.collect_labels_stmt(&body, &mut labels)?;
            self.resolve_gotos_stmt(&mut body, &labels)?;
        }

        let function = &mut self.functions[func_id];
        function.body = Some(body);
        function.param_locals = param_locals;
        function.va_area_local = va_area_local;
        function.locals = std::mem::take(&mut self.locals);
        function.is_static = is_static;

        self.active_function = None;
        Ok(())
    }

    /// ```bnf
    /// <global-variable> ::= <declarator-init> ("," <declarator-init>)* ";"
    /// <declarator-init> ::= <declarator> ("=" <initializer>)?
    /// ```
    fn parse_global_variable(&mut self, declspec: Declspec) -> Result<()> {
        self.disallow_speculation();
        if self.current().is_punct(";") {
            self.warn_current("useless type name in empty declaration");
            self.advance();
            return Ok(());
        }

        loop {
            let declarator = self.parse_declarator(declspec.ty, false)?;
            let Some(name) = declarator.name else {
                return Err(self
                    .source
                    .error_at(declarator.offset, "missing variable name"));
            };
            if declspec.noreturn {
                self.warn_at(declarator.offset, "variable declared '_Noreturn'");
            }
            if self.types.is_func(declarator.ty) {
                return Err(self
                    .source
                    .error_at(declarator.offset, "expected a global variable"));
            }

            let mut ty = declarator.ty;
            let storage = if self.current().is_punct("=") {
                self.advance();
                let init = self.parse_initializer(ty)?;
                ty = init.ty;
                GlobalStorage::Data(self.new_global_init(init)?)
            } else if declspec.storage_class == Some(StorageClass::Extern) {
                GlobalStorage::Decl
            } else {
                GlobalStorage::Zero
            };

            let global_id = self.declare_global(
                name,
                ty,
                declspec.align,
                storage,
                declspec.storage_class == Some(StorageClass::Static),
                declarator.offset,
            )?;
            let global = &self.globals[global_id];

            if !matches!(global.storage, GlobalStorage::Decl) && self.types.is_incomplete(global.ty)
            {
                return Err(self
                    .source
                    .error_at(declarator.offset, "variable has incomplete type"));
            }

            if self.current().is_punct(",") {
                self.advance();
                continue;
            }
            if self.current().is_punct(";") {
                self.advance();
                break;
            }
            return Err(self.error_current("expected ',' or ';'"));
        }
        Ok(())
    }

    /// ```bnf
    /// <stmt> ::=
    ///   "return" <expr>? ";"
    ///   | "if" "(" <expr> ")" <stmt> ("else" <stmt>)?
    ///   | "switch" "(" <expr> ")" <stmt>
    ///   | "case" <constexpr> ":" <stmt>
    ///   | "default" ":" <stmt>
    ///   | "for" "(" <expr-stmt> <expr>? ";" <expr>? ")" <stmt>
    ///   | "while" "(" <expr> ")" <stmt>
    ///   | "do" <stmt> "while" "(" <expr> ")" ";"
    ///   | "goto" <ident> ";"
    ///   | "break" ";"
    ///   | "continue" ";"
    ///   | <ident> ":" <stmt>
    ///   | "{" <compound-stmt>
    ///   | <expr-stmt>
    /// ```
    fn parse_stmt(&mut self) -> Result<Stmt> {
        let offset = self.current().offset;

        if self.current().is_keyword(Keyword::Return) {
            self.advance();
            let func_ty = self.functions[self.active_function.unwrap()].ty;
            let return_ty = self.types.as_func(func_ty, false).unwrap().return_ty;

            if self.current().is_punct(";") {
                if return_ty != Type::Void {
                    self.warn_at(offset, "return with no value in a non-void function");
                }
                self.advance();
                return Ok(Stmt::return_(None, offset));
            }

            let mut expr = self.parse_expr()?;
            self.skip_punct(";")?;
            if return_ty == Type::Void {
                self.warn_at(expr.offset, "return with a value in a void function");
                self.apply_cast(&mut expr, Type::Void)?;
            } else {
                self.apply_cast(&mut expr, return_ty)?;
            }
            return Ok(Stmt::return_(Some(expr), offset));
        }

        if self.current().is_keyword(Keyword::If) {
            self.advance();
            self.skip_punct("(")?;
            let cond = self.parse_expr()?;
            self.skip_punct(")")?;
            let then_branch = Box::new(self.parse_stmt()?);
            let else_branch = if self.current().is_keyword(Keyword::Else) {
                self.advance();
                Some(Box::new(self.parse_stmt()?))
            } else {
                None
            };
            return Ok(Stmt::if_(cond, then_branch, else_branch, offset));
        }

        if self.current().is_keyword(Keyword::Switch) {
            self.advance();
            self.skip_punct("(")?;
            let mut cond = self.parse_expr()?;
            self.infer_type(&mut cond)?;
            self.skip_punct(")")?;

            let Some(ty) = cond
                .expect_ty()
                .promote_int()
                .and_then(|ty| self.types.to_const(ty))
            else {
                return Err(self
                    .source
                    .error_at(offset, "switch condition is not an integer"));
            };

            let brk_label = self.unique_label();
            let prev_brk_label = self.active_brk_label.replace(brk_label.clone());
            let prev_switch = self.active_switch.replace(SwitchContext {
                ty,
                cases: Vec::new(),
                default: None,
            });

            let body = self.parse_stmt()?;

            self.active_brk_label = prev_brk_label;
            let switch = std::mem::replace(&mut self.active_switch, prev_switch).unwrap();

            return Ok(Stmt::switch(
                cond,
                Box::new(body),
                switch.cases,
                switch.default,
                brk_label,
                offset,
            ));
        }

        if self.current().is_keyword(Keyword::Case) {
            self.advance();
            let Some(ty) = self.active_switch.as_ref().map(|switch| switch.ty) else {
                return Err(self
                    .source
                    .error_at(offset, "case label not within a switch"));
            };
            let val = self.parse_constexpr()?.cast(ty);
            self.skip_punct(":")?;
            let label = self.unique_label();
            let body = self.parse_stmt()?;

            match self.active_switch {
                Some(ref mut switch) => {
                    if switch.cases.iter().any(|(v, _)| *v == val) {
                        return Err(self.source.error_at(offset, "duplicate case value"));
                    }
                    switch.cases.push((val, label.clone()));
                },
                None => {
                    return Err(self
                        .source
                        .error_at(offset, "case label not within a switch"));
                },
            };
            return Ok(Stmt::case(label, Box::new(body), offset));
        }

        if self.current().is_keyword(Keyword::Default) {
            self.advance();
            self.skip_punct(":")?;
            let label = self.unique_label();
            let body = self.parse_stmt()?;

            match self.active_switch {
                Some(ref mut switch) => {
                    if switch.default.replace(label.clone()).is_some() {
                        return Err(self
                            .source
                            .error_at(offset, "multiple default labels in one switch"));
                    }
                },
                None => {
                    return Err(self
                        .source
                        .error_at(offset, "default label not within a switch"));
                },
            };
            return Ok(Stmt::case(label, Box::new(body), offset));
        }

        if self.current().is_keyword(Keyword::For) {
            self.advance();
            self.skip_punct("(")?;

            self.enter_scope();
            let brk_label = self.unique_label();
            let cont_label = self.unique_label();
            let prev_brk_label = self.active_brk_label.replace(brk_label.clone());
            let prev_cont_label = self.active_cont_label.replace(cont_label.clone());

            let init = Box::new(if self.at_typename() {
                let declspec = self.parse_declspec(DeclspecContext::ForLoopInitializer)?;
                self.parse_declaration(declspec)?
            } else {
                self.parse_expr_stmt()?
            });

            let cond = if self.current().is_punct(";") {
                None
            } else {
                Some(self.parse_expr()?)
            };
            self.skip_punct(";")?;

            let inc = if self.current().is_punct(")") {
                None
            } else {
                Some(self.parse_expr()?)
            };
            self.skip_punct(")")?;

            let body = Box::new(self.parse_stmt()?);

            self.active_brk_label = prev_brk_label;
            self.active_cont_label = prev_cont_label;
            self.leave_scope();

            return Ok(Stmt::for_(
                init, cond, inc, body, brk_label, cont_label, offset,
            ));
        }

        if self.current().is_keyword(Keyword::While) {
            self.advance();
            self.skip_punct("(")?;
            let cond = self.parse_expr()?;
            self.skip_punct(")")?;

            let brk_label = self.unique_label();
            let cont_label = self.unique_label();
            let prev_brk_label = self.active_brk_label.replace(brk_label.clone());
            let prev_cont_label = self.active_cont_label.replace(cont_label.clone());

            let body = Box::new(self.parse_stmt()?);

            self.active_brk_label = prev_brk_label;
            self.active_cont_label = prev_cont_label;

            return Ok(Stmt::while_(
                cond, body, false, brk_label, cont_label, offset,
            ));
        }

        if self.current().is_keyword(Keyword::Do) {
            self.advance();

            let brk_label = self.unique_label();
            let cont_label = self.unique_label();
            let prev_brk_label = self.active_brk_label.replace(brk_label.clone());
            let prev_cont_label = self.active_cont_label.replace(cont_label.clone());

            let body = Box::new(self.parse_stmt()?);

            self.active_brk_label = prev_brk_label;
            self.active_cont_label = prev_cont_label;

            self.skip_keyword(Keyword::While)?;
            self.skip_punct("(")?;
            let cond = self.parse_expr()?;
            self.skip_punct(")")?;
            self.skip_punct(";")?;

            return Ok(Stmt::while_(
                cond, body, true, brk_label, cont_label, offset,
            ));
        }

        if self.current().is_keyword(Keyword::Goto) {
            self.advance();
            let ident = self.consume_ident()?;
            self.skip_punct(";")?;
            return Ok(Stmt::goto(ident, offset));
        }

        if self.current().is_keyword(Keyword::Break) {
            let Some(brk_label) = self.active_brk_label.clone() else {
                return Err(self.error_current("break statement not within a loop or switch"));
            };
            self.advance();
            self.skip_punct(";")?;
            return Ok(Stmt::jump(brk_label, offset));
        }

        if self.current().is_keyword(Keyword::Continue) {
            let Some(cont_label) = self.active_cont_label.clone() else {
                return Err(self.error_current("continue statement not within a loop"));
            };
            self.advance();
            self.skip_punct(";")?;
            return Ok(Stmt::jump(cont_label, offset));
        }

        if let Some(ident) = self.current().as_ident()
            && self.peek(1).is_some_and(|tok| tok.is_punct(":"))
        {
            self.advance();
            self.advance();
            let label = self.unique_label();
            let body = self.parse_stmt()?;
            return Ok(Stmt::label(label, Box::new(body), ident, offset));
        }

        if self.current().is_punct("{") {
            self.advance();
            return Ok(Stmt::block(self.parse_compound_stmt()?, offset));
        }

        self.parse_expr_stmt()
    }

    /// ```bnf
    /// <compound-stmt> ::= (<declaration> | <stmt>)* "}"
    /// ```
    fn parse_compound_stmt(&mut self) -> Result<Vec<Stmt>> {
        let mut stmts = Vec::new();
        self.enter_scope();

        while !self.current().is_punct("}") {
            let mut stmt =
                if self.at_typename() && !self.peek(1).is_some_and(|tok| tok.is_punct(":")) {
                    let declspec = self.parse_declspec(DeclspecContext::BlockScopeDecl)?;
                    if declspec.storage_class == Some(StorageClass::Typedef) {
                        self.parse_typedef_tail(declspec.ty, declspec.noreturn)?;
                        continue;
                    }
                    self.parse_declaration(declspec)?
                } else {
                    self.parse_stmt()?
                };
            self.infer_type_stmt(&mut stmt)?;
            stmts.push(stmt);
        }

        self.leave_scope();
        self.advance();
        Ok(stmts)
    }

    /// ```bnf
    /// <expr-stmt> ::= <expr>? ";"
    /// ```
    fn parse_expr_stmt(&mut self) -> Result<Stmt> {
        if self.current().is_punct(";") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Stmt::block(Vec::new(), offset));
        }

        let offset = self.current().offset;
        let expr = self.parse_expr()?;
        self.skip_punct(";")?;
        Ok(Stmt::expr(expr, offset))
    }

    /// ```bnf
    /// <declaration> ::=
    ///   <declspec> (<declarator-init> ("," <declarator-init>)*)? ";"
    /// <declarator-init> ::= <declarator> ("=" <initializer>)?
    /// ```
    fn parse_declaration(&mut self, declspec: Declspec) -> Result<Stmt> {
        let offset = self.current().offset;
        let mut stmts = Vec::new();
        if self.current().is_punct(";") {
            self.warn_current("useless type name in empty declaration");
            self.advance();
            return Ok(Stmt::block(Vec::new(), offset));
        }

        loop {
            let declarator = self.parse_declarator(declspec.ty, false)?;
            let Some(name) = declarator.name else {
                return Err(self
                    .source
                    .error_at(declarator.offset, "missing declarator name"));
            };

            // Check whether this declaration would conflict with an existing
            // binding in the current scope; this should (and should only) be
            // called before introducing a block-scope object with no linkage
            let check_no_linkage_decl_conflict = || {
                let Some(ident) = self.find_ident_current(&name) else {
                    return Ok(());
                };

                match ident {
                    OrdinaryIdent::Global(_, true) => Err(self.source.error_at(
                        declarator.offset,
                        "declaration with no linkage follows extern declaration",
                    )),
                    OrdinaryIdent::Local(_) | OrdinaryIdent::Global(..) => Err(self
                        .source
                        .error_at(declarator.offset, "redefinition of local variable")),
                    _ => Err(self.source.error_at(
                        declarator.offset,
                        "redeclared as a different kind of symbol",
                    )),
                }
            };

            // Block-scope function declaration, e.g., "int f(int);"
            if self.types.is_func(declarator.ty) {
                if self.current().is_punct("=") {
                    return Err(self.source.error_at(
                        declarator.offset,
                        "function declaration cannot be initialized",
                    ));
                }
                if !matches!(declspec.storage_class, None | Some(StorageClass::Extern)) {
                    return Err(self.source.error_at(
                        declarator.offset,
                        "invalid storage class specifier in function declaration",
                    ));
                }

                self.declare_function(name, declarator.ty, declspec.noreturn, declarator.offset)?;
            } else {
                if declspec.noreturn {
                    self.warn_at(declarator.offset, "variable declared '_Noreturn'");
                }

                // Block-scope extern object declaration
                if declspec.storage_class == Some(StorageClass::Extern) {
                    if self.current().is_punct("=") {
                        return Err(self.source.error_at(
                            declarator.offset,
                            "extern declaration cannot be initialized",
                        ));
                    }

                    self.declare_global(
                        name,
                        declarator.ty,
                        declspec.align,
                        GlobalStorage::Decl,
                        false,
                        declarator.offset,
                    )?;
                } else if declspec.storage_class == Some(StorageClass::Static) {
                    // Block-scope static object
                    check_no_linkage_decl_conflict()?;

                    let mut ty = declarator.ty;
                    let storage = if self.current().is_punct("=") {
                        self.advance();
                        let init = self.parse_initializer(ty)?;
                        ty = init.ty;
                        GlobalStorage::Data(self.new_global_init(init)?)
                    } else {
                        GlobalStorage::Zero
                    };

                    if self.types.is_incomplete(ty) {
                        return Err(self
                            .source
                            .error_at(declarator.offset, "variable has incomplete type"));
                    }

                    // A block-scope static object is backed by a hidden global
                    // storage, then its spelled local name is bound to that storage
                    // (with no linkage)
                    let label = self.unique_label();
                    let global_id = self.create_global(label, ty, declspec.align, storage, true);
                    self.push_scope_ident(name, OrdinaryIdent::Global(global_id, false));
                } else {
                    // Normal local object
                    check_no_linkage_decl_conflict()?;

                    let local_id = self.create_local(name, declarator.ty, declspec.align);

                    let mut ty = self.locals[local_id].ty;
                    if self.current().is_punct("=") {
                        let offset = self.current().offset;
                        self.advance();
                        let init = self.parse_initializer(ty)?;
                        ty = init.ty;
                        self.new_local_init(local_id, init, offset, &mut stmts)?;
                    }

                    if self.types.is_incomplete(ty) {
                        return Err(self
                            .source
                            .error_at(declarator.offset, "variable has incomplete type"));
                    }
                    self.locals[local_id].ty = ty;
                }
            }

            if self.current().is_punct(",") {
                self.advance();
                continue;
            }
            if self.current().is_punct(";") {
                self.advance();
                break;
            }
            return Err(self.error_current("expected ',' or ';'"));
        }
        Ok(Stmt::block(stmts, offset))
    }

    /// ```bnf
    /// <initializer> ::=
    ///   <string-initializer>
    ///   | <array-initializer>
    ///   | <struct-initializer>
    ///   | <union-initializer>
    ///   | "{" <initializer> "}"
    ///   | <assign>
    /// ```
    fn parse_initializer(&mut self, mut ty: Type) -> Result<Initializer> {
        if let Some(array) = self.types.as_array(ty).cloned() {
            let elements = if array.base == Type::CHAR
                && let Some(content) = self.current().as_str()
            {
                self.parse_string_initializer(content, &array)
            } else {
                self.parse_array_initializer(&array)?
            };

            if array.len.is_none() {
                // If array length is omitted we infer from the length of the
                // initializer; note that we have to create new type instead of
                // completing the original, because e.g., "typedef int T[];",
                // then "T a1 = {1};" and "T a2 = {1,2};" have different types
                ty = self.types.array(array.base, Some(elements.len()));
            }

            return Ok(Initializer {
                ty,
                kind: InitializerKind::Aggregate(elements),
            });
        }

        if let Some(sou) = self.types.as_struct_or_union(ty).cloned() {
            if !self.current().is_punct("{") {
                // Check whether this is initializing a struct/union with
                // another struct/union of **exactly** the same type
                let (is_copy_init, _) = self.speculate(|this| {
                    let mut expr = this.parse_assign()?;
                    this.infer_type(&mut expr)?;
                    Ok(expr.expect_ty() == ty)
                })?;

                if is_copy_init {
                    let mut assign = self.parse_assign()?;
                    self.infer_type(&mut assign)?;
                    debug_assert_eq!(assign.expect_ty(), ty);

                    return Ok(Initializer {
                        ty,
                        kind: InitializerKind::Expr(assign),
                    });
                }
            }

            let elements = if sou.is_struct {
                self.parse_struct_initializer(&sou)?
            } else {
                self.parse_union_initializer(&sou)?
            };

            return Ok(Initializer {
                ty,
                kind: InitializerKind::Aggregate(elements),
            });
        }

        if self.current().is_punct("{") {
            self.advance();
            let init = self.parse_initializer(ty)?;
            self.skip_punct("}")?;
            return Ok(init);
        }

        Ok(Initializer {
            ty,
            kind: InitializerKind::Expr(self.parse_assign()?),
        })
    }

    /// Skip one excess [`<initializer>`].
    ///
    /// [`<initializer>`]: Self::parse_initializer
    fn skip_initializer(&mut self) -> Result<()> {
        if self.current().is_punct("{") {
            self.advance();

            let mut first = true;
            while !self.maybe_skip_list_end(true) {
                if !first {
                    self.skip_punct(",")?;
                }
                first = false;
                self.skip_initializer()?;
            }

            return Ok(());
        }

        self.parse_assign()?;
        Ok(())
    }

    /// ```bnf
    /// <string-initializer> ::= <str>
    /// ```
    fn parse_string_initializer(
        &mut self,
        content: Rc<[u8]>,
        array: &ArrayTypeData,
    ) -> Vec<Initializer> {
        let offset = self.current().offset;
        let len = array.len.unwrap_or(content.len());

        if content.len() > len + 1 {
            // The terminating null character is automatically not
            // included (allowed per C spec) if there is no room for it,
            // so we warn only if the content is at least 2 bytes longer
            self.warn_current("initializer string is too long");
        }

        let elements = content
            .iter()
            .take(len)
            .map(|&byte| Initializer {
                ty: Type::CHAR,
                kind: InitializerKind::Expr(Node::num(byte as i8 as _, Type::INT, offset)),
            })
            .collect();

        self.advance();
        elements
    }

    /// ```bnf
    /// <array-initializer> ::=
    ///   "{" <initializer> ("," <initializer>)* ","? "}"
    ///   | <initializer> ("," <initializer>)*
    /// ```
    fn parse_array_initializer(&mut self, array: &ArrayTypeData) -> Result<Vec<Initializer>> {
        let braced = self.current().is_punct("{");
        if braced {
            self.advance();
        }

        let mut elements = Vec::with_capacity(array.len.unwrap_or(0));
        let mut i = 0;
        loop {
            if self.maybe_skip_list_end(braced) {
                break;
            }
            let has_space = array.len.is_none_or(|len| i < len);
            if !braced && !has_space {
                // In flattened form, once the array is full, the remaining
                // tokens are no longer considered excess elements of this
                // array, so we have to break rather than keep consuming
                break;
            }
            if i > 0 {
                self.skip_punct(",")?;
            }

            if has_space {
                elements.push(self.parse_initializer(array.base)?);
            } else {
                self.warn_current("excess elements in array initializer");
                self.skip_initializer()?;
            }
            i += 1;
        }

        Ok(elements)
    }

    /// ```bnf
    /// <struct-initializer> ::=
    ///   "{" <initializer> ("," <initializer>)* ","? "}"
    ///   | <initializer> ("," <initializer>)*
    /// ```
    fn parse_struct_initializer(
        &mut self,
        sou: &StructOrUnionTypeData,
    ) -> Result<Vec<Initializer>> {
        let braced = self.current().is_punct("{");
        if braced {
            self.advance();
        }

        let members = sou.members.clone().unwrap_or_default();
        let mut elements = Vec::with_capacity(members.len());
        let mut i = 0;
        loop {
            if self.maybe_skip_list_end(braced) {
                break;
            }
            if !braced && i >= members.len() {
                // In flattened form, once all members are filled, the remaining
                // tokens are no longer considered excess elements of this
                // struct/union, so we have to break rather than keep consuming
                break;
            }
            if i > 0 {
                self.skip_punct(",")?;
            }

            if i < members.len() {
                let ty = members[i].ty;
                if self.types.is_incomplete(ty) {
                    debug_assert!(
                        self.types.as_array(ty).is_some(),
                        "incomplete types other than flexible array member leaked",
                    );
                    return Err(self.error_current("cannot initialize a flexible array member"));
                }
                elements.push(self.parse_initializer(ty)?);
            } else {
                self.warn_current("excess elements in struct initializer");
                self.skip_initializer()?;
            }
            i += 1;
        }

        Ok(elements)
    }

    /// ```bnf
    /// <union-initializer> ::= "{" <initializer> ","? "}" | <initializer>
    /// ```
    fn parse_union_initializer(&mut self, sou: &StructOrUnionTypeData) -> Result<Vec<Initializer>> {
        let braced = self.current().is_punct("{");
        if braced {
            self.advance();
        }

        let mut elements = Vec::new();
        // Union initializer takes only one initializer and initializes the
        // first union member
        if let Some(member) = sou.members.as_ref().and_then(|members| members.first()) {
            elements.push(self.parse_initializer(member.ty)?);
        }

        if braced {
            self.skip_list_end()?;
        }
        Ok(elements)
    }

    /// ```bnf
    /// <assign> ::= <conditional> (<assign-op> <assign>)?
    /// <assign-op> ::=
    ///   "="
    ///   | "+="
    ///   | "-="
    ///   | "*="
    ///   | "/="
    ///   | "%="
    ///   | "&="
    ///   | "|="
    ///   | "^="
    ///   | "<<="
    ///   | ">>="
    /// ```
    fn parse_assign(&mut self) -> Result<Node> {
        let node = self.parse_conditional()?;
        let offset = self.current().offset;

        if self.current().is_punct("=") {
            self.advance();
            let assign = self.parse_assign()?;
            return Ok(Node::assign(node, assign, offset));
        }

        if self.current().is_punct("+=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = self.new_add(node, assign, offset)?;
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct("-=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = self.new_sub(node, assign, offset)?;
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct("*=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = Node::binary(BinaryOp::Mul, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct("/=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = Node::binary(BinaryOp::Div, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct("%=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = Node::binary(BinaryOp::Mod, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct("&=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = Node::binary(BinaryOp::BitAnd, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct("|=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = Node::binary(BinaryOp::BitOr, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct("^=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = Node::binary(BinaryOp::BitXor, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct("<<=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = Node::binary(BinaryOp::BitShl, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct(">>=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = Node::binary(BinaryOp::BitShr, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        Ok(node)
    }

    /// ```bnf
    /// <conditional> ::= <or> ("?" <expr> ":" <conditional>)?
    /// ```
    fn parse_conditional(&mut self) -> Result<Node> {
        let node = self.parse_or()?;

        if !self.current().is_punct("?") {
            return Ok(node);
        }

        let offset = self.current().offset;
        self.advance();
        let then_expr = self.parse_expr()?;
        self.skip_punct(":")?;
        let else_expr = self.parse_conditional()?;

        Ok(Node::conditional(node, then_expr, else_expr, offset))
    }

    /// ```bnf
    /// <or> ::= <and> ("||" <and>)*
    /// ```
    fn parse_or(&mut self) -> Result<Node> {
        let mut node = self.parse_and()?;

        while self.current().is_punct("||") {
            let offset = self.current().offset;
            self.advance();
            node = Node::or(node, self.parse_and()?, offset);
        }

        Ok(node)
    }

    /// ```bnf
    /// <and> ::= <bit-or> ("&&" <bit-or>)*
    /// ```
    fn parse_and(&mut self) -> Result<Node> {
        let mut node = self.parse_bit_or()?;

        while self.current().is_punct("&&") {
            let offset = self.current().offset;
            self.advance();
            node = Node::and(node, self.parse_bit_or()?, offset);
        }

        Ok(node)
    }

    /// ```bnf
    /// <bit-or> ::= <bit-xor> ("|" <bit-xor>)*
    /// ```
    fn parse_bit_or(&mut self) -> Result<Node> {
        let mut node = self.parse_bit_xor()?;

        while self.current().is_punct("|") {
            let offset = self.current().offset;
            self.advance();
            node = Node::binary(BinaryOp::BitOr, node, self.parse_bit_xor()?, offset);
        }

        Ok(node)
    }

    /// ```bnf
    /// <bit-xor> ::= <bit-and> ("^" <bit-and>)*
    /// ```
    fn parse_bit_xor(&mut self) -> Result<Node> {
        let mut node = self.parse_bit_and()?;

        while self.current().is_punct("^") {
            let offset = self.current().offset;
            self.advance();
            node = Node::binary(BinaryOp::BitXor, node, self.parse_bit_and()?, offset);
        }

        Ok(node)
    }

    /// ```bnf
    /// <bit-and> ::= <equality> ("&" <equality>)*
    /// ```
    fn parse_bit_and(&mut self) -> Result<Node> {
        let mut node = self.parse_equality()?;

        while self.current().is_punct("&") {
            let offset = self.current().offset;
            self.advance();
            node = Node::binary(BinaryOp::BitAnd, node, self.parse_equality()?, offset);
        }

        Ok(node)
    }

    /// ```bnf
    /// <equality> ::= <relational> ("==" <relational> | "!=" <relational>)*
    /// ```
    fn parse_equality(&mut self) -> Result<Node> {
        let mut node = self.parse_relational()?;

        loop {
            let offset = self.current().offset;

            if self.current().is_punct("==") {
                self.advance();
                node = Node::binary(BinaryOp::Eq, node, self.parse_relational()?, offset);
                continue;
            }

            if self.current().is_punct("!=") {
                self.advance();
                node = Node::binary(BinaryOp::Ne, node, self.parse_relational()?, offset);
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <relational> ::=
    ///   <shift> ("<" <shift> | "<=" <shift> | ">" <shift> | ">=" <shift>)*
    /// ```
    fn parse_relational(&mut self) -> Result<Node> {
        let mut node = self.parse_shift()?;

        loop {
            let offset = self.current().offset;

            if self.current().is_punct("<") {
                self.advance();
                node = Node::binary(BinaryOp::Lt, node, self.parse_shift()?, offset);
                continue;
            }

            if self.current().is_punct("<=") {
                self.advance();
                node = Node::binary(BinaryOp::Le, node, self.parse_shift()?, offset);
                continue;
            }

            if self.current().is_punct(">") {
                self.advance();
                node = Node::binary(BinaryOp::Lt, self.parse_shift()?, node, offset);
                continue;
            }

            if self.current().is_punct(">=") {
                self.advance();
                node = Node::binary(BinaryOp::Le, self.parse_shift()?, node, offset);
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <shift> ::= <add> ("<<" <add> | ">>" <add>)*
    /// ```
    fn parse_shift(&mut self) -> Result<Node> {
        let mut node = self.parse_add()?;

        loop {
            let offset = self.current().offset;

            if self.current().is_punct("<<") {
                self.advance();
                node = Node::binary(BinaryOp::BitShl, node, self.parse_add()?, offset);
                continue;
            }

            if self.current().is_punct(">>") {
                self.advance();
                node = Node::binary(BinaryOp::BitShr, node, self.parse_add()?, offset);
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <add> ::= <mul> ("+" <mul> | "-" <mul>)*
    /// ```
    fn parse_add(&mut self) -> Result<Node> {
        let mut node = self.parse_mul()?;

        loop {
            let offset = self.current().offset;

            if self.current().is_punct("+") {
                self.advance();
                let rhs = self.parse_mul()?;
                node = self.new_add(node, rhs, offset)?;
                continue;
            }

            if self.current().is_punct("-") {
                self.advance();
                let rhs = self.parse_mul()?;
                node = self.new_sub(node, rhs, offset)?;
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <mul> ::= <cast> (("*" | "/" | "%") <cast>)*
    /// ```
    fn parse_mul(&mut self) -> Result<Node> {
        let mut node = self.parse_cast()?;

        loop {
            let offset = self.current().offset;

            if self.current().is_punct("*") {
                self.advance();
                node = Node::binary(BinaryOp::Mul, node, self.parse_cast()?, offset);
                continue;
            }

            if self.current().is_punct("/") {
                self.advance();
                node = Node::binary(BinaryOp::Div, node, self.parse_cast()?, offset);
                continue;
            }

            if self.current().is_punct("%") {
                self.advance();
                node = Node::binary(BinaryOp::Mod, node, self.parse_cast()?, offset);
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <cast> ::= "(" <typename> ")" <cast> | <unary>
    /// ```
    fn parse_cast(&mut self) -> Result<Node> {
        let offset = self.current().offset;

        if !self.preprocess && self.current().is_punct("(") {
            let pos = self.pos;
            self.advance();

            if self.at_typename() {
                let ty = self.parse_typename()?;
                self.skip_punct(")")?;

                // Compound literal also starts with "(" <typename> ")" (see
                // <unary> then <postfix>), and "{" is the characteristic that
                // tells compound literals apart
                if !self.current().is_punct("{") {
                    let mut expr = self.parse_cast()?;
                    self.infer_type(&mut expr)?;
                    return Ok(Node::cast(expr, ty, offset));
                }
            }

            self.pos = pos;
        }

        self.parse_unary()
    }

    /// ```bnf
    /// <unary> ::=
    ///   ("+" | "-" | "&" | "*" | "!" | "~") <cast>
    ///   | ("++" | "--") <unary>
    ///   | <postfix>
    /// ```
    fn parse_unary(&mut self) -> Result<Node> {
        let offset = self.current().offset;

        if self.current().is_punct("+") {
            self.advance();
            return self.parse_cast();
        }

        if self.current().is_punct("-") {
            self.advance();
            return Ok(Node::neg(self.parse_cast()?, offset));
        }

        if !self.preprocess && self.current().is_punct("&") {
            self.advance();
            return Ok(Node::addr(self.parse_cast()?, offset));
        }

        if !self.preprocess && self.current().is_punct("*") {
            self.advance();
            return Ok(Node::deref(self.parse_cast()?, offset));
        }

        if self.current().is_punct("!") {
            self.advance();
            return Ok(Node::not(self.parse_cast()?, offset));
        }

        if self.current().is_punct("~") {
            self.advance();
            return Ok(Node::bit_not(self.parse_cast()?, offset));
        }

        if !self.preprocess && self.current().is_punct("++") {
            self.advance();
            let unary = self.parse_unary()?;
            let binary = self.new_add(unary, Node::num(1, Type::INT, offset), offset)?;
            return self.new_compound_assign(binary, offset);
        }

        if !self.preprocess && self.current().is_punct("--") {
            self.advance();
            let unary = self.parse_unary()?;
            let binary = self.new_sub(unary, Node::num(1, Type::INT, offset), offset)?;
            return self.new_compound_assign(binary, offset);
        }

        if self.preprocess {
            self.parse_primary()
        } else {
            self.parse_postfix()
        }
    }

    /// ```bnf
    /// <postfix> ::=
    ///   "(" <typename> ")" "{" <initializer> "}" | <primary> <postfix-tail>*
    /// <postfix-tail> ::=
    ///   "[" <expr> "]"
    ///   | <func-call>
    ///   | ("." | "->") <ident>
    ///   | "++"
    ///   | "--"
    /// ```
    fn parse_postfix(&mut self) -> Result<Node> {
        let (is_compound_literal, _) = self.speculate(|parser| {
            if !parser.current().is_punct("(") {
                return Ok(false);
            }
            parser.advance();
            if !parser.at_typename() {
                return Ok(false);
            }
            let _ = parser.parse_typename()?;
            parser.skip_punct(")")?;
            Ok(parser.current().is_punct("{"))
        })?;

        if is_compound_literal {
            let offset = self.current().offset;
            self.skip_punct("(")?;
            let ty = self.parse_typename()?;
            self.skip_punct(")")?;
            let init = self.parse_initializer(ty)?;
            return self.new_compound_literal(init, offset);
        }

        let mut node = self.parse_primary()?;

        loop {
            let offset = self.current().offset;

            if self.current().is_punct("(") {
                node = self.parse_func_call(node)?;
                continue;
            }

            if self.current().is_punct("[") {
                self.advance();
                let index = self.parse_expr()?;
                self.skip_punct("]")?;
                // Canonicalize a[b] to *(a + b)
                node = Node::deref(self.new_add(node, index, offset)?, offset);
                continue;
            }

            if self.current().is_punct(".") {
                self.advance();
                node = self.new_member_access(node)?;
                self.advance();
                continue;
            }

            if self.current().is_punct("->") {
                self.advance();
                // Canonicalize a->b to (*a).b
                node = Node::deref(node, offset);
                node = self.new_member_access(node)?;
                self.advance();
                continue;
            }

            if self.current().is_punct("++") {
                self.advance();
                node = self.new_post_inc_dec(node, true, offset)?;
                continue;
            }

            if self.current().is_punct("--") {
                self.advance();
                node = self.new_post_inc_dec(node, false, offset)?;
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <primary> ::=
    ///   "(" "{" <compound-stmt> ")"
    ///   | "(" <expr> ")"
    ///   | "sizeof" ("(" <typename> ")" | <unary>)
    ///   | "_Alignof" ("(" <typename> ")" | <unary>)
    ///   | <ident>
    ///   | <str>
    ///   | <num>
    ///   | <flonum>
    /// ```
    fn parse_primary(&mut self) -> Result<Node> {
        let offset = self.current().offset;

        if self.current().is_punct("(") {
            self.advance();

            if self.current().is_punct("{") {
                if self.preprocess {
                    return Err(self.error_current(
                        "statement expression is not valid in preprocessor expressions",
                    ));
                }

                self.advance();
                let body = self.parse_compound_stmt()?;
                self.skip_punct(")")?;
                return Ok(Node::stmt_expr(body, offset));
            }

            let node = if self.preprocess {
                self.parse_conditional()?
            } else {
                self.parse_expr()?
            };
            self.skip_punct(")")?;
            return Ok(node);
        }

        if !self.preprocess && self.current().is_keyword(Keyword::Sizeof) {
            self.advance();

            if self.current().is_punct("(") {
                let pos = self.pos;
                self.advance();

                if self.at_typename() {
                    let offset = self.current().offset;
                    let ty = self.parse_typename()?;
                    self.skip_punct(")")?;

                    if self.types.is_incomplete(ty) {
                        return Err(self
                            .source
                            .error_at(offset, "cannot apply 'sizeof' to incomplete type"));
                    }
                    return Ok(Node::num(self.types.size(ty), Type::ULONG, offset));
                }

                self.pos = pos;
            }

            let mut operand = self.parse_unary()?;
            self.infer_type(&mut operand)?;
            let size = self.types.size(operand.expect_ty());
            return Ok(Node::num(size, Type::ULONG, offset));
        }

        if !self.preprocess && self.current().is_keyword(Keyword::Alignof) {
            self.advance();

            if self.current().is_punct("(") {
                let pos = self.pos;
                self.advance();

                if self.at_typename() {
                    let offset = self.current().offset;
                    let ty = self.parse_typename()?;
                    self.skip_punct(")")?;

                    if self.types.is_incomplete(ty) {
                        return Err(self
                            .source
                            .error_at(offset, "cannot apply '_Alignof' to incomplete type"));
                    }
                    return Ok(Node::num(self.types.align(ty), Type::ULONG, offset));
                }

                self.pos = pos;
            }

            let mut operand = self.parse_unary()?;
            self.infer_type(&mut operand)?;
            let size = self.types.align(operand.expect_ty());
            return Ok(Node::num(size, Type::ULONG, offset));
        }

        if let Some(name) = self.current().as_ident() {
            debug_assert!(
                !self.preprocess,
                "idents should not leak here in preprocessing mode",
            );
            self.advance();

            let node = match self.find_ident(&name) {
                Some(OrdinaryIdent::Local(local_id)) => {
                    Node::entity(EntityRef::Local(local_id), offset)
                },
                Some(OrdinaryIdent::Global(global_id, _)) => {
                    Node::entity(EntityRef::Global(global_id), offset)
                },
                Some(OrdinaryIdent::Function(id)) => Node::entity(EntityRef::Function(id), offset),
                Some(OrdinaryIdent::Enumerator(val)) => {
                    Node::num(val.bits(), val.ty.into(), offset)
                },
                _ if self.current().is_punct("(") => {
                    return Err(self
                        .source
                        .error_at(offset, "implicit declaration of a function"));
                },
                _ => return Err(self.source.error_at(offset, "undefined identifier")),
            };

            return Ok(node);
        }

        if let Some(content) = self.current().as_str() {
            if self.preprocess {
                return Err(
                    self.error_current("string literal is not valid in preprocessor expressions")
                );
            }

            let ty = self.types.array(Type::CHAR, Some(content.len()));
            let label = self.unique_label();
            let global_id = self.create_global(
                label,
                ty,
                None,
                GlobalStorage::Data(GlobalInitData {
                    bytes: content,
                    relocations: Default::default(),
                }),
                true,
            );
            self.advance();
            return Ok(Node::entity(EntityRef::Global(global_id), offset));
        }

        if let Some((num, ty)) = self.current().as_num() {
            self.advance();
            return Ok(Node::num(num, ty, offset));
        }

        if let Some((num, ty)) = self.current().as_flonum() {
            if self.preprocess {
                return Err(self.error_current(
                    "floating-point constant is not valid in preprocessor expressions",
                ));
            }

            self.advance();
            return Ok(Node::flonum(num, ty, offset));
        }

        Err(self.error_current("expected an expression"))
    }

    /// ```bnf
    /// <func-call> ::= "(" (<assign> ("," <assign>)*)? ")"
    /// ```
    fn parse_func_call(&mut self, mut callee: Node) -> Result<Node> {
        let offset = self.current().offset;
        self.skip_punct("(")?;

        self.infer_type(&mut callee)?;
        let Some(func) = self.types.as_func(callee.expect_ty(), true).cloned() else {
            return Err(self.source.error_at(offset, "not a function"));
        };
        let mut param_tys = func.params.iter().copied();

        let mut args = Vec::new();
        while !self.current().is_punct(")") {
            if !args.is_empty() {
                self.skip_punct(",")?;
            }

            let mut arg = self.parse_assign()?;
            self.infer_type(&mut arg)?;

            if let Some(param_ty) = param_tys.next() {
                if self.types.as_struct_or_union(param_ty).is_some() {
                    return Err(self.source.error_at(
                        arg.offset,
                        "passing struct or union by value is not supported yet",
                    ));
                }
                self.apply_cast(&mut arg, param_ty)?;
            } else {
                if !func.is_variadic {
                    return Err(self.source.error_at(arg.offset, "too many arguments"));
                }
                // Variadic function call applies default argument promotions
                let ty = arg.expect_ty();
                let promoted = if ty.is_flonum() {
                    Type::DOUBLE
                } else {
                    ty.promote_int().unwrap_or(ty)
                };
                self.apply_cast(&mut arg, promoted)?;
            }

            args.push(arg);
        }

        if param_tys.next().is_some() {
            return Err(self.error_current("too few arguments"));
        }

        self.skip_punct(")")?;
        Ok(Node::func_call(callee, args, func.return_ty, offset))
    }

    /// ```bnf
    /// <typedef-tail> ::= <declarator> ("," <declarator>)* ";"
    /// ```
    fn parse_typedef_tail(&mut self, base_ty: Type, noreturn: bool) -> Result<()> {
        if self.current().is_punct(";") {
            self.warn_current("useless type name in empty declaration");
            self.advance();
            return Ok(());
        }

        loop {
            let declarator = self.parse_declarator(base_ty, false)?;
            if noreturn {
                self.warn_at(declarator.offset, "typedef declared '_Noreturn'");
            }
            let Some(name) = declarator.name else {
                return Err(self
                    .source
                    .error_at(declarator.offset, "missing typedef name"));
            };

            if let Some(ident) = self.find_ident_current(&name) {
                match ident {
                    OrdinaryIdent::Typedef(ty) => {
                        // Duplicate typedef with same type is allowed
                        if !self.types.same_type(ty, declarator.ty) {
                            return Err(self
                                .source
                                .error_at(declarator.offset, "conflicting types"));
                        }
                    },
                    _ => {
                        return Err(self.source.error_at(
                            declarator.offset,
                            "redeclared as a different kind of symbol",
                        ));
                    },
                }
            }

            let typedef = OrdinaryIdent::Typedef(declarator.ty);
            self.push_scope_ident(name, typedef);

            if self.current().is_punct(",") {
                self.advance();
                continue;
            }
            if self.current().is_punct(";") {
                self.advance();
                break;
            }
            return Err(self.error_current("expected ',' or ';'"));
        }
        Ok(())
    }

    /// ```bnf
    /// <constexpr> ::= <conditional>
    /// ```
    ///
    /// Semantically, not every conditional expression is a valid constant
    /// expression, and that restriction is forced by [`Self::eval`].
    pub fn parse_constexpr(&mut self) -> Result<ConstValue> {
        let mut node = self.parse_conditional()?;
        let val = self.eval(&mut node)?;

        if self.preprocess && !self.current().is_eof() {
            return Err(self.error_current("extraneous tokens"));
        }
        Ok(val)
    }

    /// Enter a new variable scope.
    fn enter_scope(&mut self) {
        self.scopes.push(ScopeFrame::default());
    }

    /// Leave the current variable scope.
    fn leave_scope(&mut self) {
        debug_assert!(self.scopes.len() > 1, "cannot leave root scope");
        self.scopes.pop();
    }

    /// Push an ordinary identifier into the current scope.
    fn push_scope_ident(&mut self, name: SmolStr, ident: OrdinaryIdent) {
        self.scopes
            .last_mut()
            .expect("no scope to push ordinary identifier into")
            .idents
            .insert(name, ident);
    }

    /// Push a struct or union tag into the current scope.
    fn push_scope_tag(&mut self, name: SmolStr, ty: Type) {
        self.scopes
            .last_mut()
            .expect("no scope to push struct or union tag into")
            .tags
            .insert(name, ty);
    }

    /// Find an ordinary identifier by name.
    fn find_ident(&self, name: &str) -> Option<OrdinaryIdent> {
        for frame in self.scopes.iter().rev() {
            if let Some(entry) = frame.idents.get(name) {
                return Some(*entry);
            }
        }
        None
    }

    /// Find an ordinary identifier in the current scope only.
    fn find_ident_current(&self, name: &str) -> Option<OrdinaryIdent> {
        self.scopes.last()?.idents.get(name).copied()
    }

    /// Find a struct or union tag by name.
    fn find_tag(&self, tag: &str) -> Option<Type> {
        for frame in self.scopes.iter().rev() {
            if let Some(ty) = frame.tags.get(tag) {
                return Some(*ty);
            }
        }
        None
    }

    /// Find a struct or union tag in the current scope only.
    fn find_tag_current(&self, tag: &str) -> Option<Type> {
        self.scopes.last()?.tags.get(tag).copied()
    }

    /// Create a new local variable.
    fn create_local(&mut self, name: impl Into<SmolStr>, ty: Type, align: Option<u64>) -> usize {
        self.disallow_speculation();

        let name = name.into();
        self.locals.push(LocalVar {
            _name: name.clone(),
            ty,
            align,
            offset: 0, // Assigned during codegen
        });

        let id = self.locals.len() - 1;
        let entity = OrdinaryIdent::Local(id);
        self.push_scope_ident(name, entity);
        id
    }

    /// Create local variables for function parameters.
    ///
    /// Parameters are pushed in reverse order to ensure the first parameter
    /// gets the lowest local ID.
    fn create_param_locals(&mut self, params: Vec<Parameter>) -> Result<Vec<usize>> {
        let mut param_ids = Vec::with_capacity(params.len());

        for param in params.into_iter().rev() {
            let Some(name) = param.name else {
                return Err(self.source.error_at(param.offset, "missing parameter name"));
            };
            param_ids.push(self.create_local(name, param.ty, None));
        }

        param_ids.reverse();
        Ok(param_ids)
    }

    /// Create a new global variable.
    fn create_global(
        &mut self,
        name: impl Into<SmolStr>,
        ty: Type,
        align: Option<u64>,
        storage: GlobalStorage,
        is_static: bool,
    ) -> usize {
        self.disallow_speculation();

        let name = name.into();
        self.globals.push(GlobalVar {
            name: name.clone(),
            ty,
            align,
            storage,
            is_static,
        });

        let id = self.globals.len() - 1;
        let entity = OrdinaryIdent::Global(id, true);
        self.push_scope_ident(name, entity);
        id
    }

    /// Declare a new global variable.
    ///
    /// This is similar to [`create_global`], but it attempts to reuse an
    /// existing global declaration with the same name, if any and if
    /// compatible. Note also that the caller should specify `storage` as
    /// [`GlobalStorage::Decl`] only for "extern".
    ///
    /// [`create_global`]: Self::create_global
    fn declare_global(
        &mut self,
        name: SmolStr,
        ty: Type,
        align: Option<u64>,
        storage: GlobalStorage,
        is_static: bool,
        offset: usize,
    ) -> Result<usize> {
        self.disallow_speculation();

        let global_id = match self.find_ident_current(&name) {
            // Reuse same-scope linkage-bearing global declaration
            Some(OrdinaryIdent::Global(global_id, true)) => Some(global_id),
            // Conflict with same-scope object that has no linkage
            Some(OrdinaryIdent::Global(_, false) | OrdinaryIdent::Local(_)) => {
                return Err(self.source.error_at(
                    offset,
                    "extern declaration follows declaration with no linkage",
                ));
            },
            Some(_) => {
                return Err(self
                    .source
                    .error_at(offset, "redeclared as a different kind of symbol"));
            },
            // Reuse linkage-bearing globals from outer scopes if any
            None => self.scopes.iter().rev().skip(1).find_map(|frame| {
                let ident = frame.idents.get(&name).copied()?;
                let OrdinaryIdent::Global(global_id, true) = ident else {
                    return None;
                };
                Some(global_id)
            }),
        };

        let Some(global_id) = global_id else {
            return Ok(self.create_global(name, ty, align, storage, is_static));
        };

        let global = &mut self.globals[global_id];
        global.align = global.align.max(align);

        global.ty = self
            .types
            .merge(global.ty, ty)
            .ok_or_else(|| self.source.error_at(offset, "conflicting types"))?;

        if global.is_static && !is_static && !matches!(storage, GlobalStorage::Decl) {
            // A non-static extern can follow a static declaration
            return Err(self
                .source
                .error_at(offset, "non-static declaration follows static declaration"));
        }
        if !global.is_static && is_static {
            return Err(self
                .source
                .error_at(offset, "static declaration follows non-static declaration"));
        }

        if global.storage.merge(storage) {
            return Err(self
                .source
                .error_at(offset, "redefinition of global variable"));
        }

        // Rebind the reused global in current scope
        self.push_scope_ident(name, OrdinaryIdent::Global(global_id, true));
        Ok(global_id)
    }

    /// Create a new function declaration.
    ///
    /// If the function is also defined, relevant fields need to be filled in
    /// later, looked up via the returned ID.
    fn create_function(&mut self, name: impl Into<SmolStr>, ty: Type, noreturn: bool) -> usize {
        self.disallow_speculation();

        let name = name.into();
        self.functions.push(Function {
            name: name.clone(),
            ty,
            body: None,
            param_locals: Default::default(),
            va_area_local: None,
            locals: Default::default(),
            is_static: false,
            noreturn,
        });

        let id = self.functions.len() - 1;
        let entity = OrdinaryIdent::Function(id);
        self.push_scope_ident(name, entity);
        id
    }

    /// Declare a new function declaration.
    ///
    /// This is similar to [`create_function`], but it attempts to reuse an
    /// existing function declaration with the same name, if any and if
    /// compatible.
    ///
    /// [`create_function`]: Self::create_function
    fn declare_function(
        &mut self,
        name: SmolStr,
        ty: Type,
        noreturn: bool,
        offset: usize,
    ) -> Result<usize> {
        self.disallow_speculation();

        let func_id = match self.find_ident_current(&name) {
            // Reuse same-scope function declaration
            Some(OrdinaryIdent::Function(func_id)) => Some(func_id),
            Some(_) => {
                return Err(self
                    .source
                    .error_at(offset, "redeclared as a different kind of symbol"));
            },
            // Reuse function declarations from outer scopes if any
            None => self.scopes.iter().rev().skip(1).find_map(|frame| {
                let ident = frame.idents.get(&name).copied()?;
                let OrdinaryIdent::Function(func_id) = ident else {
                    return None;
                };
                Some(func_id)
            }),
        };

        let Some(func_id) = func_id else {
            return Ok(self.create_function(name, ty, noreturn));
        };

        if !self.types.same_type(self.functions[func_id].ty, ty) {
            return Err(self.source.error_at(offset, "conflicting types"));
        }
        self.functions[func_id].noreturn |= noreturn;

        self.push_scope_ident(name, OrdinaryIdent::Function(func_id));
        Ok(func_id)
    }

    /// Build an addition node with pointer scaling.
    fn new_add(&mut self, mut lhs: Node, mut rhs: Node, offset: usize) -> Result<Node> {
        self.infer_type(&mut lhs)?;
        self.infer_type(&mut rhs)?;

        let lhs_ty = lhs.expect_ty();
        let rhs_ty = rhs.expect_ty();

        // num + num
        if lhs_ty.is_arith() && rhs_ty.is_arith() {
            return Ok(Node::binary(BinaryOp::Add, lhs, rhs, offset));
        }

        if self.types.base(lhs_ty).is_some() && self.types.base(rhs_ty).is_some() {
            return Err(self.source.error_at(offset, "invalid operands"));
        }

        // Canonicalize num + ptr to ptr + num
        if self.types.base(lhs_ty).is_none() && self.types.base(rhs_ty).is_some() {
            std::mem::swap(&mut lhs, &mut rhs);
        }

        // ptr + num
        let base_ty = self.types.base(lhs.expect_ty()).unwrap();
        let base_size = self.types.size(base_ty);
        let scaled_rhs = Node::binary(
            BinaryOp::Mul,
            rhs,
            Node::num(base_size, Type::LONG, offset),
            offset,
        );
        let node = Node::binary(BinaryOp::Add, lhs, scaled_rhs, offset);
        Ok(node)
    }

    /// Build a subtraction node with pointer scaling.
    fn new_sub(&mut self, mut lhs: Node, mut rhs: Node, offset: usize) -> Result<Node> {
        self.infer_type(&mut lhs)?;
        self.infer_type(&mut rhs)?;

        let lhs_ty = lhs.expect_ty();
        let rhs_ty = rhs.expect_ty();

        // num - num
        if lhs_ty.is_arith() && rhs_ty.is_arith() {
            return Ok(Node::binary(BinaryOp::Sub, lhs, rhs, offset));
        }

        // ptr - num
        if let Some(base_ty) = self.types.base(lhs_ty)
            && rhs_ty.is_integer()
        {
            let base_size = self.types.size(base_ty);
            let scaled_rhs = Node::binary(
                BinaryOp::Mul,
                rhs,
                Node::num(base_size, Type::LONG, offset),
                offset,
            );
            let node = Node::binary(BinaryOp::Sub, lhs, scaled_rhs, offset);
            return Ok(node);
        }

        // ptr - ptr
        if let Some(base_ty) = self.types.base(lhs_ty)
            && self.types.base(rhs_ty).is_some()
        {
            let base_size = self.types.size(base_ty);
            let mut diff = Node::binary(BinaryOp::Sub, lhs, rhs, offset);
            diff.ty = Some(Type::LONG);
            let node = Node::binary(
                BinaryOp::Div,
                diff,
                Node::num(base_size, Type::LONG, offset),
                offset,
            );
            return Ok(node);
        }

        Err(self.source.error_at(offset, "invalid operands"))
    }

    /// Build a compound assignment operation node.
    ///
    /// This is desugared into making a temporary pointer to `lhs`, performing
    /// the binary operation, and assigning the result back to `lhs`.
    fn new_compound_assign(&mut self, binary: Node, offset: usize) -> Result<Node> {
        let NodeKind::Binary {
            op,
            mut lhs,
            mut rhs,
        } = binary.kind
        else {
            unreachable!();
        };

        self.infer_type(&mut lhs)?;
        self.infer_type(&mut rhs)?;

        // (typeof lhs) *tmp;
        let lhs_ty = self.types.ptr(lhs.expect_ty());
        let tmp = EntityRef::Local(self.create_local("", lhs_ty, None));

        // tmp = &lhs;
        let assign1 = Node::assign(Node::entity(tmp, offset), Node::addr(lhs, offset), offset);

        // *tmp = *tmp op rhs;
        let assign2 = Node::assign(
            Node::deref(Node::entity(tmp, offset), offset),
            Node::binary(
                op,
                Node::deref(Node::entity(tmp, offset), offset),
                rhs,
                offset,
            ),
            offset,
        );

        // (tmp = &lhs, *tmp = *tmp op rhs)
        Ok(Node::comma(assign1, assign2, offset))
    }

    /// Build a post increment/decrement node.
    ///
    /// Post increment is desugared into `(typeof node)((node += 1) - 1)`, and
    /// post decrement is desugared into `(typeof node)((node -= 1) + 1)`.
    fn new_post_inc_dec(&mut self, mut node: Node, is_inc: bool, offset: usize) -> Result<Node> {
        self.infer_type(&mut node)?;
        let ty = node.expect_ty();

        let mut addend = Node::num(1, Type::LONG, offset);
        let mut neg_addend = Node::neg(Node::num(1, Type::LONG, offset), offset);
        if !is_inc {
            std::mem::swap(&mut addend, &mut neg_addend);
        }

        // node += addend
        let binary = self.new_add(node, addend, offset)?;
        let assign = self.new_compound_assign(binary, offset)?;

        // (node += addend) - addend
        let mut post = self.new_add(assign, neg_addend, offset)?;
        self.infer_type(&mut post)?;

        // (typeof node)((node += addend) - addend) (or reverse)
        Ok(Node::cast(post, ty, offset))
    }

    /// Build a member access node for the given node.
    fn new_member_access(&mut self, mut node: Node) -> Result<Node> {
        self.infer_type(&mut node)?;
        let ty = node.expect_ty();
        if self.types.is_incomplete(ty) {
            return Err(self.error_current("request for member in an incomplete type"));
        }

        let sou = match self.types.as_struct_or_union(node.expect_ty()) {
            Some(sou) => sou,
            None => {
                return Err(self.error_current(
                    "request for member in something that is not a struct or union",
                ));
            },
        };

        let ident = match self.current().as_ident() {
            Some(ident) => ident,
            None => return Err(self.error_current("not an ident")),
        };

        let member = match sou
            .members
            .as_ref()
            .unwrap()
            .iter()
            .find(|member| member.name == ident)
        {
            Some(member) => member.clone(),
            None => return Err(self.error_current("no such member")),
        };

        Ok(Node::member(node, member, self.current().offset))
    }

    /// Lower a local variable initializer.
    ///
    /// This pushes into `stmts` the assignment statements for the explicit
    /// initializer pieces.
    fn new_local_init(
        &mut self,
        local_id: usize,
        init: Initializer,
        offset: usize,
        stmts: &mut Vec<Stmt>,
    ) -> Result<()> {
        if matches!(init.kind, InitializerKind::Aggregate(_)) {
            stmts.push(Stmt::memzero_local(local_id, offset));
        }
        self.new_local_init2(local_id, init, &mut Vec::new(), stmts)
    }

    fn new_local_init2(
        &mut self,
        local_id: usize,
        init: Initializer,
        path: &mut Vec<InitializerStep>,
        stmts: &mut Vec<Stmt>,
    ) -> Result<()> {
        match init.kind {
            InitializerKind::Expr(rhs) => {
                let offset = rhs.offset;

                let mut lhs = Node::entity(EntityRef::Local(local_id), offset);
                for step in path.iter() {
                    lhs = match step {
                        InitializerStep::Index(index) => {
                            let index = Node::num(*index as _, Type::ULONG, offset);
                            Node::deref(self.new_add(lhs, index, offset)?, offset)
                        },
                        InitializerStep::Member(member) => {
                            Node::member(lhs, member.clone(), offset)
                        },
                    };
                }

                let expr = Node::assign(lhs, rhs, offset);
                stmts.push(Stmt::expr(expr, offset));
            },
            InitializerKind::Aggregate(children) => {
                if self.types.as_array(init.ty).is_some() {
                    for (i, child) in children.into_iter().enumerate() {
                        path.push(InitializerStep::Index(i));
                        self.new_local_init2(local_id, child, path, stmts)?;
                        path.pop();
                    }
                } else if let Some(sou) = self.types.as_struct_or_union(init.ty) {
                    for (member, child) in
                        sou.members.clone().unwrap_or_default().iter().zip(children)
                    {
                        path.push(InitializerStep::Member(member.clone()));
                        self.new_local_init2(local_id, child, path, stmts)?;
                        path.pop();
                    }
                }
            },
        }

        Ok(())
    }

    /// Write the initialization data of a global variable initializer.
    fn new_global_init(&mut self, init: Initializer) -> Result<GlobalInitData> {
        let mut bytes = vec![0; self.types.size(init.ty) as usize];
        let mut relocations = Vec::new();
        self.new_global_init2(init, &mut bytes, &mut relocations, 0)?;
        Ok(GlobalInitData {
            bytes: bytes.into(),
            relocations: relocations.into(),
        })
    }

    fn new_global_init2(
        &mut self,
        init: Initializer,
        bytes: &mut [u8],
        relocations: &mut Vec<Relocation>,
        offset: usize,
    ) -> Result<()> {
        let mut write = |val: ConstValue, size| match size {
            1 => bytes[offset] = val.bits() as _,
            2 => bytes[offset..offset + 2].copy_from_slice(&(val.bits() as u16).to_ne_bytes()),
            4 => bytes[offset..offset + 4].copy_from_slice(&(val.bits() as u32).to_ne_bytes()),
            8 => bytes[offset..offset + 8].copy_from_slice(&val.bits().to_ne_bytes()),
            _ => unreachable!(),
        };

        match init.kind {
            InitializerKind::Expr(mut rhs) => match self.eval_global_init(&mut rhs)? {
                GlobalInitValue::Num(val) => {
                    let Some(const_ty) = self.types.to_const(init.ty) else {
                        return Err(self
                            .source
                            .error_at(rhs.offset, "not a compile-time constant"));
                    };
                    write(val.cast(const_ty), self.types.size(init.ty))
                },
                GlobalInitValue::Reloc(label, addend) => {
                    if self.types.size(init.ty) != 8 {
                        // Global relocations are emitted as ".quad" so they must
                        // occpy one pointer-sized slot
                        return Err(self.source.error_at(rhs.offset, "invalid initializer"));
                    }
                    relocations.push(Relocation {
                        offset,
                        label,
                        addend,
                    });
                },
            },
            InitializerKind::Aggregate(children) => {
                if let Some(array) = self.types.as_array(init.ty) {
                    let stride = self.types.size(array.base) as usize;
                    for (i, child) in children.into_iter().enumerate() {
                        self.new_global_init2(child, bytes, relocations, offset + i * stride)?;
                    }
                } else if let Some(sou) = self.types.as_struct_or_union(init.ty) {
                    let members = sou.members.clone().unwrap_or_default();
                    for (member, child) in members.iter().zip(children) {
                        self.new_global_init2(child, bytes, relocations, offset + member.offset)?;
                    }
                }
            },
        }

        Ok(())
    }

    /// Build a compound literal expression node.
    ///
    /// Inside a function, this is desugared into a statement expression, which
    /// makes a temporary variable, initializes it, and use it as the result.
    /// Otherwise (at file scope), this is desugared into an initialized hidden
    /// global object.
    fn new_compound_literal(&mut self, init: Initializer, offset: usize) -> Result<Node> {
        if self.types.is_incomplete(init.ty) {
            return Err(self
                .source
                .error_at(offset, "compound literal has incomplete type"));
        }

        if self.active_function.is_none() {
            let label = self.unique_label();
            let ty = init.ty;
            let storage = GlobalStorage::Data(self.new_global_init(init)?);
            let global_id = self.create_global(label, ty, None, storage, true);
            return Ok(Node::entity(EntityRef::Global(global_id), offset));
        }

        let tmp_id = self.create_local("", init.ty, None);
        let tmp = EntityRef::Local(tmp_id);
        let mut stmts = Vec::new();
        self.new_local_init(tmp_id, init, offset, &mut stmts)?;
        stmts.push(Stmt::expr(Node::entity(tmp, offset), offset));
        Ok(Node::stmt_expr(stmts, offset))
    }

    /// Apply a cast on the given node to the given type.
    fn apply_cast(&mut self, node: &mut Node, ty: Type) -> Result<()> {
        let offset = node.offset;
        let mut old = std::mem::take(node);
        self.infer_type(&mut old)?;
        *node = Node::cast(old, ty, offset);
        Ok(())
    }

    /// Apply [usual arithmetic conversion][1] on the given operands.
    ///
    /// Both operands are casted to the coerced type, and that type is also
    /// returned for convenience.
    ///
    /// [1]: https://en.cppreference.com/cpp/language/usual_arithmetic_conversions
    fn apply_usual_arith_conv(&mut self, lhs: &mut Node, rhs: &mut Node) -> Result<Type> {
        let ty = self.types.coerce(lhs.expect_ty(), rhs.expect_ty());
        self.apply_cast(lhs, ty)?;
        self.apply_cast(rhs, ty)?;
        Ok(ty)
    }

    /// Infer types for a statement subtree.
    fn infer_type_stmt(&mut self, stmt: &mut Stmt) -> Result<()> {
        match &mut stmt.kind {
            StmtKind::Expr(expr) => self.infer_type(expr)?,
            StmtKind::Return(expr) => {
                if let Some(expr) = expr {
                    self.infer_type(expr)?;
                }
            },
            StmtKind::Loop {
                init,
                cond,
                inc,
                body,
                ..
            } => {
                if let Some(init) = init {
                    self.infer_type_stmt(init)?;
                }
                if let Some(cond) = cond {
                    self.infer_type(cond)?;
                }
                if let Some(inc) = inc {
                    self.infer_type(inc)?;
                }
                self.infer_type_stmt(body)?;
            },
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.infer_type(cond)?;
                self.infer_type_stmt(then_branch)?;
                if let Some(else_branch) = else_branch {
                    self.infer_type_stmt(else_branch)?;
                }
            },
            StmtKind::Switch { cond, body, .. } => {
                self.infer_type(cond)?;
                self.infer_type_stmt(body)?;
            },
            StmtKind::Block(stmts) => {
                for stmt in stmts {
                    self.infer_type_stmt(stmt)?;
                }
            },
            StmtKind::Label { body, .. } => self.infer_type_stmt(body)?,
            StmtKind::Jump { .. } | StmtKind::MemzeroLocal(_) => {},
        }

        Ok(())
    }

    /// Infer types for an expression subtree.
    fn infer_type(&mut self, node: &mut Node) -> Result<()> {
        if node.ty.is_some() {
            return Ok(());
        }

        node.ty = Some(match &mut node.kind {
            NodeKind::FuncCall { callee, args } => {
                self.infer_type(callee)?;
                for arg in args {
                    self.infer_type(arg)?;
                }
                let ty = callee.expect_ty();
                let Some(func) = self.types.as_func(ty, true) else {
                    return Err(self.source.error_at(node.offset, "not a function"));
                };
                func.return_ty
            },
            NodeKind::Addr(expr) => {
                self.infer_type(expr)?;
                self.types.ptr(expr.expect_ty())
            },
            NodeKind::Deref(expr) => {
                self.infer_type(expr)?;
                let Some(base) = self.types.base(expr.expect_ty()) else {
                    return Err(self
                        .source
                        .error_at(node.offset, "invalid pointer dereference"));
                };
                if base == Type::Void {
                    return Err(self
                        .source
                        .error_at(node.offset, "dereferencing a void pointer"));
                }
                base
            },
            NodeKind::Neg(expr) => {
                self.infer_type(expr)?;
                let mut ty = expr.expect_ty();
                if !ty.is_flonum() {
                    ty = ty
                        .promote_int()
                        .ok_or_else(|| self.source.error_at(node.offset, "invalid operand type"))?
                };
                self.apply_cast(expr, ty)?;
                ty
            },
            NodeKind::Not(expr) => {
                self.infer_type(expr)?;
                Type::INT // C logical operators give int 0/1 not bool
            },
            NodeKind::BitNot(expr) => {
                self.infer_type(expr)?;
                let Some(ty) = expr.expect_ty().promote_int() else {
                    return Err(self.source.error_at(node.offset, "invalid operand type"));
                };
                self.apply_cast(expr, ty)?;
                ty
            },
            NodeKind::Entity(entity) => match *entity {
                EntityRef::Local(local_id) => self.locals[local_id].ty,
                EntityRef::Global(global_id) => self.globals[global_id].ty,
                EntityRef::Function(function_id) => self.functions[function_id].ty,
            },
            NodeKind::Assign { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;

                let lhs_ty = lhs.expect_ty();
                if self.types.as_array(lhs_ty).is_some() {
                    return Err(self.source.error_at(lhs.offset, "not an lvalue"));
                }
                if self.types.as_struct_or_union(lhs_ty).is_none() {
                    self.apply_cast(rhs, lhs_ty)?;
                }
                lhs_ty
            },
            NodeKind::Comma { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                rhs.expect_ty()
            },
            NodeKind::And { lhs, rhs } | NodeKind::Or { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                Type::INT // C logical operators give int 0/1 not bool
            },
            NodeKind::Binary { op, lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                let lhs_ty = lhs.expect_ty();
                let rhs_ty = rhs.expect_ty();

                match op {
                    BinaryOp::Add | BinaryOp::Sub => self.apply_usual_arith_conv(lhs, rhs)?,
                    BinaryOp::Mul | BinaryOp::Div => {
                        if !lhs_ty.is_arith() || !rhs_ty.is_arith() {
                            return Err(self.source.error_at(node.offset, "invalid operands"));
                        }
                        self.apply_usual_arith_conv(lhs, rhs)?
                    },
                    BinaryOp::Mod | BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => {
                        if !lhs_ty.is_integer() || !rhs_ty.is_integer() {
                            return Err(self.source.error_at(node.offset, "invalid operands"));
                        }
                        self.apply_usual_arith_conv(lhs, rhs)?
                    },
                    BinaryOp::BitShl | BinaryOp::BitShr => {
                        let Some(lhs_ty) = lhs_ty.promote_int() else {
                            return Err(self.source.error_at(node.offset, "invalid operands"));
                        };
                        let Some(rhs_ty) = rhs_ty.promote_int() else {
                            return Err(self.source.error_at(node.offset, "invalid operands"));
                        };
                        self.apply_cast(lhs, lhs_ty)?;
                        self.apply_cast(rhs, rhs_ty)?;
                        lhs_ty
                    },
                    BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le => {
                        self.apply_usual_arith_conv(lhs, rhs)?;
                        Type::INT
                    },
                }
            },
            NodeKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => {
                self.infer_type(cond)?;
                self.infer_type(then_expr)?;
                self.infer_type(else_expr)?;

                let then_ty = then_expr.expect_ty();
                let else_ty = else_expr.expect_ty();

                if then_ty == Type::Void || else_ty == Type::Void {
                    Type::Void
                } else {
                    self.apply_usual_arith_conv(then_expr, else_expr)?
                }
            },
            NodeKind::Member { member, .. } => member.ty,
            NodeKind::StmtExpr(body) => {
                for stmt in body.iter_mut() {
                    self.infer_type_stmt(stmt)?;
                }
                if let Some(stmt) = body.last_mut()
                    && let StmtKind::Expr(expr) = &mut stmt.kind
                {
                    self.infer_type(expr)?;
                    expr.expect_ty()
                } else {
                    return Err(self.source.error_at(
                        node.offset,
                        "statement expression returning void is not supported",
                    ));
                }
            },
            NodeKind::Num(_) | NodeKind::Flonum(_) | NodeKind::Cast(_) => {
                unreachable!("node type should have been set upon creation")
            },
            NodeKind::Dummy => unreachable!(),
        });

        Ok(())
    }

    /// Collect [label]s from a statement subtree.
    ///
    /// [label]: StmtKind::Label
    fn collect_labels_stmt(
        &self,
        stmt: &Stmt,
        labels: &mut FxHashMap<SmolStr, SmolStr>,
    ) -> Result<()> {
        let offset = stmt.offset;
        match &stmt.kind {
            StmtKind::Expr(node) => self.collect_labels(node, labels)?,
            StmtKind::Return(node) => {
                if let Some(node) = node {
                    self.collect_labels(node, labels)?;
                }
            },
            StmtKind::Loop {
                init,
                cond,
                inc,
                body,
                ..
            } => {
                if let Some(init) = init {
                    self.collect_labels_stmt(init, labels)?;
                }
                if let Some(cond) = cond {
                    self.collect_labels(cond, labels)?;
                }
                if let Some(inc) = inc {
                    self.collect_labels(inc, labels)?;
                }
                self.collect_labels_stmt(body, labels)?;
            },
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.collect_labels(cond, labels)?;
                self.collect_labels_stmt(then_branch, labels)?;
                if let Some(else_branch) = else_branch {
                    self.collect_labels_stmt(else_branch, labels)?;
                }
            },
            StmtKind::Switch { cond, body, .. } => {
                self.collect_labels(cond, labels)?;
                self.collect_labels_stmt(body, labels)?;
            },
            StmtKind::Block(stmts) => {
                for stmt in stmts {
                    self.collect_labels_stmt(stmt, labels)?;
                }
            },
            StmtKind::Label { label, body, name } => {
                if let Some(name) = name
                    && labels.insert(name.clone(), label.clone()).is_some()
                {
                    return Err(self.source.error_at(offset, "duplicate label"));
                }
                self.collect_labels_stmt(body, labels)?;
            },
            StmtKind::Jump { .. } | StmtKind::MemzeroLocal(_) => {},
        }

        Ok(())
    }

    /// Collect [label]s from an expression subtree.
    ///
    /// [label]: StmtKind::Label
    fn collect_labels(&self, node: &Node, labels: &mut FxHashMap<SmolStr, SmolStr>) -> Result<()> {
        match &node.kind {
            NodeKind::Entity(_) | NodeKind::Num(_) | NodeKind::Flonum(_) => {},
            NodeKind::Addr(expr)
            | NodeKind::Deref(expr)
            | NodeKind::Neg(expr)
            | NodeKind::BitNot(expr)
            | NodeKind::Not(expr)
            | NodeKind::Cast(expr)
            | NodeKind::Member { parent: expr, .. } => self.collect_labels(expr, labels)?,
            NodeKind::Assign { lhs, rhs }
            | NodeKind::Comma { lhs, rhs }
            | NodeKind::And { lhs, rhs }
            | NodeKind::Or { lhs, rhs }
            | NodeKind::Binary { lhs, rhs, .. } => {
                self.collect_labels(lhs, labels)?;
                self.collect_labels(rhs, labels)?;
            },
            NodeKind::FuncCall { callee, args } => {
                self.collect_labels(callee, labels)?;
                for arg in args {
                    self.collect_labels(arg, labels)?;
                }
            },
            NodeKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => {
                self.collect_labels(cond, labels)?;
                self.collect_labels(then_expr, labels)?;
                self.collect_labels(else_expr, labels)?;
            },
            NodeKind::StmtExpr(body) => {
                for stmt in body {
                    self.collect_labels_stmt(stmt, labels)?;
                }
            },
            NodeKind::Dummy => unreachable!(),
        }

        Ok(())
    }

    /// Resolve [goto]s from a statement subtree.
    ///
    /// [goto]: StmtKind::Jump
    fn resolve_gotos_stmt(
        &self,
        stmt: &mut Stmt,
        labels: &FxHashMap<SmolStr, SmolStr>,
    ) -> Result<()> {
        match &mut stmt.kind {
            StmtKind::Expr(node) => self.resolve_gotos(node, labels)?,
            StmtKind::Return(node) => {
                if let Some(node) = node {
                    self.resolve_gotos(node, labels)?;
                }
            },
            StmtKind::Loop {
                init,
                cond,
                inc,
                body,
                ..
            } => {
                if let Some(init) = init {
                    self.resolve_gotos_stmt(init, labels)?;
                }
                if let Some(cond) = cond {
                    self.resolve_gotos(cond, labels)?;
                }
                if let Some(inc) = inc {
                    self.resolve_gotos(inc, labels)?;
                }
                self.resolve_gotos_stmt(body, labels)?;
            },
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.resolve_gotos(cond, labels)?;
                self.resolve_gotos_stmt(then_branch, labels)?;
                if let Some(else_branch) = else_branch {
                    self.resolve_gotos_stmt(else_branch, labels)?;
                }
            },
            StmtKind::Switch { cond, body, .. } => {
                self.resolve_gotos(cond, labels)?;
                self.resolve_gotos_stmt(body, labels)?;
            },
            StmtKind::Block(stmts) => {
                for stmt in stmts {
                    self.resolve_gotos_stmt(stmt, labels)?;
                }
            },
            StmtKind::Jump { label, label_name } => {
                if let Some(name) = label_name {
                    let Some(name) = labels.get(name) else {
                        return Err(self.source.error_at(stmt.offset, "use of undeclared label"));
                    };
                    let _ = label.insert(name.clone());
                }
            },
            StmtKind::Label { body, .. } => self.resolve_gotos_stmt(body, labels)?,
            StmtKind::MemzeroLocal(_) => {},
        }

        Ok(())
    }

    /// Resolve [goto]s from an expression subtree.
    ///
    /// [goto]: StmtKind::Jump
    fn resolve_gotos(&self, node: &mut Node, labels: &FxHashMap<SmolStr, SmolStr>) -> Result<()> {
        match &mut node.kind {
            NodeKind::Entity(_) | NodeKind::Num(_) | NodeKind::Flonum(_) => {},
            NodeKind::Addr(expr)
            | NodeKind::Deref(expr)
            | NodeKind::Neg(expr)
            | NodeKind::BitNot(expr)
            | NodeKind::Not(expr)
            | NodeKind::Cast(expr)
            | NodeKind::Member { parent: expr, .. } => self.resolve_gotos(expr, labels)?,
            NodeKind::Assign { lhs, rhs }
            | NodeKind::Comma { lhs, rhs }
            | NodeKind::And { lhs, rhs }
            | NodeKind::Or { lhs, rhs }
            | NodeKind::Binary { lhs, rhs, .. } => {
                self.resolve_gotos(lhs, labels)?;
                self.resolve_gotos(rhs, labels)?;
            },
            NodeKind::FuncCall { callee, args } => {
                self.resolve_gotos(callee, labels)?;
                for arg in args {
                    self.resolve_gotos(arg, labels)?;
                }
            },
            NodeKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => {
                self.resolve_gotos(cond, labels)?;
                self.resolve_gotos(then_expr, labels)?;
                self.resolve_gotos(else_expr, labels)?;
            },
            NodeKind::StmtExpr(body) => {
                for stmt in body {
                    self.resolve_gotos_stmt(stmt, labels)?;
                }
            },
            NodeKind::Dummy => unreachable!(),
        }

        Ok(())
    }

    /// Evaluate an expression as a compile-time constant.
    ///
    /// This method assumes that integer promotions and usual arithmetic
    /// conversions have already been lowered into explicit casts in the AST.
    fn eval(&mut self, node: &mut Node) -> Result<ConstValue> {
        self.infer_type(node)?;
        let Some(ty) = self.types.to_const(node.expect_ty()) else {
            return Err(self
                .source
                .error_at(node.offset, "not a compile-time constant"));
        };

        Ok(match &mut node.kind {
            NodeKind::Num(val) => ConstValue::int(*val, ty),
            NodeKind::Flonum(val) => ConstValue::float(*val, ty),
            NodeKind::Neg(expr) => self.eval(expr)?.neg(ty),
            NodeKind::Not(expr) => self.eval(expr)?.not(ty),
            NodeKind::BitNot(expr) => self.eval(expr)?.bit_not(ty),
            NodeKind::Comma { rhs, .. } => self.eval(rhs)?,
            NodeKind::And { lhs, rhs } => self.eval(lhs)?.and(|| self.eval(rhs), ty)?,
            NodeKind::Or { lhs, rhs } => self.eval(lhs)?.or(|| self.eval(rhs), ty)?,
            NodeKind::Binary { op, lhs, rhs } => match op {
                BinaryOp::Add => self.eval(lhs)?.add(self.eval(rhs)?, ty),
                BinaryOp::Sub => self.eval(lhs)?.sub(self.eval(rhs)?, ty),
                BinaryOp::Mul => self.eval(lhs)?.mul(self.eval(rhs)?, ty),
                BinaryOp::Div => self
                    .eval(lhs)?
                    .div(self.eval(rhs)?, ty)
                    .ok_or_else(|| self.source.error_at(node.offset, "division by zero"))?,
                BinaryOp::Mod => self
                    .eval(lhs)?
                    .rem(self.eval(rhs)?, ty)
                    .ok_or_else(|| self.source.error_at(node.offset, "division by zero"))?,
                BinaryOp::BitAnd => self.eval(lhs)?.bit_and(self.eval(rhs)?, ty),
                BinaryOp::BitOr => self.eval(lhs)?.bit_or(self.eval(rhs)?, ty),
                BinaryOp::BitXor => self.eval(lhs)?.bit_xor(self.eval(rhs)?, ty),
                BinaryOp::BitShl => self.eval(lhs)?.bit_shl(self.eval(rhs)?, ty),
                BinaryOp::BitShr => self.eval(lhs)?.bit_shr(self.eval(rhs)?, ty),
                BinaryOp::Eq => self.eval(lhs)?.eq(self.eval(rhs)?, ty),
                BinaryOp::Ne => self.eval(lhs)?.ne(self.eval(rhs)?, ty),
                BinaryOp::Lt => self.eval(lhs)?.lt(self.eval(rhs)?, ty),
                BinaryOp::Le => self.eval(lhs)?.le(self.eval(rhs)?, ty),
            },
            NodeKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => {
                if self.eval(cond)?.into() {
                    self.eval(then_expr)?
                } else {
                    self.eval(else_expr)?
                }
            },
            NodeKind::Cast(expr) => {
                debug_assert!(
                    !self.preprocess,
                    "casts should not appear in preprocessing mode",
                );
                self.eval(expr)?.cast(ty)
            },
            NodeKind::Dummy => unreachable!(),
            _ => {
                return Err(self
                    .source
                    .error_at(node.offset, "not a compile-time constant"));
            },
        })
    }

    /// Evaluate a global initializer.
    ///
    /// On top of [`eval`], this further accepts "ptr+n" where "ptr" is a
    /// pointer to a global variable or function and "n" is a number.
    ///
    /// [`eval`]: Self::eval
    fn eval_global_init(&mut self, node: &mut Node) -> Result<GlobalInitValue> {
        self.infer_type(node)?;
        let ty = node.expect_ty();

        if self.types.as_array(ty).is_some() || self.types.is_func(ty) {
            // Arrays and functions decay to addresses in global initializers,
            // so evaluating them is evaluating their addresses rather than
            // plain integer constant evaluation
            return self.eval_global_addr(node);
        }

        let Some(const_ty) = self.types.to_const(ty) else {
            return Err(self
                .source
                .error_at(node.offset, "not a compile-time constant"));
        };

        Ok(match &mut node.kind {
            NodeKind::Binary { op, lhs, rhs } => {
                let lhs = self.eval_global_init(lhs)?;
                let rhs = self.eval_global_init(rhs)?;
                match op {
                    BinaryOp::Add => match (lhs, rhs) {
                        (GlobalInitValue::Num(lhs), GlobalInitValue::Num(rhs)) => {
                            GlobalInitValue::Num(lhs.add(rhs, const_ty))
                        },
                        (GlobalInitValue::Reloc(label, addend), GlobalInitValue::Num(rhs))
                        | (GlobalInitValue::Num(rhs), GlobalInitValue::Reloc(label, addend)) => {
                            if rhs.ty.is_flonum() {
                                return Err(self
                                    .source
                                    .error_at(node.offset, "invalid initializer"));
                            }
                            GlobalInitValue::Reloc(label, addend.wrapping_add(rhs.bits() as i64))
                        },
                        _ => return Err(self.source.error_at(node.offset, "invalid initializer")),
                    },
                    BinaryOp::Sub => match (lhs, rhs) {
                        (GlobalInitValue::Num(lhs), GlobalInitValue::Num(rhs)) => {
                            GlobalInitValue::Num(lhs.sub(rhs, const_ty))
                        },
                        (GlobalInitValue::Reloc(label, addend), GlobalInitValue::Num(rhs)) => {
                            if rhs.ty.is_flonum() {
                                return Err(self
                                    .source
                                    .error_at(node.offset, "invalid initializer"));
                            }
                            GlobalInitValue::Reloc(label, addend.wrapping_sub(rhs.bits() as i64))
                        },
                        _ => return Err(self.source.error_at(node.offset, "invalid initializer")),
                    },
                    _ => GlobalInitValue::Num(self.eval(node)?),
                }
            },
            NodeKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => {
                if self.eval(cond)?.into() {
                    self.eval_global_init(then_expr)?
                } else {
                    self.eval_global_init(else_expr)?
                }
            },
            NodeKind::Comma { rhs, .. } => self.eval_global_init(rhs)?,
            NodeKind::Cast(expr) => match self.eval_global_init(expr)? {
                GlobalInitValue::Num(val) => GlobalInitValue::Num(val.cast(const_ty)),
                GlobalInitValue::Reloc(label, addend) => {
                    if matches!(ty, Type::LONG | Type::ULONG) || self.types.is_ptr(ty) {
                        GlobalInitValue::Reloc(label, addend)
                    } else {
                        return Err(self.source.error_at(node.offset, "invalid initializer"));
                    }
                },
            },
            NodeKind::Addr(expr) => self.eval_global_addr(expr)?,
            _ => GlobalInitValue::Num(self.eval(node)?),
        })
    }

    /// Evaluate an expression as relocatable address for a global initializer.
    fn eval_global_addr(&mut self, node: &mut Node) -> Result<GlobalInitValue> {
        self.infer_type(node)?;

        Ok(match &mut node.kind {
            NodeKind::Entity(entity) => {
                let label = match entity {
                    EntityRef::Global(global_id) => self.globals[*global_id].name.clone(),
                    EntityRef::Function(function_id) => self.functions[*function_id].name.clone(),
                    _ => return Err(self.source.error_at(node.offset, "invalid initializer")),
                };
                GlobalInitValue::Reloc(label, 0)
            },
            NodeKind::Deref(expr) => self.eval_global_init(expr)?,
            NodeKind::Member { parent, member } => match self.eval_global_addr(parent)? {
                GlobalInitValue::Reloc(label, addend) => {
                    GlobalInitValue::Reloc(label, addend + member.offset as i64)
                },
                _ => return Err(self.source.error_at(node.offset, "invalid initializer")),
            },
            _ => return Err(self.source.error_at(node.offset, "invalid initializer")),
        })
    }
}
