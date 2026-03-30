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

/// Expression types used for semantic analysis.
#[derive(Clone, Debug)]
pub struct Type(Rc<TypeInner>);

#[derive(Debug)]
struct TypeInner {
    kind: TypeKind,
    size: i64,
    align: i64,
}

/// The specific type form carried by [`Type`].
#[derive(Debug)]
enum TypeKind {
    Char,
    Int,
    Ptr(Box<Type>),
    Array {
        base: Box<Type>,
        _len: usize,
    },
    Func {
        _return_ty: Box<Type>,
        _params: Vec<Type>,
    },
    Struct(Vec<Member>),
}

impl Type {
    fn new(kind: TypeKind, size: i64, align: i64) -> Self {
        Self(Rc::new(TypeInner { kind, size, align }))
    }

    /// Construct a dummy type. This should **NOT** be considered a real type!
    pub fn dummy() -> Self {
        Self::char()
    }

    /// Construct a character type.
    pub fn char() -> Self {
        Self::new(TypeKind::Char, 1, 1)
    }

    /// Construct an integer type.
    pub fn int() -> Self {
        Self::new(TypeKind::Int, 8, 8)
    }

    /// Construct a pointer type to the given base type.
    pub fn ptr(base: Type) -> Self {
        Self::new(TypeKind::Ptr(Box::new(base)), 8, 8)
    }

    /// Construct a function type with the given return type and parameters.
    pub fn func(return_ty: Type, params: Vec<Type>) -> Self {
        Self::new(
            TypeKind::Func {
                _return_ty: Box::new(return_ty),
                _params: params,
            },
            0, // Not applicable
            0, // Not applicable
        )
    }

    /// Construct an array type with the given element type and length.
    pub fn array(base: Type, len: usize) -> Self {
        let size = base.size() * (len as i64);
        let align = base.align();
        Self::new(
            TypeKind::Array {
                base: Box::new(base),
                _len: len,
            },
            size,
            align,
        )
    }

    /// Construct a struct type with the given members.
    ///
    /// The member offsets will be assigned here so they do not need to be
    /// pre-computed.
    pub fn struct_(mut members: Vec<Member>) -> Self {
        let mut offset = 0;
        let mut align = 1;
        for member in members.iter_mut() {
            let member_align = member.ty.align();
            offset = align_to(offset, member_align); // Internal field padding
            member.offset = offset as usize;
            offset += member.ty.size();
            align = align.max(member_align);
        }
        let size = align_to(offset, align); // Trailing padding
        Self::new(TypeKind::Struct(members), size, align)
    }

    /// Return the size of the type in bytes.
    pub fn size(&self) -> i64 {
        self.0.size
    }

    /// Return the byte alignment of the type.
    pub fn align(&self) -> i64 {
        self.0.align
    }

    /// Return whether the type is an integer data type.
    pub fn is_int(&self) -> bool {
        matches!(self.0.kind, TypeKind::Char | TypeKind::Int)
    }

    /// Return whether the type is a function.
    pub fn is_func(&self) -> bool {
        matches!(self.0.kind, TypeKind::Func { .. })
    }

    /// Return whether the type is an array.
    pub fn is_array(&self) -> bool {
        matches!(self.0.kind, TypeKind::Array { .. })
    }

    /// Return the base type for arrays and pointers.
    pub fn base(&self) -> Option<&Type> {
        match &self.0.kind {
            TypeKind::Ptr(base) => Some(base),
            TypeKind::Array { base, .. } => Some(base),
            _ => None,
        }
    }

    /// Return the members of the struct type.
    pub fn members(&self) -> Option<&[Member]> {
        match &self.0.kind {
            TypeKind::Struct(members) => Some(members),
            _ => None,
        }
    }
}
