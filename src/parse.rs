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
use crate::types::{Member, Type};
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

/// A declaration specifier.
struct Declspec {
    ty: Type,
    is_typedef: bool,
}

/// An ordinary identifier.
#[derive(Debug, Clone)]
enum OrdinaryIdent {
    Entity(EntityRef),
    Typedef(Type),
}

impl OrdinaryIdent {
    fn as_entity(&self) -> Option<&EntityRef> {
        match self {
            OrdinaryIdent::Entity(entity) => Some(entity),
            _ => None,
        }
    }

    fn as_typedef(&self) -> Option<&Type> {
        match self {
            OrdinaryIdent::Typedef(ty) => Some(ty),
            _ => None,
        }
    }
}

/// A scope frame.
#[derive(Debug, Default)]
struct ScopeFrame {
    /// The namespace of ordinary identifiers.
    idents: FxHashMap<SmolStr, OrdinaryIdent>,
    /// The namespace of struct and union tags.
    tags: FxHashMap<SmolStr, Type>,
}

/// Stateful parser over the token stream during parsing.
pub struct Parser<'a> {
    source: &'a Source,
    tokens: Vec<Token<'a>>,
    pos: usize,

    // Mutable states
    locals: Vec<LocalVar>,
    functions: Vec<Function>,
    /// The index of the function currently being parsed.
    active_function: Option<usize>,
    globals: Vec<GlobalVar>,
    scopes: Vec<ScopeFrame>,
    next_anon_global: usize,
    speculate_depth: usize,
}

impl<'a> Parser<'a> {
    /// Create a parser over a token stream.
    pub fn new(source: &'a Source, tokens: Vec<Token<'a>>) -> Self {
        Self {
            source,
            tokens,
            pos: 0,
            locals: Vec::new(),
            functions: Vec::new(),
            active_function: None,
            globals: Vec::new(),
            scopes: vec![ScopeFrame::default()],
            next_anon_global: 0,
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

    /// Assume and skip a specific punctuator.
    fn skip_punct(&mut self, expected: &str) -> Result<()> {
        if !self.current().is_punct(expected) {
            return Err(self.error_current(format_smolstr!("expected '{expected}'")));
        }
        self.advance();
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
        self.find_ident(name)
            .and_then(OrdinaryIdent::as_typedef)
            .is_some()
    }

    /// Run a parser operation speculatively.
    ///
    /// All read operations on the parser states allowed. Mutation is only
    /// allowed for:
    ///
    /// - Mutating position;
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
        let saved_scope_depth = self.scopes.len();
        let saved_locals_len = self.locals.len();
        let saved_functions_len = self.functions.len();
        let saved_active_function = self.active_function;
        let saved_globals_len = self.globals.len();
        let saved_next_anon_global = self.next_anon_global;
        self.speculate_depth += 1;

        let result = f(self).map(|value| (value, self.pos));

        debug_assert!(self.speculate_depth > 0, "speculation state is broken",);
        debug_assert!(
            self.scopes.len() >= saved_scope_depth,
            "cannot pop more scope frames than appended during speculation",
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
            self.active_function == saved_active_function,
            "cannot change active function during speculation",
        );
        debug_assert!(
            self.globals.len() >= saved_globals_len,
            "cannot remove pre-existing globals during speculation",
        );

        self.pos = saved_pos;
        self.scopes.truncate(saved_scope_depth);
        self.locals.truncate(saved_locals_len);
        self.functions.truncate(saved_functions_len);
        self.active_function = saved_active_function;
        self.globals.truncate(saved_globals_len);
        self.next_anon_global = saved_next_anon_global;
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
    ///   | "void"
    ///   | "char"
    ///   | "short"
    ///   | "int"
    ///   | "long"
    ///   | <struct-or-union-decl>
    ///   | <typedef-name>
    /// ```
    ///
    /// As per C language specification, type specifiers are order-insensitive,
    /// but only certain combinations are legal.
    fn parse_declspec(&mut self) -> Result<Declspec> {
        enum TypeSpec {
            Void,
            Char,
            Short,
            Int,
            Long,
            Other(Type),
        }

        let mut spec = None;
        let mut long_count = 0;
        let mut is_typedef = false;
        while self.at_typename() {
            let offset = self.current().offset;
            let keyword = self.current().as_keyword();
            let ident = self.current().as_ident();
            let typedef_ty = ident
                .and_then(|ident| self.find_ident(ident))
                .and_then(OrdinaryIdent::as_typedef)
                .cloned();

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
                Some(Keyword::Typedef) => is_typedef = true,
                Some(Keyword::Void) => match spec {
                    None => spec = Some(TypeSpec::Void),
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
                    None => spec = Some(TypeSpec::Other(self.parse_struct_or_union_decl(true)?)),
                    _ => bail_multiple_types!(),
                },
                Some(Keyword::Union) => match spec {
                    None => spec = Some(TypeSpec::Other(self.parse_struct_or_union_decl(false)?)),
                    _ => bail_multiple_types!(),
                },
                _ => match typedef_ty {
                    Some(ty) if spec.is_none() => spec = Some(TypeSpec::Other(ty)),
                    Some(_) => unreachable!(), // Early breaked
                    None => unreachable!("all typename tokens should have been handled"),
                },
            }
        }

        let ty = match spec {
            Some(TypeSpec::Void) => Type::void(),
            Some(TypeSpec::Char) => Type::char(),
            Some(TypeSpec::Short) => Type::short(),
            Some(TypeSpec::Int) => Type::int(),
            Some(TypeSpec::Long) => Type::long(),
            Some(TypeSpec::Other(ty)) => ty,
            None if is_typedef => {
                return Err(self.error_current("missing type specifier in typedef"));
            },
            None => return Err(self.error_current("expected a typename")),
        };

        Ok(Declspec { ty, is_typedef })
    }

    /// ```bnf
    /// <declarator> ::= "*"* (<ident> | "(" <declarator> ")") <type-suffix>
    /// ```
    fn parse_declarator(&mut self, mut ty: Type) -> Result<Declarator> {
        while self.current().is_punct("*") {
            self.advance();
            ty = Type::ptr(ty);
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
        let Some(name) = self.current().as_ident() else {
            return Err(self.error_current("expected a variable name"));
        };

        self.advance();
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
            ty = Type::ptr(ty);
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
        let offset = self.current().offset;
        let declspec = self.parse_declspec()?;
        if declspec.is_typedef {
            return Err(self
                .source
                .error_at(offset, "typedef is not allowed as typename"));
        }
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

            let offset = self.current().offset;
            let declspec = self.parse_declspec()?;
            if declspec.is_typedef {
                return Err(self
                    .source
                    .error_at(offset, "typedef is not allowed in parameter declaration"));
            }

            let offset = self.current().offset;
            let declarator = self.parse_declarator(declspec.ty)?;
            if declarator.ty.is_void() {
                return Err(self
                    .source
                    .error_at(offset, "parameter has incomplete type"));
            }

            if !param_names.insert(declarator.name.clone()) {
                return Err(self.source.error_at(offset, "redefinition of parameter"));
            }

            params.push(Parameter {
                name: declarator.name,
                ty: declarator.ty,
            });

            if params.len() > MAX_FUNC_PARAMS {
                return Err(self
                    .source
                    .error_at(declarator.offset, "too many parameters"));
            }
        }

        self.skip_punct(")")?;
        let param_tys = params.iter().map(|param| param.ty.clone()).collect();
        Ok((Type::func(return_ty, param_tys), params))
    }

    /// ```bnf
    /// <array-dimensions> ::= ("[" <num> "]")*
    /// ```
    fn parse_array_dimensions(&mut self, mut ty: Type) -> Result<Type> {
        if self.current().is_punct("[") {
            self.advance();
            let Some(len) = self.current().as_num() else {
                return Err(self.error_current("expected a number"));
            };
            self.advance();
            self.skip_punct("]")?;
            ty = self.parse_array_dimensions(ty)?;
            return Ok(Type::array(ty, len as _));
        }
        Ok(ty)
    }

    /// ```bnf
    /// <struct-or-union-decl> ::= <ident> | <ident>? "{" <struct_member>* "}"
    /// <struct-or-union-member> ::=
    ///   <declspec> <declarator> ("," <declarator>)* ";"
    /// ```
    fn parse_struct_or_union_decl(&mut self, is_struct: bool) -> Result<Type> {
        let offset = self.current().offset;
        let tag = self.current().as_ident();

        let repr = || if is_struct { "struct" } else { "union" };

        if let Some(tag) = tag {
            self.advance();
            if !self.current().is_punct("{") {
                return self.find_tag(tag).cloned().ok_or_else(|| {
                    self.source
                        .error_at(offset, format_smolstr!("unknown {} type", repr()))
                });
            }
        }

        self.skip_punct("{")?;

        let mut members = Vec::new();
        while !self.current().is_punct("}") {
            let offset = self.current().offset;
            let declspec = self.parse_declspec()?;
            if declspec.is_typedef {
                return Err(self.source.error_at(
                    offset,
                    format_smolstr!("typedef is not allowed in {} member declaration", repr()),
                ));
            }

            let mut first = true;
            while !self.current().is_punct(";") {
                if !first {
                    self.skip_punct(",")?;
                }
                first = false;

                let declarator = self.parse_declarator(declspec.ty.clone())?;
                if declarator.ty.is_void() {
                    return Err(self
                        .source
                        .error_at(declarator.offset, "field declared void"));
                }
                members.push(Member {
                    name: declarator.name,
                    ty: declarator.ty,
                    offset: 0, // Assigned in the constructor
                });
            }

            self.advance();
        }

        self.advance();

        let ty = Type::struct_or_union(is_struct, members);
        if let Some(tag) = tag {
            self.push_scope_tag(tag.to_smolstr(), ty.clone());
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
            let declspec = self.parse_declspec()?;
            if declspec.is_typedef {
                self.parse_typedef_tail(&declspec.ty)?;
                continue;
            }

            if self.is_function()? {
                self.parse_function(declspec.ty)?;
                continue;
            }

            self.parse_global_variable(declspec.ty)?;
        }

        Ok(Program {
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

        let (ty, _) = self.speculate(|parser| {
            let declarator = parser.parse_declarator(Default::default())?;
            Ok(declarator.ty)
        })?;
        Ok(ty.is_func())
    }

    /// ```bnf
    /// <function> ::= <declarator> (";" | "{" <compound-stmt>)
    /// ```
    fn parse_function(&mut self, return_ty: Type) -> Result<()> {
        self.disallow_speculation();

        let declarator = self.parse_declarator(return_ty)?;
        if !declarator.ty.is_func() {
            return Err(self.error_current("expected a function"));
        }

        let func_id = self.create_function_decl(declarator.name.clone(), declarator.ty.clone());
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
        let body = Stmt::block(self.parse_compound_stmt()?, body_offset);
        self.leave_scope();

        let function = &mut self.functions[func_id];
        function.body = Some(body);
        function.param_locals = param_locals;
        function.locals = std::mem::take(&mut self.locals);

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

            let declarator = self.parse_declarator(base_ty.clone())?;
            if declarator.ty.is_func() {
                return Err(self
                    .source
                    .error_at(declarator.offset, "expected a global variable"));
            }

            self.create_global(declarator.name, declarator.ty, None);
        }

        self.skip_punct(";")?;
        Ok(())
    }

    /// ```bnf
    /// <stmt> ::=
    ///   "return" <expr> ";"
    ///   | "if" "(" <expr> ")" <stmt> ("else" <stmt>)?
    ///   | "for" "(" <expr-stmt> <expr>? ";" <expr>? ")" <stmt>
    ///   | "while" "(" <expr> ")" <stmt>
    ///   | "{" <compound-stmt>
    ///   | <expr-stmt>
    /// ```
    fn parse_stmt(&mut self) -> Result<Stmt> {
        if self.current().is_keyword(Keyword::Return) {
            let offset = self.current().offset;
            self.advance();
            let mut expr = self.parse_expr()?;
            self.skip_punct(";")?;

            let return_ty = self.functions[self.active_function.unwrap()]
                .ty
                .as_func()
                .unwrap()
                .return_ty
                .clone();
            self.apply_cast(&mut expr, return_ty)?;
            return Ok(Stmt::return_(expr, offset));
        }

        if self.current().is_keyword(Keyword::If) {
            let offset = self.current().offset;
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

        if self.current().is_keyword(Keyword::For) {
            let offset = self.current().offset;
            self.advance();
            self.skip_punct("(")?;
            let init = Box::new(self.parse_expr_stmt()?);
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
            return Ok(Stmt::for_(init, cond, inc, body, offset));
        }

        if self.current().is_keyword(Keyword::While) {
            let offset = self.current().offset;
            self.advance();
            self.skip_punct("(")?;
            let cond = self.parse_expr()?;
            self.skip_punct(")")?;
            let body = Box::new(self.parse_stmt()?);
            // "while" can be desugared into "for" without init and inc
            return Ok(Stmt::while_(cond, body, offset));
        }

        if self.current().is_punct("{") {
            let offset = self.current().offset;
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
            let mut stmt = if self.at_typename() {
                let declspec = self.parse_declspec()?;
                if declspec.is_typedef {
                    self.parse_typedef_tail(&declspec.ty)?;
                    continue;
                }
                self.parse_declaration(&declspec.ty)?
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
    /// <declarator-init> ::= <declarator> ("=" <assign>)?
    /// ```
    fn parse_declaration(&mut self, base_ty: &Type) -> Result<Stmt> {
        let offset = self.current().offset;
        let mut stmts = Vec::new();
        let mut first = true;

        while !self.current().is_punct(";") {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let offset = self.current().offset;
            let declarator = self.parse_declarator(base_ty.clone())?;
            if declarator.ty.is_void() {
                return Err(self.source.error_at(offset, "variable declared void"));
            }
            let local_id = self.create_local(declarator.name, declarator.ty);

            if !self.current().is_punct("=") {
                continue;
            }

            // If there is an initializer, treat it as an assignment to the
            // just-created variable
            self.advance();
            let lhs = Node::entity(EntityRef::Local(local_id), declarator.offset);
            let rhs = self.parse_assign()?;
            let expr = Node::assign(lhs, rhs, declarator.offset);
            stmts.push(Stmt::expr(expr, declarator.offset));
        }

        self.skip_punct(";")?;
        Ok(Stmt::block(stmts, offset))
    }

    /// ```bnf
    /// <assign> ::= <equality> ("=" <assign>)?
    /// ```
    fn parse_assign(&mut self) -> Result<Node> {
        let node = self.parse_equality()?;

        if self.current().is_punct("=") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::assign(node, self.parse_assign()?, offset));
        }

        Ok(node)
    }

    /// ```bnf
    /// <equality> ::= <relational> ("==" <relational> | "!=" <relational>)*
    /// ```
    fn parse_equality(&mut self) -> Result<Node> {
        let mut node = self.parse_relational()?;

        loop {
            if self.current().is_punct("==") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Eq, node, self.parse_relational()?, offset);
                continue;
            }

            if self.current().is_punct("!=") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Ne, node, self.parse_relational()?, offset);
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <relational> ::= <add> ("<" <add> | "<=" <add> | ">" <add> | ">=" <add>)*
    /// ```
    fn parse_relational(&mut self) -> Result<Node> {
        let mut node = self.parse_add()?;

        loop {
            if self.current().is_punct("<") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Lt, node, self.parse_add()?, offset);
                continue;
            }

            if self.current().is_punct("<=") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Le, node, self.parse_add()?, offset);
                continue;
            }

            if self.current().is_punct(">") {
                let offset = self.current().offset;
                self.advance();
                // Reuse < with flipped operands
                node = Node::binary(BinaryOp::Lt, self.parse_add()?, node, offset);
                continue;
            }

            if self.current().is_punct(">=") {
                let offset = self.current().offset;
                self.advance();
                // Reuse <= with flipped operands
                node = Node::binary(BinaryOp::Le, self.parse_add()?, node, offset);
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
            if self.current().is_punct("+") {
                let offset = self.current().offset;
                self.advance();
                let rhs = self.parse_mul()?;
                node = self.new_add(node, rhs, offset)?;
                continue;
            }

            if self.current().is_punct("-") {
                let offset = self.current().offset;
                self.advance();
                let rhs = self.parse_mul()?;
                node = self.new_sub(node, rhs, offset)?;
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <mul> ::= <cast> ("*" <cast> | "/" <cast>)*
    /// ```
    fn parse_mul(&mut self) -> Result<Node> {
        let mut node = self.parse_cast()?;

        loop {
            if self.current().is_punct("*") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Mul, node, self.parse_cast()?, offset);
                continue;
            }

            if self.current().is_punct("/") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Div, node, self.parse_cast()?, offset);
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
    /// <unary> ::= ("+" | "-" | "*" | "&") <cast> | <postfix>
    /// ```
    fn parse_unary(&mut self) -> Result<Node> {
        if self.current().is_punct("+") {
            self.advance();
            return self.parse_cast();
        }

        if self.current().is_punct("-") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::neg(self.parse_cast()?, offset));
        }

        if self.current().is_punct("&") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::addr(self.parse_cast()?, offset));
        }

        if self.current().is_punct("*") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::deref(self.parse_cast()?, offset));
        }

        self.parse_postfix()
    }

    /// ```bnf
    /// <postfix> ::= <primary> ("[" <expr> "]" | "." <ident> | "->" <ident>)*
    /// ```
    fn parse_postfix(&mut self) -> Result<Node> {
        let mut node = self.parse_primary()?;

        loop {
            if self.current().is_punct("[") {
                let offset = self.current().offset;
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
                let offset = self.current().offset;
                self.advance();
                // Canonicalize a->b to (*a).b
                node = Node::deref(node, offset);
                node = self.new_member_access(node)?;
                self.advance();
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
                    let ty = self.parse_typename()?;
                    self.skip_punct(")")?;
                    return Ok(Node::num(ty.size(), offset, false));
                }

                self.pos = pos;
            }

            let mut operand = self.parse_unary()?;
            self.infer_type(&mut operand)?;
            let size = operand.expect_ty().size();
            return Ok(Node::num(size, offset, false));
        }

        if let Some(name) = self.current().as_ident() {
            if self.peek(1).is_some_and(|tok| tok.is_punct("(")) {
                return self.parse_func_call(name);
            }

            self.advance();
            let Some(entity) = self.find_ident(name).and_then(OrdinaryIdent::as_entity) else {
                return Err(self.source.error_at(offset, "undefined variable"));
            };
            return Ok(Node::entity(*entity, offset));
        }

        if let Some(content) = self.current().as_str() {
            let ty = Type::array(Type::char(), content.len());
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
            .and_then(OrdinaryIdent::as_entity)
            .ok_or_else(|| {
                self.source
                    .error_at(offset, "implicit declaration of a function")
            })?;

        let EntityRef::Function(func_id) = entity else {
            return Err(self.source.error_at(offset, "not a function"));
        };

        let return_ty = self.functions[*func_id]
            .ty
            .as_func()
            .unwrap()
            .return_ty
            .clone();

        let mut args = Vec::new();
        while !self.current().is_punct(")") {
            if !args.is_empty() {
                self.skip_punct(",")?;
            }
            let mut arg = self.parse_assign()?;
            self.infer_type(&mut arg)?;
            args.push(arg);
        }

        self.skip_punct(")")?;
        Ok(Node::func_call(name, args, return_ty, offset))
    }

    /// ```bnf
    /// <typedef-tail> ::= <declarator> ("," <declarator>)* ";"
    /// ```
    fn parse_typedef_tail(&mut self, base_ty: &Type) -> Result<()> {
        let mut first = true;

        while !self.current().is_punct(";") {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let declarator = self.parse_declarator(base_ty.clone())?;
            let typedef = OrdinaryIdent::Typedef(declarator.ty);
            self.push_scope_ident(declarator.name, typedef);
        }

        self.skip_punct(";")?;
        Ok(())
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
    fn find_ident(&self, name: &str) -> Option<&OrdinaryIdent> {
        for frame in self.scopes.iter().rev() {
            if let Some(entry) = frame.idents.get(name) {
                return Some(entry);
            }
        }
        None
    }

    /// Find a struct or union tag by name.
    fn find_tag(&self, tag: &str) -> Option<&Type> {
        for frame in self.scopes.iter().rev() {
            if let Some(ty) = frame.tags.get(tag) {
                return Some(ty);
            }
        }
        None
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

        let name = format_smolstr!(".L..{}", self.next_anon_global);
        self.next_anon_global += 1;
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
        });

        let id = self.functions.len() - 1;
        let entity = OrdinaryIdent::Entity(EntityRef::Function(id));
        self.push_scope_ident(name, entity);
        id
    }

    /// Build an addition node with pointer scaling.
    fn new_add(&self, mut lhs: Node, mut rhs: Node, offset: usize) -> Result<Node> {
        self.infer_type(&mut lhs)?;
        self.infer_type(&mut rhs)?;

        let lhs_ty = lhs.ty.clone().unwrap();
        let rhs_ty = rhs.ty.clone().unwrap();

        // num + num
        if lhs_ty.is_int() && rhs_ty.is_int() {
            return Ok(Node::binary(BinaryOp::Add, lhs, rhs, offset));
        }

        if lhs_ty.base().is_some() && rhs_ty.base().is_some() {
            return Err(self.source.error_at(offset, "invalid operands"));
        }

        // Canonicalize num + ptr to ptr + num
        if lhs_ty.base().is_none() && rhs_ty.base().is_some() {
            std::mem::swap(&mut lhs, &mut rhs);
        }

        // ptr + num
        let base_size = lhs.expect_ty().base().unwrap().size();
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
    fn new_sub(&self, mut lhs: Node, mut rhs: Node, offset: usize) -> Result<Node> {
        self.infer_type(&mut lhs)?;
        self.infer_type(&mut rhs)?;

        let lhs_ty = lhs.ty.clone().unwrap();
        let rhs_ty = rhs.ty.clone().unwrap();

        // num - num
        if lhs_ty.is_int() && rhs_ty.is_int() {
            return Ok(Node::binary(BinaryOp::Sub, lhs, rhs, offset));
        }

        // ptr - num
        if lhs_ty.base().is_some() && rhs_ty.is_int() {
            let base_size = lhs_ty.base().unwrap().size();
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
        if lhs_ty.base().is_some() && rhs_ty.base().is_some() {
            let base_size = lhs_ty.base().unwrap().size();
            let mut diff = Node::binary(BinaryOp::Sub, lhs, rhs, offset);
            diff.ty = Some(Type::int());
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

    /// Build a member access node for the given node.
    fn new_member_access(&self, mut node: Node) -> Result<Node> {
        self.infer_type(&mut node)?;

        let sou = match node.expect_ty().as_struct_or_union() {
            Some(members) => members,
            None => return Err(self.error_current("not a struct or union")),
        };

        let ident = match self.current().as_ident() {
            Some(ident) => ident,
            None => return Err(self.error_current("not an ident")),
        };

        let member = match sou.members.iter().find(|member| member.name == ident) {
            Some(member) => member.clone(),
            None => return Err(self.error_current("no such member")),
        };

        Ok(Node::member(node, member, self.current().offset))
    }

    /// Apply a cast on the given node to the given type.
    fn apply_cast(&self, node: &mut Node, ty: Type) -> Result<()> {
        let offset = node.offset;
        let mut old = std::mem::take(node);
        self.infer_type(&mut old)?;
        *node = Node::cast(old, ty, offset);
        Ok(())
    }

    /// Apply a usual arithmetic conversion on the given operands.
    ///
    /// Returns the coerced common type.
    fn apply_usual_arith_conv(&self, lhs: &mut Node, rhs: &mut Node) -> Result<Type> {
        let ty = lhs.expect_ty().coerce(rhs.expect_ty());
        self.apply_cast(lhs, ty.clone())?;
        self.apply_cast(rhs, ty.clone())?;
        Ok(ty)
    }

    /// Infer types for a statement subtree.
    fn infer_type_stmt(&self, stmt: &mut Stmt) -> Result<()> {
        match &mut stmt.kind {
            StmtKind::Expr(expr) | StmtKind::Return(expr) => self.infer_type(expr),
            StmtKind::Loop {
                init,
                cond,
                inc,
                body,
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
                self.infer_type_stmt(body)
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
                Ok(())
            },
            StmtKind::Block(stmts) => {
                for stmt in stmts {
                    self.infer_type_stmt(stmt)?;
                }
                Ok(())
            },
        }
    }

    /// Infer the type for an expression subtree.
    fn infer_type(&self, node: &mut Node) -> Result<()> {
        if node.ty.is_some() {
            return Ok(());
        }

        node.ty = Some(match &mut node.kind {
            NodeKind::FuncCall { args, .. } => {
                for arg in args {
                    self.infer_type(arg)?;
                }
                Type::long()
            },
            NodeKind::Neg(expr) => {
                self.infer_type(expr)?;
                let ty = Type::int().coerce(expr.expect_ty());
                self.apply_cast(expr, ty.clone())?;
                ty
            },
            NodeKind::Entity(entity) => match *entity {
                EntityRef::Local(local_id) => self.locals[local_id].ty.clone(),
                EntityRef::Global(global_id) => self.globals[global_id].ty.clone(),
                EntityRef::Function(function_id) => self.functions[function_id].ty.clone(),
            },
            NodeKind::Addr(expr) => {
                self.infer_type(expr)?;
                let pointee = expr.expect_ty();
                let base = if pointee.is_array() {
                    // In C, array decays into a pointer to its first element
                    // when taking its address, so we need to take its base type
                    pointee.base().cloned().unwrap()
                } else {
                    pointee.clone()
                };
                Type::ptr(base)
            },
            NodeKind::Deref(expr) => {
                self.infer_type(expr)?;
                let Some(base) = expr.expect_ty().base() else {
                    return Err(self
                        .source
                        .error_at(node.offset, "invalid pointer dereference"));
                };
                if base.is_void() {
                    return Err(self
                        .source
                        .error_at(node.offset, "dereferencing a void pointer"));
                }
                base.clone()
            },
            NodeKind::Assign { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                if lhs.expect_ty().is_array() {
                    return Err(self.source.error_at(lhs.offset, "not an lvalue"));
                }
                if lhs.expect_ty().as_struct_or_union().is_none() {
                    self.apply_cast(rhs, lhs.expect_ty().clone())?;
                }
                lhs.expect_ty().clone()
            },
            NodeKind::Comma { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                rhs.expect_ty().clone()
            },
            NodeKind::Binary { op, lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                let ty = self.apply_usual_arith_conv(lhs, rhs)?;
                match op {
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => ty,
                    BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le => Type::int(),
                }
            },
            NodeKind::Member { member, .. } => member.ty.clone(),
            NodeKind::StmtExpr(body) => {
                if let Some(stmt) = body.last_mut()
                    && let StmtKind::Expr(expr) = &mut stmt.kind
                {
                    self.infer_type(expr)?;
                    expr.expect_ty().clone()
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
}
