//! Generate x86-64 assembly from an AST.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use smol_str::{SmolStr, format_smolstr};

use crate::ast::{
    BinaryOp, EntityRef, Function, GlobalVar, LocalVar, Node, NodeKind, Program, Stmt, StmtKind,
};
use crate::error::Result;
use crate::source::Source;
use crate::types::{Type, TypeId};
use crate::utils::{MAX_FUNC_PARAMS, align_to};

const GP_ARG_REGS_8: [&str; MAX_FUNC_PARAMS] = ["%dil", "%sil", "%dl", "%cl", "%r8b", "%r9b"];
const GP_ARG_REGS_16: [&str; MAX_FUNC_PARAMS] = ["%di", "%si", "%dx", "%cx", "%r8w", "%r9w"];
const GP_ARG_REGS_32: [&str; MAX_FUNC_PARAMS] = ["%edi", "%esi", "%edx", "%ecx", "%r8d", "%r9d"];
const GP_ARG_REGS_64: [&str; MAX_FUNC_PARAMS] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

/// Width of an integer scalar used to select size-specific x86-64 operations.
#[derive(Clone, Copy)]
enum ScalarWidth {
    Byte,
    Word,
    Dword,
    Qword,
}

impl ScalarWidth {
    /// Convert a scalar size in bytes to its corresponding width.
    fn from_size(size: i64) -> Self {
        match size {
            1 => Self::Byte,
            2 => Self::Word,
            4 => Self::Dword,
            8 => Self::Qword,
            _ => unreachable!("unsupported scalar width: {size}"),
        }
    }

    /// Return the register width used for binary integer operations.
    ///
    /// char, short, and int are computed in 32-bit registers. long and pointers
    /// are computed in 64-bit registers.
    fn from_promoted_binary_type(ty: &Type) -> Self {
        if ty.base().is_some() || ty.size() == 8 {
            Self::Qword
        } else {
            Self::Dword
        }
    }

    /// Return the general-purpose argument register for this width at `index`.
    fn gp_arg_reg(&self, index: usize) -> &'static str {
        match self {
            Self::Byte => GP_ARG_REGS_8[index],
            Self::Word => GP_ARG_REGS_16[index],
            Self::Dword => GP_ARG_REGS_32[index],
            Self::Qword => GP_ARG_REGS_64[index],
        }
    }

    /// Return the accumulator register for this width.
    fn acc_reg(&self) -> &'static str {
        match self {
            Self::Byte => "%al",
            Self::Word => "%ax",
            Self::Dword => "%eax",
            Self::Qword => "%rax",
        }
    }

    /// Return the `%rdi`-family register for this width.
    fn rdi_reg(&self) -> &'static str {
        match self {
            Self::Byte => "%dil",
            Self::Word => "%di",
            Self::Dword => "%edi",
            Self::Qword => "%rdi",
        }
    }

    /// Return the `%rdx`-family register for this width.
    fn rdx_reg(&self) -> &'static str {
        match self {
            Self::Byte => "%dl",
            Self::Word => "%dx",
            Self::Dword => "%edx",
            Self::Qword => "%rdx",
        }
    }

    /// Return the mnemonic used to load a signed scalar of this width.
    fn signed_load_mnemonic(&self) -> &'static str {
        match self {
            Self::Byte => "movsbl",
            Self::Word => "movswl",
            Self::Dword => "movsxd",
            Self::Qword => "mov",
        }
    }

    /// Return the destination register used by a signed load of this width.
    fn signed_load_dest_reg(&self) -> &'static str {
        match self {
            Self::Byte | Self::Word => "%eax",
            Self::Dword | Self::Qword => "%rax",
        }
    }

    /// Return the sign-extension mnemonic used before signed division.
    fn signed_div_extend_mnemonic(&self) -> &'static str {
        match self {
            Self::Byte => "cbw",
            Self::Word => "cwd",
            Self::Dword => "cdq",
            Self::Qword => "cqo",
        }
    }
}

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
    function_names: Vec<SmolStr>,
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
            function_names: Vec::new(),
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

        writeln!(self.out, ".file 1 \"{}\"", self.source.file())?;

        self.function_names = functions
            .iter()
            .map(|function| function.name.clone())
            .collect();
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
            body,
            param_locals,
            mut locals,
            is_static,
            ..
        } = function;

        let Some(body) = body else {
            return Ok(());
        };

        let stack_size = assign_lvar_offsets(&mut locals);

        if is_static {
            writeln!(self.out, "  .local {name}")?;
        } else {
            writeln!(self.out, "  .globl {name}")?;
        }

        writeln!(self.out, "  .text")?;
        writeln!(self.out, "{name}:")?;
        writeln!(self.out, "  push %rbp")?;
        writeln!(self.out, "  mov %rsp, %rbp")?;
        writeln!(self.out, "  sub ${stack_size}, %rsp")?;

        for (i, param_id) in param_locals.iter().enumerate() {
            let local = &locals[*param_id];
            self.store_gp(i, local.offset, local.ty.size())?;
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
        writeln!(self.out, "  .loc 1 {}", self.source.line_no(stmt.offset))?;

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
                    self.cmp_zero(cond.expect_ty())?;
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
                self.cmp_zero(cond.expect_ty())?;
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

    /// Generate the address of an addressable expression into `%rax`.
    fn gen_addr(&mut self, node: &Node) -> Result<()> {
        match &node.kind {
            NodeKind::Entity(entity) => match entity {
                EntityRef::Local(local_id) => {
                    let offset = self.function().locals[*local_id].offset;
                    writeln!(self.out, "  lea {offset}(%rbp), %rax")?;
                    Ok(())
                },
                EntityRef::Global(global_id) => {
                    let name = &self.globals[*global_id].name;
                    writeln!(self.out, "  lea {name}(%rip), %rax")?;
                    Ok(())
                },
                EntityRef::Function(function_id) => {
                    let name = &self.function_names[*function_id];
                    writeln!(self.out, "  lea {name}(%rip), %rax")?;
                    Ok(())
                },
            },
            NodeKind::Deref(expr) => self.gen_expr(expr),
            NodeKind::Comma { lhs, rhs } => {
                self.gen_expr(lhs)?;
                self.gen_addr(rhs)?;
                Ok(())
            },
            NodeKind::Member { parent, member } => {
                self.gen_addr(parent)?;
                writeln!(self.out, "  add ${}, %rax", member.offset)?;
                Ok(())
            },
            _ => Err(self.source.error_at(node.offset, "not an lvalue")),
        }
    }

    /// Generate assembly for a type cast.
    fn gen_cast(&mut self, from: &Type, to: &Type) -> Result<()> {
        if to.is_void() {
            return Ok(());
        }

        if to.is_bool() {
            self.cmp_zero(from)?;
            writeln!(self.out, "  setne %al")?;
            writeln!(self.out, "  movzx %al, %eax")?;
            return Ok(());
        }

        let Ok(from) = TypeId::try_from(from) else {
            return Ok(());
        };
        let Ok(to) = TypeId::try_from(to) else {
            return Ok(());
        };

        use TypeId::*;

        match (from, to) {
            (I8, I64) => writeln!(self.out, "movsxd %eax, %rax")?,
            (I16, I8) => writeln!(self.out, "movsbl %al, %eax")?,
            (I16, I64) => writeln!(self.out, "movsxd %eax, %rax")?,
            (I32, I8) => writeln!(self.out, "movsbl %al, %eax")?,
            (I32, I16) => writeln!(self.out, "movswl %ax, %eax")?,
            (I32, I64) => writeln!(self.out, "movsxd %eax, %rax")?,
            (I64, I8) => writeln!(self.out, "movsbl %al, %eax")?,
            (I64, I16) => writeln!(self.out, "movswl %ax, %eax")?,
            _ => {},
        }
        Ok(())
    }

    /// Generate assembly for the given expression node.
    fn gen_expr(&mut self, node: &Node) -> Result<()> {
        writeln!(self.out, "  .loc 1 {}", self.source.line_no(node.offset))?;

        match &node.kind {
            NodeKind::Num(value) => {
                writeln!(self.out, "  mov ${value}, %rax")?;
            },
            NodeKind::FuncCall { name, args } => {
                if args.len() > MAX_FUNC_PARAMS {
                    return Err(self.source.error_at(
                        node.offset,
                        format_smolstr!("too many arguments; expected at most {MAX_FUNC_PARAMS}"),
                    ));
                }

                for arg in args {
                    self.gen_expr(arg)?;
                    self.push()?;
                }
                for register in GP_ARG_REGS_64.iter().take(args.len()).rev() {
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
            NodeKind::Not(expr) => {
                self.gen_expr(expr)?;
                self.cmp_zero(expr.expect_ty())?;
                writeln!(self.out, "  sete %al")?;
                writeln!(self.out, "  movzx %al, %rax")?;
            },
            NodeKind::BitNot(expr) => {
                self.gen_expr(expr)?;
                writeln!(self.out, "  not %rax")?;
            },
            NodeKind::Entity(_) | NodeKind::Member { .. } => {
                self.gen_addr(node)?;
                self.load(node.expect_ty())?;
            },
            NodeKind::Assign { lhs, rhs } => {
                self.gen_addr(lhs)?;
                self.push()?;
                self.gen_expr(rhs)?;
                self.store(lhs.expect_ty())?;
            },
            NodeKind::Comma { lhs, rhs } => {
                self.gen_expr(lhs)?;
                self.gen_expr(rhs)?;
            },
            NodeKind::Binary { op, lhs, rhs } => {
                self.gen_expr(rhs)?;
                self.push()?;
                self.gen_expr(lhs)?;
                self.pop("%rdi")?;

                let width = ScalarWidth::from_promoted_binary_type(lhs.expect_ty());
                let acc = width.acc_reg();
                let rdi = width.rdi_reg();

                match op {
                    BinaryOp::Add => writeln!(self.out, "  add {rdi}, {acc}")?,
                    BinaryOp::Sub => writeln!(self.out, "  sub {rdi}, {acc}")?,
                    BinaryOp::Mul => writeln!(self.out, "  imul {rdi}, {acc}")?,
                    BinaryOp::Div => {
                        writeln!(self.out, "  {}", width.signed_div_extend_mnemonic())?;
                        writeln!(self.out, "  idiv {rdi}")?;
                    },
                    BinaryOp::Mod => {
                        writeln!(self.out, "  {}", width.signed_div_extend_mnemonic())?;
                        writeln!(self.out, "  idiv {rdi}")?;
                        writeln!(self.out, "  mov {}, {}", width.rdx_reg(), acc)?;
                    },
                    BinaryOp::BitAnd => writeln!(self.out, "  and {rdi}, {acc}")?,
                    BinaryOp::BitOr => writeln!(self.out, "  or {rdi}, {acc}")?,
                    BinaryOp::BitXor => writeln!(self.out, "  xor {rdi}, {acc}")?,
                    BinaryOp::Eq => {
                        writeln!(self.out, "  cmp {rdi}, {acc}")?;
                        writeln!(self.out, "  sete %al")?;
                        writeln!(self.out, "  movzb %al, %rax")?;
                    },
                    BinaryOp::Ne => {
                        writeln!(self.out, "  cmp {rdi}, {acc}")?;
                        writeln!(self.out, "  setne %al")?;
                        writeln!(self.out, "  movzb %al, %rax")?;
                    },
                    BinaryOp::Lt => {
                        writeln!(self.out, "  cmp {rdi}, {acc}")?;
                        writeln!(self.out, "  setl %al")?;
                        writeln!(self.out, "  movzb %al, %rax")?;
                    },
                    BinaryOp::Le => {
                        writeln!(self.out, "  cmp {rdi}, {acc}")?;
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
            NodeKind::Cast(expr) => {
                self.gen_expr(expr)?;
                self.gen_cast(expr.expect_ty(), node.expect_ty())?;
            },
            NodeKind::Dummy => unreachable!(),
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
        if ty.is_array() || ty.is_func() || ty.as_struct_or_union().is_some() {
            return Ok(());
        }

        let width = ScalarWidth::from_size(ty.size());
        writeln!(
            self.out,
            "  {} (%rax), {}",
            width.signed_load_mnemonic(),
            width.signed_load_dest_reg()
        )?;
        Ok(())
    }

    /// Store `%rax` into the address on top of the temporary stack.
    fn store(&mut self, ty: &Type) -> Result<()> {
        self.pop("%rdi")?;

        if ty.as_struct_or_union().is_some() {
            for i in 0..ty.size() {
                writeln!(self.out, "  mov {i}(%rax), %r8b")?;
                writeln!(self.out, "  mov %r8b, {i}(%rdi)")?;
            }
            return Ok(());
        }

        let width = ScalarWidth::from_size(ty.size());
        writeln!(self.out, "  mov {}, (%rdi)", width.acc_reg())?;
        Ok(())
    }

    /// Pop the top of the temporary stack into a register.
    fn pop(&mut self, register: &str) -> Result<()> {
        writeln!(self.out, "  pop {register}")?;
        self.function_mut().depth -= 1;
        Ok(())
    }

    /// Store an incoming general-purpose argument register to its stack slot.
    fn store_gp(&mut self, r: usize, offset: i64, size: i64) -> Result<()> {
        let register = ScalarWidth::from_size(size).gp_arg_reg(r);
        writeln!(self.out, "  mov {register}, {offset}(%rbp)")?;
        Ok(())
    }

    /// Compare a scalar value against zero.
    fn cmp_zero(&mut self, ty: &Type) -> Result<()> {
        if ty.is_integer() && ty.size() <= 4 {
            writeln!(self.out, "  cmp $0, %eax")?;
        } else {
            writeln!(self.out, "  cmp $0, %rax")?;
        }
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
        offset = align_to(offset, local.ty.align());
        local.offset = -offset;
    }

    align_to(offset, 16)
}
