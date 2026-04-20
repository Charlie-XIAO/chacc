//! Generate x86-64 assembly from an AST.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use smol_str::{SmolStr, format_smolstr};

use crate::ast::{
    BinaryOp, EntityRef, Function, GlobalInitData, GlobalStorage, GlobalVar, LocalVar, Node,
    NodeKind, Program, Stmt, StmtKind,
};
use crate::error::Result;
use crate::source::Source;
use crate::types::{Type, TypeStore};
use crate::utils::{MAX_FUNC_PARAMS, VA_AREA_SIZE, align_to};

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
    types: TypeStore,
    function_names: Vec<SmolStr>,
    globals: Vec<GlobalVar>,
    next_label: usize,
    last_loc: Option<(u32, u32)>,
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
            types: TypeStore::default(),
            function_names: Vec::new(),
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

        writeln!(self.out, "  .file 1 \"{}\"", self.source.file())?;

        self.types = types;
        self.types.frozen = true;
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

        let stack_size = self.assign_lvar_offsets(&mut locals);

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
            let offset = locals[va_area_id].offset;
            let gp_offset = (param_locals.len() * 8) as i64;
            debug_assert_eq!(self.types.size(locals[va_area_id].ty), VA_AREA_SIZE as u64);

            // __va_elem
            writeln!(self.out, "  movl ${gp_offset}, {offset}(%rbp)")?;
            writeln!(self.out, "  movl $0, {}(%rbp)", offset + 4)?;
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

        for (i, param_id) in param_locals.iter().enumerate() {
            let local = &locals[*param_id];
            self.store_gp(i, local.offset, self.types.size(local.ty))?;
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
        self.gen_loc(stmt.offset)?;

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
            NodeKind::StmtExpr(body) => {
                let Some((last, prefix)) = body.split_last() else {
                    return Err(self
                        .source
                        .error_at(node.offset, "invalid use of void expression as lvalue"));
                };
                for stmt in prefix {
                    self.gen_stmt(stmt)?;
                }
                let StmtKind::Expr(expr) = &last.kind else {
                    return Err(self
                        .source
                        .error_at(node.offset, "invalid use of void expression as lvalue"));
                };
                self.gen_addr(expr)
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
    fn gen_cast(&mut self, from: Type, to: Type) -> Result<()> {
        if to == Type::Void {
            return Ok(());
        }

        if to == Type::Bool {
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
                    Type::Char => Ok(I8),
                    Type::UChar => Ok(U8),
                    Type::Short => Ok(I16),
                    Type::UShort => Ok(U16),
                    Type::Int | Type::Enum => Ok(I32),
                    Type::UInt => Ok(U32),
                    Type::Long => Ok(I64),
                    Type::ULong => Ok(U64),
                    Type::Float => Ok(F32),
                    Type::Double => Ok(F64),
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
        self.gen_loc(node.offset)?;

        match &node.kind {
            NodeKind::Num(value) => writeln!(self.out, "  mov ${value}, %rax")?,
            NodeKind::Flonum(value) => match node.expect_ty() {
                Type::Float => {
                    writeln!(self.out, "  mov ${:#x}, %eax", (*value as f32).to_bits())?;
                    writeln!(self.out, "  movd %eax, %xmm0")?;
                },
                Type::Double => {
                    writeln!(self.out, "  movabs ${:#x}, %rax", value.to_bits())?;
                    writeln!(self.out, "  movq %rax, %xmm0")?;
                },
                _ => unreachable!(),
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

                // After the prologue and local allocation, we have made the
                // frame size a multiple of 16; each temporary push subtracts
                // 8 bytes, so an even depth would still be 16-byte aligned but
                // an odd depth would not, in which case we must subtract 8
                // bytes to realign %rsp before calling and then add it back
                let depth = self.function().depth;
                if depth.is_multiple_of(2) {
                    writeln!(self.out, "  call {name}")?;
                } else {
                    writeln!(self.out, "  sub $8, %rsp")?;
                    writeln!(self.out, "  call {name}")?;
                    writeln!(self.out, "  add $8, %rsp")?;
                }

                // Per x86-64 psABI, for "_Bool", "char", and "short" return
                // types, only the low 8 or 16 bits of %rax are guaranteed to
                // hold the correct value across a call; hence we need to
                // normalize the register to the declared return type here
                match node.expect_ty() {
                    Type::Bool => writeln!(self.out, "  movzx %al, %eax")?,
                    Type::Char => writeln!(self.out, "  movsbl %al, %eax")?,
                    Type::UChar => writeln!(self.out, "  movzbl %al, %eax")?,
                    Type::Short => writeln!(self.out, "  movswl %ax, %eax")?,
                    Type::UShort => writeln!(self.out, "  movzwl %ax, %eax")?,
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
                    self.popf("%xmm1")?;

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
                            return Err(self.source.error_at(
                                node.offset,
                                "invalid operator for floating-point operands",
                            ));
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
                            if self.types.uses_unsigned_arith(node.expect_ty()) {
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
                            if self.types.uses_unsigned_arith(lhs_ty) {
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
                            if self.types.uses_unsigned_arith(lhs_ty) {
                                writeln!(self.out, "  setb %al")?;
                            } else {
                                writeln!(self.out, "  setl %al")?;
                            }
                            writeln!(self.out, "  movzb %al, %rax")?;
                        },
                        BinaryOp::Le => {
                            writeln!(self.out, "  cmp {rdi}, {acc}")?;
                            if self.types.uses_unsigned_arith(lhs_ty) {
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
    fn gen_loc(&mut self, offset: usize) -> Result<()> {
        let loc = self.source.line_col(offset);
        if self.last_loc == Some(loc) {
            return Ok(());
        }
        self.last_loc = Some(loc);
        writeln!(self.out, "  .loc 1 {} {}", loc.0, loc.1)?;
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
            if self.types.uses_unsigned_arith(ty) {
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
    fn popf(&mut self, register: &str) -> Result<()> {
        writeln!(self.out, "  movsd (%rsp), {register}")?;
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

    /// Assign stack offsets to locals and return the aligned stack size.
    fn assign_lvar_offsets(&self, locals: &mut [LocalVar]) -> u64 {
        let mut offset = 0;

        // The first parsed local stays closest to `%rbp`
        for local in locals.iter_mut().rev() {
            offset += self.types.size(local.ty);
            offset = align_to(offset, self.types.eff_align(local.align, local.ty));
            let offset = i64::try_from(offset).expect("stack frame too large");
            local.offset = -offset;
        }

        align_to(offset, 16)
    }
}

fn fp_mnemonic_sz(ty: Type) -> &'static str {
    match ty {
        Type::Float => "s",
        Type::Double => "d",
        _ => unreachable!(),
    }
}
