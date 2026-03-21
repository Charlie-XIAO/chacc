use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
struct Cli {
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

        if matches!(ch, b'+' | b'-') {
            tokens.push(Token {
                kind: TokenKind::Punct,
                lexeme: &rest[..1],
                value: 0,
                offset,
            });
            rest = &rest[1..];
            offset += 1;
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

/// Format an error with a caret pointing at the given byte offset.
fn format_error_at(input: &str, offset: usize, message: &str) -> String {
    format!("{input}\n{}^ {message}", " ".repeat(offset))
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

    /// Read the current numeric literal.
    fn get_number(&self) -> Result<i64, String> {
        let tok = self.current();
        if tok.kind != TokenKind::Num {
            return Err(self.error_current("expected a number"));
        }
        Ok(tok.value)
    }

    /// Format an error at the current token.
    fn error_current(&self, message: &str) -> String {
        format_error_at(self.input, self.current().offset, message)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Token, TokenKind, compile_expression_program, tokenize};

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
    fn parses_the_single_input_argument() {
        let cli = Cli::try_parse_from(["chacc", "12 + 34"]).unwrap();
        assert_eq!(cli.input, "12 + 34");
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
            tokenize(" 12 + 34 - 5 ").unwrap(),
            vec![
                Token {
                    kind: TokenKind::Num,
                    lexeme: "12",
                    value: 12,
                    offset: 1,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "+",
                    value: 0,
                    offset: 4,
                },
                Token {
                    kind: TokenKind::Num,
                    lexeme: "34",
                    value: 34,
                    offset: 6,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "-",
                    value: 0,
                    offset: 9,
                },
                Token {
                    kind: TokenKind::Num,
                    lexeme: "5",
                    value: 5,
                    offset: 11,
                },
                Token {
                    kind: TokenKind::Eof,
                    lexeme: "",
                    value: 0,
                    offset: 13,
                },
            ]
        );
    }

    #[test]
    fn rejects_invalid_tokens() {
        let error = compile_expression_program("5a").unwrap_err();
        assert_eq!(error, "5a\n ^ invalid token");
    }

    #[test]
    fn reports_parser_errors_at_the_token() {
        let error = compile_expression_program("1+").unwrap_err();
        assert_eq!(error, "1+\n  ^ expected a number");
    }
}
