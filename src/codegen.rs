//! Assembly generation from the AST.

use crate::ast::{BinaryOp, Node, Stmt};

/// Emit assembly for a statement.
pub(crate) fn gen_stmt(stmt: &Stmt, assembly: &mut String, depth: &mut i32) -> Result<(), String> {
    match stmt {
        Stmt::Expr(expr) => gen_expr(expr, assembly, depth),
    }
}

/// Emit the address of an lvalue expression into `%rax`.
fn gen_addr(node: &Node, assembly: &mut String) -> Result<(), String> {
    match node {
        Node::Var(name) => {
            let offset = (name.to_digit(36).unwrap() - 9) * 8;
            assembly.push_str(&format!("  lea -{offset}(%rbp), %rax\n"));
            Ok(())
        }
        _ => Err("not an lvalue".to_owned()),
    }
}

/// Emit assembly for the given expression node.
pub(crate) fn gen_expr(node: &Node, assembly: &mut String, depth: &mut i32) -> Result<(), String> {
    match node {
        Node::Num(value) => {
            assembly.push_str(&format!("  mov ${value}, %rax\n"));
        }
        Node::Neg(expr) => {
            gen_expr(expr, assembly, depth)?;
            assembly.push_str("  neg %rax\n");
        }
        Node::Var(_) => {
            gen_addr(node, assembly)?;
            assembly.push_str("  mov (%rax), %rax\n");
        }
        Node::Assign { lhs, rhs } => {
            gen_addr(lhs, assembly)?;
            push(assembly, depth);
            gen_expr(rhs, assembly, depth)?;
            pop("%rdi", assembly, depth);
            assembly.push_str("  mov %rax, (%rdi)\n");
        }
        Node::Binary { op, lhs, rhs } => {
            gen_expr(rhs, assembly, depth)?;
            push(assembly, depth);
            gen_expr(lhs, assembly, depth)?;
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

    Ok(())
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
