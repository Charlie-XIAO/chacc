//! Recursive-descent parser for chacc expressions.

use crate::ast::{BinaryOp, LocalVar, Node, Program, Stmt};
use crate::tokenize::{Token, TokenKind, format_error_at};

/// Cursor over the token stream during parsing.
pub(crate) struct TokenCursor<'a> {
    input: &'a str,
    tokens: Vec<Token<'a>>,
    pos: usize,
    locals: Vec<LocalVar>,
}

impl<'a> TokenCursor<'a> {
    /// Create a parser over a token stream.
    pub(crate) fn new(input: &'a str, tokens: Vec<Token<'a>>) -> Self {
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
    pub(crate) fn parse_expr(&mut self) -> Result<Node, String> {
        self.parse_assign()
    }

    /// ```bnf
    /// <program> ::= "{" <compound-stmt>
    /// ```
    pub(crate) fn parse_program(&mut self) -> Result<Program, String> {
        self.skip("{")?;
        let body = self.parse_compound_stmt()?;

        Ok(Program {
            body: vec![Stmt::Block(body)],
            locals: std::mem::take(&mut self.locals),
        })
    }

    /// Format an error at the current token.
    pub(crate) fn error_current(&self, message: &str) -> String {
        format_error_at(self.input, self.current().offset, message)
    }

    /// ```bnf
    /// <stmt> ::= "return" <expr> ";"
    ///          | "if" "(" <expr> ")" <stmt> ("else" <stmt>)?
    ///          | "{" <compound-stmt>
    ///          | <expr-stmt>
    /// ```
    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        if self.at_keyword("return") {
            self.advance();
            let expr = self.parse_expr()?;
            self.skip(";")?;
            return Ok(Stmt::Return(expr));
        }

        if self.at_keyword("if") {
            self.advance();
            self.skip("(")?;
            let cond = self.parse_expr()?;
            self.skip(")")?;
            let then_branch = Box::new(self.parse_stmt()?);
            let else_branch = if self.at_keyword("else") {
                self.advance();
                Some(Box::new(self.parse_stmt()?))
            } else {
                None
            };
            return Ok(Stmt::If {
                cond,
                then_branch,
                else_branch,
            });
        }

        if self.at_punct("{") {
            self.advance();
            return Ok(Stmt::Block(self.parse_compound_stmt()?));
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
            self.advance();
            return Ok(Stmt::Block(Vec::new()));
        }

        let expr = self.parse_expr()?;
        self.skip(";")?;
        Ok(Stmt::Expr(expr))
    }

    /// ```bnf
    /// <assign> ::= <equality> ("=" <assign>)?
    /// ```
    fn parse_assign(&mut self) -> Result<Node, String> {
        let node = self.parse_equality()?;

        if self.at_punct("=") {
            self.advance();
            return Ok(Node::assign(node, self.parse_assign()?));
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
                self.advance();
                node = Node::binary(BinaryOp::Eq, node, self.parse_relational()?);
                continue;
            }

            if self.at_punct("!=") {
                self.advance();
                node = Node::binary(BinaryOp::Ne, node, self.parse_relational()?);
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
                self.advance();
                node = Node::binary(BinaryOp::Lt, node, self.parse_add()?);
                continue;
            }

            if self.at_punct("<=") {
                self.advance();
                node = Node::binary(BinaryOp::Le, node, self.parse_add()?);
                continue;
            }

            if self.at_punct(">") {
                self.advance();
                // Reuse < with flipped operands
                node = Node::binary(BinaryOp::Lt, self.parse_add()?, node);
                continue;
            }

            if self.at_punct(">=") {
                self.advance();
                // Reuse <= with flipped operands
                node = Node::binary(BinaryOp::Le, self.parse_add()?, node);
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
                self.advance();
                node = Node::binary(BinaryOp::Add, node, self.parse_mul()?);
                continue;
            }

            if self.at_punct("-") {
                self.advance();
                node = Node::binary(BinaryOp::Sub, node, self.parse_mul()?);
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
                self.advance();
                node = Node::binary(BinaryOp::Mul, node, self.parse_unary()?);
                continue;
            }

            if self.at_punct("/") {
                self.advance();
                node = Node::binary(BinaryOp::Div, node, self.parse_unary()?);
                continue;
            }

            return Ok(node);
        }
    }

    /// ```bnf
    /// <unary> ::= ("+" | "-") <unary> | <primary>
    /// ```
    fn parse_unary(&mut self) -> Result<Node, String> {
        if self.at_punct("+") {
            self.advance();
            return self.parse_unary();
        }

        if self.at_punct("-") {
            self.advance();
            return Ok(Node::neg(self.parse_unary()?));
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
            self.skip(")")?;
            return Ok(node);
        }

        let tok = self.current();
        if tok.kind == TokenKind::Ident {
            self.advance();
            return Ok(Node::Var(self.find_or_create_local(tok.lexeme)));
        }

        if tok.kind == TokenKind::Num {
            self.advance();
            return Ok(Node::Num(tok.value));
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
        tok.kind == TokenKind::Punct && tok.lexeme == expected
    }

    /// Check whether the current token matches a keyword.
    fn at_keyword(&self, expected: &str) -> bool {
        let tok = self.current();
        tok.kind == TokenKind::Keyword && tok.lexeme == expected
    }

    /// Consume a specific punctuator.
    fn skip(&mut self, expected: &str) -> Result<(), String> {
        if !self.at_punct(expected) {
            return Err(self.error_current(&format!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// Find an existing local or create a new one.
    fn find_or_create_local(&mut self, name: &str) -> usize {
        if let Some(index) = self.locals.iter().position(|local| local.name == name) {
            return index;
        }

        // Stable local id - offset assigned later
        self.locals.push(LocalVar {
            name: name.to_owned(),
            offset: 0,
        });
        self.locals.len() - 1
    }
}
