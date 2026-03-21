use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
struct Cli {
    #[arg(allow_hyphen_values = true)]
    input: String,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse_from(std::env::args_os()) {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Compile the input expression and print assembly.
fn run(cli: Cli) -> Result<(), String> {
    let assembly = compile_expression_program(&cli.input)?;
    print!("{assembly}");
    Ok(())
}

/// Compile an expression into a tiny `main` function.
fn compile_expression_program(input: &str) -> Result<String, String> {
    let tokens = tokenize(input)?;
    let mut parser = TokenCursor::new(input, tokens);
    let node = parser.parse_expr()?;

    if !parser.at_eof() {
        return Err(parser.error_current("extra token"));
    }

    let mut assembly = String::from("  .globl main\nmain:\n");
    let mut depth = 0;
    gen_expr(&node, &mut assembly, &mut depth);
    assembly.push_str("  ret\n");

    assert_eq!(depth, 0);
    Ok(assembly)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TokenKind {
    Punct,
    Num,
    Eof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Token<'a> {
    kind: TokenKind,
    lexeme: &'a str,
    value: i64,
    offset: usize,
}

/// Convert the input string into tokens.
fn tokenize(input: &str) -> Result<Vec<Token<'_>>, String> {
    let mut tokens = Vec::new();
    let mut rest = input;
    let mut offset = 0;

    while !rest.is_empty() {
        let ch = rest.as_bytes()[0];

        if ch.is_ascii_whitespace() {
            rest = &rest[1..];
            offset += 1;
            continue;
        }

        let punct_len = read_punct(rest);
        if punct_len != 0 {
            tokens.push(Token {
                kind: TokenKind::Punct,
                lexeme: &rest[..punct_len],
                value: 0,
                offset,
            });
            rest = &rest[punct_len..];
            offset += punct_len;
            continue;
        }

        if ch.is_ascii_digit() {
            let len = rest.bytes().take_while(u8::is_ascii_digit).count();
            let lexeme = &rest[..len];
            tokens.push(Token {
                kind: TokenKind::Num,
                lexeme,
                value: lexeme.parse().unwrap(),
                offset,
            });
            rest = &rest[len..];
            offset += len;
            continue;
        }

        return Err(format_error_at(input, offset, "invalid token"));
    }

    // EOF sentinel
    tokens.push(Token {
        kind: TokenKind::Eof,
        lexeme: "",
        value: 0,
        offset,
    });
    Ok(tokens)
}

/// Read a punctuator token and return its length.
fn read_punct(input: &str) -> usize {
    if ["==", "!=", "<=", ">="]
        .into_iter()
        .any(|prefix| input.starts_with(prefix))
    {
        return 2;
    }

    usize::from(
        input
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_punctuation),
    )
}

/// Format an error with a caret pointing at the given byte offset.
fn format_error_at(input: &str, offset: usize, message: &str) -> String {
    format!("{input}\n{}^ {message}", " ".repeat(offset))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
}

#[derive(Debug, Eq, PartialEq)]
enum Node {
    Num(i64),
    Neg(Box<Node>),
    Binary {
        op: BinaryOp,
        lhs: Box<Node>,
        rhs: Box<Node>,
    },
}

impl Node {
    /// Construct a binary AST node.
    fn binary(op: BinaryOp, lhs: Node, rhs: Node) -> Self {
        Self::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    /// Construct a unary negation node.
    fn neg(node: Node) -> Self {
        Self::Neg(Box::new(node))
    }
}

struct TokenCursor<'a> {
    input: &'a str,
    tokens: Vec<Token<'a>>,
    pos: usize,
}

impl<'a> TokenCursor<'a> {
    /// Create a parser over a token stream.
    fn new(input: &'a str, tokens: Vec<Token<'a>>) -> Self {
        Self {
            input,
            tokens,
            pos: 0,
        }
    }

    /// Parse `expr = equality`.
    fn parse_expr(&mut self) -> Result<Node, String> {
        self.parse_equality()
    }

    /// Parse `equality = relational ("==" relational | "!=" relational)*`.
    fn parse_equality(&mut self) -> Result<Node, String> {
        let mut node = self.parse_relational()?;

        loop {
            if self.equal("==") {
                self.advance();
                node = Node::binary(BinaryOp::Eq, node, self.parse_relational()?);
                continue;
            }

            if self.equal("!=") {
                self.advance();
                node = Node::binary(BinaryOp::Ne, node, self.parse_relational()?);
                continue;
            }

            return Ok(node);
        }
    }

    /// Parse `relational = add ("<" add | "<=" add | ">" add | ">=" add)*`.
    fn parse_relational(&mut self) -> Result<Node, String> {
        let mut node = self.parse_add()?;

        loop {
            if self.equal("<") {
                self.advance();
                node = Node::binary(BinaryOp::Lt, node, self.parse_add()?);
                continue;
            }

            if self.equal("<=") {
                self.advance();
                node = Node::binary(BinaryOp::Le, node, self.parse_add()?);
                continue;
            }

            if self.equal(">") {
                self.advance();
                node = Node::binary(BinaryOp::Lt, self.parse_add()?, node);
                continue;
            }

            if self.equal(">=") {
                self.advance();
                node = Node::binary(BinaryOp::Le, self.parse_add()?, node);
                continue;
            }

            return Ok(node);
        }
    }

    /// Parse `add = mul ("+" mul | "-" mul)*`.
    fn parse_add(&mut self) -> Result<Node, String> {
        let mut node = self.parse_mul()?;

        loop {
            if self.equal("+") {
                self.advance();
                node = Node::binary(BinaryOp::Add, node, self.parse_mul()?);
                continue;
            }

            if self.equal("-") {
                self.advance();
                node = Node::binary(BinaryOp::Sub, node, self.parse_mul()?);
                continue;
            }

            return Ok(node);
        }
    }

    /// Parse `mul = unary ("*" unary | "/" unary)*`.
    fn parse_mul(&mut self) -> Result<Node, String> {
        let mut node = self.parse_unary()?;

        loop {
            if self.equal("*") {
                self.advance();
                node = Node::binary(BinaryOp::Mul, node, self.parse_unary()?);
                continue;
            }

            if self.equal("/") {
                self.advance();
                node = Node::binary(BinaryOp::Div, node, self.parse_unary()?);
                continue;
            }

            return Ok(node);
        }
    }

    /// Parse `unary = ("+" | "-") unary | primary`.
    fn parse_unary(&mut self) -> Result<Node, String> {
        if self.equal("+") {
            self.advance();
            return self.parse_unary();
        }

        if self.equal("-") {
            self.advance();
            return Ok(Node::neg(self.parse_unary()?));
        }

        self.parse_primary()
    }

    /// Parse `primary = "(" expr ")" | num`.
    fn parse_primary(&mut self) -> Result<Node, String> {
        if self.equal("(") {
            self.advance();
            let node = self.parse_expr()?;
            self.skip(")")?;
            return Ok(node);
        }

        let tok = self.current();
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

    /// Check whether the current token is EOF.
    fn at_eof(&self) -> bool {
        self.current().kind == TokenKind::Eof
    }

    /// Return the current token.
    fn current(&self) -> Token<'a> {
        self.tokens[self.pos]
    }

    /// Check whether the current token matches a punctuator.
    fn equal(&self, expected: &str) -> bool {
        let tok = self.current();
        tok.kind == TokenKind::Punct && tok.lexeme == expected
    }

    /// Consume a specific punctuator.
    fn skip(&mut self, expected: &str) -> Result<(), String> {
        if !self.equal(expected) {
            return Err(self.error_current(&format!("expected '{expected}'")));
        }
        self.advance();
        Ok(())
    }

    /// Format an error at the current token.
    fn error_current(&self, message: &str) -> String {
        format_error_at(self.input, self.current().offset, message)
    }
}

/// Emit assembly for the given expression node.
fn gen_expr(node: &Node, assembly: &mut String, depth: &mut i32) {
    match node {
        Node::Num(value) => {
            assembly.push_str(&format!("  mov ${value}, %rax\n"));
        }
        Node::Neg(expr) => {
            gen_expr(expr, assembly, depth);
            assembly.push_str("  neg %rax\n");
        }
        Node::Binary { op, lhs, rhs } => {
            gen_expr(rhs, assembly, depth);
            push(assembly, depth);
            gen_expr(lhs, assembly, depth);
            pop("%rdi", assembly, depth);

            match op {
                BinaryOp::Add => assembly.push_str("  add %rdi, %rax\n"),
                BinaryOp::Sub => assembly.push_str("  sub %rdi, %rax\n"),
                BinaryOp::Mul => assembly.push_str("  imul %rdi, %rax\n"),
                BinaryOp::Div => {
                    assembly.push_str("  cqo\n");
                    assembly.push_str("  idiv %rdi\n");
                }
                BinaryOp::Eq => {
                    assembly.push_str("  cmp %rdi, %rax\n");
                    assembly.push_str("  sete %al\n");
                    assembly.push_str("  movzb %al, %rax\n");
                }
                BinaryOp::Ne => {
                    assembly.push_str("  cmp %rdi, %rax\n");
                    assembly.push_str("  setne %al\n");
                    assembly.push_str("  movzb %al, %rax\n");
                }
                BinaryOp::Lt => {
                    assembly.push_str("  cmp %rdi, %rax\n");
                    assembly.push_str("  setl %al\n");
                    assembly.push_str("  movzb %al, %rax\n");
                }
                BinaryOp::Le => {
                    assembly.push_str("  cmp %rdi, %rax\n");
                    assembly.push_str("  setle %al\n");
                    assembly.push_str("  movzb %al, %rax\n");
                }
            }
        }
    }
}

/// Push `%rax` onto the temporary expression stack.
fn push(assembly: &mut String, depth: &mut i32) {
    assembly.push_str("  push %rax\n");
    *depth += 1;
}

/// Pop the top of the temporary stack into a register.
fn pop(register: &str, assembly: &mut String, depth: &mut i32) {
    assembly.push_str(&format!("  pop {register}\n"));
    *depth -= 1;
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Token, TokenKind, compile_expression_program, tokenize};

    #[test]
    fn emits_expected_assembly() {
        assert_eq!(
            compile_expression_program("5+6*7").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  mov $7, %rax\n",
                "  push %rax\n",
                "  mov $6, %rax\n",
                "  pop %rdi\n",
                "  imul %rdi, %rax\n",
                "  push %rax\n",
                "  mov $5, %rax\n",
                "  pop %rdi\n",
                "  add %rdi, %rax\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_unary_minus() {
        assert_eq!(
            compile_expression_program("-10+20").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  mov $20, %rax\n",
                "  push %rax\n",
                "  mov $10, %rax\n",
                "  neg %rax\n",
                "  pop %rdi\n",
                "  add %rdi, %rax\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_equality() {
        assert_eq!(
            compile_expression_program("0==1").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  mov $1, %rax\n",
                "  push %rax\n",
                "  mov $0, %rax\n",
                "  pop %rdi\n",
                "  cmp %rdi, %rax\n",
                "  sete %al\n",
                "  movzb %al, %rax\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn parses_the_single_input_argument() {
        let cli = Cli::try_parse_from(["chacc", "12 + 34"]).unwrap();
        assert_eq!(cli.input, "12 + 34");
    }

    #[test]
    fn parses_hyphen_prefixed_input() {
        let cli = Cli::try_parse_from(["chacc", "-10+20"]).unwrap();
        assert_eq!(cli.input, "-10+20");
    }

    #[test]
    fn rejects_the_wrong_number_of_arguments() {
        let error = Cli::try_parse_from(["chacc"]).unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn tokenizes_numbers_punctuation_and_whitespace() {
        assert_eq!(
            tokenize(" (12 + 34) <= 5 ").unwrap(),
            vec![
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "(",
                    value: 0,
                    offset: 1,
                },
                Token {
                    kind: TokenKind::Num,
                    lexeme: "12",
                    value: 12,
                    offset: 2,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "+",
                    value: 0,
                    offset: 5,
                },
                Token {
                    kind: TokenKind::Num,
                    lexeme: "34",
                    value: 34,
                    offset: 7,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: ")",
                    value: 0,
                    offset: 9,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "<=",
                    value: 0,
                    offset: 11,
                },
                Token {
                    kind: TokenKind::Num,
                    lexeme: "5",
                    value: 5,
                    offset: 14,
                },
                Token {
                    kind: TokenKind::Eof,
                    lexeme: "",
                    value: 0,
                    offset: 16,
                },
            ]
        );
    }

    #[test]
    fn rejects_invalid_tokens() {
        let error = compile_expression_program("1+foo").unwrap_err();
        assert_eq!(error, "1+foo\n  ^ invalid token");
    }

    #[test]
    fn reports_missing_expressions() {
        let error = compile_expression_program("1+").unwrap_err();
        assert_eq!(error, "1+\n  ^ expected an expression");
    }

    #[test]
    fn reports_extra_tokens() {
        let error = compile_expression_program("1 2").unwrap_err();
        assert_eq!(error, "1 2\n  ^ extra token");
    }

    #[test]
    fn parses_nested_unary_operators() {
        assert_eq!(
            compile_expression_program("- - +10").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  mov $10, %rax\n",
                "  neg %rax\n",
                "  neg %rax\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn evaluates_comparisons() {
        for (input, expected) in [
            ("0==1", 0),
            ("42==42", 1),
            ("0!=1", 1),
            ("42!=42", 0),
            ("0<1", 1),
            ("1<1", 0),
            ("2<1", 0),
            ("0<=1", 1),
            ("1<=1", 1),
            ("2<=1", 0),
            ("1>0", 1),
            ("1>1", 0),
            ("1>2", 0),
            ("1>=0", 1),
            ("1>=1", 1),
            ("1>=2", 0),
        ] {
            let asm = compile_expression_program(input).unwrap();
            assert!(asm.contains("movzb %al, %rax"), "{input}: {asm}");
            assert_eq!(eval_with_cc(&asm), expected, "{input}");
        }
    }

    fn eval_with_cc(assembly: &str) -> i32 {
        use std::fs;
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let dir = std::env::temp_dir().join(format!(
            "chacc-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        fs::create_dir(&dir).unwrap();
        let asm_path = dir.join("tmp.s");
        let exe_path = dir.join("tmp");
        fs::write(&asm_path, assembly).unwrap();

        let status = Command::new("cc")
            .arg("-o")
            .arg(&exe_path)
            .arg(&asm_path)
            .status()
            .unwrap();
        assert!(status.success());

        let status = Command::new(&exe_path).status().unwrap();
        fs::remove_dir_all(dir).unwrap();
        status.code().unwrap()
    }
}
