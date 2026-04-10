//! A [recursive-descent parser][1] for the C programming language.
//!
//! [1]: https://en.wikipedia.org/wiki/Recursive_descent_parser

use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::{SmolStr, ToSmolStr, format_smolstr};

use crate::ast::{
    BinaryOp, EntityRef, Function, GlobalVar, LocalVar, Node, NodeKind, Program, Stmt, StmtKind,
};
use crate::error::{Error, Result};
use crate::source::Source;
use crate::tokenize::{Keyword, Token};
use crate::types::{ArrayType, Member, StructOrUnionType, Type, TypeStore};
use crate::utils::MAX_FUNC_PARAMS;

/// Declaration of a function parameter.
struct Parameter {
    name: SmolStr,
    ty: Type,
}

/// An object declarator.
struct Declarator {
    name: SmolStr,
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

impl DeclspecContext {
    fn allows_storage_class(self) -> bool {
        matches!(self, Self::FileScopeDecl | Self::BlockScopeDecl)
    }
}

/// A declaration specifier.
struct Declspec {
    ty: Type,
    storage_class: Option<StorageClass>,
}

/// An ordinary identifier.
#[derive(Debug, Copy, Clone)]
enum OrdinaryIdent {
    Entity(EntityRef),
    Typedef(Type),
    EnumConst(i64),
}

impl OrdinaryIdent {
    fn into_entity(self) -> Option<EntityRef> {
        match self {
            OrdinaryIdent::Entity(entity) => Some(entity),
            _ => None,
        }
    }

    fn into_typedef(self) -> Option<Type> {
        match self {
            OrdinaryIdent::Typedef(ty) => Some(ty),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone)]
struct SwitchContext {
    cases: Vec<(i64, SmolStr)>,
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
    tokens: Vec<Token<'a>>,
    pos: usize,

    // Mutable states
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
    pub fn new(source: &'a Source, tokens: Vec<Token<'a>>) -> Self {
        Self {
            source,
            tokens,
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
    fn current(&self) -> &Token<'a> {
        &self.tokens[self.pos]
    }

    /// Return the token at the given lookahead distance.
    fn peek(&self, offset: usize) -> Option<&Token<'a>> {
        self.tokens.get(self.pos + offset)
    }

    /// Return an error diagnostic at the current token.
    fn error_current(&self, message: impl Into<SmolStr>) -> Error {
        self.source.error_at(self.current().offset, message)
    }

    /// Emit a warning message at the current token.
    fn warn_current(&self, message: impl Into<SmolStr>) {
        self.source.warn_at(self.current().offset, message);
    }

    /// Assume and skip a specific punctuator.
    fn skip_punct(&mut self, expected: &str) -> Result<()> {
        if !self.current().is_punct(expected) {
            return Err(self.error_current(format_smolstr!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// Assume and consume an identifier.
    fn consume_ident(&mut self) -> Result<&'a str> {
        let Some(ident) = self.current().as_ident() else {
            return Err(self.error_current("expected an identifier"));
        };
        self.advance();
        Ok(ident)
    }

    /// Return whether the current token can be interpreted as a typename.
    fn at_typename(&self) -> bool {
        if self.current().is_typename_keyword() {
            return true;
        }
        let Some(name) = self.current().as_ident() else {
            return false;
        };
        self.find_ident(name)
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
    ///   | "void"
    ///   | "_Bool"
    ///   | "char"
    ///   | "short"
    ///   | "int"
    ///   | "long"
    ///   | <struct-or-union-decl>
    ///   | <typedef-name>
    ///   | <enum-specifier>
    /// ```
    ///
    /// As per C language specification, type specifiers are order-insensitive,
    /// but only certain combinations are legal.
    fn parse_declspec(&mut self, context: DeclspecContext) -> Result<Declspec> {
        enum TypeSpec {
            Void,
            Bool,
            Char,
            Short,
            Int,
            Long,
            Other(Type),
        }

        let mut spec = None;
        let mut long_count = 0;
        let mut storage_class = None;

        while self.at_typename() {
            let offset = self.current().offset;
            let keyword = self.current().as_keyword();
            let ident = self.current().as_ident();
            let typedef_ty = ident
                .and_then(|ident| self.find_ident(ident))
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

            match keyword {
                Some(Keyword::Void) => match spec {
                    None => spec = Some(TypeSpec::Void),
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Bool) => match spec {
                    None => spec = Some(TypeSpec::Bool),
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Char) => match spec {
                    None => spec = Some(TypeSpec::Char),
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Short) => match spec {
                    None | Some(TypeSpec::Int) => spec = Some(TypeSpec::Short),
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Int) => match spec {
                    None => spec = Some(TypeSpec::Int),
                    Some(TypeSpec::Short | TypeSpec::Long) => {},
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Long) => match spec {
                    None | Some(TypeSpec::Int) | Some(TypeSpec::Long) if long_count < 2 => {
                        spec = Some(TypeSpec::Long);
                        long_count += 1;
                    },
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Struct) => match spec {
                    None => {
                        spec = Some(TypeSpec::Other(
                            self.parse_struct_or_union_decl(true, context)?,
                        ))
                    },
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Union) => match spec {
                    None => {
                        spec = Some(TypeSpec::Other(
                            self.parse_struct_or_union_decl(false, context)?,
                        ))
                    },
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Enum) => match spec {
                    None => spec = Some(TypeSpec::Other(self.parse_enum_specifier()?)),
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Typedef | Keyword::Static) => {
                    if !context.allows_storage_class() {
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
                    storage_class = Some(match keyword.unwrap() {
                        Keyword::Typedef => StorageClass::Typedef,
                        Keyword::Static => StorageClass::Static,
                        _ => unreachable!(),
                    });
                },
                _ => match typedef_ty {
                    Some(ty) if spec.is_none() => spec = Some(TypeSpec::Other(ty)),
                    Some(_) => unreachable!(), // Early break'ed
                    None => unreachable!("all typename tokens should have been handled"),
                },
            }
        }

        if spec.is_none() {
            if let Some(storage_class) = storage_class {
                return Err(self
                    .error_current(format_smolstr!("missing type specifier in {storage_class}")));
            }
            return Err(self.error_current("expected a typename"));
        }

        let ty = match spec.unwrap() {
            TypeSpec::Void => Type::Void,
            TypeSpec::Bool => Type::Bool,
            TypeSpec::Char => Type::Char,
            TypeSpec::Short => Type::Short,
            TypeSpec::Int => Type::Int,
            TypeSpec::Long => Type::Long,
            TypeSpec::Other(ty) => ty,
        };

        Ok(Declspec { ty, storage_class })
    }

    /// ```bnf
    /// <declarator> ::= "*"* (<ident> | "(" <declarator> ")") <type-suffix>
    /// ```
    fn parse_declarator(&mut self, mut ty: Type) -> Result<Declarator> {
        while self.current().is_punct("*") {
            self.advance();
            ty = self.types.ptr(ty);
        }

        if self.current().is_punct("(") {
            self.advance();
            let inner_pos = self.pos; // After "("

            // Try to parse the inner declarator to find where it ends, i.e.,
            // the matching ")"
            let (_, next_pos) = self.speculate(|parser| {
                parser.parse_declarator(Default::default())?;
                parser.skip_punct(")")?;
                Ok(())
            })?;

            // Parse the type suffix after ")"
            self.pos = next_pos;
            let (ty, params) = self.parse_type_suffix(ty)?;
            let next_pos = self.pos;

            // Rewind to parse the inner declarator again, this time with the
            // real type; we don't go through the type suffix again but rather
            // directly take its params
            self.pos = inner_pos;
            let mut declarator = self.parse_declarator(ty)?;
            declarator.params = params;
            self.pos = next_pos;
            return Ok(declarator);
        }

        let offset = self.current().offset;
        let name = self.consume_ident()?;
        let (ty, params) = self.parse_type_suffix(ty)?;

        Ok(Declarator {
            name: SmolStr::new(name),
            ty,
            offset,
            params,
        })
    }

    /// ```bnf
    /// <abstract-declarator> ::=
    ///   "*"* ("(" <abstract-declarator> ")")? <type-suffix>
    /// ```
    fn parse_abstract_declarator(&mut self, mut ty: Type) -> Result<Type> {
        while self.current().is_punct("*") {
            self.advance();
            ty = self.types.ptr(ty);
        }

        // The following part of logic is analogous to "parse_declarator"
        if self.current().is_punct("(") {
            self.advance();
            let inner_pos = self.pos;

            let (_, next_pos) = self.speculate(|parser| {
                parser.parse_abstract_declarator(Default::default())?;
                parser.skip_punct(")")?;
                Ok(())
            })?;

            self.pos = next_pos;
            let (ty, _) = self.parse_type_suffix(ty)?;
            let next_pos = self.pos;

            self.pos = inner_pos;
            let ty = self.parse_abstract_declarator(ty)?;
            self.pos = next_pos;
            return Ok(ty);
        }

        let (ty, _) = self.parse_type_suffix(ty)?;
        Ok(ty)
    }

    /// ```bnf
    /// <typename> ::= <declspec> <abstract-declarator>
    /// ```
    fn parse_typename(&mut self) -> Result<Type> {
        let declspec = self.parse_declspec(DeclspecContext::Typename)?;
        self.parse_abstract_declarator(declspec.ty)
    }

    /// ```bnf
    /// <type-suffix> ::= "(" <func-params> | ("[" <num> "]")*
    /// ```
    fn parse_type_suffix(&mut self, ty: Type) -> Result<(Type, Vec<Parameter>)> {
        if self.current().is_punct("(") {
            self.advance();
            return self.parse_func_params(ty);
        }

        let ty = self.parse_array_dimensions(ty)?;
        Ok((ty, Vec::new()))
    }

    /// ```bnf
    /// <func-params> ::= (<param> ("," <param>)*)? ")"
    /// <param> ::= <declspec> <declarator>
    /// ```
    fn parse_func_params(&mut self, return_ty: Type) -> Result<(Type, Vec<Parameter>)> {
        let mut params = Vec::new();
        let mut param_names = FxHashSet::default();

        while !self.current().is_punct(")") {
            if !params.is_empty() {
                self.skip_punct(",")?;
            }

            let declspec = self.parse_declspec(DeclspecContext::ParameterDecl)?;

            let offset = self.current().offset;
            let declarator = self.parse_declarator(declspec.ty)?;

            let ty = if let Some(array) = self.types.as_array(declarator.ty) {
                // Array decay will convert "array of T" to "pointer to T" in
                // parameter declarations; e.g., "*argv[]" being converted to
                // "**argv" is because of this rule
                self.types.ptr(array.base)
            } else {
                declarator.ty
            };

            if self.types.is_incomplete(ty) {
                return Err(self
                    .source
                    .error_at(offset, "parameter has incomplete type"));
            }

            if !param_names.insert(declarator.name.clone()) {
                return Err(self.source.error_at(offset, "redefinition of parameter"));
            }

            params.push(Parameter {
                name: declarator.name,
                ty,
            });

            if params.len() > MAX_FUNC_PARAMS {
                return Err(self
                    .source
                    .error_at(declarator.offset, "too many parameters"));
            }
        }

        self.skip_punct(")")?;
        let param_tys = params.iter().map(|param| param.ty).collect();
        Ok((self.types.func(return_ty, param_tys), params))
    }

    /// ```bnf
    /// <array-dimensions> ::= ("[" <constexpr>? "]")*
    /// ```
    fn parse_array_dimensions(&mut self, ty: Type) -> Result<Type> {
        if !self.current().is_punct("[") {
            return Ok(ty);
        }

        let offset = self.current().offset;
        self.advance();

        let len = if self.current().is_punct("]") {
            None
        } else {
            let len = self.parse_constexpr()?;
            let Ok(len) = usize::try_from(len) else {
                return Err(self.error_current("array size is negative or out of range"));
            };
            Some(len)
        };

        self.skip_punct("]")?;

        let ty = self.parse_array_dimensions(ty)?;
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
                    let Some(ty) = self.find_tag_current(tag) else {
                        let ty = self.types.struct_or_union(is_struct, None);
                        self.push_scope_tag(tag.to_smolstr(), ty);
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
                let Some(ty) = self.find_tag(tag) else {
                    let ty = self.types.struct_or_union(is_struct, None);
                    self.push_scope_tag(tag.to_smolstr(), ty);
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
            let Some(ty) = self.find_tag_current(tag) else {
                // Note: We have to create an incomplete type first, then parse
                // the members and complete the type; this is to handle self-
                // referential structs, so members can see that this type is
                // already declared and will not create a separate declaration
                let ty = self.types.struct_or_union(is_struct, None);
                self.push_scope_tag(tag.to_smolstr(), ty);
                self.advance();
                let members = self.parse_members_decl(member_context)?;
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
                let members = self.parse_members_decl(member_context)?;
                self.types.complete_struct_or_union(is_struct, members, ty);
                return Ok(ty);
            }

            return Err(self
                .source
                .error_at(offset, format_smolstr!("defined as wrong kind of tag")));
        }

        // "struct {...}", which is an anonymous definition
        self.skip_punct("{")?;
        let members = self.parse_members_decl(member_context)?;
        let ty = self.types.struct_or_union(is_struct, Some(members));
        Ok(ty)
    }

    /// ```bnf
    /// <members-decl> ::= <member-decl>* "}"
    /// <member-decl> ::= <declspec> <declarator> ("," <declarator>)* ";"
    /// ```
    fn parse_members_decl(&mut self, context: DeclspecContext) -> Result<Vec<Member>> {
        let mut members = Vec::new();

        while !self.current().is_punct("}") {
            let declspec = self.parse_declspec(context)?;

            let mut first = true;
            while !self.current().is_punct(";") {
                if !first {
                    self.skip_punct(",")?;
                }
                first = false;

                let declarator = self.parse_declarator(declspec.ty)?;
                if self.types.is_incomplete(declarator.ty) {
                    return Err(self
                        .source
                        .error_at(declarator.offset, "field has incomplete type"));
                }
                members.push(Member {
                    name: declarator.name,
                    ty: declarator.ty,
                    offset: 0, // union requires 0; struct fills in later
                });
            }

            self.advance();
        }

        self.advance();
        Ok(members)
    }

    /// ```bnf
    /// <enum-specifier> ::= <ident>? "{" <enum-list>? "}" | <ident>
    /// <enum-list> ::=
    ///   <ident> ("=" <constexpr>)? ("," <ident> ("=" <constexpr>)?)*
    /// ```
    fn parse_enum_specifier(&mut self) -> Result<Type> {
        let offset = self.current().offset;
        let tag = self.current().as_ident();

        if let Some(tag) = tag {
            self.advance();
            if !self.current().is_punct("{") {
                let ty = self
                    .find_tag(tag)
                    .ok_or_else(|| self.source.error_at(offset, "unknown enum type"))?;
                if !matches!(ty, Type::Enum) {
                    return Err(self.source.error_at(offset, "not an enum tag"));
                }
                return Ok(ty);
            }
        }

        self.skip_punct("{")?;

        let mut first = true;
        let mut val = 0;
        while !self.current().is_punct("}") {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let name = self.consume_ident()?;
            if self.current().is_punct("=") {
                self.advance();
                val = self.parse_constexpr()?;
            }

            self.push_scope_ident(name.to_smolstr(), OrdinaryIdent::EnumConst(val));
            val += 1;
        }

        self.advance();

        let ty = Type::Enum;
        if let Some(tag) = tag {
            self.push_scope_tag(tag.to_smolstr(), ty);
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
                self.parse_typedef_tail(declspec.ty)?;
                continue;
            }

            if self.is_function()? {
                self.parse_function(
                    declspec.ty,
                    declspec.storage_class == Some(StorageClass::Static),
                )?;
                continue;
            }

            // TODO: Fix static global variables being treated as non-static
            self.parse_global_variable(declspec.ty)?;
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
    fn is_function(&mut self) -> Result<bool> {
        if self.current().is_punct(";") {
            return Ok(false);
        }

        let (result, _) = self.speculate(|parser| {
            let declarator = parser.parse_declarator(Default::default())?;
            Ok(parser.types.is_func(declarator.ty))
        })?;
        Ok(result)
    }

    /// ```bnf
    /// <function> ::= <declarator> (";" | "{" <compound-stmt>)
    /// ```
    fn parse_function(&mut self, return_ty: Type, is_static: bool) -> Result<()> {
        self.disallow_speculation();

        let declarator = self.parse_declarator(return_ty)?;
        if !self.types.is_func(declarator.ty) {
            return Err(self.error_current("expected a function"));
        }

        let func_id = self.create_function_decl(declarator.name.clone(), declarator.ty);
        if self.current().is_punct(";") {
            self.advance();
            return Ok(());
        }

        self.active_function = Some(func_id);

        let body_offset = self.current().offset;
        self.locals.clear();
        self.enter_scope();
        let param_locals = self.create_param_locals(declarator.params);
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
        function.locals = std::mem::take(&mut self.locals);
        function.is_static = is_static;

        self.active_function = None;
        Ok(())
    }

    /// ```bnf
    /// <global-variable> ::= <declarator> ("," <declarator>)* ";"
    /// ```
    fn parse_global_variable(&mut self, base_ty: Type) -> Result<()> {
        self.disallow_speculation();
        let mut first = true;

        while !self.current().is_punct(";") {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let declarator = self.parse_declarator(base_ty)?;
            if self.types.is_func(declarator.ty) {
                return Err(self
                    .source
                    .error_at(declarator.offset, "expected a global variable"));
            }
            let global_id = self.create_global(declarator.name, declarator.ty, None);
            let mut ty = self.globals[global_id].ty;

            if self.current().is_punct("=") {
                self.advance();
                let init = self.parse_initializer(ty)?;
                ty = init.ty;
                let init_data = self.new_global_init(init)?;
                self.globals[global_id].init_data = Some(init_data.into());
            }

            if self.types.is_incomplete(ty) {
                return Err(self
                    .source
                    .error_at(declarator.offset, "variable has incomplete type"));
            }
            self.globals[global_id].ty = ty;
        }

        self.skip_punct(";")?;
        Ok(())
    }

    /// ```bnf
    /// <stmt> ::=
    ///   "return" <expr> ";"
    ///   | "if" "(" <expr> ")" <stmt> ("else" <stmt>)?
    ///   | "switch" "(" <expr> ")" <stmt>
    ///   | "case" <constexpr> ":" <stmt>
    ///   | "default" ":" <stmt>
    ///   | "for" "(" <expr-stmt> <expr>? ";" <expr>? ")" <stmt>
    ///   | "while" "(" <expr> ")" <stmt>
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
            let mut expr = self.parse_expr()?;
            self.skip_punct(";")?;

            let func_ty = self.functions[self.active_function.unwrap()].ty;
            let return_ty = self.types.as_func(func_ty).unwrap().return_ty;
            self.apply_cast(&mut expr, return_ty)?;
            return Ok(Stmt::return_(expr, offset));
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
            let cond = self.parse_expr()?;
            self.skip_punct(")")?;

            let brk_label = self.unique_label();
            let prev_brk_label = self.active_brk_label.replace(brk_label.clone());
            let prev_switch = self.active_switch.replace(SwitchContext::default());

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
            let val = self.parse_constexpr()?;
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
                self.parse_declaration(declspec.ty)?
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

            return Ok(Stmt::while_(cond, body, brk_label, cont_label, offset));
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
                        self.parse_typedef_tail(declspec.ty)?;
                        continue;
                    }
                    // TODO: Fix static local variables being treated as non-static
                    self.parse_declaration(declspec.ty)?
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
    fn parse_declaration(&mut self, base_ty: Type) -> Result<Stmt> {
        let offset = self.current().offset;
        let mut stmts = Vec::new();
        let mut first = true;

        while !self.current().is_punct(";") {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let declarator = self.parse_declarator(base_ty)?;
            let local_id = self.create_local(declarator.name, declarator.ty);

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

        self.skip_punct(";")?;
        Ok(Stmt::block(stmts, offset))
    }

    /// ```bnf
    /// <initializer> ::=
    ///   <string-initializer>
    ///   | <array-initializer>
    ///   | <struct-initializer>
    ///   | <union-initializer>
    ///   | <assign>
    /// ```
    fn parse_initializer(&mut self, mut ty: Type) -> Result<Initializer> {
        if let Some(array) = self.types.as_array(ty).cloned() {
            let elements = if matches!(array.base, Type::Char)
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
                let mut assign = self.parse_assign()?;
                self.infer_type(&mut assign)?;
                if assign.expect_ty() != ty {
                    return Err(self.error_current("invalid initializer"));
                }
                return Ok(Initializer {
                    ty,
                    kind: InitializerKind::Expr(assign),
                });
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
            while !self.current().is_punct("}") {
                if !first {
                    self.skip_punct(",")?;
                }
                first = false;
                self.skip_initializer()?;
            }

            self.advance();
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
        array: &ArrayType,
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
                ty: Type::Char,
                kind: InitializerKind::Expr(Node::num(byte as i8 as _, offset, false)),
            })
            .collect();

        self.advance();
        elements
    }

    /// ```bnf
    /// <array-initializer> ::= "{" <initializer> ("," <initializer>)* "}"
    /// ```
    fn parse_array_initializer(&mut self, array: &ArrayType) -> Result<Vec<Initializer>> {
        self.skip_punct("{")?;

        let mut elements = Vec::with_capacity(array.len.unwrap_or(0));
        let mut i = 0;
        while !self.current().is_punct("}") {
            if i > 0 {
                self.skip_punct(",")?;
            }

            if array.len.is_none_or(|len| i < len) {
                // Note that for incomplete array (with unspecified length), it
                // will be later completed according to the number of elements
                // so we should always push in that case
                elements.push(self.parse_initializer(array.base)?);
            } else {
                self.warn_current("excess elements in array initializer");
                self.skip_initializer()?;
            }
            i += 1;
        }

        self.advance();
        Ok(elements)
    }

    /// ```bnf
    /// <struct-initializer> ::= "{" <initializer> ("," <initializer>)* "}"
    /// ```
    fn parse_struct_initializer(&mut self, sou: &StructOrUnionType) -> Result<Vec<Initializer>> {
        self.skip_punct("{")?;

        let members = sou.members.clone().unwrap_or_default();
        let mut elements = Vec::with_capacity(members.len());
        let mut i = 0;
        while !self.current().is_punct("}") {
            if i > 0 {
                self.skip_punct(",")?;
            }

            if i < members.len() {
                elements.push(self.parse_initializer(members[i].ty)?);
            } else {
                self.warn_current("excess elements in struct initializer");
                self.skip_initializer()?;
            }
            i += 1;
        }

        self.advance();
        Ok(elements)
    }

    /// ```bnf
    /// <union-initializer> ::= "{" <initializer> "}"
    /// ```
    fn parse_union_initializer(&mut self, sou: &StructOrUnionType) -> Result<Vec<Initializer>> {
        self.skip_punct("{")?;

        let mut elements = Vec::new();
        // Union initializer takes only one initializer and initializes the
        // first union member
        if let Some(member) = sou.members.as_ref().and_then(|members| members.first()) {
            elements.push(self.parse_initializer(member.ty)?);
        }

        self.skip_punct("}")?;
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
            let binary = Node::binary(BinaryOp::BitLeftShift, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct(">>=") {
            self.advance();
            let assign = self.parse_assign()?;
            let binary = Node::binary(BinaryOp::BitRightShift, node, assign, offset);
            return self.new_compound_assign(binary, offset);
        }

        Ok(node)
    }

    /// ```bnf
    /// <conditional> ::= <logical-or> ("?" <expr> ":" <conditional>)?
    /// ```
    fn parse_conditional(&mut self) -> Result<Node> {
        let node = self.parse_logical_or()?;

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
    /// <logical-or> ::= <logical-and> ("||" <logical-and>)*
    /// ```
    fn parse_logical_or(&mut self) -> Result<Node> {
        let mut node = self.parse_logical_and()?;

        while self.current().is_punct("||") {
            let offset = self.current().offset;
            self.advance();
            node = Node::logical_or(node, self.parse_logical_and()?, offset);
        }

        Ok(node)
    }

    /// ```bnf
    /// <logical-and> ::= <bit-or> ("&&" <bit-or>)*
    /// ```
    fn parse_logical_and(&mut self) -> Result<Node> {
        let mut node = self.parse_bit_or()?;

        while self.current().is_punct("&&") {
            let offset = self.current().offset;
            self.advance();
            node = Node::logical_and(node, self.parse_bit_or()?, offset);
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
                // Reuse < with flipped operands
                node = Node::binary(BinaryOp::Lt, self.parse_shift()?, node, offset);
                continue;
            }

            if self.current().is_punct(">=") {
                self.advance();
                // Reuse <= with flipped operands
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
                node = Node::binary(BinaryOp::BitLeftShift, node, self.parse_add()?, offset);
                continue;
            }

            if self.current().is_punct(">>") {
                self.advance();
                node = Node::binary(BinaryOp::BitRightShift, node, self.parse_add()?, offset);
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

        if self.current().is_punct("(") {
            let pos = self.pos;
            self.advance();

            if self.at_typename() {
                let ty = self.parse_typename()?;
                self.skip_punct(")")?;
                let mut expr = self.parse_cast()?;
                self.infer_type(&mut expr)?;
                return Ok(Node::cast(expr, ty, offset));
            }

            self.pos = pos;
        }

        self.parse_unary()
    }

    /// ```bnf
    /// <unary> ::=
    ///   ("+" | "-" | "*" | "&" | "!" | "~") <cast>
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

        if self.current().is_punct("&") {
            self.advance();
            return Ok(Node::addr(self.parse_cast()?, offset));
        }

        if self.current().is_punct("*") {
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

        if self.current().is_punct("++") {
            self.advance();
            let unary = self.parse_unary()?;
            let binary = self.new_add(unary, Node::num(1, offset, false), offset)?;
            return self.new_compound_assign(binary, offset);
        }

        if self.current().is_punct("--") {
            self.advance();
            let unary = self.parse_unary()?;
            let binary = self.new_sub(unary, Node::num(1, offset, false), offset)?;
            return self.new_compound_assign(binary, offset);
        }

        self.parse_postfix()
    }

    /// ```bnf
    /// <postfix> ::=
    ///   <primary> ("[" <expr> "]" | "." <ident> | "->" <ident> | "++" | "--")*
    /// ```
    fn parse_postfix(&mut self) -> Result<Node> {
        let mut node = self.parse_primary()?;

        loop {
            let offset = self.current().offset;

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
    ///   | "sizeof" "(" <typename> ")"
    ///   | "sizeof" <unary>
    ///   | <func-call>
    ///   | <ident>
    ///   | <str>
    ///   | <num>
    /// ```
    fn parse_primary(&mut self) -> Result<Node> {
        let offset = self.current().offset;

        if self.current().is_punct("(") {
            self.advance();

            if self.current().is_punct("{") {
                self.advance();
                let body = self.parse_compound_stmt()?;
                self.skip_punct(")")?;
                return Ok(Node::stmt_expr(body, offset));
            }

            let node = self.parse_expr()?;
            self.skip_punct(")")?;
            return Ok(node);
        }

        if self.current().is_keyword(Keyword::Sizeof) {
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
                            .error_at(offset, "cannot apply sizeof to incomplete type"));
                    }
                    return Ok(Node::num(self.types.size(ty), offset, false));
                }

                self.pos = pos;
            }

            let mut operand = self.parse_unary()?;
            self.infer_type(&mut operand)?;
            let size = self.types.size(operand.expect_ty());
            return Ok(Node::num(size, offset, false));
        }

        if let Some(name) = self.current().as_ident() {
            if self.peek(1).is_some_and(|tok| tok.is_punct("(")) {
                return self.parse_func_call(name);
            }

            self.advance();

            let node = match self.find_ident(name) {
                Some(OrdinaryIdent::Entity(entity)) => Node::entity(entity, offset),
                Some(OrdinaryIdent::EnumConst(val)) => Node::num(val, offset, false),
                _ => return Err(self.source.error_at(offset, "undefined identifier")),
            };
            return Ok(node);
        }

        if let Some(content) = self.current().as_str() {
            let ty = self.types.array(Type::Char, Some(content.len()));
            let global_id = self.create_anon_global(ty, content);
            self.advance();
            return Ok(Node::entity(EntityRef::Global(global_id), offset));
        }

        if let Some(value) = self.current().as_num() {
            self.advance();
            return Ok(Node::num(value, offset, false));
        }

        Err(self.error_current("expected an expression"))
    }

    /// ```bnf
    /// <func-call> ::= <ident> "(" (<assign> ("," <assign>)*)? ")"
    /// ```
    fn parse_func_call(&mut self, name: &str) -> Result<Node> {
        let offset = self.current().offset;
        self.advance();
        self.skip_punct("(")?;

        let entity = self
            .find_ident(name)
            .and_then(OrdinaryIdent::into_entity)
            .ok_or_else(|| {
                self.source
                    .error_at(offset, "implicit declaration of a function")
            })?;

        let EntityRef::Function(func_id) = entity else {
            return Err(self.source.error_at(offset, "not a function"));
        };

        let func = self
            .types
            .as_func(self.functions[func_id].ty)
            .unwrap()
            .clone();
        let mut param_tys = func.params.iter().copied();

        let mut args = Vec::new();
        while !self.current().is_punct(")") {
            if !args.is_empty() {
                self.skip_punct(",")?;
            }

            let mut arg = self.parse_assign()?;
            if let Some(param_ty) = param_tys.next() {
                if self.types.as_struct_or_union(param_ty).is_some() {
                    return Err(self.source.error_at(
                        arg.offset,
                        "passing struct or union by value is not supported yet",
                    ));
                }
                self.apply_cast(&mut arg, param_ty)?;
            }
            args.push(arg);
        }

        self.skip_punct(")")?;
        Ok(Node::func_call(name, args, func.return_ty, offset))
    }

    /// ```bnf
    /// <typedef-tail> ::= <declarator> ("," <declarator>)* ";"
    /// ```
    fn parse_typedef_tail(&mut self, base_ty: Type) -> Result<()> {
        let mut first = true;

        while !self.current().is_punct(";") {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let declarator = self.parse_declarator(base_ty)?;
            let typedef = OrdinaryIdent::Typedef(declarator.ty);
            self.push_scope_ident(declarator.name, typedef);
        }

        self.skip_punct(";")?;
        Ok(())
    }

    /// ```bnf
    /// <constexpr> ::= <conditional>
    /// ```
    ///
    /// Semantically, not every conditional expression is a valid constant
    /// expression, and that restriction is forced by [`Self::eval`].
    fn parse_constexpr(&mut self) -> Result<i64> {
        let mut node = self.parse_conditional()?;
        self.eval(&mut node)
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
    fn create_local(&mut self, name: impl Into<SmolStr>, ty: Type) -> usize {
        self.disallow_speculation();

        let name = name.into();
        self.locals.push(LocalVar {
            _name: name.clone(),
            ty,
            offset: 0, // Assigned during codegen
        });

        let id = self.locals.len() - 1;
        let entity = OrdinaryIdent::Entity(EntityRef::Local(id));
        self.push_scope_ident(name, entity);
        id
    }

    /// Create local variables for function parameters.
    ///
    /// Parameters are pushed in reverse order to ensure the first parameter
    /// gets the lowest local ID.
    fn create_param_locals(&mut self, params: Vec<Parameter>) -> Vec<usize> {
        let mut param_ids = Vec::with_capacity(params.len());

        for param in params.into_iter().rev() {
            param_ids.push(self.create_local(param.name, param.ty));
        }

        param_ids.reverse();
        param_ids
    }

    /// Create a new global variable.
    fn create_global(
        &mut self,
        name: impl Into<SmolStr>,
        ty: Type,
        init_data: Option<Rc<[u8]>>,
    ) -> usize {
        self.disallow_speculation();

        let name = name.into();
        self.globals.push(GlobalVar {
            name: name.clone(),
            ty,
            init_data,
        });

        let id = self.globals.len() - 1;
        let entity = OrdinaryIdent::Entity(EntityRef::Global(id));
        self.push_scope_ident(name, entity);
        id
    }

    /// Create a new anonymous global variable.
    fn create_anon_global(&mut self, ty: Type, init_data: Rc<[u8]>) -> usize {
        self.disallow_speculation();

        let name = self.unique_label();
        self.create_global(name, ty, Some(init_data))
    }

    /// Create a new function declaration.
    ///
    /// If the function is also defined (i.e., has a body), it needs to be
    /// filled in later, looked up via the returned ID.
    fn create_function_decl(&mut self, name: impl Into<SmolStr>, ty: Type) -> usize {
        self.disallow_speculation();

        let name = name.into();
        self.functions.push(Function {
            name: name.clone(),
            ty,
            body: None,
            param_locals: Default::default(),
            locals: Default::default(),
            is_static: false,
        });

        let id = self.functions.len() - 1;
        let entity = OrdinaryIdent::Entity(EntityRef::Function(id));
        self.push_scope_ident(name, entity);
        id
    }

    /// Build an addition node with pointer scaling.
    fn new_add(&mut self, mut lhs: Node, mut rhs: Node, offset: usize) -> Result<Node> {
        self.infer_type(&mut lhs)?;
        self.infer_type(&mut rhs)?;

        let lhs_ty = lhs.expect_ty();
        let rhs_ty = rhs.expect_ty();

        // num + num
        if lhs_ty.is_integer() && rhs_ty.is_integer() {
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
            Node::num(base_size, offset, true),
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
        if lhs_ty.is_integer() && rhs_ty.is_integer() {
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
                Node::num(base_size, offset, true),
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
            diff.ty = Some(Type::Int);
            let node = Node::binary(
                BinaryOp::Div,
                diff,
                Node::num(base_size, offset, true),
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
        let tmp = EntityRef::Local(self.create_local("", lhs_ty));

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

        let addend = if is_inc { 1 } else { -1 };

        // node += addend
        let binary = self.new_add(node, Node::num(addend, offset, false), offset)?;
        let assign = self.new_compound_assign(binary, offset)?;

        // (node += addend) - addend
        let mut post = self.new_add(assign, Node::num(-addend, offset, false), offset)?;
        self.infer_type(&mut post)?;

        // (typeof node)((node += addend) - addend)
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
                            let index = Node::num(*index as _, offset, false);
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
    fn new_global_init(&mut self, init: Initializer) -> Result<Vec<u8>> {
        let mut data = vec![0; self.types.size(init.ty) as usize];
        self.new_global_init2(init, &mut data, 0)?;
        Ok(data)
    }

    fn new_global_init2(
        &mut self,
        init: Initializer,
        data: &mut [u8],
        offset: usize,
    ) -> Result<()> {
        let mut write = |val: i64, size| match size {
            1 => data[offset] = val as _,
            2 => data[offset..offset + 2].copy_from_slice(&(val as i16).to_ne_bytes()),
            4 => data[offset..offset + 4].copy_from_slice(&(val as i32).to_ne_bytes()),
            8 => data[offset..offset + 8].copy_from_slice(&val.to_ne_bytes()),
            _ => unreachable!(),
        };

        match init.kind {
            InitializerKind::Expr(mut rhs) => write(self.eval(&mut rhs)?, self.types.size(init.ty)),
            InitializerKind::Aggregate(children) => {
                if let Some(array) = self.types.as_array(init.ty) {
                    let stride = self.types.size(array.base) as usize;
                    for (i, child) in children.into_iter().enumerate() {
                        self.new_global_init2(child, data, offset + i * stride)?;
                    }
                } else if let Some(sou) = self.types.as_struct_or_union(init.ty) {
                    let members = sou.members.clone().unwrap_or_default();
                    for (member, child) in members.iter().zip(children) {
                        self.new_global_init2(child, data, offset + member.offset)?;
                    }
                }
            },
        }

        Ok(())
    }

    /// Apply a cast on the given node to the given type.
    fn apply_cast(&mut self, node: &mut Node, ty: Type) -> Result<()> {
        let offset = node.offset;
        let mut old = std::mem::take(node);
        self.infer_type(&mut old)?;
        *node = Node::cast(old, ty, offset);
        Ok(())
    }

    /// Apply a usual arithmetic conversion on the given operands.
    ///
    /// Returns the coerced common type. This is lhs-biased, see [`coerce`] for
    /// more details.
    ///
    /// [`coerce`]: TypeStore::coerce
    fn apply_usual_arith_conv(&mut self, lhs: &mut Node, rhs: &mut Node) -> Result<Type> {
        let ty = self.types.coerce(lhs.expect_ty(), rhs.expect_ty());
        self.apply_cast(lhs, ty)?;
        self.apply_cast(rhs, ty)?;
        Ok(ty)
    }

    /// Infer types for a statement subtree.
    fn infer_type_stmt(&mut self, stmt: &mut Stmt) -> Result<()> {
        match &mut stmt.kind {
            StmtKind::Expr(expr) | StmtKind::Return(expr) => self.infer_type(expr)?,
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
            NodeKind::FuncCall { args, .. } => {
                for arg in args {
                    self.infer_type(arg)?;
                }
                Type::Long
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
                if matches!(base, Type::Void) {
                    return Err(self
                        .source
                        .error_at(node.offset, "dereferencing a void pointer"));
                }
                base
            },
            NodeKind::Neg(expr) | NodeKind::BitNot(expr) => {
                self.infer_type(expr)?;
                let ty = self.types.coerce(Type::Int, expr.expect_ty());
                self.apply_cast(expr, ty)?;
                ty
            },
            NodeKind::Not(expr) => {
                self.infer_type(expr)?;
                Type::Int // C logical operators give int 0/1 not bool
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
            NodeKind::LogicalAnd { lhs, rhs } | NodeKind::LogicalOr { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                Type::Int // C logical operators give int 0/1 not bool
            },
            NodeKind::Binary { op, lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                let ty = self.apply_usual_arith_conv(lhs, rhs)?;
                match op {
                    BinaryOp::Add
                    | BinaryOp::Sub
                    | BinaryOp::Mul
                    | BinaryOp::Div
                    | BinaryOp::Mod
                    | BinaryOp::BitAnd
                    | BinaryOp::BitOr
                    | BinaryOp::BitXor
                    | BinaryOp::BitLeftShift
                    | BinaryOp::BitRightShift => ty,
                    BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le => Type::Int,
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

                if matches!(then_ty, Type::Void) || matches!(else_ty, Type::Void) {
                    Type::Void
                } else {
                    let (lhs, rhs) = if self.types.base(then_ty).is_some()
                        || self.types.base(else_ty).is_none()
                    {
                        (then_expr, else_expr)
                    } else {
                        // "else" is pointer but "then" is not, we must
                        // normalize this lone pointer to lhs
                        (else_expr, then_expr)
                    };
                    self.apply_usual_arith_conv(lhs, rhs)?
                }
            },
            NodeKind::Member { member, .. } => member.ty,
            NodeKind::StmtExpr(body) => {
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
            NodeKind::Num(_) | NodeKind::Cast(_) => {
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
            StmtKind::Expr(node) | StmtKind::Return(node) => self.collect_labels(node, labels)?,
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
            NodeKind::Entity(_) | NodeKind::Num(_) => {},
            NodeKind::Addr(expr)
            | NodeKind::Deref(expr)
            | NodeKind::Neg(expr)
            | NodeKind::BitNot(expr)
            | NodeKind::Not(expr)
            | NodeKind::Cast(expr)
            | NodeKind::Member { parent: expr, .. } => self.collect_labels(expr, labels)?,
            NodeKind::Assign { lhs, rhs }
            | NodeKind::Comma { lhs, rhs }
            | NodeKind::LogicalAnd { lhs, rhs }
            | NodeKind::LogicalOr { lhs, rhs }
            | NodeKind::Binary { lhs, rhs, .. } => {
                self.collect_labels(lhs, labels)?;
                self.collect_labels(rhs, labels)?;
            },
            NodeKind::FuncCall { args, .. } => {
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
            StmtKind::Expr(node) | StmtKind::Return(node) => self.resolve_gotos(node, labels)?,
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
            NodeKind::Entity(_) | NodeKind::Num(_) => {},
            NodeKind::Addr(expr)
            | NodeKind::Deref(expr)
            | NodeKind::Neg(expr)
            | NodeKind::BitNot(expr)
            | NodeKind::Not(expr)
            | NodeKind::Cast(expr)
            | NodeKind::Member { parent: expr, .. } => self.resolve_gotos(expr, labels)?,
            NodeKind::Assign { lhs, rhs }
            | NodeKind::Comma { lhs, rhs }
            | NodeKind::LogicalAnd { lhs, rhs }
            | NodeKind::LogicalOr { lhs, rhs }
            | NodeKind::Binary { lhs, rhs, .. } => {
                self.resolve_gotos(lhs, labels)?;
                self.resolve_gotos(rhs, labels)?;
            },
            NodeKind::FuncCall { args, .. } => {
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

    fn eval(&mut self, node: &mut Node) -> Result<i64> {
        self.infer_type(node)?;
        let ty = node.expect_ty();

        Ok(match &mut node.kind {
            NodeKind::Num(val) => *val,
            NodeKind::Neg(expr) => self.eval(expr)?.wrapping_neg(),
            NodeKind::Not(expr) => {
                let cond = self.eval(expr)? == 0;
                if cond { 1 } else { 0 }
            },
            NodeKind::BitNot(expr) => !self.eval(expr)?,
            NodeKind::Comma { rhs, .. } => self.eval(rhs)?,
            NodeKind::LogicalAnd { lhs, rhs } => {
                let cond = self.eval(lhs)? != 0 && self.eval(rhs)? != 0;
                if cond { 1 } else { 0 }
            },
            NodeKind::LogicalOr { lhs, rhs } => {
                let cond = self.eval(lhs)? != 0 || self.eval(rhs)? != 0;
                if cond { 1 } else { 0 }
            },
            NodeKind::Binary { op, lhs, rhs } => match op {
                BinaryOp::Add => self.eval(lhs)?.wrapping_add(self.eval(rhs)?),
                BinaryOp::Sub => self.eval(lhs)?.wrapping_sub(self.eval(rhs)?),
                BinaryOp::Mul => self.eval(lhs)?.wrapping_mul(self.eval(rhs)?),
                BinaryOp::Div => {
                    let rhs = self.eval(rhs)?;
                    if rhs == 0 {
                        return Err(self.source.error_at(node.offset, "division by zero"));
                    }
                    self.eval(lhs)?.wrapping_div(rhs)
                },
                BinaryOp::Mod => {
                    let rhs = self.eval(rhs)?;
                    if rhs == 0 {
                        return Err(self.source.error_at(node.offset, "division by zero"));
                    }
                    self.eval(lhs)?.wrapping_rem(rhs)
                },
                BinaryOp::BitAnd => self.eval(lhs)? & self.eval(rhs)?,
                BinaryOp::BitOr => self.eval(lhs)? | self.eval(rhs)?,
                BinaryOp::BitXor => self.eval(lhs)? ^ self.eval(rhs)?,
                BinaryOp::BitLeftShift => self.eval(lhs)?.wrapping_shl(self.eval(rhs)? as _),
                BinaryOp::BitRightShift => self.eval(lhs)?.wrapping_shr(self.eval(rhs)? as _),
                BinaryOp::Eq => (self.eval(lhs)? == self.eval(rhs)?) as _,
                BinaryOp::Ne => (self.eval(lhs)? != self.eval(rhs)?) as _,
                BinaryOp::Lt => (self.eval(lhs)? < self.eval(rhs)?) as _,
                BinaryOp::Le => (self.eval(lhs)? <= self.eval(rhs)?) as _,
            },
            NodeKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => {
                if self.eval(cond)? != 0 {
                    self.eval(then_expr)?
                } else {
                    self.eval(else_expr)?
                }
            },
            NodeKind::Cast(expr) => {
                let val = self.eval(expr)?;
                match ty {
                    Type::Bool => (val != 0) as _,
                    Type::Char => val as i8 as _,
                    Type::Short => val as i16 as _,
                    Type::Int => val as i32 as _,
                    _ => val,
                }
            },
            NodeKind::Dummy => unreachable!(),
            _ => {
                return Err(self
                    .source
                    .error_at(node.offset, "not a compile-time constant"));
            },
        })
    }

    /// Generate a unique label.
    fn unique_label(&mut self) -> SmolStr {
        let label = format_smolstr!(".L..{}", self.next_unique_label);
        self.next_unique_label += 1;
        label
    }
}
