use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Parse CLI arguments, compile the expression, and print assembly.
fn run() -> Result<(), String> {
    let mut args = env::args();
    let program_name = args.next().unwrap_or_else(|| "chacc".to_owned());
    let argv: Vec<String> = args.collect();
    let assembly = compile_from_args(&program_name, &argv)?;

    print!("{assembly}");
    Ok(())
}

/// Validate the CLI shape and compile the single input expression.
fn compile_from_args(program_name: &str, args: &[String]) -> Result<String, String> {
    let [input] = args else {
        return Err(format!("{program_name}: invalid number of arguments"));
    };

    compile_expression_program(input)
}

/// Compile an expression into a tiny `main` function.
fn compile_expression_program(input: &str) -> Result<String, String> {
    let tokens = tokenize(input)?;
    let mut parser = Parser::new(tokens);
    let mut assembly = String::from("  .globl main\nmain:\n");

    let value = parser.get_number()?;
    assembly.push_str(&format!("  mov ${value}, %rax\n"));
    parser.advance();

    while !parser.at_eof() {
        if parser.equal("+") {
            parser.advance();
            let value = parser.get_number()?;
            assembly.push_str(&format!("  add ${value}, %rax\n"));
            parser.advance();
            continue;
        }

        parser.skip("-")?;
        let value = parser.get_number()?;
        assembly.push_str(&format!("  sub ${value}, %rax\n"));
        parser.advance();
    }

    assembly.push_str("  ret\n");
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
}

/// Convert the input string into tokens.
fn tokenize(mut input: &str) -> Result<Vec<Token<'_>>, String> {
    let mut tokens = Vec::new();

    while !input.is_empty() {
        let ch = input.as_bytes()[0];

        if ch.is_ascii_whitespace() {
            input = &input[1..];
            continue;
        }

        if matches!(ch, b'+' | b'-') {
            tokens.push(Token {
                kind: TokenKind::Punct,
                lexeme: &input[..1],
                value: 0,
            });
            input = &input[1..];
            continue;
        }

        if ch.is_ascii_digit() {
            let len = input.bytes().take_while(u8::is_ascii_digit).count();
            let lexeme = &input[..len];
            tokens.push(Token {
                kind: TokenKind::Num,
                lexeme,
                value: lexeme.parse().unwrap(),
            });
            input = &input[len..];
            continue;
        }

        return Err("invalid token".to_owned());
    }

    // EOF sentinel
    tokens.push(Token {
        kind: TokenKind::Eof,
        lexeme: "",
        value: 0,
    });
    Ok(tokens)
}

struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    pos: usize,
}

impl<'a> Parser<'a> {
    /// Create a parser over a token stream.
    fn new(tokens: Vec<Token<'a>>) -> Self {
        Self { tokens, pos: 0 }
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
            return Err(format!("expected '{expected}'"));
        }
        self.advance();
        Ok(())
    }

    /// Read the current numeric literal.
    fn get_number(&self) -> Result<i64, String> {
        let tok = self.current();
        if tok.kind != TokenKind::Num {
            return Err("expected a number".to_owned());
        }
        Ok(tok.value)
    }
}

#[cfg(test)]
mod tests {
    use super::{Token, TokenKind, compile_expression_program, compile_from_args, tokenize};

    #[test]
    fn emits_expected_assembly() {
        assert_eq!(
            compile_expression_program("5+20-4").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  mov $5, %rax\n",
                "  add $20, %rax\n",
                "  sub $4, %rax\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn rejects_the_wrong_number_of_arguments() {
        let error = compile_from_args("chacc", &[]).unwrap_err();
        assert_eq!(error, "chacc: invalid number of arguments");
    }

    #[test]
    fn tokenizes_numbers_punctuation_and_whitespace() {
        assert_eq!(
            tokenize(" 12 + 34 - 5 ").unwrap(),
            vec![
                Token {
                    kind: TokenKind::Num,
                    lexeme: "12",
                    value: 12,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "+",
                    value: 0,
                },
                Token {
                    kind: TokenKind::Num,
                    lexeme: "34",
                    value: 34,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "-",
                    value: 0,
                },
                Token {
                    kind: TokenKind::Num,
                    lexeme: "5",
                    value: 5,
                },
                Token {
                    kind: TokenKind::Eof,
                    lexeme: "",
                    value: 0,
                },
            ]
        );
    }

    #[test]
    fn rejects_invalid_tokens() {
        let error = compile_expression_program("5a").unwrap_err();
        assert_eq!(error, "invalid token");
    }
}
