//! The Cha C compiler (chacc).

mod ast;
mod codegen;
mod parse;
mod tokenize;
mod types;

use ast::Program;
use codegen::Codegen;
use parse::TokenCursor;
use tokenize::tokenize;

/// Compile the input program into a tiny `main` function.
pub fn compile_expression_program(input: &str) -> Result<String, String> {
    let tokens = tokenize(input)?;
    let mut parser = TokenCursor::new(input, tokens);
    let Program { body, locals } = parser.parse_program()?;
    let mut codegen = Codegen::new(input, locals);

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
    use crate::tokenize::{Keyword, Token, tokenize};

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
            compile_expression_program("{ int foo=3; foo; }").unwrap(),
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
    fn emits_expected_assembly_for_if() {
        assert_eq!(
            compile_expression_program("{ if (0) return 2; return 3; }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $0, %rsp\n",
                "  mov $0, %rax\n",
                "  cmp $0, %rax\n",
                "  je  .L.else.1\n",
                "  mov $2, %rax\n",
                "  jmp .L.return\n",
                "  jmp .L.end.1\n",
                ".L.else.1:\n",
                ".L.end.1:\n",
                "  mov $3, %rax\n",
                "  jmp .L.return\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_for() {
        assert_eq!(
            compile_expression_program("{ for (;;) return 3; }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $0, %rsp\n",
                ".L.begin.1:\n",
                "  mov $3, %rax\n",
                "  jmp .L.return\n",
                "  jmp .L.begin.1\n",
                ".L.end.1:\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_while() {
        assert_eq!(
            compile_expression_program("{ while (0) return 3; }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $0, %rsp\n",
                ".L.begin.1:\n",
                "  mov $0, %rax\n",
                "  cmp $0, %rax\n",
                "  je  .L.end.1\n",
                "  mov $3, %rax\n",
                "  jmp .L.return\n",
                "  jmp .L.begin.1\n",
                ".L.end.1:\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_address_and_deref() {
        assert_eq!(
            compile_expression_program("{ int x=3; return *&x; }").unwrap(),
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
                "  jmp .L.return\n",
                ".L.return:\n",
                "  mov %rbp, %rsp\n",
                "  pop %rbp\n",
                "  ret\n",
            )
        );
    }

    #[test]
    fn emits_expected_assembly_for_funcall() {
        assert_eq!(
            compile_expression_program("{ return ret3(); }").unwrap(),
            concat!(
                "  .globl main\n",
                "main:\n",
                "  push %rbp\n",
                "  mov %rsp, %rbp\n",
                "  sub $0, %rsp\n",
                "  mov $0, %rax\n",
                "  call ret3\n",
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
                Token::ident(1, "foo123"),
                Token::punct(7, "="),
                Token::num(8, 3),
                Token::punct(9, ";"),
                Token::ident(11, "bar"),
                Token::punct(14, "="),
                Token::num(15, 5),
                Token::punct(16, ";"),
                Token::ident(18, "foo123"),
                Token::punct(24, "+"),
                Token::ident(25, "bar"),
                Token::punct(28, ";"),
                Token::eof(29),
            ]
        );
    }

    #[test]
    fn tokenizes_keywords() {
        assert_eq!(
            tokenize("if else for while return int foo;").unwrap(),
            vec![
                Token::keyword(0, Keyword::If),
                Token::keyword(3, Keyword::Else),
                Token::keyword(8, Keyword::For),
                Token::keyword(12, Keyword::While),
                Token::keyword(18, Keyword::Return),
                Token::keyword(25, Keyword::Int),
                Token::ident(29, "foo"),
                Token::punct(32, ";"),
                Token::eof(33),
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
    fn rejects_undefined_variables() {
        let error = compile_expression_program("{ return a; }").unwrap_err();
        assert_eq!(error, "{ return a; }\n         ^ undefined variable");
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

        let asm = compile_expression_program("{ ;;; return 5; }").unwrap();
        assert_eq!(eval_with_cc(&asm), 5);
    }

    #[test]
    fn evaluates_assignments() {
        for (input, expected) in [
            ("{ int a; a=3; a; }", 3),
            ("{ int a=3; a; }", 3),
            ("{ int a=3; int z=5; a+z; }", 8),
            ("{ int a; int b; a=b=3; a+b; }", 6),
            ("{ int foo=3; foo; }", 3),
            ("{ int foo123=3; int bar=5; foo123+bar; }", 8),
            ("{ int x, y; x=3; y=5; x+y; }", 8),
            ("{ int x=3, y=5; x+y; }", 8),
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
            ("{ int a=3; return a; }", 3),
            ("{ int a=3; int z=5; return a+z; }", 8),
            ("{ return 1; 2; 3; }", 1),
            ("{ 1; return 2; 3; }", 2),
            ("{ 1; 2; return 3; }", 3),
            ("{ {1; {2;} return 3;} }", 3),
            ("{ ;;; return 5; }", 5),
            ("{ if (0) return 2; return 3; }", 3),
            ("{ if (1-1) return 2; return 3; }", 3),
            ("{ if (1) return 2; return 3; }", 2),
            ("{ if (2-1) return 2; return 3; }", 2),
            ("{ if (0) { 1; 2; return 3; } else { return 4; } }", 4),
            ("{ if (1) { 1; 2; return 3; } else { return 4; } }", 3),
            (
                "{ int i=0; int j=0; for (i=0; i<=10; i=i+1) j=i+j; return j; }",
                55,
            ),
            ("{ for (;;) return 3; return 5; }", 3),
            ("{ int i=0; while(i<10) i=i+1; return i; }", 10),
            ("{ int x=3; return *&x; }", 3),
            ("{ int x=3; int *y=&x; int **z=&y; return **z; }", 3),
            ("{ int x=3; int y=5; return *(&x+1); }", 5),
            ("{ int x=3; int y=5; return *(&y-1); }", 3),
            ("{ int x=3; int y=5; return *(&x-(-1)); }", 5),
            ("{ int x=3; int *y=&x; *y=5; return x; }", 5),
            ("{ int x=3; int y=5; *(&x+1)=7; return y; }", 7),
            ("{ int x=3; int y=5; *(&y-2+1)=7; return x; }", 7),
            ("{ int x=3; return (&x+2)-&x+3; }", 5),
            (
                "{ int i=0; int j=0; while(i<=10) {j=i+j; i=i+1;} return j; }",
                55,
            ),
            ("{ return ret3(); }", 3),
            ("{ return ret5(); }", 5),
        ] {
            let asm = compile_expression_program(input).unwrap();
            assert_eq!(eval_with_cc(&asm), expected, "{input}");
        }
    }

    #[test]
    fn rejects_non_lvalues_on_assignment() {
        let error = compile_expression_program("{ 1=2; }").unwrap_err();
        assert_eq!(error, "{ 1=2; }\n  ^ not an lvalue");
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
        let helper_c_path = dir.join("helpers.c");
        let helper_o_path = dir.join("helpers.o");
        fs::write(&asm_path, assembly).unwrap();
        fs::write(
            &helper_c_path,
            "int ret3() { return 3; }\nint ret5() { return 5; }\n",
        )
        .unwrap();

        let status = Command::new("cc")
            .arg("-c")
            .arg("-o")
            .arg(&helper_o_path)
            .arg(&helper_c_path)
            .status()
            .unwrap();
        assert!(status.success());

        let status = Command::new("cc")
            .arg("-o")
            .arg(&exe_path)
            .arg(&asm_path)
            .arg(&helper_o_path)
            .status()
            .unwrap();
        assert!(status.success());

        let status = Command::new(&exe_path).status().unwrap();
        fs::remove_dir_all(dir).unwrap();
        status.code().unwrap()
    }
}
