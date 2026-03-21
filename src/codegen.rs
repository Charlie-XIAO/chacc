//! Assembly generation from the AST.

use crate::ast::{BinaryOp, Node, Stmt};

/// Emit assembly for a statement.
pub(crate) fn gen_stmt(stmt: &Stmt, assembly: &mut String, depth: &mut i32) {
    match stmt {
        Stmt::Expr(expr) => gen_expr(expr, assembly, depth),
    }
}

/// Emit assembly for the given expression node.
pub(crate) fn gen_expr(node: &Node, assembly: &mut String, depth: &mut i32) {
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
