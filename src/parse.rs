//! A recursive-descent parser.

use crate::ast::{BinaryOp, LocalVar, Node, NodeKind, Program, Stmt, StmtKind};
use crate::tokenize::{Keyword, Token, format_error_at};
use crate::types::Type;

/// Cursor over the token stream during parsing.
pub struct TokenCursor<'a> {
    input: &'a str,
    tokens: Vec<Token<'a>>,
    pos: usize,
    locals: Vec<LocalVar>,
}

impl<'a> TokenCursor<'a> {
    /// Create a parser over a token stream.
    pub fn new(input: &'a str, tokens: Vec<Token<'a>>) -> Self {
        Self {
            input,
            tokens,
            pos: 0,
            locals: Vec::new(),
        }
    }

    /// ```bnf
    /// <expr> ::= <assign>
    /// ```
    pub fn parse_expr(&mut self) -> Result<Node, String> {
        self.parse_assign()
    }

    /// ```bnf
    /// <program> ::= "{" <compound-stmt>
    /// ```
    pub fn parse_program(&mut self) -> Result<Program, String> {
        let offset = self.current().offset;
        self.skip_punct("{")?;
        let body = self.parse_compound_stmt()?;

        Ok(Program {
            body: vec![Stmt::block(body, offset)],
            locals: std::mem::take(&mut self.locals),
        })
    }

    /// Format an error at the current token.
    pub fn error_current(&self, message: &str) -> String {
        format_error_at(self.input, self.current().offset, message)
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
    fn parse_stmt(&mut self) -> Result<Stmt, String> {
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
    fn parse_compound_stmt(&mut self) -> Result<Vec<Stmt>, String> {
        let mut stmts = Vec::new();

        while !self.current().is_punct("}") {
            let mut stmt = if self.current().is_keyword(Keyword::Int) {
                self.parse_declaration()?
            } else {
                self.parse_stmt()?
            };
            self.infer_type_stmt(&mut stmt)?;
            stmts.push(stmt);
        }

        self.advance();
        Ok(stmts)
    }

    /// ```bnf
    /// <expr-stmt> ::= <expr>? ";"
    /// ```
    fn parse_expr_stmt(&mut self) -> Result<Stmt, String> {
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
    ///   <declspec>
    ///   (<declarator> ("=" <assign>)?
    ///     ("," <declarator> ("=" <assign>)?)*)?
    ///   ";"
    /// ```
    fn parse_declaration(&mut self) -> Result<Stmt, String> {
        let offset = self.current().offset;
        let base_ty = self.parse_declspec()?;
        let mut stmts = Vec::new();
        let mut first = true;

        while !self.current().is_punct(";") {
            if !first {
                self.skip_punct(",")?;
            }
            first = false;

            let (name, ty, name_offset) = self.parse_declarator(base_ty.clone())?;
            let local_id = self.create_local(name, ty);

            if !self.current().is_punct("=") {
                continue;
            }

            // If there is an initializer, treat it as an assignment to the
            // just-created variable
            self.advance();
            let lhs = Node::var(local_id, name_offset);
            let rhs = self.parse_assign()?;
            let expr = Node::assign(lhs, rhs, name_offset);
            stmts.push(Stmt::expr(expr, name_offset));
        }

        self.skip_punct(";")?;
        Ok(Stmt::block(stmts, offset))
    }

    /// ```bnf
    /// <declspec> ::= "int"
    /// ```
    fn parse_declspec(&mut self) -> Result<Type, String> {
        self.skip_keyword(Keyword::Int)?;
        Ok(Type::Int)
    }

    /// ```bnf
    /// <declarator> ::= "*"* ident
    /// ```
    fn parse_declarator(&mut self, mut ty: Type) -> Result<(String, Type, usize), String> {
        while self.current().is_punct("*") {
            self.advance();
            ty = Type::ptr(ty);
        }

        let tok = self.current();
        let Some(name) = tok.as_ident() else {
            return Err(self.error_current("expected a variable name"));
        };

        self.advance();
        Ok((name.to_owned(), ty, tok.offset))
    }

    /// ```bnf
    /// <assign> ::= <equality> ("=" <assign>)?
    /// ```
    fn parse_assign(&mut self) -> Result<Node, String> {
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
    fn parse_equality(&mut self) -> Result<Node, String> {
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
    fn parse_relational(&mut self) -> Result<Node, String> {
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
    fn parse_add(&mut self) -> Result<Node, String> {
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
    fn parse_mul(&mut self) -> Result<Node, String> {
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
    /// <unary> ::= ("+" | "-" | "*" | "&") <unary> | <primary>
    /// ```
    fn parse_unary(&mut self) -> Result<Node, String> {
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

        self.parse_primary()
    }

    /// ```bnf
    /// <primary> ::= "(" <expr> ")" | ident <args>? | num
    /// <args> ::= "(" ")"
    /// ```
    fn parse_primary(&mut self) -> Result<Node, String> {
        if self.current().is_punct("(") {
            self.advance();
            let node = self.parse_expr()?;
            self.skip_punct(")")?;
            return Ok(node);
        }

        let tok = self.current();
        if let Some(name) = tok.as_ident() {
            if self.peek(1).is_some_and(|tok| tok.is_punct("(")) {
                self.advance();
                self.skip_punct("(")?;
                self.skip_punct(")")?;
                return Ok(Node::funcall(name.to_owned(), tok.offset));
            }

            self.advance();
            let Some(local_id) = self.find_local(name) else {
                return Err(format_error_at(
                    self.input,
                    tok.offset,
                    "undefined variable",
                ));
            };
            return Ok(Node::var(local_id, tok.offset));
        }

        if let Some(value) = tok.as_num() {
            self.advance();
            return Ok(Node::num(value, tok.offset));
        }

        Err(self.error_current("expected an expression"))
    }

    /// Advance to the next token.
    fn advance(&mut self) {
        self.pos += 1;
    }

    /// Return the current token.
    fn current(&self) -> Token<'a> {
        self.tokens[self.pos]
    }

    /// Return a token at a fixed lookahead distance.
    fn peek(&self, offset: usize) -> Option<Token<'a>> {
        self.tokens.get(self.pos + offset).copied()
    }

    /// Consume a specific punctuator.
    fn skip_punct(&mut self, expected: &str) -> Result<(), String> {
        if !self.current().is_punct(expected) {
            return Err(self.error_current(&format!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// Consume a specific keyword.
    fn skip_keyword(&mut self, expected: Keyword) -> Result<(), String> {
        if !self.current().is_keyword(expected) {
            return Err(self.error_current(&format!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// Find an existing local by name; newest binding wins.
    fn find_local(&self, name: &str) -> Option<usize> {
        self.locals.iter().rposition(|local| local.name == name)
    }

    /// Create a new local variable.
    fn create_local(&mut self, name: String, ty: Type) -> usize {
        self.locals.push(LocalVar {
            name,
            ty,
            offset: 0, // Assigned during codegen
        });
        self.locals.len() - 1
    }

    /// Build an addition node with pointer scaling.
    fn new_add(&self, mut lhs: Node, mut rhs: Node, offset: usize) -> Result<Node, String> {
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
            return Err(format_error_at(self.input, offset, "invalid operands"));
        }

        // Canonicalize num + ptr to ptr + num
        if lhs_ty.base().is_none() && rhs_ty.base().is_some() {
            std::mem::swap(&mut lhs, &mut rhs);
        }

        // ptr + num
        let ptr_ty = lhs.ty.clone();
        let base_size = lhs.ty.as_ref().unwrap().base().unwrap().size();
        let scaled_rhs = Node::binary(BinaryOp::Mul, rhs, Node::num(base_size, offset), offset);
        let mut node = Node::binary(BinaryOp::Add, lhs, scaled_rhs, offset);
        node.ty = ptr_ty;
        Ok(node)
    }

    /// Build a subtraction node with pointer scaling.
    fn new_sub(&self, mut lhs: Node, mut rhs: Node, offset: usize) -> Result<Node, String> {
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

        Err(format_error_at(self.input, offset, "invalid operands"))
    }

    /// Infer types for a statement subtree.
    fn infer_type_stmt(&self, stmt: &mut Stmt) -> Result<(), String> {
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
    fn infer_type(&self, node: &mut Node) -> Result<(), String> {
        if node.ty.is_some() {
            return Ok(());
        }

        match &mut node.kind {
            NodeKind::Num(_) => {
                node.ty = Some(Type::Int);
            },
            NodeKind::FuncCall(_) => {
                node.ty = Some(Type::Int);
            },
            NodeKind::Neg(expr) => {
                self.infer_type(expr)?;
                node.ty = Some(Type::Int);
            },
            NodeKind::Var(local_id) => {
                node.ty = Some(self.locals[*local_id].ty.clone());
            },
            NodeKind::Addr(expr) => {
                self.infer_type(expr)?;
                node.ty = Some(Type::ptr(expr.ty.clone().unwrap()));
            },
            NodeKind::Deref(expr) => {
                self.infer_type(expr)?;
                let Some(base) = expr.ty.as_ref().and_then(Type::base) else {
                    return Err(format_error_at(
                        self.input,
                        node.offset,
                        "invalid pointer dereference",
                    ));
                };
                node.ty = Some(base.clone());
            },
            NodeKind::Assign { lhs, rhs } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                node.ty = lhs.ty.clone();
            },
            NodeKind::Binary { lhs, rhs, .. } => {
                self.infer_type(lhs)?;
                self.infer_type(rhs)?;
                node.ty = Some(Type::Int);
            },
        }

        Ok(())
    }
}
