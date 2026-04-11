//! The type system for expressions.

use std::rc::Rc;

use smol_str::SmolStr;

use crate::utils::align_to;

/// A stable handle to a non-simple type stored in [`TypeStore`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeId(usize);

/// Expression types used for semantic analysis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    pub len: Option<usize>,
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
    /// Optional alignment override via "_Alignas".
    pub align: Option<i64>,
    /// The byte offset of the member in the struct.
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct StructOrUnionType {
    pub is_struct: bool,
    /// If `None`, this represents an incomplete struct or union type.
    pub members: Option<Rc<[Member]>>,
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

    fn get_mut(&mut self, ty: Type) -> Option<&mut TypeData> {
        match ty {
            Type::Stored(TypeId(id)) => Some(&mut self.types[id]),
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
        self.push(TypeKind::Array(ArrayType { base, len }), size, align)
    }

    /// Construct a struct or union type with the given members.
    ///
    /// If `members` is `None`, this represents an incomplete struct or union
    /// type. Otherwise, for a struct, the member offsets will be assigned here
    /// so they do not need to be pre-computed. For a union, the member offsets
    /// must be all 0.
    pub fn struct_or_union(&mut self, is_struct: bool, members: Option<Vec<Member>>) -> Type {
        let ty = self.push(
            TypeKind::StructOrUnion(StructOrUnionType {
                is_struct,
                members: None,
            }),
            -1,
            1,
        );
        if let Some(members) = members {
            self.complete_struct_or_union(is_struct, members, ty);
        }
        ty
    }

    /// Complete an existing incomplete struct or union type.
    ///
    /// See [`TypeStore::struct_or_union`] for more details.
    pub fn complete_struct_or_union(
        &mut self,
        is_struct: bool,
        mut members: Vec<Member>,
        ty: Type,
    ) {
        let mut offset = 0;
        let mut align = 1;

        if is_struct {
            for member in members.iter_mut() {
                let member_align = self.eff_align(member.align, member.ty);
                offset = align_to(offset, member_align); // Field alignment
                member.offset = offset as usize;
                offset += self.size(member.ty);
                align = align.max(member_align);
            }
        } else {
            for member in members.iter() {
                offset = offset.max(self.size(member.ty));
                align = align.max(self.eff_align(member.align, member.ty));
            }
        }

        let data = self.get_mut(ty).expect("type not found");
        let TypeKind::StructOrUnion(sou) = &mut data.kind else {
            panic!("not a struct or union type");
        };

        debug_assert!(
            sou.members.is_none(),
            "cannot complete an already completed struct or union type",
        );

        sou.members = Some(members.into());
        data.size = align_to(offset, align); // Trailing padding
        data.align = align;
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

    /// Return the effective byte alignment of the type with optional override.
    pub fn eff_align(&self, align_override: Option<i64>, ty: Type) -> i64 {
        align_override.unwrap_or_else(|| self.align(ty))
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

    /// Return the array type if it is one.
    pub fn as_array(&self, ty: Type) -> Option<&ArrayType> {
        let data = self.get(ty)?;
        match &data.kind {
            TypeKind::Array(array) => Some(array),
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

    /// Return whether the type is a pointer.
    pub fn is_ptr(&self, ty: Type) -> bool {
        self.get(ty)
            .is_some_and(|data| matches!(data.kind, TypeKind::Ptr(_)))
    }

    /// Return whether the type is a function.
    pub fn is_func(&self, ty: Type) -> bool {
        self.get(ty)
            .is_some_and(|data| matches!(data.kind, TypeKind::Func(_)))
    }

    /// Return whether the type is incomplete.
    pub fn is_incomplete(&self, ty: Type) -> bool {
        if ty == Type::Void {
            return true;
        }
        let Some(data) = self.get(ty) else {
            return false;
        };
        matches!(
            data.kind,
            TypeKind::Array(ArrayType { len: None, .. })
                | TypeKind::StructOrUnion(StructOrUnionType { members: None, .. })
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

    /// Merge two declarations of the same type.
    ///
    /// This method returns `None` except for the following supported cases:
    ///
    /// - **Exactly** the same type, which will be returned as is;
    /// - An incomplete array type and a complete array type with the same base,
    ///   where the complete one will be returned.
    pub fn merge(&self, this: Type, other: Type) -> Option<Type> {
        if this == other {
            return Some(this);
        }

        let this_array = self.as_array(this)?;
        let other_array = self.as_array(other)?;
        if this_array.base != other_array.base {
            return None;
        }

        match (this_array.len, other_array.len) {
            (None, Some(_)) => Some(other),
            (Some(_), None) => Some(this),
            _ => None,
        }
    }
}

impl Type {
    /// Return whether the type is an integer type.
    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::Bool | Type::Char | Type::Short | Type::Int | Type::Long | Type::Enum
        )
    }
}
