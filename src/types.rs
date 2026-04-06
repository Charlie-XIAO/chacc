//! The type system for expressions.

use std::rc::Rc;

use smol_str::SmolStr;

use crate::utils::align_to;

/// A stable handle to a non-simple type stored in [`TypeStore`].
#[derive(Clone, Copy, Debug)]
pub struct TypeId(usize);

/// Expression types used for semantic analysis.
#[derive(Clone, Copy, Debug, Default)]
pub enum Type {
    #[default]
    Dummy,
    Void,
    Bool,
    Char,
    Short,
    Int,
    Long,
    Enum,
    /// A non-simple type stored in [`TypeStore`].
    Stored(TypeId),
}

#[derive(Debug, Clone)]
struct TypeData {
    kind: TypeKind,
    size: i64,
    align: i64,
}

#[derive(Debug, Clone)]
enum TypeKind {
    Ptr(Type),
    Array(ArrayType),
    Func(FuncType),
    StructOrUnion(StructOrUnionType),
}

#[derive(Debug, Clone)]
pub struct ArrayType {
    pub base: Type,
    /// If `None`, this represents an incomplete array type.
    _len: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct FuncType {
    pub return_ty: Type,
    pub params: Rc<[Type]>,
}

/// A member of a struct.
#[derive(Debug, Clone)]
pub struct Member {
    pub ty: Type,
    pub name: SmolStr,
    /// The byte offset of the member in the struct.
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct StructOrUnionType {
    pub is_struct: bool,
    pub members: Rc<[Member]>,
}

/// The global store of non-simple type definitions used by the parsed program.
#[derive(Debug, Default)]
pub struct TypeStore {
    types: Vec<TypeData>,
    pub frozen: bool,
}

impl TypeStore {
    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn truncate(&mut self, len: usize) {
        debug_assert!(!self.frozen, "cannot truncate a frozen type store");
        self.types.truncate(len);
    }

    fn push(&mut self, kind: TypeKind, size: i64, align: i64) -> Type {
        debug_assert!(!self.frozen, "cannot push new type to a frozen type store");
        let ty = Type::Stored(TypeId(self.types.len()));
        self.types.push(TypeData { kind, size, align });
        ty
    }

    fn get(&self, ty: Type) -> Option<&TypeData> {
        match ty {
            Type::Stored(TypeId(id)) => Some(&self.types[id]),
            _ => None,
        }
    }

    /// Construct a pointer type to the given base type.
    pub fn ptr(&mut self, base: Type) -> Type {
        self.push(TypeKind::Ptr(base), 8, 8)
    }

    /// Construct a function type with the given return type and parameters.
    pub fn func(&mut self, return_ty: Type, params: Vec<Type>) -> Type {
        self.push(
            TypeKind::Func(FuncType {
                return_ty,
                params: params.into(),
            }),
            0, // Not applicable
            0, // Not applicable
        )
    }

    /// Construct an array type with the given element type and length.
    ///
    /// If `len` is `None`, this represents an incomplete array type.
    pub fn array(&mut self, base: Type, len: Option<usize>) -> Type {
        debug_assert!(
            !self.is_incomplete(base) && !self.is_func(base),
            "invalid array element type: {base:?}",
        );

        let size = match len {
            Some(len) => self.size(base) * (len as i64),
            None => -1,
        };
        let align = self.align(base);
        self.push(TypeKind::Array(ArrayType { base, _len: len }), size, align)
    }

    /// Construct a struct or union type with the given members.
    ///
    /// For a struct, the member offsets will be assigned here so they do not
    /// need to be pre-computed. For a union, the member offsets must be all 0.
    pub fn struct_or_union(&mut self, is_struct: bool, mut members: Vec<Member>) -> Type {
        let mut offset = 0;
        let mut align = 1;

        if is_struct {
            for member in members.iter_mut() {
                let member_align = self.align(member.ty);
                offset = align_to(offset, member_align); // Field alignment
                member.offset = offset as usize;
                offset += self.size(member.ty);
                align = align.max(member_align);
            }
        } else {
            for member in members.iter() {
                offset = offset.max(self.size(member.ty));
                align = align.max(self.align(member.ty));
            }
        }

        self.push(
            TypeKind::StructOrUnion(StructOrUnionType {
                is_struct,
                members: members.into(),
            }),
            align_to(offset, align), // Trailing padding
            align,
        )
    }

    /// Coerce two operand types for a validated binary operation.
    ///
    /// This helper is intentionally `lhs`-biased. If exactly one operand is a
    /// pointer, it must already have been canonicalized to `lhs` by the
    /// caller. This method also does not perform pointer legality checks and
    /// the caller is responsible for those beforehand.
    pub fn coerce(&mut self, lhs: Type, rhs: Type) -> Type {
        debug_assert!(
            self.base(lhs).is_some() || self.base(rhs).is_none(),
            "pointer coercion expects any lone pointer operand to be lhs",
        );

        if let Some(base) = self.base(lhs) {
            return self.ptr(base);
        }
        if self.size(lhs) == 8 || self.size(rhs) == 8 {
            return Type::Long;
        }
        Type::Int
    }

    /// Return the byte alignment of the type.
    pub fn align(&self, ty: Type) -> i64 {
        match ty {
            Type::Dummy => 0,
            Type::Void => 1,
            Type::Bool | Type::Char => 1,
            Type::Short => 2,
            Type::Int | Type::Enum => 4,
            Type::Long => 8,
            Type::Stored(_) => self.get(ty).unwrap().align,
        }
    }

    /// Return the size of the type in bytes.
    pub fn size(&self, ty: Type) -> i64 {
        match ty {
            Type::Dummy => 0,
            Type::Void => 1,
            Type::Bool | Type::Char => 1,
            Type::Short => 2,
            Type::Int | Type::Enum => 4,
            Type::Long => 8,
            Type::Stored(_) => self.get(ty).unwrap().size,
        }
    }

    /// Return the base type for arrays and pointers.
    pub fn base(&self, ty: Type) -> Option<Type> {
        let data = self.get(ty)?;
        match &data.kind {
            TypeKind::Ptr(base) => Some(*base),
            TypeKind::Array(ArrayType { base, .. }) => Some(*base),
            _ => None,
        }
    }

    /// Return the function type if it is one.
    pub fn as_func(&self, ty: Type) -> Option<&FuncType> {
        let data = self.get(ty)?;
        match &data.kind {
            TypeKind::Func(func) => Some(func),
            _ => None,
        }
    }

    /// Return the struct or union type if it is one.
    pub fn as_struct_or_union(&self, ty: Type) -> Option<&StructOrUnionType> {
        let data = self.get(ty)?;
        match &data.kind {
            TypeKind::StructOrUnion(sou) => Some(sou),
            _ => None,
        }
    }

    /// Return whether the type is a function.
    pub fn is_func(&self, ty: Type) -> bool {
        self.get(ty)
            .is_some_and(|data| matches!(data.kind, TypeKind::Func(_)))
    }

    /// Return whether the type is an array.
    pub fn is_array(&self, ty: Type) -> bool {
        self.get(ty)
            .is_some_and(|data| matches!(data.kind, TypeKind::Array(_)))
    }

    /// Return whether the type is incomplete.
    pub fn is_incomplete(&self, ty: Type) -> bool {
        if matches!(ty, Type::Void) {
            return true;
        }
        let Some(data) = self.get(ty) else {
            return false;
        };
        matches!(data.kind, TypeKind::Array(ArrayType { _len: None, .. }))
    }

    /// Return whether the type is an integer type.
    pub fn is_integer(&self, ty: Type) -> bool {
        matches!(
            ty,
            Type::Bool | Type::Char | Type::Short | Type::Int | Type::Long | Type::Enum
        )
    }
}
