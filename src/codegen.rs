//! Generate x86-64 assembly from an AST.

use std::fmt::Write as _;

use crate::ast::{
    BinaryOp, Function, GlobalVar, LocalVar, Node, NodeKind, Program, Stmt, StmtKind, VarRef,
};
use crate::source::Source;
use crate::types::Type;

const ARGREG8: [&str; 6] = ["%dil", "%sil", "%dl", "%cl", "%r8b", "%r9b"];
const ARGREG64: [&str; 6] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

/// A wrapper around [`writeln!`] that panics on error.
///
/// Since we are mostly writing to a string in this module, the writing is
/// mostly infallible unless we run out of memory.
macro_rules! emitln {
    ($dst:expr $(,)?) => {
        ::core::writeln!($dst).expect("failed to write")
    };
    ($dst:expr, $($arg:tt)*) => {
        ::core::writeln!($dst, $($arg)*).expect("failed to write")
    };
}

/// A state snapshot when generating a function.
struct FunctionState {
    name: String,
    locals: Vec<LocalVar>,
    depth: usize,
}

/// A x86-64 assembly code generator.
pub struct Codegen<'a> {
    source: &'a Source,
    asm: String,
    globals: Vec<GlobalVar>,
    next_label: usize,
    function: Option<FunctionState>,
}

impl<'a> Codegen<'a> {
    /// Create a code generator from source.
    pub fn new(source: &'a Source) -> Self {
        Self {
            source,
            asm: String::new(),
            globals: Vec::new(),
            next_label: 1,
            function: None,
        }
    }

    /// Get a reference of function state, expecting it to be set.
    fn function(&self) -> &FunctionState {
        self.function
            .as_ref()
            .expect("codegen is in a broken state: no function state")
    }

    /// Get a mutable reference of function state, expecting it to be set.
    fn function_mut(&mut self) -> &mut FunctionState {
        self.function
            .as_mut()
            .expect("codegen is in a broken state: no function state")
    }

    /// Generate assembly for an entire [`Program`].
    pub fn generate(mut self, program: Program) -> Result<String, String> {
        let Program { functions, globals } = program;

        self.globals = globals;
        self.gen_globals();

        for function in functions {
            self.gen_function(function)?;
        }

        Ok(self.asm)
    }

    /// Generate assembly for global variables.
    fn gen_globals(&mut self) {
        for global in &self.globals {
            emitln!(self.asm, "  .data");
            emitln!(self.asm, "  .globl {}", global.name);
            emitln!(self.asm, "{}:", global.name);

            if let Some(init_data) = &global.init_data {
                for byte in init_data.iter() {
                    emitln!(self.asm, "  .byte {byte}");
                }
            } else {
                emitln!(self.asm, "  .zero {}", global.ty.size());
            }
        }
    }

    /// Generate assembly for a function.
    fn gen_function(&mut self, function: Function) -> Result<(), String> {
        let Function {
            name,
            params,
            body,
            mut locals,
        } = function;

        let stack_size = assign_lvar_offsets(&mut locals);
        emitln!(self.asm, "  .globl {name}");
        emitln!(self.asm, "  .text");
        emitln!(self.asm, "{name}:");
        emitln!(self.asm, "  push %rbp");
        emitln!(self.asm, "  mov %rsp, %rbp");
        emitln!(self.asm, "  sub ${stack_size}, %rsp");

        for (i, param_id) in params.iter().enumerate() {
            let local = &locals[*param_id];
            let reg = if local.ty.size() == 1 {
                ARGREG8[i]
            } else {
                ARGREG64[i]
            };
            emitln!(self.asm, "  mov {reg}, {}(%rbp)", local.offset);
        }

        self.function = Some(FunctionState {
            name,
            locals,
            depth: 0,
        });

        self.gen_stmt(&body)?;
        assert_eq!(self.function().depth, 0);

        emitln!(self.asm, ".L.return.{}:", self.function().name.clone());
        emitln!(self.asm, "  mov %rbp, %rsp");
        emitln!(self.asm, "  pop %rbp");
        emitln!(self.asm, "  ret");

        self.function = None;
        Ok(())
    }

    /// Generate assembly for a statement.
    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match &stmt.kind {
            StmtKind::Expr(expr) => self.gen_expr(expr),
            StmtKind::Return(expr) => {
                self.gen_expr(expr)?;
                emitln!(self.asm, "  jmp .L.return.{}", self.function().name.clone());
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
                emitln!(self.asm, ".L.begin.{label}:");
                if let Some(cond) = cond {
                    self.gen_expr(cond)?;
                    emitln!(self.asm, "  cmp $0, %rax");
                    emitln!(self.asm, "  je  .L.end.{label}");
                }
                self.gen_stmt(body)?;
                if let Some(inc) = inc {
                    self.gen_expr(inc)?;
                }
                emitln!(self.asm, "  jmp .L.begin.{label}");
                emitln!(self.asm, ".L.end.{label}:");
                Ok(())
            },
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let label = self.take_label();
                self.gen_expr(cond)?;
                emitln!(self.asm, "  cmp $0, %rax");
                emitln!(self.asm, "  je  .L.else.{label}");
                self.gen_stmt(then_branch)?;
                emitln!(self.asm, "  jmp .L.end.{label}");
                emitln!(self.asm, ".L.else.{label}:");
                if let Some(else_branch) = else_branch {
                    self.gen_stmt(else_branch)?;
                }
                emitln!(self.asm, ".L.end.{label}:");
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

    /// Emit the address of an lvalue expression into `%rax`.
    fn gen_addr(&mut self, node: &Node) -> Result<(), String> {
        match &node.kind {
            NodeKind::Var(var) => match var {
                VarRef::Local(local_id) => {
                    let offset = self.function().locals[*local_id].offset;
                    emitln!(self.asm, "  lea {offset}(%rbp), %rax");
                    Ok(())
                },
                VarRef::Global(global_id) => {
                    let name = &self.globals[*global_id].name;
                    emitln!(self.asm, "  lea {name}(%rip), %rax");
                    Ok(())
                },
            },
            NodeKind::Deref(expr) => self.gen_expr(expr),
            _ => Err(self.source.error_at(node.offset, "not an lvalue")),
        }
    }

    /// Emit assembly for the given expression node.
    fn gen_expr(&mut self, node: &Node) -> Result<(), String> {
        match &node.kind {
            NodeKind::Num(value) => {
                emitln!(self.asm, "  mov ${value}, %rax");
            },
            NodeKind::FuncCall { name, args } => {
                if args.len() > 6 {
                    let msg = format!("too many arguments: expected at most 6, got {}", args.len());
                    return Err(self.source.error_at(node.offset, &msg));
                }

                for arg in args {
                    self.gen_expr(arg)?;
                    self.push();
                }
                for register in ARGREG64.iter().take(args.len()).rev() {
                    self.pop(register);
                }

                emitln!(self.asm, "  mov $0, %rax");
                emitln!(self.asm, "  call {name}");
            },
            NodeKind::Addr(expr) => {
                self.gen_addr(expr)?;
            },
            NodeKind::Deref(expr) => {
                self.gen_expr(expr)?;
                self.load(node.expect_ty());
            },
            NodeKind::Neg(expr) => {
                self.gen_expr(expr)?;
                emitln!(self.asm, "  neg %rax");
            },
            NodeKind::Var(_) => {
                self.gen_addr(node)?;
                self.load(node.expect_ty());
            },
            NodeKind::Assign { lhs, rhs } => {
                self.gen_addr(lhs)?;
                self.push();
                self.gen_expr(rhs)?;
                self.store(lhs.expect_ty());
            },
            NodeKind::Binary { op, lhs, rhs } => {
                self.gen_expr(rhs)?;
                self.push();
                self.gen_expr(lhs)?;
                self.pop("%rdi");

                match op {
                    BinaryOp::Add => emitln!(self.asm, "  add %rdi, %rax"),
                    BinaryOp::Sub => emitln!(self.asm, "  sub %rdi, %rax"),
                    BinaryOp::Mul => emitln!(self.asm, "  imul %rdi, %rax"),
                    BinaryOp::Div => {
                        emitln!(self.asm, "  cqo");
                        emitln!(self.asm, "  idiv %rdi");
                    },
                    BinaryOp::Eq => {
                        emitln!(self.asm, "  cmp %rdi, %rax");
                        emitln!(self.asm, "  sete %al");
                        emitln!(self.asm, "  movzb %al, %rax");
                    },
                    BinaryOp::Ne => {
                        emitln!(self.asm, "  cmp %rdi, %rax");
                        emitln!(self.asm, "  setne %al");
                        emitln!(self.asm, "  movzb %al, %rax");
                    },
                    BinaryOp::Lt => {
                        emitln!(self.asm, "  cmp %rdi, %rax");
                        emitln!(self.asm, "  setl %al");
                        emitln!(self.asm, "  movzb %al, %rax");
                    },
                    BinaryOp::Le => {
                        emitln!(self.asm, "  cmp %rdi, %rax");
                        emitln!(self.asm, "  setle %al");
                        emitln!(self.asm, "  movzb %al, %rax");
                    },
                }
            },
            NodeKind::StmtExpr(body) => {
                for stmt in body {
                    self.gen_stmt(stmt)?;
                }
            },
        }

        Ok(())
    }

    /// Push `%rax` onto the temporary expression stack.
    fn push(&mut self) {
        emitln!(self.asm, "  push %rax");
        self.function_mut().depth += 1;
    }

    /// Load a value from where `%rax` is pointing to.
    ///
    /// In particular, if the node is an array, we do not attempt to load the
    /// value to the register because in general we cannot load an entire array
    /// into a register. Consequently, the result of an evaluation of an array
    /// becomes not the array itself but the address of the array, which is why
    /// "array is a pointer to its first element" in C.
    fn load(&mut self, ty: &Type) {
        if ty.is_array() {
            return;
        }

        if ty.size() == 1 {
            emitln!(self.asm, "  movsbq (%rax), %rax");
        } else {
            emitln!(self.asm, "  mov (%rax), %rax");
        }
    }

    /// Store `%rax` into the address on top of the temporary stack.
    fn store(&mut self, ty: &Type) {
        self.pop("%rdi");

        if ty.size() == 1 {
            emitln!(self.asm, "  mov %al, (%rdi)");
        } else {
            emitln!(self.asm, "  mov %rax, (%rdi)");
        }
    }

    /// Pop the top of the temporary stack into a register.
    fn pop(&mut self, register: &str) {
        emitln!(self.asm, "  pop {register}");
        self.function_mut().depth -= 1;
    }

    /// Allocate a fresh numeric suffix for local labels.
    fn take_label(&mut self) -> usize {
        let label = self.next_label;
        self.next_label += 1;
        label
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
