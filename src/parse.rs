//! A recursive-descent parser.

use crate::ast::{BinaryOp, LocalVar, Node, Program, Stmt};
use crate::tokenize::{Keyword, Token, TokenKind, format_error_at};

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
    /// <stmt> ::= "return" <expr> ";"
    ///          | "if" "(" <expr> ")" <stmt> ("else" <stmt>)?
    ///          | "for" "(" <expr-stmt> <expr>? ";" <expr>? ")" <stmt>
    ///          | "while" "(" <expr> ")" <stmt>
    ///          | "{" <compound-stmt>
    ///          | <expr-stmt>
    /// ```
    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        if self.at_keyword(Keyword::Return) {
            let offset = self.current().offset;
            self.advance();
            let expr = self.parse_expr()?;
            self.skip_punct(";")?;
            return Ok(Stmt::return_(expr, offset));
        }

        if self.at_keyword(Keyword::If) {
            let offset = self.current().offset;
            self.advance();
            self.skip_punct("(")?;
            let cond = self.parse_expr()?;
            self.skip_punct(")")?;
            let then_branch = Box::new(self.parse_stmt()?);
            let else_branch = if self.at_keyword(Keyword::Else) {
                self.advance();
                Some(Box::new(self.parse_stmt()?))
            } else {
                None
            };
            return Ok(Stmt::if_(cond, then_branch, else_branch, offset));
        }

        if self.at_keyword(Keyword::For) {
            let offset = self.current().offset;
            self.advance();
            self.skip_punct("(")?;
            let init = Box::new(self.parse_expr_stmt()?);
            let cond = if self.at_punct(";") {
                None
            } else {
                Some(self.parse_expr()?)
            };
            self.skip_punct(";")?;
            let inc = if self.at_punct(")") {
                None
            } else {
                Some(self.parse_expr()?)
            };
            self.skip_punct(")")?;
            let body = Box::new(self.parse_stmt()?);
            return Ok(Stmt::for_(init, cond, inc, body, offset));
        }

        if self.at_keyword(Keyword::While) {
            let offset = self.current().offset;
            self.advance();
            self.skip_punct("(")?;
            let cond = self.parse_expr()?;
            self.skip_punct(")")?;
            let body = Box::new(self.parse_stmt()?);
            // "while" can be desugared into "for" without init and inc
            return Ok(Stmt::while_(cond, body, offset));
        }

        if self.at_punct("{") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Stmt::block(self.parse_compound_stmt()?, offset));
        }

        self.parse_expr_stmt()
    }

    /// ```bnf
    /// <compound-stmt> ::= <stmt>* "}"
    /// ```
    fn parse_compound_stmt(&mut self) -> Result<Vec<Stmt>, String> {
        let mut stmts = Vec::new();

        while !self.at_punct("}") {
            stmts.push(self.parse_stmt()?);
        }

        self.advance();
        Ok(stmts)
    }

    /// ```bnf
    /// <expr-stmt> ::= <expr>? ";"
    /// ```
    fn parse_expr_stmt(&mut self) -> Result<Stmt, String> {
        if self.at_punct(";") {
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
    /// <assign> ::= <equality> ("=" <assign>)?
    /// ```
    fn parse_assign(&mut self) -> Result<Node, String> {
        let node = self.parse_equality()?;

        if self.at_punct("=") {
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
            if self.at_punct("==") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Eq, node, self.parse_relational()?, offset);
                continue;
            }

            if self.at_punct("!=") {
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
            if self.at_punct("<") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Lt, node, self.parse_add()?, offset);
                continue;
            }

            if self.at_punct("<=") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Le, node, self.parse_add()?, offset);
                continue;
            }

            if self.at_punct(">") {
                let offset = self.current().offset;
                self.advance();
                // Reuse < with flipped operands
                node = Node::binary(BinaryOp::Lt, self.parse_add()?, node, offset);
                continue;
            }

            if self.at_punct(">=") {
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
            if self.at_punct("+") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Add, node, self.parse_mul()?, offset);
                continue;
            }

            if self.at_punct("-") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Sub, node, self.parse_mul()?, offset);
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
            if self.at_punct("*") {
                let offset = self.current().offset;
                self.advance();
                node = Node::binary(BinaryOp::Mul, node, self.parse_unary()?, offset);
                continue;
            }

            if self.at_punct("/") {
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
        if self.at_punct("+") {
            self.advance();
            return self.parse_unary();
        }

        if self.at_punct("-") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::neg(self.parse_unary()?, offset));
        }

        if self.at_punct("&") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::addr(self.parse_unary()?, offset));
        }

        if self.at_punct("*") {
            let offset = self.current().offset;
            self.advance();
            return Ok(Node::deref(self.parse_unary()?, offset));
        }

        self.parse_primary()
    }

    /// ```bnf
    /// <primary> ::= "(" <expr> ")" | ident | num
    /// ```
    fn parse_primary(&mut self) -> Result<Node, String> {
        if self.at_punct("(") {
            self.advance();
            let node = self.parse_expr()?;
            self.skip_punct(")")?;
            return Ok(node);
        }

        let tok = self.current();
        if let TokenKind::Ident(name) = tok.kind {
            self.advance();
            return Ok(Node::var(self.find_or_create_local(name), tok.offset));
        }

        if let TokenKind::Num(value) = tok.kind {
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

    /// Check whether the current token matches a punctuator.
    fn at_punct(&self, expected: &str) -> bool {
        let tok = self.current();
        tok.kind == TokenKind::Punct(expected)
    }

    /// Check whether the current token matches a keyword.
    fn at_keyword(&self, expected: Keyword) -> bool {
        let tok = self.current();
        tok.kind == TokenKind::Keyword(expected)
    }

    /// Consume a specific punctuator.
    ///
    /// This returns an error if the current token does not match the expected
    /// punctuator.
    fn skip_punct(&mut self, expected: &str) -> Result<(), String> {
        if !self.at_punct(expected) {
            return Err(self.error_current(&format!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// Find an existing local or create a new one.
    ///
    /// An index of the local variable is returned, which will be used to assign
    /// offset during code generation.
    fn find_or_create_local(&mut self, name: &str) -> usize {
        if let Some(index) = self.locals.iter().position(|local| local.name == name) {
            return index;
        }

        self.locals.push(LocalVar {
            name: name.to_owned(),
            offset: 0, // To be assigned later during codegen
        });
        self.locals.len() - 1
    }
}
