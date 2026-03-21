//! The Cha C compiler (chacc) library.

mod ast;
mod codegen;
mod parse;
mod tokenize;

use codegen::gen_expr;
use parse::TokenCursor;
use tokenize::tokenize;

/// Compile an expression into a tiny `main` function.
pub fn compile_expression_program(input: &str) -> Result<String, String> {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::compile_expression_program;
    use crate::tokenize::{Token, TokenKind, tokenize};

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

    /// Assemble and run generated code, returning the exit status.
    fn eval_with_cc(assembly: &str) -> i32 {
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
