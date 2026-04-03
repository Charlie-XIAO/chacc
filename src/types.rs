//! The type system for expressions.

use std::rc::Rc;

use smol_str::SmolStr;

use crate::utils::align_to;

/// A member of a struct.
#[derive(Clone, Debug)]
pub struct Member {
    pub ty: Type,
    pub name: SmolStr,
    /// The byte offset of the member in the struct.
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeId {
    I8,
    I16,
    I32,
    I64,
}

/// Expression types used for semantic analysis.
#[derive(Clone, Debug, Default)]
pub struct Type(Rc<TypeInner>);

#[derive(Debug, Default)]
struct TypeInner {
    kind: TypeKind,
    size: i64,
    align: i64,
}

/// The specific type form carried by [`Type`].
#[derive(Debug, Default)]
enum TypeKind {
    #[default] // For ergonomics
    Void,
    Char,
    Short,
    Int,
    Long,
    Ptr(Type),
    Array(ArrayType),
    Func(FuncType),
    StructOrUnion(StructOrUnionType),
}

#[derive(Debug)]
pub struct ArrayType {
    pub base: Type,
    _len: usize,
}

#[derive(Debug)]
pub struct FuncType {
    pub return_ty: Type,
    pub params: Vec<Type>,
}

#[derive(Debug)]
pub struct StructOrUnionType {
    _is_struct: bool,
    pub members: Vec<Member>,
}

impl Type {
    fn new(kind: TypeKind, size: i64, align: i64) -> Self {
        Self(Rc::new(TypeInner { kind, size, align }))
    }

    /// Construct a void type.
    pub fn void() -> Self {
        Self::new(TypeKind::Void, 1, 1)
    }

    /// Construct a character type.
    pub fn char() -> Self {
        Self::new(TypeKind::Char, 1, 1)
    }

    /// Construct a short integer type.
    pub fn short() -> Self {
        Self::new(TypeKind::Short, 2, 2)
    }

    /// Construct an integer type.
    pub fn int() -> Self {
        Self::new(TypeKind::Int, 4, 4)
    }

    /// Construct a long integer type.
    pub fn long() -> Self {
        Self::new(TypeKind::Long, 8, 8)
    }

    /// Construct a pointer type to the given base type.
    pub fn ptr(base: Type) -> Self {
        Self::new(TypeKind::Ptr(base), 8, 8)
    }

    /// Construct a function type with the given return type and parameters.
    pub fn func(return_ty: Type, params: Vec<Type>) -> Self {
        Self::new(
            TypeKind::Func(FuncType { return_ty, params }),
            0, // Not applicable
            0, // Not applicable
        )
    }

    /// Construct an array type with the given element type and length.
    pub fn array(base: Type, len: usize) -> Self {
        let size = base.size() * (len as i64);
        let align = base.align();
        Self::new(TypeKind::Array(ArrayType { base, _len: len }), size, align)
    }

    /// Construct a struct or union type with the given members.
    ///
    /// For a struct, the member offsets will be assigned here so they do not
    /// need to be pre-computed. For a union, the member offsets must be all 0.
    pub fn struct_or_union(is_struct: bool, mut members: Vec<Member>) -> Self {
        let mut offset = 0;
        let mut align = 1;

        if is_struct {
            for member in members.iter_mut() {
                let member_align = member.ty.align();
                offset = align_to(offset, member_align); // Field alignment
                member.offset = offset as usize;
                offset += member.ty.size();
                align = align.max(member_align);
            }
        } else {
            for member in members.iter() {
                offset = offset.max(member.ty.size());
                align = align.max(member.ty.align());
            }
        }

        let size = align_to(offset, align); // Trailing padding
        Self::new(
            TypeKind::StructOrUnion(StructOrUnionType {
                _is_struct: is_struct,
                members,
            }),
            size,
            align,
        )
    }

    /// Return the size of the type in bytes.
    pub fn size(&self) -> i64 {
        self.0.size
    }

    /// Return the byte alignment of the type.
    pub fn align(&self) -> i64 {
        self.0.align
    }

    /// Return whether the type is a void type.
    pub fn is_void(&self) -> bool {
        matches!(self.0.kind, TypeKind::Void)
    }

    /// Return whether the type is an integer type.
    pub fn is_int(&self) -> bool {
        matches!(
            self.0.kind,
            TypeKind::Char | TypeKind::Short | TypeKind::Int | TypeKind::Long
        )
    }

    /// Return whether the type is a function.
    pub fn is_func(&self) -> bool {
        matches!(self.0.kind, TypeKind::Func { .. })
    }

    /// Return whether the type is an array.
    pub fn is_array(&self) -> bool {
        matches!(self.0.kind, TypeKind::Array { .. })
    }

    /// Return the function type if it is one.
    pub fn as_func(&self) -> Option<&FuncType> {
        match &self.0.kind {
            TypeKind::Func(func) => Some(func),
            _ => None,
        }
    }

    /// Return the struct or union type if it is one.
    pub fn as_struct_or_union(&self) -> Option<&StructOrUnionType> {
        match &self.0.kind {
            TypeKind::StructOrUnion(sou) => Some(sou),
            _ => None,
        }
    }

    /// Return the base type for arrays and pointers.
    pub fn base(&self) -> Option<&Type> {
        match &self.0.kind {
            TypeKind::Ptr(base) => Some(base),
            TypeKind::Array(ArrayType { base, .. }) => Some(base),
            _ => None,
        }
    }

    /// Coerce with another operand type for a validated binary operation.
    ///
    /// This helper is intentionally lhs-biased. If exactly one operand is a
    /// pointer, it must already have been canonicalized to `self` (lhs) by the
    /// caller. This method also does not perform pointer legality checks and
    /// the caller is responsible for those beforehand.
    pub fn coerce(&self, other: &Type) -> Type {
        debug_assert!(
            self.base().is_some() || other.base().is_none(),
            "pointer coercion expects any lone pointer operand to be lhs",
        );

        if let Some(base) = self.base() {
            return Type::ptr(base.clone());
        }
        if self.size() == 8 || other.size() == 8 {
            return Type::long();
        }
        Type::int()
    }
}

impl TryFrom<&Type> for TypeId {
    type Error = ();

    fn try_from(ty: &Type) -> Result<Self, Self::Error> {
        match ty.0.kind {
            TypeKind::Char => Ok(TypeId::I8),
            TypeKind::Short => Ok(TypeId::I16),
            TypeKind::Int => Ok(TypeId::I32),
            TypeKind::Long => Ok(TypeId::I64),
            _ => Err(()),
        }
    }
}
