//! Assembly generation from the AST.

use crate::ast::{BinaryOp, LocalVar, Node, NodeKind, Stmt, StmtKind};
use crate::tokenize::format_error_at;

/// Stateful code generator for a single function body.
pub(crate) struct Codegen<'a> {
    input: &'a str,
    assembly: String,
    depth: i32,
    locals: Vec<LocalVar>,
    next_label: usize,
}

impl<'a> Codegen<'a> {
    /// Create a code generator with the standard function prologue.
    pub(crate) fn new(input: &'a str, mut locals: Vec<LocalVar>) -> Self {
        let stack_size = assign_lvar_offsets(&mut locals);
        let mut assembly = String::new();
        assembly.push_str("  .globl main\n");
        assembly.push_str("main:\n");
        assembly.push_str("  push %rbp\n");
        assembly.push_str("  mov %rsp, %rbp\n");
        assembly.push_str(&format!("  sub ${stack_size}, %rsp\n"));

        Self {
            input,
            assembly,
            depth: 0,
            locals,
            next_label: 1,
        }
    }

    /// Emit a statement.
    pub(crate) fn gen_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match &stmt.kind {
            StmtKind::Expr(expr) => self.gen_expr(expr),
            StmtKind::Return(expr) => {
                self.gen_expr(expr)?;
                self.assembly.push_str("  jmp .L.return\n");
                Ok(())
            },
            StmtKind::For {
                init,
                cond,
                inc,
                body,
            } => {
                let label = self.take_label();
                if let Some(init) = init {
                    self.gen_stmt(init)?;
                }
                self.assembly.push_str(&format!(".L.begin.{label}:\n"));
                if let Some(cond) = cond {
                    self.gen_expr(cond)?;
                    self.assembly.push_str("  cmp $0, %rax\n");
                    self.assembly.push_str(&format!("  je  .L.end.{label}\n"));
                }
                self.gen_stmt(body)?;
                if let Some(inc) = inc {
                    self.gen_expr(inc)?;
                }
                self.assembly.push_str(&format!("  jmp .L.begin.{label}\n"));
                self.assembly.push_str(&format!(".L.end.{label}:\n"));
                Ok(())
            },
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let label = self.take_label();
                self.gen_expr(cond)?;
                self.assembly.push_str("  cmp $0, %rax\n");
                self.assembly.push_str(&format!("  je  .L.else.{label}\n"));
                self.gen_stmt(then_branch)?;
                self.assembly.push_str(&format!("  jmp .L.end.{label}\n"));
                self.assembly.push_str(&format!(".L.else.{label}:\n"));
                if let Some(else_branch) = else_branch {
                    self.gen_stmt(else_branch)?;
                }
                self.assembly.push_str(&format!(".L.end.{label}:\n"));
                Ok(())
            },
            StmtKind::Block(stmts) => {
                for stmt in stmts {
                    self.gen_stmt(stmt)?;
                }
                Ok(())
            },
        }
    }

    /// Check that the temporary expression stack is balanced.
    pub(crate) fn assert_balanced(&self) {
        assert_eq!(self.depth, 0);
    }

    /// Finish code generation and return the final assembly.
    pub(crate) fn finish(mut self) -> String {
        self.assembly.push_str(".L.return:\n");
        self.assembly.push_str("  mov %rbp, %rsp\n");
        self.assembly.push_str("  pop %rbp\n");
        self.assembly.push_str("  ret\n");
        self.assembly
    }

    /// Emit the address of an lvalue expression into `%rax`.
    fn gen_addr(&mut self, node: &Node) -> Result<(), String> {
        match &node.kind {
            NodeKind::Var(local_id) => {
                let offset = self.locals[*local_id].offset;
                self.assembly
                    .push_str(&format!("  lea {offset}(%rbp), %rax\n"));
                Ok(())
            },
            _ => Err(format_error_at(self.input, node.offset, "not an lvalue")),
        }
    }

    /// Emit assembly for the given expression node.
    fn gen_expr(&mut self, node: &Node) -> Result<(), String> {
        match &node.kind {
            NodeKind::Num(value) => {
                self.assembly.push_str(&format!("  mov ${value}, %rax\n"));
            },
            NodeKind::Neg(expr) => {
                self.gen_expr(expr)?;
                self.assembly.push_str("  neg %rax\n");
            },
            NodeKind::Var(_) => {
                self.gen_addr(node)?;
                self.assembly.push_str("  mov (%rax), %rax\n");
            },
            NodeKind::Assign { lhs, rhs } => {
                self.gen_addr(lhs)?;
                self.push();
                self.gen_expr(rhs)?;
                self.pop("%rdi");
                self.assembly.push_str("  mov %rax, (%rdi)\n");
            },
            NodeKind::Binary { op, lhs, rhs } => {
                self.gen_expr(rhs)?;
                self.push();
                self.gen_expr(lhs)?;
                self.pop("%rdi");

                match op {
                    BinaryOp::Add => self.assembly.push_str("  add %rdi, %rax\n"),
                    BinaryOp::Sub => self.assembly.push_str("  sub %rdi, %rax\n"),
                    BinaryOp::Mul => self.assembly.push_str("  imul %rdi, %rax\n"),
                    BinaryOp::Div => {
                        self.assembly.push_str("  cqo\n");
                        self.assembly.push_str("  idiv %rdi\n");
                    },
                    BinaryOp::Eq => {
                        self.assembly.push_str("  cmp %rdi, %rax\n");
                        self.assembly.push_str("  sete %al\n");
                        self.assembly.push_str("  movzb %al, %rax\n");
                    },
                    BinaryOp::Ne => {
                        self.assembly.push_str("  cmp %rdi, %rax\n");
                        self.assembly.push_str("  setne %al\n");
                        self.assembly.push_str("  movzb %al, %rax\n");
                    },
                    BinaryOp::Lt => {
                        self.assembly.push_str("  cmp %rdi, %rax\n");
                        self.assembly.push_str("  setl %al\n");
                        self.assembly.push_str("  movzb %al, %rax\n");
                    },
                    BinaryOp::Le => {
                        self.assembly.push_str("  cmp %rdi, %rax\n");
                        self.assembly.push_str("  setle %al\n");
                        self.assembly.push_str("  movzb %al, %rax\n");
                    },
                }
            },
        }

        Ok(())
    }

    /// Push `%rax` onto the temporary expression stack.
    fn push(&mut self) {
        self.assembly.push_str("  push %rax\n");
        self.depth += 1;
    }

    /// Pop the top of the temporary stack into a register.
    fn pop(&mut self, register: &str) {
        self.assembly.push_str(&format!("  pop {register}\n"));
        self.depth -= 1;
    }

    /// Allocate a fresh numeric suffix for local labels.
    fn take_label(&mut self) -> usize {
        let label = self.next_label;
        self.next_label += 1;
        label
    }
}

/// Assign stack offsets to locals and return the aligned stack size.
fn assign_lvar_offsets(locals: &mut [LocalVar]) -> i32 {
    let mut offset = 0;

    // The first parsed local stays closest to `%rbp`
    for local in locals.iter_mut().rev() {
        offset += 8;
        local.offset = -offset;
    }

    align_to(offset, 16)
}

/// Round `n` up to the nearest multiple of `align`.
fn align_to(n: i32, align: i32) -> i32 {
    (n + align - 1) / align * align
}
