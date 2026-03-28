//! Generate x86-64 assembly from an AST.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use smol_str::SmolStr;

use crate::ast::{
    BinaryOp, Function, GlobalVar, LocalVar, Node, NodeKind, Program, Stmt, StmtKind, VarRef,
};
use crate::error::Result;
use crate::source::Source;
use crate::types::Type;

const ARGREG8: [&str; 6] = ["%dil", "%sil", "%dl", "%cl", "%r8b", "%r9b"];
const ARGREG64: [&str; 6] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

/// A state snapshot when generating a function.
struct FunctionState {
    name: SmolStr,
    locals: Vec<LocalVar>,
    depth: usize,
}

/// A x86-64 assembly code generator.
pub struct Codegen<'a> {
    source: &'a Source,
    out: BufWriter<File>,
    globals: Vec<GlobalVar>,
    next_label: usize,
    function: Option<FunctionState>,
}

impl<'a> Codegen<'a> {
    /// Create a code generator from source.
    pub fn new(source: &'a Source, output: &'a Path) -> Result<Self> {
        let out_file = File::create(output)?;
        let out = BufWriter::new(out_file);

        Ok(Self {
            source,
            out,
            globals: Vec::new(),
            next_label: 1,
            function: None,
        })
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
    pub fn generate(mut self, program: Program) -> Result<()> {
        let Program { functions, globals } = program;

        self.globals = globals;
        self.gen_globals()?;

        for function in functions {
            self.gen_function(function)?;
        }

        Ok(())
    }

    /// Generate assembly for global variables.
    fn gen_globals(&mut self) -> Result<()> {
        for global in &self.globals {
            writeln!(self.out, "  .data")?;
            writeln!(self.out, "  .globl {}", global.name)?;
            writeln!(self.out, "{}:", global.name)?;

            if let Some(init_data) = &global.init_data {
                for byte in init_data.iter() {
                    writeln!(self.out, "  .byte {byte}")?;
                }
            } else {
                writeln!(self.out, "  .zero {}", global.ty.size())?;
            }
        }
        Ok(())
    }

    /// Generate assembly for a function.
    fn gen_function(&mut self, function: Function) -> Result<()> {
        let Function {
            name,
            params,
            body,
            mut locals,
        } = function;

        let stack_size = assign_lvar_offsets(&mut locals);
        writeln!(self.out, "  .globl {name}")?;
        writeln!(self.out, "  .text")?;
        writeln!(self.out, "{name}:")?;
        writeln!(self.out, "  push %rbp")?;
        writeln!(self.out, "  mov %rsp, %rbp")?;
        writeln!(self.out, "  sub ${stack_size}, %rsp")?;

        for (i, param_id) in params.iter().enumerate() {
            let local = &locals[*param_id];
            let reg = if local.ty.size() == 1 {
                ARGREG8[i]
            } else {
                ARGREG64[i]
            };
            writeln!(self.out, "  mov {reg}, {}(%rbp)", local.offset)?;
        }

        self.function = Some(FunctionState {
            name,
            locals,
            depth: 0,
        });

        self.gen_stmt(&body)?;
        assert_eq!(self.function().depth, 0);

        writeln!(self.out, ".L.return.{}:", self.function().name.clone())?;
        writeln!(self.out, "  mov %rbp, %rsp")?;
        writeln!(self.out, "  pop %rbp")?;
        writeln!(self.out, "  ret")?;

        self.function = None;
        Ok(())
    }

    /// Generate assembly for a statement.
    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<()> {
        match &stmt.kind {
            StmtKind::Expr(expr) => self.gen_expr(expr),
            StmtKind::Return(expr) => {
                self.gen_expr(expr)?;
                writeln!(self.out, "  jmp .L.return.{}", self.function().name.clone())?;
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
                writeln!(self.out, ".L.begin.{label}:")?;
                if let Some(cond) = cond {
                    self.gen_expr(cond)?;
                    writeln!(self.out, "  cmp $0, %rax")?;
                    writeln!(self.out, "  je  .L.end.{label}")?;
                }
                self.gen_stmt(body)?;
                if let Some(inc) = inc {
                    self.gen_expr(inc)?;
                }
                writeln!(self.out, "  jmp .L.begin.{label}")?;
                writeln!(self.out, ".L.end.{label}:")?;
                Ok(())
            },
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let label = self.take_label();
                self.gen_expr(cond)?;
                writeln!(self.out, "  cmp $0, %rax")?;
                writeln!(self.out, "  je  .L.else.{label}")?;
                self.gen_stmt(then_branch)?;
                writeln!(self.out, "  jmp .L.end.{label}")?;
                writeln!(self.out, ".L.else.{label}:")?;
                if let Some(else_branch) = else_branch {
                    self.gen_stmt(else_branch)?;
                }
                writeln!(self.out, ".L.end.{label}:")?;
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
    fn gen_addr(&mut self, node: &Node) -> Result<()> {
        match &node.kind {
            NodeKind::Var(var) => match var {
                VarRef::Local(local_id) => {
                    let offset = self.function().locals[*local_id].offset;
                    writeln!(self.out, "  lea {offset}(%rbp), %rax")?;
                    Ok(())
                },
                VarRef::Global(global_id) => {
                    let name = &self.globals[*global_id].name;
                    writeln!(self.out, "  lea {name}(%rip), %rax")?;
                    Ok(())
                },
            },
            NodeKind::Deref(expr) => self.gen_expr(expr),
            _ => Err(self.source.error_at(node.offset, "not an lvalue")),
        }
    }

    /// Emit assembly for the given expression node.
    fn gen_expr(&mut self, node: &Node) -> Result<()> {
        match &node.kind {
            NodeKind::Num(value) => {
                writeln!(self.out, "  mov ${value}, %rax")?;
            },
            NodeKind::FuncCall { name, args } => {
                if args.len() > 6 {
                    let msg = format!("too many arguments: expected at most 6, got {}", args.len());
                    return Err(self.source.error_at(node.offset, &msg));
                }

                for arg in args {
                    self.gen_expr(arg)?;
                    self.push()?;
                }
                for register in ARGREG64.iter().take(args.len()).rev() {
                    self.pop(register)?;
                }

                writeln!(self.out, "  mov $0, %rax")?;
                writeln!(self.out, "  call {name}")?;
            },
            NodeKind::Addr(expr) => {
                self.gen_addr(expr)?;
            },
            NodeKind::Deref(expr) => {
                self.gen_expr(expr)?;
                self.load(node.expect_ty())?;
            },
            NodeKind::Neg(expr) => {
                self.gen_expr(expr)?;
                writeln!(self.out, "  neg %rax")?;
            },
            NodeKind::Var(_) => {
                self.gen_addr(node)?;
                self.load(node.expect_ty())?;
            },
            NodeKind::Assign { lhs, rhs } => {
                self.gen_addr(lhs)?;
                self.push()?;
                self.gen_expr(rhs)?;
                self.store(lhs.expect_ty())?;
            },
            NodeKind::Binary { op, lhs, rhs } => {
                self.gen_expr(rhs)?;
                self.push()?;
                self.gen_expr(lhs)?;
                self.pop("%rdi")?;

                match op {
                    BinaryOp::Add => writeln!(self.out, "  add %rdi, %rax")?,
                    BinaryOp::Sub => writeln!(self.out, "  sub %rdi, %rax")?,
                    BinaryOp::Mul => writeln!(self.out, "  imul %rdi, %rax")?,
                    BinaryOp::Div => {
                        writeln!(self.out, "  cqo")?;
                        writeln!(self.out, "  idiv %rdi")?;
                    },
                    BinaryOp::Eq => {
                        writeln!(self.out, "  cmp %rdi, %rax")?;
                        writeln!(self.out, "  sete %al")?;
                        writeln!(self.out, "  movzb %al, %rax")?;
                    },
                    BinaryOp::Ne => {
                        writeln!(self.out, "  cmp %rdi, %rax")?;
                        writeln!(self.out, "  setne %al")?;
                        writeln!(self.out, "  movzb %al, %rax")?;
                    },
                    BinaryOp::Lt => {
                        writeln!(self.out, "  cmp %rdi, %rax")?;
                        writeln!(self.out, "  setl %al")?;
                        writeln!(self.out, "  movzb %al, %rax")?;
                    },
                    BinaryOp::Le => {
                        writeln!(self.out, "  cmp %rdi, %rax")?;
                        writeln!(self.out, "  setle %al")?;
                        writeln!(self.out, "  movzb %al, %rax")?;
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
    fn push(&mut self) -> Result<()> {
        writeln!(self.out, "  push %rax")?;
        self.function_mut().depth += 1;
        Ok(())
    }

    /// Load a value from where `%rax` is pointing to.
    ///
    /// In particular, if the node is an array, we do not attempt to load the
    /// value to the register because in general we cannot load an entire array
    /// into a register. Consequently, the result of an evaluation of an array
    /// becomes not the array itself but the address of the array, which is why
    /// "array is a pointer to its first element" in C.
    fn load(&mut self, ty: &Type) -> Result<()> {
        if ty.is_array() {
            return Ok(());
        }

        if ty.size() == 1 {
            writeln!(self.out, "  movsbq (%rax), %rax")?;
        } else {
            writeln!(self.out, "  mov (%rax), %rax")?;
        }
        Ok(())
    }

    /// Store `%rax` into the address on top of the temporary stack.
    fn store(&mut self, ty: &Type) -> Result<()> {
        self.pop("%rdi")?;

        if ty.size() == 1 {
            writeln!(self.out, "  mov %al, (%rdi)")?;
        } else {
            writeln!(self.out, "  mov %rax, (%rdi)")?;
        }
        Ok(())
    }

    /// Pop the top of the temporary stack into a register.
    fn pop(&mut self, register: &str) -> Result<()> {
        writeln!(self.out, "  pop {register}")?;
        self.function_mut().depth -= 1;
        Ok(())
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
