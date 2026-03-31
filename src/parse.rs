//! A [recursive-descent parser][1] for the C programming language.
//!
//! [1]: https://en.wikipedia.org/wiki/Recursive_descent_parser

use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::{SmolStr, ToSmolStr, format_smolstr};

use crate::ast::{
    BinaryOp, Function, GlobalVar, LocalVar, Node, NodeKind, Program, Stmt, StmtKind, VarRef,
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

/// A stack of variable scopes.
#[derive(Debug, Default)]
struct ScopeFrame {
    vars: FxHashMap<SmolStr, VarRef>,
    /// Struct or union tags.
    tags: FxHashMap<SmolStr, Type>,
}

/// Stateful parser over the token stream during parsing.
pub struct Parser<'a> {
    source: &'a Source,
    tokens: Vec<Token<'a>>,
    pos: usize,
    locals: Vec<LocalVar>,
    globals: Vec<GlobalVar>,
    scopes: Vec<ScopeFrame>,
    next_anon_global: usize,
}

impl<'a> Parser<'a> {
    /// Create a parser over a token stream.
    pub fn new(source: &'a Source, tokens: Vec<Token<'a>>) -> Self {
        Self {
            source,
            tokens,
            pos: 0,
            locals: Vec::new(),
            globals: Vec::new(),
            scopes: vec![ScopeFrame::default()],
            next_anon_global: 0,
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

    /// Return an error diagnostic at the current token,
    fn error_current(&self, message: &str) -> Error {
        self.source.error_at(self.current().offset, message)
    }

    /// Assume and skip a specific punctuator.
    fn skip_punct(&mut self, expected: &str) -> Result<()> {
        if !self.current().is_punct(expected) {
            return Err(self.error_current(&format!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// ```bnf
    /// <declspec> ::=
    ///   "char"
    ///   | "short"
    ///   | "int"
    ///   | "long"
    ///   | <struct-or-union-decl>
    /// ```
    fn parse_declspec(&mut self) -> Result<Type> {
        if self.current().is_keyword(Keyword::Char) {
            self.advance();
            return Ok(Type::char());
        }

        if self.current().is_keyword(Keyword::Short) {
            self.advance();
            return Ok(Type::short());
        }

        if self.current().is_keyword(Keyword::Int) {
            self.advance();
            return Ok(Type::int());
        }

        if self.current().is_keyword(Keyword::Long) {
            self.advance();
            return Ok(Type::long());
        }

        if self.current().is_keyword(Keyword::Struct) {
            self.advance();
            return self.parse_struct_or_union_decl(true);
        }

        if self.current().is_keyword(Keyword::Union) {
            self.advance();
            return self.parse_struct_or_union_decl(false);
        }

        debug_assert!(
            !self.current().is_typename_keyword(),
            "all typenames should have been handled, but {:?} is not",
            self.current()
        );
        Err(self.error_current("expected a typename"))
    }

    /// ```bnf
    /// <declarator> ::= "*"* <ident> <type-suffix>
    /// ```
    fn parse_declarator(&mut self, mut ty: Type) -> Result<Declarator> {
        while self.current().is_punct("*") {
            self.advance();
            ty = Type::ptr(ty);
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

            let base_ty = self.parse_declspec()?;
            let offset = self.current().offset;
            let declarator = self.parse_declarator(base_ty)?;

            if !param_names.insert(declarator.name.clone()) {
                return Err(self.source.error_at(
                    offset,
                    &format!("redefinition of parameter '{}'", declarator.name),
                ));
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

        if let Some(tag) = tag {
            self.advance();
            if !self.current().is_punct("{") {
                return self.find_tag(tag).ok_or_else(|| {
                    self.source.error_at(
                        offset,
                        &format!(
                            "unknown {} type",
                            if is_struct { "struct" } else { "union" }
                        ),
                    )
                });
            }
        }

        self.skip_punct("{")?;

        let mut members = Vec::new();
        while !self.current().is_punct("}") {
            let base_ty = self.parse_declspec()?;

            let mut first = true;
            while !self.current().is_punct(";") {
                if !first {
                    self.skip_punct(",")?;
                }
                first = false;

                let declarator = self.parse_declarator(base_ty.clone())?;
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
            self.push_scope_tag(tag.to_smolstr(), ty.clone(), offset)?;
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
    /// <program> ::= (<function> | <global-variable>)* <eof>
    /// ```
    pub fn parse_program(&mut self) -> Result<Program> {
        let mut functions = Vec::new();

        while !self.current().is_eof() {
            let base_ty = self.parse_declspec()?;

            if self.is_function()? {
                functions.push(self.parse_function(base_ty)?);
                continue;
            }

            self.parse_global_variable(base_ty)?;
        }

        Ok(Program {
            functions,
            globals: std::mem::take(&mut self.globals),
        })
    }

    /// Lookahead to determine whether we are at a [`<function>`].
    ///
    /// [`<function>`]: Self::parse_function
    fn is_function(&self) -> Result<bool> {
        if self.current().is_punct(";") {
            return Ok(false);
        }

        let mut offset = 0;
        while self.peek(offset).is_some_and(|token| token.is_punct("*")) {
            offset += 1;
        }

        let Some(token) = self.peek(offset) else {
            return Err(self.error_current("expected a variable name"));
        };

        if token.as_ident().is_none() {
            return Err(self
                .source
                .error_at(token.offset, "expected a variable name"));
        }

        Ok(self
            .peek(offset + 1)
            .is_some_and(|token| token.is_punct("(")))
    }

    /// ```bnf
    /// <function> ::= <declspec> <declarator> "{" <compound-stmt>
    /// ```
    fn parse_function(&mut self, return_ty: Type) -> Result<Function> {
        let declarator = self.parse_declarator(return_ty)?;
        if !declarator.ty.is_func() {
            return Err(self.error_current("expected a function"));
        }

        let body_offset = self.current().offset;
        self.locals.clear();
        self.enter_scope();
        let params = self.create_param_locals(declarator.params)?;
        self.skip_punct("{")?;
        let body = Stmt::block(self.parse_compound_stmt()?, body_offset);
        self.leave_scope();

        Ok(Function {
            name: declarator.name,
            params,
            body,
            locals: std::mem::take(&mut self.locals),
        })
    }

    /// ```bnf
    /// <global-variable> ::= <declarator> ("," <declarator>)* ";"
    /// ```
    fn parse_global_variable(&mut self, base_ty: Type) -> Result<()> {
        let mut first = true;

        while !self.current().is_punct(";") {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let offset = self.current().offset;
            let declarator = self.parse_declarator(base_ty.clone())?;
            if declarator.ty.is_func() {
                return Err(self
                    .source
                    .error_at(declarator.offset, "expected a global variable"));
            }

            self.create_global(declarator.name, declarator.ty, None, offset)?;
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
            let expr = self.parse_expr()?;
            self.skip_punct(";")?;
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
            let mut stmt = if self.current().is_typename_keyword() {
                self.parse_declaration()?
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
    fn parse_declaration(&mut self) -> Result<Stmt> {
        let offset = self.current().offset;
        let base_ty = self.parse_declspec()?;
        let mut stmts = Vec::new();
        let mut first = true;

        while !self.current().is_punct(";") {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let offset = self.current().offset;
            let declarator = self.parse_declarator(base_ty.clone())?;
            let local_id = self.create_local(declarator.name, declarator.ty, offset)?;

            if !self.current().is_punct("=") {
                continue;
            }

            // If there is an initializer, treat it as an assignment to the
            // just-created variable
            self.advance();
            let lhs = Node::var(VarRef::Local(local_id), declarator.offset);
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
    /// <mul> ::= <unary> ("*" <unary> | "/" <unary>)*
    /// ```
    fn parse_mul(&mut self) -> Result<Node> {
        let mut node = self.parse_unary()?;

        loop {
            if self.current().is_punct("*") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Mul, node, self.parse_unary()?, offset);
                continue;
            }

            if self.current().is_punct("/") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Div, node, self.parse_unary()?, offset);
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <unary> ::= ("+" | "-" | "*" | "&") <unary> | <postfix>
    /// ```
    fn parse_unary(&mut self) -> Result<Node> {
        if self.current().is_punct("+") {
            self.advance();
            return self.parse_unary();
        }

        if self.current().is_punct("-") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::neg(self.parse_unary()?, offset));
        }

        if self.current().is_punct("&") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::addr(self.parse_unary()?, offset));
        }

        if self.current().is_punct("*") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::deref(self.parse_unary()?, offset));
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
            let mut operand = self.parse_unary()?;
            self.infer_type(&mut operand)?;
            let size = operand.expect_ty().size();
            return Ok(Node::num(size, offset));
        }

        if let Some(name) = self.current().as_ident() {
            if self.peek(1).is_some_and(|tok| tok.is_punct("(")) {
                return self.parse_func_call(name, offset);
            }

            self.advance();
            let Some(var) = self.find_var(name) else {
                return Err(self.source.error_at(offset, "undefined variable"));
            };
            return Ok(Node::var(var, offset));
        }

        if let Some(content) = self.current().as_str() {
            let ty = Type::array(Type::char(), content.len());
            let global_id = self.create_anon_global(ty, content)?;
            self.advance();
            return Ok(Node::var(VarRef::Global(global_id), offset));
        }

        if let Some(value) = self.current().as_num() {
            self.advance();
            return Ok(Node::num(value, offset));
        }

        Err(self.error_current("expected an expression"))
    }

    /// ```bnf
    /// <func-call> ::= <ident> "(" (<assign> ("," <assign>)*)? ")"
    /// ```
    fn parse_func_call(&mut self, name: &str, offset: usize) -> Result<Node> {
        self.advance();
        self.skip_punct("(")?;

        let mut args = Vec::new();
        while !self.current().is_punct(")") {
            if !args.is_empty() {
                self.skip_punct(",")?;
            }
            args.push(self.parse_assign()?);
        }

        self.skip_punct(")")?;
        Ok(Node::func_call(name, args, offset))
    }

    /// Enter a new variable scope.
    fn enter_scope(&mut self) {
        self.scopes.push(ScopeFrame::default());
    }

    /// Leave the current variable scope.
    fn leave_scope(&mut self) {
        self.scopes.pop();
    }

    /// Push a variable into the current scope.
    fn push_scope_var(&mut self, name: SmolStr, var: VarRef, offset: usize) -> Result<()> {
        if self
            .scopes
            .last_mut()
            .expect("no scope to push variable into")
            .vars
            .insert(name.clone(), var)
            .is_some()
        {
            return Err(self
                .source
                .error_at(offset, &format!("redefinition of variable '{name}'")));
        }
        Ok(())
    }

    /// Push a struct or union tag into the current scope.
    fn push_scope_tag(&mut self, name: SmolStr, ty: Type, offset: usize) -> Result<()> {
        if self
            .scopes
            .last_mut()
            .expect("no scope to push struct or union tag into")
            .tags
            .insert(name.clone(), ty)
            .is_some()
        {
            return Err(self
                .source
                .error_at(offset, &format!("redefinition of tag '{name}'")));
        }
        Ok(())
    }

    /// Find a variable by name.
    fn find_var(&self, name: &str) -> Option<VarRef> {
        for frame in self.scopes.iter().rev() {
            if let Some(var) = frame.vars.get(name) {
                return Some(*var);
            }
        }
        None
    }

    /// Find a struct or union tag by name.
    fn find_tag(&self, tag: &str) -> Option<Type> {
        for frame in self.scopes.iter().rev() {
            if let Some(ty) = frame.tags.get(tag) {
                return Some(ty.clone());
            }
        }
        None
    }

    /// Create a new local variable.
    fn create_local(&mut self, name: impl Into<SmolStr>, ty: Type, offset: usize) -> Result<usize> {
        let name = name.into();
        self.locals.push(LocalVar {
            _name: name.clone(),
            ty,
            offset: 0, // Assigned during codegen
        });

        let id = self.locals.len() - 1;
        let var = VarRef::Local(id);
        self.push_scope_var(name, var, offset)?;
        Ok(id)
    }

    /// Create local variables for function parameters.
    ///
    /// Parameters are pushed in reverse order to ensure the first parameter
    /// gets the lowest local ID.
    fn create_param_locals(&mut self, params: Vec<Parameter>) -> Result<Vec<usize>> {
        let mut param_ids = Vec::with_capacity(params.len());

        for param in params.into_iter().rev() {
            param_ids.push(
                self.create_local(param.name, param.ty, usize::MAX)
                    .expect("parameter names are not unique"),
            );
        }

        param_ids.reverse();
        Ok(param_ids)
    }

    /// Create a new global variable.
    fn create_global(
        &mut self,
        name: impl Into<SmolStr>,
        ty: Type,
        init_data: Option<Rc<[u8]>>,
        offset: usize,
    ) -> Result<usize> {
        let name = name.into();
        self.globals.push(GlobalVar {
            name: name.clone(),
            ty,
            init_data,
        });

        let id = self.globals.len() - 1;
        let var = VarRef::Global(id);
        self.push_scope_var(name, var, offset)?;
        Ok(id)
    }

    /// Create a new anonymous global variable.
    fn create_anon_global(&mut self, ty: Type, init_data: Rc<[u8]>) -> Result<usize> {
        let name = format_smolstr!(".L..{}", self.next_anon_global);
        self.next_anon_global += 1;
        self.create_global(name, ty, Some(init_data), usize::MAX)
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
        let ptr_ty = lhs.ty.clone();
        let base_size = lhs.expect_ty().base().unwrap().size();
        let scaled_rhs = Node::binary(BinaryOp::Mul, rhs, Node::num(base_size, offset), offset);
        let mut node = Node::binary(BinaryOp::Add, lhs, scaled_rhs, offset);
        node.ty = ptr_ty;
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
            let scaled_rhs = Node::binary(BinaryOp::Mul, rhs, Node::num(base_size, offset), offset);
            let mut node = Node::binary(BinaryOp::Sub, lhs, scaled_rhs, offset);
            node.ty = Some(lhs_ty);
            return Ok(node);
        }

        // ptr - ptr
        if lhs_ty.base().is_some() && rhs_ty.base().is_some() {
            let base_size = lhs_ty.base().unwrap().size();
            let diff = Node::binary(BinaryOp::Sub, lhs, rhs, offset);
            let mut node = Node::binary(BinaryOp::Div, diff, Node::num(base_size, offset), offset);
            node.ty = Some(Type::int());
            return Ok(node);
        }

        Err(self.source.error_at(offset, "invalid operands"))
    }

    /// Build a member access node for the given node.
    fn new_member_access(&self, mut node: Node) -> Result<Node> {
        self.infer_type(&mut node)?;

        let members = match node.expect_ty().members() {
            Some(members) => members,
            None => return Err(self.error_current("not a struct or union")),
        };

        let ident = match self.current().as_ident() {
            Some(ident) => ident,
            None => return Err(self.error_current("not an ident")),
        };

        let member = match members.iter().find(|member| member.name == ident) {
            Some(member) => member.clone(),
            None => return Err(self.error_current("no such member")),
        };

        Ok(Node::member(node, member, self.current().offset))
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
            NodeKind::Num(_) => Type::long(),
            NodeKind::FuncCall { args, .. } => {
                for arg in args {
                    self.infer_type(arg)?;
                }
                Type::long()
            },
            NodeKind::Neg(expr) => {
                self.infer_type(expr)?;
                expr.expect_ty().clone()
            },
            NodeKind::Var(var) => match *var {
                VarRef::Local(local_id) => self.locals[local_id].ty.clone(),
                VarRef::Global(global_id) => self.globals[global_id].ty.clone(),
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
                base.clone()
            },
            NodeKind::Assign { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                if lhs.expect_ty().is_array() {
                    return Err(self.source.error_at(lhs.offset, "not an lvalue"));
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
                match op {
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => {
                        lhs.expect_ty().clone()
                    },
                    BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le => Type::long(),
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
        });

        Ok(())
    }
}
