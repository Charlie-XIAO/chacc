//! Generate x86-64 assembly from an AST.

use crate::ast::{
    BinaryOp, Function, GlobalVar, LocalVar, Node, NodeKind, Program, Stmt, StmtKind, VarRef,
};
use crate::tokenize::format_error_at;

/// Registers used for passing integer arguments, in order.
const ARG_REGS: [&str; 6] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

/// Code generator for a single function body.
struct Codegen<'a> {
    input: &'a str,
    function_name: String,
    assembly: String,
    depth: i32,
    locals: Vec<LocalVar>,
    globals: &'a [GlobalVar],
    next_label: usize,
}

impl<'a> Codegen<'a> {
    /// Create a code generator with the standard function prologue.
    fn new(
        input: &'a str,
        function_name: String,
        params: &[usize],
        mut locals: Vec<LocalVar>,
        globals: &'a [GlobalVar],
    ) -> Self {
        let stack_size = assign_lvar_offsets(&mut locals);
        let mut assembly = String::new();
        assembly.push_str(&format!("  .globl {function_name}\n"));
        assembly.push_str("  .text\n");
        assembly.push_str(&format!("{function_name}:\n"));
        assembly.push_str("  push %rbp\n");
        assembly.push_str("  mov %rsp, %rbp\n");
        assembly.push_str(&format!("  sub ${stack_size}, %rsp\n"));

        for (i, param_id) in params.iter().enumerate() {
            let offset = locals[*param_id].offset;
            assembly.push_str(&format!("  mov {}, {offset}(%rbp)\n", ARG_REGS[i]));
        }

        Self {
            input,
            function_name,
            assembly,
            depth: 0,
            locals,
            globals,
            next_label: 1,
        }
    }

    /// Emit a statement.
    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match &stmt.kind {
            StmtKind::Expr(expr) => self.gen_expr(expr),
            StmtKind::Return(expr) => {
                self.gen_expr(expr)?;
                self.assembly
                    .push_str(&format!("  jmp .L.return.{}\n", self.function_name));
                Ok(())
            },
            StmtKind::Loop {
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

    /// Assert that the temporary expression stack is balanced.
    fn assert_balanced(&self) {
        assert_eq!(self.depth, 0);
    }

    /// Finish code generation and return the final assembly.
    fn finish(mut self) -> String {
        self.assembly
            .push_str(&format!(".L.return.{}:\n", self.function_name));
        self.assembly.push_str("  mov %rbp, %rsp\n");
        self.assembly.push_str("  pop %rbp\n");
        self.assembly.push_str("  ret\n");
        self.assembly
    }

    /// Emit the address of an lvalue expression into `%rax`.
    fn gen_addr(&mut self, node: &Node) -> Result<(), String> {
        match &node.kind {
            NodeKind::Var(var) => match var {
                VarRef::Local(local_id) => {
                    let offset = self.locals[*local_id].offset;
                    self.assembly
                        .push_str(&format!("  lea {offset}(%rbp), %rax\n"));
                    Ok(())
                },
                VarRef::Global(global_id) => {
                    let name = &self.globals[*global_id].name;
                    self.assembly
                        .push_str(&format!("  lea {name}(%rip), %rax\n"));
                    Ok(())
                },
            },
            NodeKind::Deref(expr) => self.gen_expr(expr),
            _ => Err(format_error_at(self.input, node.offset, "not an lvalue")),
        }
    }

    /// Emit assembly for the given expression node.
    fn gen_expr(&mut self, node: &Node) -> Result<(), String> {
        match &node.kind {
            NodeKind::Num(value) => {
                self.assembly.push_str(&format!("  mov ${value}, %rax\n"));
            },
            NodeKind::FuncCall { name, args } => {
                if args.len() > ARG_REGS.len() {
                    let msg = format!(
                        "too many arguments: expected at most {}, got {}",
                        ARG_REGS.len(),
                        args.len()
                    );
                    return Err(format_error_at(self.input, node.offset, &msg));
                }

                for arg in args {
                    self.gen_expr(arg)?;
                    self.push();
                }
                for register in ARG_REGS.iter().take(args.len()).rev() {
                    self.pop(register);
                }

                self.assembly.push_str("  mov $0, %rax\n");
                self.assembly.push_str(&format!("  call {name}\n"));
            },
            NodeKind::Addr(expr) => {
                self.gen_addr(expr)?;
            },
            NodeKind::Deref(expr) => {
                self.gen_expr(expr)?;
                self.load(node);
            },
            NodeKind::Neg(expr) => {
                self.gen_expr(expr)?;
                self.assembly.push_str("  neg %rax\n");
            },
            NodeKind::Var(_) => {
                self.gen_addr(node)?;
                self.load(node);
            },
            NodeKind::Assign { lhs, rhs } => {
                self.gen_addr(lhs)?;
                self.push();
                self.gen_expr(rhs)?;
                self.store();
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

    /// Load a value from where `%rax` is pointing to.
    ///
    /// In particular, if the node is an array, we do not attempt to load the
    /// value to the register because in general we cannot load an entire array
    /// into a register. Consequently, the result of an evaluation of an array
    /// becomes not the array itself but the address of the array, which is why
    /// "array is a pointer to its first element" in C.
    fn load(&mut self, node: &Node) {
        if node.ty.as_ref().is_some_and(|ty| ty.is_array()) {
            return;
        }
        self.assembly.push_str("  mov (%rax), %rax\n");
    }

    /// Store `%rax` into the address on top of the temporary stack.
    fn store(&mut self) {
        self.pop("%rdi");
        self.assembly.push_str("  mov %rax, (%rdi)\n");
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

/// Generate assembly for a full program.
pub fn codegen_program(input: &str, program: Program) -> Result<String, String> {
    let Program { functions, globals } = program;
    let mut assembly = String::new();

    emit_data(&mut assembly, &globals);

    for Function {
        name,
        params,
        body,
        locals,
    } in functions
    {
        let mut codegen = Codegen::new(input, name, &params, locals, &globals);
        codegen.gen_stmt(&body)?;
        codegen.assert_balanced();
        assembly.push_str(&codegen.finish());
    }

    Ok(assembly)
}

/// Emit assembly for global variables.
fn emit_data(assembly: &mut String, globals: &[GlobalVar]) {
    for global in globals {
        assembly.push_str("  .data\n");
        assembly.push_str(&format!("  .globl {}\n", global.name));
        assembly.push_str(&format!("{}:\n", global.name));
        assembly.push_str(&format!("  .zero {}\n", global.ty.size()));
    }
}

/// Assign stack offsets to locals and return the aligned stack size.
fn assign_lvar_offsets(locals: &mut [LocalVar]) -> i64 {
    let mut offset = 0;

    // The first parsed local stays closest to `%rbp`
    for local in locals.iter_mut().rev() {
        offset += local.ty.size();
        local.offset = -offset;
    }

    align_to(offset, 16)
}

/// Round `n` up to the nearest multiple of `align`, which must be a power of 2.
fn align_to(n: i64, align: i64) -> i64 {
    debug_assert!(
        align > 0 && (align & (align - 1)) == 0,
        "align must be a power of 2"
    );
    (n + align - 1) & !(align - 1)
}
