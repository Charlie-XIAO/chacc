//! Generate x86-64 assembly from an AST.

use std::io::Write;

use smol_str::SmolStr;

use crate::ast::{
    BinaryOp, EntityRef, Function, GlobalInitData, GlobalStorage, GlobalVar, LocalVar, Node,
    NodeKind, Program, Stmt, StmtKind,
};
use crate::error::Result;
use crate::source::{SourceMap, SourceSpan};
use crate::types::{Type, TypeStore};
use crate::utils::{MAX_FP_ARG_REGS, MAX_GP_ARG_REGS, VA_AREA_SIZE, align_to};

const GP_ARG_REGS_8: [&str; MAX_GP_ARG_REGS] = ["%dil", "%sil", "%dl", "%cl", "%r8b", "%r9b"];
const GP_ARG_REGS_16: [&str; MAX_GP_ARG_REGS] = ["%di", "%si", "%dx", "%cx", "%r8w", "%r9w"];
const GP_ARG_REGS_32: [&str; MAX_GP_ARG_REGS] = ["%edi", "%esi", "%edx", "%ecx", "%r8d", "%r9d"];
const GP_ARG_REGS_64: [&str; MAX_GP_ARG_REGS] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

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
    fn from_size(size: u64) -> Self {
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
    fn from_promoted_binary_type(ty: Type, types: &TypeStore) -> Self {
        if types.base(ty).is_some() || types.size(ty) == 8 {
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

    /// Return the `%rcx`-family register for this width.
    fn rcx_reg(&self) -> &'static str {
        match self {
            Self::Byte => "%cl",
            Self::Word => "%cx",
            Self::Dword => "%ecx",
            Self::Qword => "%rcx",
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

    /// Return the mnemonic used to load an unsigned scalar of this width.
    fn unsigned_load_mnemonic(&self) -> &'static str {
        match self {
            Self::Byte => "movzbl",
            Self::Word => "movzwl",
            Self::Dword => "mov",
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

/// Subset of [`Function`] necessary for codegen.
struct FunctionData {
    name: SmolStr,
    is_defined: bool,
}

impl From<&Function> for FunctionData {
    fn from(function: &Function) -> Self {
        Self {
            name: function.name.clone(),
            is_defined: function.body.is_some(),
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
pub struct Codegen<'a, W: Write> {
    source_map: &'a SourceMap,
    out: &'a mut W,
    types: TypeStore,
    functions: Vec<FunctionData>,
    globals: Vec<GlobalVar>,
    next_label: usize,
    last_loc: Option<(usize, u32, u32)>,
    function: Option<FunctionState>,
}

impl<'a, W: Write> Codegen<'a, W> {
    /// Create a code generator from source.
    pub fn new(source_map: &'a SourceMap, out: &'a mut W) -> Result<Self> {
        Ok(Self {
            source_map,
            out,
            types: TypeStore::default(),
            functions: Vec::new(),
            globals: Vec::new(),
            next_label: 1,
            last_loc: None,
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
        let Program {
            types,
            functions,
            globals,
        } = program;

        for source in self.source_map.iter() {
            writeln!(self.out, "  .file {} \"{}\"", source.id + 1, source.name)?;
        }

        self.types = types;
        self.types.frozen = true;
        self.functions = functions.iter().map(Into::into).collect();
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
            let align = self.types.eff_align(global.align, global.ty);

            match &global.storage {
                GlobalStorage::Decl => {},
                GlobalStorage::Zero => {
                    if global.is_static {
                        writeln!(self.out, "  .local {}", global.name)?;
                    } else {
                        writeln!(self.out, "  .globl {}", global.name)?;
                    }
                    writeln!(self.out, "  .bss")?;
                    writeln!(self.out, "  .align {align}")?;
                    writeln!(self.out, "{}:", global.name)?;
                    writeln!(self.out, "  .zero {}", self.types.size(global.ty))?;
                },
                GlobalStorage::Data(GlobalInitData { bytes, relocations }) => {
                    if global.is_static {
                        writeln!(self.out, "  .local {}", global.name)?;
                    } else {
                        writeln!(self.out, "  .globl {}", global.name)?;
                    }
                    writeln!(self.out, "  .data")?;
                    writeln!(self.out, "  .align {align}")?;
                    writeln!(self.out, "{}:", global.name)?;

                    let mut pos = 0;
                    for reloc in relocations.iter() {
                        while pos < reloc.offset {
                            writeln!(self.out, "  .byte {}", bytes[pos])?;
                            pos += 1;
                        }
                        if reloc.addend == 0 {
                            writeln!(self.out, "  .quad {}", reloc.label)?;
                        } else {
                            writeln!(self.out, "  .quad {}{:+}", reloc.label, reloc.addend)?;
                        }
                        pos += 8;
                    }

                    while pos < bytes.len() {
                        writeln!(self.out, "  .byte {}", bytes[pos])?;
                        pos += 1;
                    }
                },
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
            va_area_local,
            mut locals,
            is_static,
            ..
        } = function;

        let Some(body) = body else {
            return Ok(());
        };

        let stack_size = self.assign_lvar_offsets(&mut locals, &param_locals);

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

        if let Some(va_area_id) = va_area_local {
            let mut gp = 0;
            let mut fp = 0;
            for param_id in param_locals.iter() {
                if locals[*param_id].ty.is_flonum() {
                    fp += 1;
                } else {
                    gp += 1;
                }
            }

            let offset = locals[va_area_id].offset;
            debug_assert_eq!(self.types.size(locals[va_area_id].ty), VA_AREA_SIZE as u64);

            // __va_elem
            writeln!(self.out, "  movl ${}, {offset}(%rbp)", gp * 8)?;
            writeln!(self.out, "  movl ${}, {}(%rbp)", fp * 8 + 48, offset + 4)?;
            writeln!(self.out, "  movq %rbp, {}(%rbp)", offset + 16)?;
            writeln!(self.out, "  addq ${}, {}(%rbp)", offset + 24, offset + 16)?;

            // __va_elem.reg_save_area
            writeln!(self.out, "  movq %rdi, {}(%rbp)", offset + 24)?;
            writeln!(self.out, "  movq %rsi, {}(%rbp)", offset + 32)?;
            writeln!(self.out, "  movq %rdx, {}(%rbp)", offset + 40)?;
            writeln!(self.out, "  movq %rcx, {}(%rbp)", offset + 48)?;
            writeln!(self.out, "  movq %r8, {}(%rbp)", offset + 56)?;
            writeln!(self.out, "  movq %r9, {}(%rbp)", offset + 64)?;
            writeln!(self.out, "  movsd %xmm0, {}(%rbp)", offset + 72)?;
            writeln!(self.out, "  movsd %xmm1, {}(%rbp)", offset + 80)?;
            writeln!(self.out, "  movsd %xmm2, {}(%rbp)", offset + 88)?;
            writeln!(self.out, "  movsd %xmm3, {}(%rbp)", offset + 96)?;
            writeln!(self.out, "  movsd %xmm4, {}(%rbp)", offset + 104)?;
            writeln!(self.out, "  movsd %xmm5, {}(%rbp)", offset + 112)?;
            writeln!(self.out, "  movsd %xmm6, {}(%rbp)", offset + 120)?;
            writeln!(self.out, "  movsd %xmm7, {}(%rbp)", offset + 128)?;
        }

        let mut gp = 0;
        let mut fp = 0;

        for param_id in param_locals {
            let local = &locals[param_id];
            if local.offset > 0 {
                continue;
            }
            if local.ty.is_flonum() {
                self.store_fp(fp, local.offset, local.ty)?;
                fp += 1;
            } else {
                self.store_gp(gp, local.offset, self.types.size(local.ty))?;
                gp += 1;
            }
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
        self.gen_loc(stmt.span)?;

        match &stmt.kind {
            StmtKind::Expr(expr) => self.gen_expr(expr)?,
            StmtKind::Return(expr) => {
                if let Some(expr) = expr {
                    self.gen_expr(expr)?;
                }
                writeln!(self.out, "  jmp .L.return.{}", self.function().name.clone())?;
            },
            StmtKind::Loop {
                init,
                cond,
                inc,
                body,
                brk_label,
                cont_label,
                do_while: false,
            } => {
                let label = self.take_label();
                if let Some(init) = init {
                    self.gen_stmt(init)?;
                }
                writeln!(self.out, ".L.begin.{label}:")?;
                if let Some(cond) = cond {
                    let ty = cond.expect_ty();
                    self.gen_expr(cond)?;
                    self.cmp_zero(ty)?;
                    self.jump_if_zero(ty, brk_label)?;
                }
                self.gen_stmt(body)?;
                writeln!(self.out, "{cont_label}:")?;
                if let Some(inc) = inc {
                    self.gen_expr(inc)?;
                }
                writeln!(self.out, "  jmp .L.begin.{label}")?;
                writeln!(self.out, "{brk_label}:")?;
            },
            StmtKind::Loop {
                init,
                cond,
                inc,
                body,
                brk_label,
                cont_label,
                do_while: true,
            } => {
                debug_assert!(init.is_none(), "do-while has no initialization statement");
                debug_assert!(inc.is_none(), "do-while has no loop increment");
                let cond = cond.as_ref().expect("do-while must have a condition");
                let ty = cond.expect_ty();
                let label = self.take_label();
                writeln!(self.out, ".L.begin.{label}:")?;
                self.gen_stmt(body)?;
                writeln!(self.out, "{cont_label}:")?;
                self.gen_expr(cond)?;
                self.cmp_zero(ty)?;
                self.jump_if_nonzero(ty, &format!(".L.begin.{label}"))?;
                writeln!(self.out, "{brk_label}:")?;
            },
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let ty = cond.expect_ty();
                let label = self.take_label();
                self.gen_expr(cond)?;
                self.cmp_zero(ty)?;
                self.jump_if_zero(ty, &format!(".L.else.{label}"))?;
                self.gen_stmt(then_branch)?;
                writeln!(self.out, "  jmp .L.end.{label}")?;
                writeln!(self.out, ".L.else.{label}:")?;
                if let Some(else_branch) = else_branch {
                    self.gen_stmt(else_branch)?;
                }
                writeln!(self.out, ".L.end.{label}:")?;
            },
            StmtKind::Switch {
                cond,
                body,
                cases,
                default,
                brk_label,
            } => {
                self.gen_expr(cond)?;
                let width = ScalarWidth::from_promoted_binary_type(cond.expect_ty(), &self.types);
                let acc = width.acc_reg();
                for (val, label) in cases {
                    writeln!(self.out, "  cmp ${}, {acc}", val.bits() as i64)?;
                    writeln!(self.out, "  je {label}")?;
                }
                if let Some(default) = default {
                    writeln!(self.out, "  jmp {default}")?;
                }
                writeln!(self.out, "  jmp {brk_label}")?;
                self.gen_stmt(body)?;
                writeln!(self.out, "{brk_label}:")?;
            },
            StmtKind::Block(stmts) => {
                for stmt in stmts {
                    self.gen_stmt(stmt)?;
                }
            },
            StmtKind::Jump { label, .. } => {
                let label = label.as_ref().expect("unresolved goto leaked to codegen");
                writeln!(self.out, "  jmp {label}")?;
            },
            StmtKind::Label { label, body, .. } => {
                writeln!(self.out, "{label}:")?;
                self.gen_stmt(body)?;
            },
            StmtKind::MemzeroLocal(local_id) => {
                let local = &self.function().locals[*local_id];
                let offset = local.offset;
                let size = self.types.size(local.ty);
                writeln!(self.out, "  lea {offset}(%rbp), %rdi")?;
                writeln!(self.out, "  mov ${size}, %ecx")?;
                writeln!(self.out, "  xor %eax, %eax")?;
                writeln!(self.out, "  rep stosb")?;
            },
        }

        Ok(())
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
                    let function = &self.functions[*function_id];
                    if function.is_defined {
                        writeln!(self.out, "  lea {}(%rip), %rax", function.name)?;
                    } else {
                        writeln!(self.out, "  mov {}@GOTPCREL(%rip), %rax", function.name)?;
                    }
                    Ok(())
                },
            },
            NodeKind::Deref(expr) => self.gen_expr(expr),
            NodeKind::Comma { lhs, rhs } => {
                self.gen_expr(lhs)?;
                self.gen_addr(rhs)?;
                Ok(())
            },
            NodeKind::StmtExpr(body) => {
                let Some((last, prefix)) = body.split_last() else {
                    return Err(self
                        .source_map
                        .error(node.span, "invalid use of void expression as lvalue"));
                };
                for stmt in prefix {
                    self.gen_stmt(stmt)?;
                }
                let StmtKind::Expr(expr) = &last.kind else {
                    return Err(self
                        .source_map
                        .error(node.span, "invalid use of void expression as lvalue"));
                };
                self.gen_addr(expr)
            },
            NodeKind::Member { parent, member } => {
                self.gen_addr(parent)?;
                writeln!(self.out, "  add ${}, %rax", member.offset)?;
                Ok(())
            },
            _ => Err(self.source_map.error(node.span, "not an lvalue")),
        }
    }

    /// Generate assembly for a type cast.
    fn gen_cast(&mut self, from: Type, to: Type) -> Result<()> {
        if to == Type::Void {
            return Ok(());
        }

        if to == Type::BOOL {
            self.cmp_zero(from)?;
            self.set_is_nonzero(from)?;
            writeln!(self.out, "  movzx %al, %eax")?;
            return Ok(());
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum CastTypeId {
            I8,
            U8,
            I16,
            U16,
            I32,
            U32,
            I64,
            U64,
            F32,
            F64,
        }

        use CastTypeId::*;

        impl TryFrom<Type> for CastTypeId {
            type Error = ();

            fn try_from(value: Type) -> Result<Self, Self::Error> {
                match value {
                    Type::CHAR => Ok(I8),
                    Type::UCHAR => Ok(U8),
                    Type::SHORT => Ok(I16),
                    Type::USHORT => Ok(U16),
                    Type::INT | Type::Enum => Ok(I32),
                    Type::UINT => Ok(U32),
                    Type::LONG => Ok(I64),
                    Type::ULONG => Ok(U64),
                    Type::FLOAT => Ok(F32),
                    Type::DOUBLE => Ok(F64),
                    _ => Err(()),
                }
            }
        }

        let Ok(from_) = CastTypeId::try_from(from) else {
            return Ok(());
        };
        let Ok(to_) = CastTypeId::try_from(to) else {
            return Ok(());
        };

        if from_ == to_ {
            return Ok(());
        }

        // Short-circuit for float-to-float cast
        match (from_, to_) {
            (F32, F64) => return Ok(writeln!(self.out, "  cvtss2sd %xmm0, %xmm0")?),
            (F64, F32) => return Ok(writeln!(self.out, "  cvtsd2ss %xmm0, %xmm0")?),
            _ => {},
        };

        // Cast float to integer
        if from.is_flonum() {
            let sz = fp_mnemonic_sz(from);
            match to_ {
                U32 | I64 | U64 => writeln!(self.out, "  cvtts{sz}2siq %xmm0, %rax")?,
                _ => writeln!(self.out, "  cvtts{sz}2sil %xmm0, %eax")?,
            }
        }

        // Cast integer to integer
        match (from_, to_) {
            (_, I8) => writeln!(self.out, "  movsbl %al, %eax")?,
            (_, U8) => writeln!(self.out, "  movzbl %al, %eax")?,
            (I8 | U8, I16) => {},
            (_, I16) => writeln!(self.out, "  movswl %ax, %eax")?,
            (U8, U16) => {},
            (_, U16) => writeln!(self.out, "  movzwl %ax, %eax")?,
            (I64 | U64 | F32 | F64, I64 | U64) => {},
            (U32, I64 | U64 | F32 | F64) => writeln!(self.out, "  mov %eax, %eax")?,
            (_, I64 | U64) => writeln!(self.out, "  movsxd %eax, %rax")?,
            _ => {},
        }

        // Cast integer to float
        if to.is_flonum() {
            let sz = fp_mnemonic_sz(to);
            match from_ {
                U64 => {
                    writeln!(self.out, "  test %rax, %rax")?;
                    writeln!(self.out, "  js 1f")?;
                    writeln!(self.out, "  pxor %xmm0, %xmm0")?;
                    writeln!(self.out, "  cvtsi2s{sz}q %rax, %xmm0")?;
                    writeln!(self.out, "  jmp 2f")?;
                    writeln!(self.out, "1:")?;
                    writeln!(self.out, "  mov %rax, %rdi")?;
                    writeln!(self.out, "  and $1, %eax")?;
                    writeln!(self.out, "  pxor %xmm0, %xmm0")?;
                    writeln!(self.out, "  shr %rdi")?;
                    writeln!(self.out, "  or %rax, %rdi")?;
                    writeln!(self.out, "  cvtsi2s{sz}q %rdi, %xmm0")?;
                    writeln!(self.out, "  adds{sz} %xmm0, %xmm0")?;
                    writeln!(self.out, "2:")?;
                },
                U32 | I64 => writeln!(self.out, "  cvtsi2s{sz}q %rax, %xmm0")?,
                _ => writeln!(self.out, "  cvtsi2s{sz}l %eax, %xmm0")?,
            }
        }

        Ok(())
    }

    /// Generate assembly for the given expression node.
    fn gen_expr(&mut self, node: &Node) -> Result<()> {
        self.gen_loc(node.span)?;

        match &node.kind {
            NodeKind::Num(value) => writeln!(self.out, "  mov ${value}, %rax")?,
            NodeKind::Flonum(value) => match node.expect_ty() {
                Type::FLOAT => {
                    writeln!(self.out, "  mov ${:#x}, %eax", (*value as f32).to_bits())?;
                    writeln!(self.out, "  movd %eax, %xmm0")?;
                },
                Type::DOUBLE => {
                    writeln!(self.out, "  movabs ${:#x}, %rax", value.to_bits())?;
                    writeln!(self.out, "  movq %rax, %xmm0")?;
                },
                _ => unreachable!(),
            },
            NodeKind::FuncCall { callee, args } => {
                // Mask that is true for arguments that need to be passed on
                // stack (versus in registers)
                let mut gp = 0;
                let mut fp = 0;
                let stack_mask = args
                    .iter()
                    .map(|arg| {
                        if arg.expect_ty().is_flonum() {
                            fp += 1;
                            fp > MAX_FP_ARG_REGS
                        } else {
                            gp += 1;
                            gp > MAX_GP_ARG_REGS
                        }
                    })
                    .collect::<Vec<_>>();

                // Ensure 16-byte alignment of the stack before the call by
                // reserving an extra slot if necessary (each slot is 8 bytes)
                let mut n_stack_slots = stack_mask.iter().filter(|stack| **stack).count();
                if !(self.function().depth + n_stack_slots).is_multiple_of(2) {
                    writeln!(self.out, "  sub $8, %rsp")?;
                    self.function_mut().depth += 1;
                    n_stack_slots += 1;
                }

                let mut push_args = |stack: bool| -> Result<()> {
                    for (arg, &pass_by_stack) in args.iter().zip(&stack_mask).rev() {
                        if pass_by_stack != stack {
                            continue;
                        }
                        self.gen_expr(arg)?;
                        if arg.expect_ty().is_flonum() {
                            self.pushf()?;
                        } else {
                            self.push()?;
                        }
                    }
                    Ok(())
                };

                // Stack arguments are pushed first and left there until end of
                // call; register arguments are then pushed temporarily, but
                // will be popped before call later
                push_args(true)?;
                push_args(false)?;

                self.gen_expr(callee)?;

                gp = 0;
                fp = 0;
                for (arg, &pass_by_stack) in args.iter().zip(&stack_mask) {
                    if pass_by_stack {
                        continue;
                    }
                    if arg.expect_ty().is_flonum() {
                        self.popf(fp)?;
                        fp += 1;
                    } else {
                        self.pop(GP_ARG_REGS_64[gp])?;
                        gp += 1;
                    }
                }

                // Call the function pointer in %rax, but we have to first move
                // it elsewhere because %rax must carry the number of floating
                // point register arguments (for variadic calls)
                writeln!(self.out, "  mov %rax, %r10")?;
                writeln!(self.out, "  mov ${fp}, %rax")?;
                writeln!(self.out, "  call *%r10")?;

                // Clean up stack arguments if any
                if n_stack_slots > 0 {
                    writeln!(self.out, "  add ${}, %rsp", n_stack_slots * 8)?;
                    self.function_mut().depth -= n_stack_slots;
                }

                // Per x86-64 psABI, for "_Bool", "char", and "short" return
                // types, only the low 8 or 16 bits of %rax are guaranteed to
                // hold the correct value across a call; hence we need to
                // normalize the register to the declared return type here
                match node.expect_ty() {
                    Type::BOOL => writeln!(self.out, "  movzx %al, %eax")?,
                    Type::CHAR => writeln!(self.out, "  movsbl %al, %eax")?,
                    Type::UCHAR => writeln!(self.out, "  movzbl %al, %eax")?,
                    Type::SHORT => writeln!(self.out, "  movswl %ax, %eax")?,
                    Type::USHORT => writeln!(self.out, "  movzwl %ax, %eax")?,
                    _ => {},
                }
            },
            NodeKind::Addr(expr) => self.gen_addr(expr)?,
            NodeKind::Deref(expr) => {
                self.gen_expr(expr)?;
                self.load(node.expect_ty())?;
            },
            NodeKind::Neg(expr) => {
                self.gen_expr(expr)?;
                let ty = node.expect_ty();
                if ty.is_flonum() {
                    let sz = fp_mnemonic_sz(node.expect_ty());
                    writeln!(self.out, "  mov $1, %rax")?;
                    writeln!(self.out, "  shl ${}, %rax", self.types.size(ty) * 8 - 1)?;
                    writeln!(self.out, "  movq %rax, %xmm1")?;
                    writeln!(self.out, "  xorp{sz} %xmm1, %xmm0")?;
                } else {
                    writeln!(self.out, "  neg %rax")?;
                }
            },
            NodeKind::Not(expr) => {
                self.gen_expr(expr)?;
                let ty = expr.expect_ty();
                self.cmp_zero(ty)?;
                self.set_is_zero(ty)?;
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
            NodeKind::And { lhs, rhs } => {
                let lhs_ty = lhs.expect_ty();
                let rhs_ty = rhs.expect_ty();
                let label = self.take_label();
                self.gen_expr(lhs)?;
                self.cmp_zero(lhs_ty)?;
                self.jump_if_zero(lhs_ty, &format!(".L.false.{label}"))?;
                self.gen_expr(rhs)?;
                self.cmp_zero(rhs_ty)?;
                self.jump_if_zero(rhs_ty, &format!(".L.false.{label}"))?;
                writeln!(self.out, "  mov $1, %rax")?;
                writeln!(self.out, "  jmp .L.end.{label}")?;
                writeln!(self.out, ".L.false.{label}:")?;
                writeln!(self.out, "  mov $0, %rax")?;
                writeln!(self.out, ".L.end.{label}:")?;
            },
            NodeKind::Or { lhs, rhs } => {
                let lhs_ty = lhs.expect_ty();
                let rhs_ty = rhs.expect_ty();
                let label = self.take_label();
                self.gen_expr(lhs)?;
                self.cmp_zero(lhs_ty)?;
                self.jump_if_nonzero(lhs_ty, &format!(".L.true.{label}"))?;
                self.gen_expr(rhs)?;
                self.cmp_zero(rhs_ty)?;
                self.jump_if_nonzero(rhs_ty, &format!(".L.true.{label}"))?;
                writeln!(self.out, "  mov $0, %rax")?;
                writeln!(self.out, "  jmp .L.end.{label}")?;
                writeln!(self.out, ".L.true.{label}:")?;
                writeln!(self.out, "  mov $1, %rax")?;
                writeln!(self.out, ".L.end.{label}:")?;
            },
            NodeKind::Binary { op, lhs, rhs } => {
                let lhs_ty = lhs.expect_ty();

                if lhs_ty.is_flonum() {
                    self.gen_expr(rhs)?;
                    self.pushf()?;
                    self.gen_expr(lhs)?;
                    self.popf(1)?;

                    let sz = fp_mnemonic_sz(lhs_ty);

                    match op {
                        BinaryOp::Add => writeln!(self.out, "  adds{sz} %xmm1, %xmm0")?,
                        BinaryOp::Sub => writeln!(self.out, "  subs{sz} %xmm1, %xmm0")?,
                        BinaryOp::Mul => writeln!(self.out, "  muls{sz} %xmm1, %xmm0")?,
                        BinaryOp::Div => writeln!(self.out, "  divs{sz} %xmm1, %xmm0")?,
                        BinaryOp::Eq => {
                            writeln!(self.out, "  ucomis{sz} %xmm0, %xmm1")?;
                            writeln!(self.out, "  sete %al")?;
                            writeln!(self.out, "  setnp %dl")?;
                            writeln!(self.out, "  and %dl, %al")?;
                        },
                        BinaryOp::Ne => {
                            writeln!(self.out, "  ucomis{sz} %xmm0, %xmm1")?;
                            writeln!(self.out, "  setne %al")?;
                            writeln!(self.out, "  setp %dl")?;
                            writeln!(self.out, "  or %dl, %al")?;
                        },
                        BinaryOp::Lt => {
                            writeln!(self.out, "  ucomis{sz} %xmm0, %xmm1")?;
                            writeln!(self.out, "  seta %al")?;
                        },
                        BinaryOp::Le => {
                            writeln!(self.out, "  ucomis{sz} %xmm0, %xmm1")?;
                            writeln!(self.out, "  setae %al")?;
                        },
                        _ => {
                            return Err(self
                                .source_map
                                .error(node.span, "invalid operator for floating-point operands"));
                        },
                    }

                    writeln!(self.out, "  and $1, %al")?;
                    writeln!(self.out, "  movzb %al, %rax")?;
                } else {
                    self.gen_expr(rhs)?;
                    self.push()?;
                    self.gen_expr(lhs)?;
                    self.pop("%rdi")?;

                    let width = ScalarWidth::from_promoted_binary_type(lhs_ty, &self.types);
                    let acc = width.acc_reg();
                    let rdi = width.rdi_reg();
                    let rcx = width.rcx_reg();
                    let rdx = width.rdx_reg();

                    match op {
                        BinaryOp::Add => writeln!(self.out, "  add {rdi}, {acc}")?,
                        BinaryOp::Sub => writeln!(self.out, "  sub {rdi}, {acc}")?,
                        BinaryOp::Mul => writeln!(self.out, "  imul {rdi}, {acc}")?,
                        BinaryOp::Div | BinaryOp::Mod => {
                            let ty = node.expect_ty();
                            if ty.is_unsigned() || self.types.is_ptr(ty) {
                                writeln!(self.out, "  mov $0, {rdx}")?;
                                writeln!(self.out, "  div {rdi}")?;
                            } else {
                                writeln!(self.out, "  {}", width.signed_div_extend_mnemonic())?;
                                writeln!(self.out, "  idiv {rdi}")?;
                            }
                            if *op == BinaryOp::Mod {
                                writeln!(self.out, "  mov {}, {}", width.rdx_reg(), acc)?;
                            }
                        },
                        BinaryOp::BitAnd => writeln!(self.out, "  and {rdi}, {acc}")?,
                        BinaryOp::BitOr => writeln!(self.out, "  or {rdi}, {acc}")?,
                        BinaryOp::BitXor => writeln!(self.out, "  xor {rdi}, {acc}")?,
                        BinaryOp::BitShl => {
                            writeln!(self.out, "  mov {rdi}, {rcx}")?;
                            writeln!(self.out, "  shl %cl, {acc}")?;
                        },
                        BinaryOp::BitShr => {
                            writeln!(self.out, "  mov {rdi}, {rcx}")?;
                            if lhs_ty.is_unsigned() || self.types.is_ptr(lhs_ty) {
                                writeln!(self.out, "  shr %cl, {acc}")?;
                            } else {
                                writeln!(self.out, "  sar %cl, {acc}")?;
                            }
                        },
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
                            if lhs_ty.is_unsigned() || self.types.is_ptr(lhs_ty) {
                                writeln!(self.out, "  setb %al")?;
                            } else {
                                writeln!(self.out, "  setl %al")?;
                            }
                            writeln!(self.out, "  movzb %al, %rax")?;
                        },
                        BinaryOp::Le => {
                            writeln!(self.out, "  cmp {rdi}, {acc}")?;
                            if lhs_ty.is_unsigned() || self.types.is_ptr(lhs_ty) {
                                writeln!(self.out, "  setbe %al")?;
                            } else {
                                writeln!(self.out, "  setle %al")?;
                            }
                            writeln!(self.out, "  movzb %al, %rax")?;
                        },
                    }
                }
            },
            NodeKind::Conditional {
                cond,
                then_expr,
                else_expr,
            } => {
                let ty = cond.expect_ty();
                let label = self.take_label();
                self.gen_expr(cond)?;
                self.cmp_zero(ty)?;
                self.jump_if_zero(ty, &format!(".L.else.{label}"))?;
                self.gen_expr(then_expr)?;
                writeln!(self.out, "  jmp .L.end.{label}")?;
                writeln!(self.out, ".L.else.{label}:")?;
                self.gen_expr(else_expr)?;
                writeln!(self.out, ".L.end.{label}:")?;
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

    /// Generate a `.loc` directive if the source location changed.
    fn gen_loc(&mut self, span: SourceSpan) -> Result<()> {
        let loc = self.source_map.file_line_col(span);
        if self.last_loc == Some(loc) {
            return Ok(());
        }
        self.last_loc = Some(loc);
        writeln!(self.out, "  .loc {} {} {}", loc.0, loc.1, loc.2)?;
        Ok(())
    }

    /// Push `%rax` onto the temporary expression stack.
    fn push(&mut self) -> Result<()> {
        writeln!(self.out, "  push %rax")?;
        self.function_mut().depth += 1;
        Ok(())
    }

    /// Push `%xmm0` onto the temporary expression stack.
    fn pushf(&mut self) -> Result<()> {
        writeln!(self.out, "  sub $8, %rsp")?;
        writeln!(self.out, "  movsd %xmm0, (%rsp)")?;
        self.function_mut().depth += 1;
        Ok(())
    }

    /// Load a scalar value from where `%rax` points to.
    ///
    /// This does not attempt to load arrays, functions, and aggregates as
    /// scalar register values, so that they are left as addresses in `%rax`.
    fn load(&mut self, ty: Type) -> Result<()> {
        if self.types.as_array(ty).is_some()
            || self.types.is_func(ty)
            || self.types.as_struct_or_union(ty).is_some()
        {
            return Ok(());
        }

        if ty.is_flonum() {
            let sz = fp_mnemonic_sz(ty);
            writeln!(self.out, "  movs{sz} (%rax), %xmm0")?;
            return Ok(());
        }

        let width = ScalarWidth::from_size(self.types.size(ty));
        writeln!(
            self.out,
            "  {} (%rax), {}",
            if ty.is_unsigned() || self.types.is_ptr(ty) {
                width.unsigned_load_mnemonic()
            } else {
                width.signed_load_mnemonic()
            },
            width.signed_load_dest_reg()
        )?;
        Ok(())
    }

    /// Store `%rax` into the address on top of the temporary stack.
    fn store(&mut self, ty: Type) -> Result<()> {
        self.pop("%rdi")?;

        if self.types.as_struct_or_union(ty).is_some() {
            for i in 0..self.types.size(ty) {
                writeln!(self.out, "  mov {i}(%rax), %r8b")?;
                writeln!(self.out, "  mov %r8b, {i}(%rdi)")?;
            }
            return Ok(());
        }

        if ty.is_flonum() {
            let sz = fp_mnemonic_sz(ty);
            writeln!(self.out, "  movs{sz} %xmm0, (%rdi)")?;
            return Ok(());
        }

        let width = ScalarWidth::from_size(self.types.size(ty));
        writeln!(self.out, "  mov {}, (%rdi)", width.acc_reg())?;
        Ok(())
    }

    /// Pop the top of the temporary stack into a register.
    fn pop(&mut self, register: &str) -> Result<()> {
        writeln!(self.out, "  pop {register}")?;
        self.function_mut().depth -= 1;
        Ok(())
    }

    /// Pop the top of the temporary stack into an XMM register.
    fn popf(&mut self, register: usize) -> Result<()> {
        debug_assert!(
            register < MAX_FP_ARG_REGS,
            "invalid floating-point argument register index",
        );
        writeln!(self.out, "  movsd (%rsp), %xmm{register}")?;
        writeln!(self.out, "  add $8, %rsp")?;
        self.function_mut().depth -= 1;
        Ok(())
    }

    /// Store an incoming general-purpose argument register to its stack slot.
    fn store_gp(&mut self, r: usize, offset: i64, size: u64) -> Result<()> {
        let register = ScalarWidth::from_size(size).gp_arg_reg(r);
        writeln!(self.out, "  mov {register}, {offset}(%rbp)")?;
        Ok(())
    }

    /// Store an incoming floating-point argument register to its stack slot.
    fn store_fp(&mut self, r: usize, offset: i64, ty: Type) -> Result<()> {
        let sz = fp_mnemonic_sz(ty);
        writeln!(self.out, "  movs{sz} %xmm{r}, {offset}(%rbp)")?;
        Ok(())
    }

    /// Compare a scalar value against zero.
    fn cmp_zero(&mut self, ty: Type) -> Result<()> {
        if ty.is_flonum() {
            let sz = fp_mnemonic_sz(ty);
            writeln!(self.out, "  xorp{sz} %xmm1, %xmm1")?;
            writeln!(self.out, "  ucomis{sz} %xmm1, %xmm0")?;
            return Ok(());
        }

        if ty.is_integer() && self.types.size(ty) <= 4 {
            writeln!(self.out, "  cmp $0, %eax")?;
        } else {
            writeln!(self.out, "  cmp $0, %rax")?;
        }
        Ok(())
    }

    /// Set `%al` to 1 if the previously compared scalar is zero.
    fn set_is_zero(&mut self, ty: Type) -> Result<()> {
        writeln!(self.out, "  sete %al")?;
        if ty.is_flonum() {
            writeln!(self.out, "  setnp %dl")?;
            writeln!(self.out, "  and %dl, %al")?;
        }
        Ok(())
    }

    /// Set `%al` to 1 if the previously compared scalar is non-zero.
    fn set_is_nonzero(&mut self, ty: Type) -> Result<()> {
        writeln!(self.out, "  setne %al")?;
        if ty.is_flonum() {
            writeln!(self.out, "  setp %dl")?;
            writeln!(self.out, "  or %dl, %al")?;
        }
        Ok(())
    }

    /// Jump to the given label if the previously compared scalar is zero.
    fn jump_if_zero(&mut self, ty: Type, label: &str) -> Result<()> {
        if ty.is_flonum() {
            let skip = self.take_label();
            writeln!(self.out, "  jp .L.skip.{skip}")?;
            writeln!(self.out, "  je {label}")?;
            writeln!(self.out, ".L.skip.{skip}:")?;
        } else {
            writeln!(self.out, "  je {label}")?;
        }
        Ok(())
    }

    /// Jump to the given label if the previously compared scalar is non-zero.
    fn jump_if_nonzero(&mut self, ty: Type, label: &str) -> Result<()> {
        if ty.is_flonum() {
            writeln!(self.out, "  jne {label}")?;
            writeln!(self.out, "  jp {label}")?;
        } else {
            writeln!(self.out, "  jne {label}")?;
        }
        Ok(())
    }

    /// Allocate a fresh numeric suffix for local labels.
    fn take_label(&mut self) -> usize {
        let label = self.next_label;
        self.next_label += 1;
        label
    }

    /// Assign frame offsets to parameters and locals.
    ///
    /// Returns the total size of the stack frame in bytes, 16-byte aligned.
    fn assign_lvar_offsets(&self, locals: &mut [LocalVar], param_locals: &[usize]) -> u64 {
        let mut top = 16;
        let mut bottom = 0;

        let mut gp = 0;
        let mut fp = 0;

        // Assign offsets to pass-by-stack parameters
        for &param_id in param_locals {
            let local = &mut locals[param_id];
            if local.ty.is_flonum() {
                fp += 1;
                if fp <= MAX_FP_ARG_REGS {
                    continue;
                }
            } else {
                gp += 1;
                if gp <= MAX_GP_ARG_REGS {
                    continue;
                }
            }
            top = align_to(top, 8);
            local.offset = i64::try_from(top).expect("stack frame too large");
            top += self.types.size(local.ty);
        }

        // Assign offsets to register-passed parameters and local variables
        for local in locals.iter_mut().rev() {
            if local.offset != 0 {
                continue;
            }
            bottom += self.types.size(local.ty);
            bottom = align_to(bottom, self.types.eff_align(local.align, local.ty));
            let offset = i64::try_from(bottom).expect("stack frame too large");
            local.offset = -offset;
        }

        align_to(bottom, 16)
    }
}

fn fp_mnemonic_sz(ty: Type) -> &'static str {
    match ty {
        Type::FLOAT => "s",
        Type::DOUBLE => "d",
        _ => unreachable!(),
    }
}
