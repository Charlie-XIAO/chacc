//! The Cha C compiler (chacc) library.

mod ast;
mod codegen;
mod parse;
mod tokenize;

use ast::Program;
use codegen::Codegen;
use parse::TokenCursor;
use tokenize::tokenize;

/// Compile the input program into a tiny `main` function.
pub fn compile_expression_program(input: &str) -> Result<String, String> {
    let tokens = tokenize(input)?;
    let mut parser = TokenCursor::new(input, tokens);
    let Program { body, locals } = parser.parse_program()?;
    let mut codegen = Codegen::new(locals);

    for stmt in &body {
        codegen.gen_stmt(stmt)?;
        codegen.assert_balanced();
    }

    Ok(codegen.finish())
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
            compile_expression_program("{ 5+6*7; }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $0, %rsp\n",
                "  mov $7, %rax\n",
                "  push %rax\n",
                "  mov $6, %rax\n",
                "  pop %rdi\n",
                "  imul %rdi, %rax\n",
                "  push %rax\n",
                "  mov $5, %rax\n",
                "  pop %rdi\n",
                "  add %rdi, %rax\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_unary_minus() {
        assert_eq!(
            compile_expression_program("{ -10+20; }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $0, %rsp\n",
                "  mov $20, %rax\n",
                "  push %rax\n",
                "  mov $10, %rax\n",
                "  neg %rax\n",
                "  pop %rdi\n",
                "  add %rdi, %rax\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_equality() {
        assert_eq!(
            compile_expression_program("{ 0==1; }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $0, %rsp\n",
                "  mov $1, %rax\n",
                "  push %rax\n",
                "  mov $0, %rax\n",
                "  pop %rdi\n",
                "  cmp %rdi, %rax\n",
                "  sete %al\n",
                "  movzb %al, %rax\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_assignment() {
        assert_eq!(
            compile_expression_program("{ foo=3; foo; }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $16, %rsp\n",
                "  lea -8(%rbp), %rax\n",
                "  push %rax\n",
                "  mov $3, %rax\n",
                "  pop %rdi\n",
                "  mov %rax, (%rdi)\n",
                "  lea -8(%rbp), %rax\n",
                "  mov (%rax), %rax\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_return() {
        assert_eq!(
            compile_expression_program("{ return 42; }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $0, %rsp\n",
                "  mov $42, %rax\n",
                "  jmp .L.return\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn tokenizes_identifiers_punctuation_and_whitespace() {
        assert_eq!(
            tokenize(" foo123=3; bar=5; foo123+bar;").unwrap(),
            vec![
                Token {
                    kind: TokenKind::Ident,
                    lexeme: "foo123",
                    value: 0,
                    offset: 1,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "=",
                    value: 0,
                    offset: 7,
                },
                Token {
                    kind: TokenKind::Num,
                    lexeme: "3",
                    value: 3,
                    offset: 8,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: ";",
                    value: 0,
                    offset: 9,
                },
                Token {
                    kind: TokenKind::Ident,
                    lexeme: "bar",
                    value: 0,
                    offset: 11,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "=",
                    value: 0,
                    offset: 14,
                },
                Token {
                    kind: TokenKind::Num,
                    lexeme: "5",
                    value: 5,
                    offset: 15,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: ";",
                    value: 0,
                    offset: 16,
                },
                Token {
                    kind: TokenKind::Ident,
                    lexeme: "foo123",
                    value: 0,
                    offset: 18,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: "+",
                    value: 0,
                    offset: 24,
                },
                Token {
                    kind: TokenKind::Ident,
                    lexeme: "bar",
                    value: 0,
                    offset: 25,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: ";",
                    value: 0,
                    offset: 28,
                },
                Token {
                    kind: TokenKind::Eof,
                    lexeme: "",
                    value: 0,
                    offset: 29,
                },
            ]
        );
    }

    #[test]
    fn tokenizes_return_as_a_keyword() {
        assert_eq!(
            tokenize("return foo;").unwrap(),
            vec![
                Token {
                    kind: TokenKind::Keyword,
                    lexeme: "return",
                    value: 0,
                    offset: 0,
                },
                Token {
                    kind: TokenKind::Ident,
                    lexeme: "foo",
                    value: 0,
                    offset: 7,
                },
                Token {
                    kind: TokenKind::Punct,
                    lexeme: ";",
                    value: 0,
                    offset: 10,
                },
                Token {
                    kind: TokenKind::Eof,
                    lexeme: "",
                    value: 0,
                    offset: 11,
                },
            ]
        );
    }

    #[test]
    fn rejects_invalid_tokens() {
        let error = compile_expression_program("{ 1+@; }").unwrap_err();
        assert_eq!(error, "{ 1+@; }\n    ^ expected an expression");
    }

    #[test]
    fn reports_missing_expressions() {
        let error = compile_expression_program("{ 1+; }").unwrap_err();
        assert_eq!(error, "{ 1+; }\n    ^ expected an expression");
    }

    #[test]
    fn reports_missing_semicolons() {
        let error = compile_expression_program("{ 1 2; }").unwrap_err();
        assert_eq!(error, "{ 1 2; }\n    ^ expected ';'");
    }

    #[test]
    fn parses_nested_unary_operators() {
        assert_eq!(
            compile_expression_program("{ - - +10; }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $0, %rsp\n",
                "  mov $10, %rax\n",
                "  neg %rax\n",
                "  neg %rax\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn evaluates_comparisons() {
        for (input, expected) in [
            ("{ 0==1; }", 0),
            ("{ 42==42; }", 1),
            ("{ 0!=1; }", 1),
            ("{ 42!=42; }", 0),
            ("{ 0<1; }", 1),
            ("{ 1<1; }", 0),
            ("{ 2<1; }", 0),
            ("{ 0<=1; }", 1),
            ("{ 1<=1; }", 1),
            ("{ 2<=1; }", 0),
            ("{ 1>0; }", 1),
            ("{ 1>1; }", 0),
            ("{ 1>2; }", 0),
            ("{ 1>=0; }", 1),
            ("{ 1>=1; }", 1),
            ("{ 1>=2; }", 0),
        ] {
            let asm = compile_expression_program(input).unwrap();
            assert!(asm.contains("movzb %al, %rax"), "{input}: {asm}");
            assert_eq!(eval_with_cc(&asm), expected, "{input}");
        }
    }

    #[test]
    fn evaluates_multiple_statements() {
        let asm = compile_expression_program("{ 1; 2; 3; }").unwrap();
        assert_eq!(eval_with_cc(&asm), 3);
    }

    #[test]
    fn evaluates_assignments() {
        for (input, expected) in [
            ("{ a=3; a; }", 3),
            ("{ a=3; z=5; a+z; }", 8),
            ("{ a=b=3; a+b; }", 6),
            ("{ foo=3; foo; }", 3),
            ("{ foo123=3; bar=5; foo123+bar; }", 8),
        ] {
            let asm = compile_expression_program(input).unwrap();
            assert_eq!(eval_with_cc(&asm), expected, "{input}");
        }
    }

    #[test]
    fn evaluates_returns() {
        for (input, expected) in [
            ("{ return 0; }", 0),
            ("{ return 42; }", 42),
            ("{ a=3; return a; }", 3),
            ("{ a=3; z=5; return a+z; }", 8),
            ("{ return 1; 2; 3; }", 1),
            ("{ 1; return 2; 3; }", 2),
            ("{ 1; 2; return 3; }", 3),
            ("{ {1; {2;} return 3;} }", 3),
        ] {
            let asm = compile_expression_program(input).unwrap();
            assert_eq!(eval_with_cc(&asm), expected, "{input}");
        }
    }

    #[test]
    fn rejects_non_lvalues_on_assignment() {
        let error = compile_expression_program("{ 1=2; }").unwrap_err();
        assert_eq!(error, "not an lvalue");
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
