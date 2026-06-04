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
use crate::utils::{MAX_FP_ARG_REGS, MAX_GP_ARG_REGS, VA_AREA_SIZE, align_up_to};

const GP_ARG_REGS_8: [&str; MAX_GP_ARG_REGS] = ["%dil", "%sil", "%dl", "%cl", "%r8b", "%r9b"];
const GP_ARG_REGS_16: [&str; MAX_GP_ARG_REGS] = ["%di", "%si", "%dx", "%cx", "%r8w", "%r9w"];
const GP_ARG_REGS_32: [&str; MAX_GP_ARG_REGS] = ["%edi", "%esi", "%edx", "%ecx", "%r8d", "%r9d"];
const GP_ARG_REGS_64: [&str; MAX_GP_ARG_REGS] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];
const GP_RET_REGS_8: [&str; 2] = ["%al", "%dl"];
const GP_RET_REGS_64: [&str; 2] = ["%rax", "%rdx"];

/// Width of an integer scalar used to select size-specific x86-64 operations.
#[derive(Clone, Copy)]
enum ScalarWidth {
    Byte,
    Word,
    Dword,
    Qword,
}

impl TryFrom<u64> for ScalarWidth {
    type Error = ();

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(()),
        }
    }
}

impl ScalarWidth {
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

/// Register indexes for argument parsing.
#[derive(Debug, Default)]
struct ArgRegIndex {
    fp: usize,
    gp: usize,
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
    ret_buf_local: Option<usize>,
    depth: usize,
}

/// A x86-64 assembly code generator.
pub struct Codegen<'a> {
    source_map: &'a SourceMap,
    out: Vec<u8>,
    types: TypeStore,
    functions: Vec<FunctionData>,
    globals: Vec<GlobalVar>,
    next_label: usize,
    last_loc: Option<(usize, u32, u32)>,
    function: Option<FunctionState>,
}

impl<'a> Codegen<'a> {
    /// Create a code generator from source.
    pub fn new(source_map: &'a SourceMap) -> Result<Self> {
        Ok(Self {
            source_map,
            out: Vec::new(),
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
    pub fn generate(mut self, program: Program) -> Result<Vec<u8>> {
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

        Ok(self.out)
    }

    /// Generate assembly for global variables.
    fn gen_globals(&mut self) -> Result<()> {
        for global in &self.globals {
            let mut align = self.types.eff_align(global.align, global.ty);
            if self.types.as_array(global.ty).is_some() && self.types.size(global.ty) >= 16 {
                // Array of at least 16 bytes must be aligned to at least
                // 16-byte boundaries per AMD64 System V ABI rules
                align = align.max(16);
            }

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
            ret_buf_local,
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
            writeln!(self.out, "  movq %rbp, {}(%rbp)", offset + 8)?;
            writeln!(self.out, "  addq $16, {}(%rbp)", offset + 8)?;
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

        let mut rindex = ArgRegIndex::default();

        for param_id in param_locals {
            let &LocalVar { ty, offset, .. } = &locals[param_id];
            if offset > 0 {
                continue;
            }
            let size = self.types.size(ty);

            if self.types.as_struct_or_union(ty).is_some() {
                // For struct or union arguments, bytes 0..8 and 8..16 are two
                // separate chunks that may be passed in registers; remaining
                // bytes are passed on stack so we don't care about them here
                self.store_arg(
                    self.types.is_fp_chunk(ty, 0, 8, 0),
                    &mut rindex,
                    offset,
                    size.min(8),
                )?;
                if size > 8 {
                    self.store_arg(
                        self.types.is_fp_chunk(ty, 8, 16, 0),
                        &mut rindex,
                        offset + 8,
                        size - 8,
                    )?;
                }
            } else {
                self.store_arg(ty.is_flonum(), &mut rindex, offset, size)?;
            }
        }

        self.function = Some(FunctionState {
            name,
            locals,
            ret_buf_local,
            depth: 0,
        });

        self.gen_stmt(&body)?;
        assert_eq!(self.function().depth, 0);

        if self.function().name == "main" {
            // Per C spec, reaching the end of the main function is equivalent
            // to returning 0
            writeln!(self.out, "  mov $0, %rax")?;
        }

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
                    let ty = expr.expect_ty();
                    if self.types.as_struct_or_union(ty).is_some() {
                        if self.types.size(ty) <= 16 {
                            // Copy to registers
                            let mut rindex = ArgRegIndex::default();
                            writeln!(self.out, "  mov %rax, %rdi")?;
                            self.load_ret_chunk(ty, 0, &mut rindex)?;
                            if self.types.size(ty) > 8 {
                                self.load_ret_chunk(ty, 8, &mut rindex)?;
                            }
                        } else {
                            // Copy to the return buffer
                            let local_id = self
                                .function()
                                .ret_buf_local
                                .expect("large aggregate return without hidden return buffer");
                            let offset = self.function().locals[local_id].offset;
                            writeln!(self.out, "  mov {offset}(%rbp), %rdi")?;
                            for i in 0..self.types.size(ty) {
                                writeln!(self.out, "  mov {i}(%rax), %dl")?;
                                writeln!(self.out, "  mov %dl, {i}(%rdi)")?;
                            }
                            writeln!(self.out, "  mov %rdi, %rax")?;
                        }
                    }
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
            NodeKind::FuncCall {
                ret_buf_local: Some(_),
                ..
            } => self.gen_expr(node),
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
        let ty = node.expect_ty();

        match &node.kind {
            NodeKind::Num(value) => writeln!(self.out, "  mov ${value}, %rax")?,
            NodeKind::Flonum(value) => match ty {
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
            NodeKind::FuncCall {
                callee,
                args,
                ret_buf_local,
            } => {
                let mut rindex = ArgRegIndex::default();
                if ret_buf_local.is_some() {
                    rindex.gp += 1;
                }

                let args_regs = args
                    .iter()
                    .map(|arg| self.arg_regs(arg.expect_ty(), &mut rindex))
                    .collect::<Vec<_>>();

                let mut n_stack_slots = args
                    .iter()
                    .zip(&args_regs)
                    .try_fold(0usize, |acc, (arg, regs)| {
                        if regs.is_some() {
                            Some(acc) // Register argument, no stack slot needed
                        } else {
                            let ty = arg.expect_ty();
                            if self.types.as_struct_or_union(ty).is_some() {
                                let size = align_up_to(self.types.size(ty), 8);
                                acc.checked_add((size / 8) as usize)
                            } else {
                                acc.checked_add(1)
                            }
                        }
                    })
                    .expect("stack arguments exceed available stack space");

                // Ensure 16-byte alignment of the stack before the call by
                // reserving an extra slot if necessary (each slot is 8 bytes)
                if !(self.function().depth + n_stack_slots).is_multiple_of(2) {
                    writeln!(self.out, "  sub $8, %rsp")?;
                    self.function_mut().depth += 1;
                    n_stack_slots += 1;
                }

                let mut push_args = |pass_by_stack: bool| -> Result<()> {
                    for (arg, regs) in args.iter().zip(&args_regs).rev() {
                        if regs.is_none() != pass_by_stack {
                            continue;
                        }
                        self.gen_expr(arg)?;

                        let ty = arg.expect_ty();
                        if self.types.as_struct_or_union(ty).is_some() {
                            self.push_struct_or_union(ty)?;
                        } else if ty.is_flonum() {
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
                if let Some(local_id) = ret_buf_local {
                    let offset = self.function().locals[*local_id].offset;
                    writeln!(self.out, "  lea {offset}(%rbp), %rax")?;
                    self.push()?;
                }

                self.gen_expr(callee)?;

                // Pop register arguments into appropriate registers, also used
                // to count how many fp registers are used
                let mut rindex = ArgRegIndex::default();
                if ret_buf_local.is_some() {
                    self.pop(GP_ARG_REGS_64[rindex.gp])?;
                    rindex.gp += 1;
                }

                for regs in args_regs.iter().flatten() {
                    for is_fp in regs {
                        if *is_fp {
                            self.popf(rindex.fp)?;
                            rindex.fp += 1;
                        } else {
                            self.pop(GP_ARG_REGS_64[rindex.gp])?;
                            rindex.gp += 1;
                        }
                    }
                }

                // Call the function pointer in %rax, but we have to first move
                // it elsewhere because %rax must carry the number of floating
                // point register arguments (for variadic calls)
                writeln!(self.out, "  mov %rax, %r10")?;
                writeln!(self.out, "  mov ${}, %rax", rindex.fp)?;
                writeln!(self.out, "  call *%r10")?;

                // Clean up stack arguments if any
                if n_stack_slots > 0 {
                    writeln!(self.out, "  add ${}, %rsp", n_stack_slots * 8)?;
                    self.function_mut().depth -= n_stack_slots;
                }

                if let Some(local_id) = ret_buf_local {
                    let offset = self.function().locals[*local_id].offset;
                    if self.types.size(ty) <= 16 {
                        let mut rindex = ArgRegIndex::default();
                        self.store_ret_chunk(ty, 0, offset, &mut rindex)?;
                        if self.types.size(ty) > 8 {
                            self.store_ret_chunk(ty, 8, offset + 8, &mut rindex)?;
                        }
                    }
                    writeln!(self.out, "  lea {offset}(%rbp), %rax")?;
                    return Ok(());
                }

                // Per x86-64 psABI, for "_Bool", "char", and "short" return
                // types, only the low 8 or 16 bits of %rax are guaranteed to
                // hold the correct value across a call; hence we need to
                // normalize the register to the declared return type here
                match ty {
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
                self.load(ty)?;
            },
            NodeKind::Neg(expr) => {
                self.gen_expr(expr)?;
                if ty.is_flonum() {
                    let sz = fp_mnemonic_sz(ty);
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
            NodeKind::Entity(_) => {
                self.gen_addr(node)?;
                self.load(ty)?;
            },
            NodeKind::Assign { lhs, rhs } => {
                self.gen_addr(lhs)?;
                self.push()?;
                self.gen_expr(rhs)?;

                if let NodeKind::Member { member, .. } = &lhs.kind
                    && let Some((bit_width, bit_offset)) = member.bit_field
                {
                    // If lhs is a bit-field, we need to read the current value
                    // from memory and merge it with a properly shifted and
                    // masked rhs value
                    writeln!(self.out, "  mov %rax, %r8")?;
                    writeln!(self.out, "  mov %rax, %rdi")?;
                    writeln!(self.out, "  and ${}, %rdi", (1u128 << bit_width) - 1)?;
                    writeln!(self.out, "  shl ${bit_offset}, %rdi")?;
                    writeln!(self.out, "  mov (%rsp), %rax")?;
                    self.load(member.ty)?;
                    let mask = (((1u128 << bit_width) - 1) << bit_offset) as u64;
                    writeln!(self.out, "  mov ${}, %r9", !mask as i64)?;
                    writeln!(self.out, "  and %r9, %rax")?;
                    writeln!(self.out, "  or %rdi, %rax")?;
                    self.store(lhs.expect_ty())?;
                    writeln!(self.out, "  mov %r8, %rax")?;
                } else {
                    self.store(lhs.expect_ty())?;
                }
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
            NodeKind::Member { member, .. } => {
                self.gen_addr(node)?;
                self.load(ty)?;
                if let Some((bit_width, bit_offset)) = member.bit_field {
                    writeln!(self.out, "  shl ${}, %rax", 64 - bit_width - bit_offset)?;
                    if member.ty.is_unsigned() {
                        writeln!(self.out, "  shr ${}, %rax", 64 - bit_width)?;
                    } else {
                        writeln!(self.out, "  sar ${}, %rax", 64 - bit_width)?;
                    }
                }
            },
            NodeKind::StmtExpr(body) => {
                for stmt in body {
                    self.gen_stmt(stmt)?;
                }
            },
            NodeKind::Cast(expr) => {
                self.gen_expr(expr)?;
                self.gen_cast(expr.expect_ty(), ty)?;
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

    /// Push the struct or union pointer by `%rax` onto the temporary stack.
    fn push_struct_or_union(&mut self, ty: Type) -> Result<()> {
        let size = self.types.size(ty);
        let aligned = align_up_to(size, 8);

        writeln!(self.out, "  sub ${aligned}, %rsp")?;
        self.function_mut().depth += (aligned / 8) as usize;

        for i in 0..size {
            writeln!(self.out, "  mov {i}(%rax), %r10b")?;
            writeln!(self.out, "  mov %r10b, {i}(%rsp)")?;
        }
        Ok(())
    }

    /// Store a small-aggregate return chunk taken from return registers.
    ///
    /// `lo` is the byte offset of the 8-byte ABI chunk within `ty`.
    /// `dst_offset` is the destination stack offset from `%rbp`.
    fn store_ret_chunk(
        &mut self,
        ty: Type,
        lo: u64,
        dst_offset: i64,
        rindex: &mut ArgRegIndex,
    ) -> Result<()> {
        let size = (self.types.size(ty) - lo).min(8);

        if self.types.is_fp_chunk(ty, lo, lo + 8, 0) {
            let sz = fp_mnemonic_sz_from_size(size);
            writeln!(self.out, "  movs{sz} %xmm{}, {dst_offset}(%rbp)", rindex.fp)?;
            rindex.fp += 1;
            return Ok(());
        }

        // GP return chunks are packed little-endian in %rax then %rdx, so we
        // peel one byte at a time into the destination object
        for i in 0..size {
            writeln!(
                self.out,
                "  mov {}, {}(%rbp)",
                GP_RET_REGS_8[rindex.gp],
                dst_offset + i as i64,
            )?;
            writeln!(self.out, "  shr $8, {}", GP_RET_REGS_64[rindex.gp])?;
        }

        rindex.gp += 1;
        Ok(())
    }

    /// Load a small-aggregate return chunk into return registers.
    ///
    /// `lo` is the byte offset of the 8-byte ABI chunk within `ty`.
    fn load_ret_chunk(&mut self, ty: Type, lo: u64, rindex: &mut ArgRegIndex) -> Result<()> {
        let size = (self.types.size(ty) - lo).min(8);

        if self.types.is_fp_chunk(ty, lo, lo + 8, 0) {
            let sz = fp_mnemonic_sz_from_size(size);
            writeln!(self.out, "  movs{sz} {lo}(%rdi), %xmm{}", rindex.fp)?;
            rindex.fp += 1;
            return Ok(());
        }

        // Build the little-endian integer register value by shifting and or-ing
        // one byte at a time from the source object
        writeln!(self.out, "  mov $0, {}", GP_RET_REGS_64[rindex.gp])?;
        for i in (0..size).rev() {
            writeln!(self.out, "  shl $8, {}", GP_RET_REGS_64[rindex.gp])?;
            writeln!(
                self.out,
                "  mov {}(%rdi), {}",
                lo + i,
                GP_RET_REGS_8[rindex.gp],
            )?;
        }

        rindex.gp += 1;
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

        let width = ScalarWidth::try_from(self.types.size(ty)).expect("invalid scalar width");
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

        let size = self.types.size(ty);
        if self.types.as_struct_or_union(ty).is_some() {
            for i in 0..size {
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

        let width = ScalarWidth::try_from(size).expect("invalid scalar width");
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

    /// Store an incoming argument register to its stack slot.
    fn store_arg(
        &mut self,
        is_fp: bool,
        rindex: &mut ArgRegIndex,
        offset: i64,
        size: u64,
    ) -> Result<()> {
        if is_fp {
            let sz = fp_mnemonic_sz_from_size(size);
            writeln!(self.out, "  movs{sz} %xmm{}, {offset}(%rbp)", rindex.fp)?;
            rindex.fp += 1;
            return Ok(());
        }

        if let Ok(width) = ScalarWidth::try_from(size) {
            let register = width.gp_arg_reg(rindex.gp);
            writeln!(self.out, "  mov {register}, {offset}(%rbp)")?;
            rindex.gp += 1;
            return Ok(());
        }

        // Not trivially representable with a single instruction, so we take the
        // dumb (and inefficient) approach of storing byte by byte
        for off in offset..offset + size as i64 {
            writeln!(self.out, "  mov {}, {off}(%rbp)", GP_ARG_REGS_8[rindex.gp])?;
            writeln!(self.out, "  shr $8, {}", GP_ARG_REGS_64[rindex.gp])?;
        }
        rindex.gp += 1;
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

    /// Determine the register assignments for an argument.
    ///
    /// Returns `None` if the argument should be passed on stack. Otherwise,
    /// each boolean in the returned vector represents whether the register
    /// should be floating-point (true) or general-purpose (false). The returned
    /// vector can have only one or two elements.
    fn arg_regs(&self, ty: Type, rindex: &mut ArgRegIndex) -> Option<Vec<bool>> {
        if self.types.as_struct_or_union(ty).is_some() {
            let size = self.types.size(ty);
            if size > 16 {
                return None;
            }

            let mut regs = vec![self.types.is_fp_chunk(ty, 0, 8, 0)];
            if size > 8 {
                regs.push(self.types.is_fp_chunk(ty, 8, 16, 0));
            }

            let fp_needed = regs.iter().filter(|is_fp| **is_fp).count();
            let gp_needed = regs.len() - fp_needed;
            if rindex.gp + gp_needed > MAX_GP_ARG_REGS || rindex.fp + fp_needed > MAX_FP_ARG_REGS {
                return None;
            }

            rindex.gp += gp_needed;
            rindex.fp += fp_needed;
            return Some(regs);
        }

        if ty.is_flonum() {
            if rindex.fp >= MAX_FP_ARG_REGS {
                return None;
            }
            rindex.fp += 1;
            return Some(vec![true]);
        }

        if rindex.gp >= MAX_GP_ARG_REGS {
            return None;
        }
        rindex.gp += 1;
        Some(vec![false])
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

        let mut rindex = ArgRegIndex::default();

        // Assign offsets to pass-by-stack parameters
        for &param_id in param_locals {
            let local = &mut locals[param_id];
            if self.arg_regs(local.ty, &mut rindex).is_some() {
                continue;
            }
            top = align_up_to(top, 8);
            local.offset = i64::try_from(top).expect("stack frame too large");
            top += self.types.size(local.ty);
        }

        // Assign offsets to register-passed parameters and local variables
        for local in locals.iter_mut().rev() {
            if local.offset != 0 {
                continue;
            }

            let mut align = self.types.eff_align(local.align, local.ty);
            if self.types.as_array(local.ty).is_some() && self.types.size(local.ty) >= 16 {
                // Array of at least 16 bytes must be aligned to at least
                // 16-byte boundaries per AMD64 System V ABI rules
                align = align.max(16);
            }

            bottom += self.types.size(local.ty);
            bottom = align_up_to(bottom, align);
            let offset = i64::try_from(bottom).expect("stack frame too large");
            local.offset = -offset;
        }

        align_up_to(bottom, 16)
    }
}

fn fp_mnemonic_sz(ty: Type) -> &'static str {
    match ty {
        Type::FLOAT => "s",
        Type::DOUBLE => "d",
        _ => unreachable!("not a floating-point type"),
    }
}

fn fp_mnemonic_sz_from_size(size: u64) -> &'static str {
    match size {
        4 => "s",
        8 => "d",
        _ => unreachable!("invalid floating-point argument chunk size: {size}"),
    }
}
