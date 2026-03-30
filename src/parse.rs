//! A [recursive-descent parser][1] for the C programming language.
//!
//! [1]: https://en.wikipedia.org/wiki/Recursive_descent_parser

use std::rc::Rc;

use rustc_hash::FxHashMap;
use smol_str::{SmolStr, format_smolstr};

use crate::ast::{
    BinaryOp, Function, GlobalVar, LocalVar, Node, NodeKind, Program, Stmt, StmtKind, VarRef,
};
use crate::error::{Error, Result};
use crate::source::Source;
use crate::tokenize::{Keyword, Token};
use crate::types::Type;

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

/// A trait for parsing tokens into an AST.
///
/// This is implemented by both [`Cursor`] and [`LookaheadCursor`] to reduce
/// code duplication when both normal parsing and lookahead are needed.
trait Parser<'a> {
    /// Return a reference to the original source.
    fn source(&self) -> &'a Source;

    /// Return a token at a fixed lookahead distance.
    fn peek(&self, offset: usize) -> Option<&Token<'a>>;

    /// Advance to the next token.
    fn advance(&mut self);

    /// Return the current token.
    fn current(&self) -> &Token<'a> {
        self.peek(0).expect("parser is in a broken state")
    }

    /// Format an error message at the current token.
    fn error_current(&self, message: &str) -> Error {
        self.source().error_at(self.current().offset, message)
    }

    /// Assume and skip a specific punctuator.
    fn skip_punct(&mut self, expected: &str) -> Result<()> {
        if !self.current().is_punct(expected) {
            return Err(self.error_current(&format!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// Assume and skip a specific keyword.
    fn skip_keyword(&mut self, expected: Keyword) -> Result<()> {
        if !self.current().is_keyword(expected) {
            return Err(self.error_current(&format!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// ```bnf
    /// <declspec> ::= "char" | "int"
    /// ```
    fn parse_declspec(&mut self) -> Result<Type> {
        if self.current().is_keyword(Keyword::Char) {
            self.skip_keyword(Keyword::Char)?;
            return Ok(Type::Char);
        }

        self.skip_keyword(Keyword::Int)?;
        Ok(Type::Int)
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

        while !self.current().is_punct(")") {
            if !params.is_empty() {
                self.skip_punct(",")?;
            }

            let base_ty = self.parse_declspec()?;
            let declarator = self.parse_declarator(base_ty)?;
            params.push(Parameter {
                name: declarator.name,
                ty: declarator.ty,
            });

            if params.len() > 6 {
                return Err(self
                    .source()
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
}

/// A stack of variable scopes.
#[derive(Debug, Default)]
struct ScopeFrame {
    vars: FxHashMap<SmolStr, VarRef>,
}

/// Cursor over the token stream during parsing.
pub struct Cursor<'a> {
    source: &'a Source,
    tokens: Vec<Token<'a>>,
    pos: usize,
    locals: Vec<LocalVar>,
    globals: Vec<GlobalVar>,
    scopes: Vec<ScopeFrame>,
    next_anon_global: usize,
}

impl<'a> Parser<'a> for Cursor<'a> {
    fn source(&self) -> &'a Source {
        self.source
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn peek(&self, offset: usize) -> Option<&Token<'a>> {
        self.tokens.get(self.pos + offset)
    }
}

impl<'a> Cursor<'a> {
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

    /// Create a [`LookaheadCursor`] at the current position.
    fn lookahead(&self) -> LookaheadCursor<'_, 'a> {
        LookaheadCursor {
            source: self.source,
            tokens: &self.tokens,
            pos: self.pos,
        }
    }

    /// ```bnf
    /// <expr> ::= <assign> ("," <expr>)?
    /// ```
    pub fn parse_expr(&mut self) -> Result<Node> {
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

        let mut lookahead = self.lookahead();
        let declarator = lookahead.parse_declarator(Type::default())?;
        Ok(declarator.ty.is_func())
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
        let params = self.create_param_locals(declarator.params);
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

            let declarator = self.parse_declarator(base_ty.clone())?;
            let local_id = self.create_local(declarator.name, declarator.ty);

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
    /// <postfix> ::= <primary> ("[" <expr> "]")*
    fn parse_postfix(&mut self) -> Result<Node> {
        let mut node = self.parse_primary()?;

        while self.current().is_punct("[") {
            let offset = self.current().offset;
            self.advance();
            let index = self.parse_expr()?;
            self.skip_punct("]")?;
            // Canonicalize a[b] to *(a + b)
            node = Node::deref(self.new_add(node, index, offset)?, offset);
        }

        Ok(node)
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
            let ty = Type::array(Type::Char, content.len());
            let global_id = self.create_anon_global(ty, content);
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
    fn push_scope(&mut self, name: SmolStr, var: VarRef) {
        self.scopes
            .last_mut()
            .expect("no scope to push variable into")
            .vars
            .insert(name, var);
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

    /// Create a new local variable.
    fn create_local(&mut self, name: impl Into<SmolStr>, ty: Type) -> usize {
        let name = name.into();
        self.locals.push(LocalVar {
            name: name.clone(),
            ty,
            offset: 0, // Assigned during codegen
        });

        let id = self.locals.len() - 1;
        let var = VarRef::Local(id);
        self.push_scope(name, var);
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
        let name = name.into();
        self.globals.push(GlobalVar {
            name: name.clone(),
            ty,
            init_data,
        });

        let id = self.globals.len() - 1;
        let var = VarRef::Global(id);
        self.push_scope(name, var);
        id
    }

    /// Create a new anonymous global variable.
    fn create_anon_global(&mut self, ty: Type, init_data: Rc<[u8]>) -> usize {
        let name = format_smolstr!(".L..{}", self.next_anon_global);
        self.next_anon_global += 1;
        self.create_global(name, ty, Some(init_data))
    }

    /// Build an addition node with pointer scaling.
    fn new_add(&self, mut lhs: Node, mut rhs: Node, offset: usize) -> Result<Node> {
        self.infer_type(&mut lhs)?;
        self.infer_type(&mut rhs)?;

        let lhs_ty = lhs.ty.clone().unwrap();
        let rhs_ty = rhs.ty.clone().unwrap();

        // num + num
        if lhs_ty.is_int() && rhs_ty.is_int() {
            let mut node = Node::binary(BinaryOp::Add, lhs, rhs, offset);
            node.ty = Some(Type::Int);
            return Ok(node);
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
            let mut node = Node::binary(BinaryOp::Sub, lhs, rhs, offset);
            node.ty = Some(Type::Int);
            return Ok(node);
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
            node.ty = Some(Type::Int);
            return Ok(node);
        }

        Err(self.source.error_at(offset, "invalid operands"))
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

        match &mut node.kind {
            NodeKind::Num(_) => {
                node.ty = Some(Type::Int);
            },
            NodeKind::FuncCall { args, .. } => {
                for arg in args {
                    self.infer_type(arg)?;
                }
                node.ty = Some(Type::Int);
            },
            NodeKind::Neg(expr) => {
                self.infer_type(expr)?;
                node.ty = Some(Type::Int);
            },
            NodeKind::Var(var) => {
                let ty = match *var {
                    VarRef::Local(local_id) => self.locals[local_id].ty.clone(),
                    VarRef::Global(global_id) => self.globals[global_id].ty.clone(),
                };
                node.ty = Some(ty);
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
                node.ty = Some(Type::ptr(base));
            },
            NodeKind::Deref(expr) => {
                self.infer_type(expr)?;
                let Some(base) = expr.expect_ty().base() else {
                    return Err(self
                        .source
                        .error_at(node.offset, "invalid pointer dereference"));
                };
                node.ty = Some(base.clone());
            },
            NodeKind::Assign { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                if lhs.expect_ty().is_array() {
                    return Err(self.source.error_at(lhs.offset, "not an lvalue"));
                }
                node.ty = lhs.ty.clone();
            },
            NodeKind::Comma { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                node.ty = rhs.ty.clone();
            },
            NodeKind::Binary { lhs, rhs, .. } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                node.ty = Some(Type::Int);
            },
            NodeKind::StmtExpr(body) => {
                if let Some(stmt) = body.last_mut()
                    && let StmtKind::Expr(expr) = &mut stmt.kind
                {
                    self.infer_type(expr)?;
                    node.ty = expr.ty.clone();
                } else {
                    return Err(self.source.error_at(
                        node.offset,
                        "statement expression returning void is not supported",
                    ));
                }
            },
        }

        Ok(())
    }
}

/// Cursor for looking ahead in the token stream without advancing.
struct LookaheadCursor<'cur, 'a> {
    source: &'a Source,
    tokens: &'cur [Token<'a>],
    pos: usize,
}

impl<'cur, 'a> Parser<'a> for LookaheadCursor<'cur, 'a> {
    fn source(&self) -> &'a Source {
        self.source
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn peek(&self, offset: usize) -> Option<&Token<'a>> {
        self.tokens.get(self.pos + offset)
    }
}
