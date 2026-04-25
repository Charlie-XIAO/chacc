//! The type system for expressions.

use std::cmp::Ordering;
use std::rc::Rc;

use smol_str::SmolStr;

use crate::constexpr::ConstType;
use crate::utils::align_to;

/// Expression types used for semantic analysis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Type {
    #[default]
    Dummy,
    Void,
    Bool,
    Char,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    Enum,
    Float,
    Double,
    /// A non-trivial type stored in [`TypeStore`].
    Stored(usize),
}

#[derive(Debug, Clone)]
struct TypeData {
    kind: TypeKind,
    size: u64,
    align: u64,
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
    pub is_variadic: bool,
}

/// A member of a struct.
#[derive(Debug, Clone)]
pub struct Member {
    pub ty: Type,
    pub name: SmolStr,
    /// Optional alignment override via "_Alignas".
    pub align: Option<u64>,
    /// The byte offset of the member in the struct.
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct StructOrUnionType {
    pub is_struct: bool,
    /// If `None`, this represents an incomplete struct or union type.
    pub members: Option<Rc<[Member]>>,
}

/// The global store of non-trivial type definitions used by the parsed program.
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

    fn push(&mut self, kind: TypeKind, size: u64, align: u64) -> Type {
        debug_assert!(!self.frozen, "cannot push new type to a frozen type store");
        let ty = Type::Stored(self.types.len());
        self.types.push(TypeData { kind, size, align });
        ty
    }

    fn get(&self, ty: Type) -> Option<&TypeData> {
        match ty {
            Type::Stored(id) => Some(&self.types[id]),
            _ => None,
        }
    }

    fn get_mut(&mut self, ty: Type) -> Option<&mut TypeData> {
        match ty {
            Type::Stored(id) => Some(&mut self.types[id]),
            _ => None,
        }
    }

    /// Construct a pointer type to the given base type.
    pub fn ptr(&mut self, base: Type) -> Type {
        self.push(TypeKind::Ptr(base), 8, 8)
    }

    /// Construct a function type with the given return type and parameters.
    pub fn func(&mut self, return_ty: Type, params: Vec<Type>, is_variadic: bool) -> Type {
        self.push(
            TypeKind::Func(FuncType {
                return_ty,
                params: params.into(),
                is_variadic,
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
            Some(len) => self.size(base) * (len as u64),
            None => 0,
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
            0,
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
    pub fn align(&self, ty: Type) -> u64 {
        match ty {
            Type::Dummy => 0,
            Type::Void => 1,
            Type::Bool | Type::Char | Type::UChar => 1,
            Type::Short | Type::UShort => 2,
            Type::Int | Type::UInt | Type::Enum | Type::Float => 4,
            Type::Long | Type::ULong | Type::Double => 8,
            Type::Stored(_) => self.get(ty).unwrap().align,
        }
    }

    /// Return the effective byte alignment of the type with optional override.
    pub fn eff_align(&self, align_override: Option<u64>, ty: Type) -> u64 {
        align_override.unwrap_or_else(|| self.align(ty))
    }

    /// Return the size of the type in bytes.
    pub fn size(&self, ty: Type) -> u64 {
        match ty {
            Type::Dummy => 0,
            Type::Void => 1,
            Type::Bool | Type::Char | Type::UChar => 1,
            Type::Short | Type::UShort => 2,
            Type::Int | Type::UInt | Type::Enum | Type::Float => 4,
            Type::Long | Type::ULong | Type::Double => 8,
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
    ///
    /// If `accept_ptr` is true, a function pointer type is also accepted and
    /// the pointed function type is returned.
    pub fn as_func(&self, ty: Type, accept_ptr: bool) -> Option<&FuncType> {
        let data = self.get(ty)?;
        match &data.kind {
            TypeKind::Func(func) => Some(func),
            _ if accept_ptr => self.base(ty).and_then(|base| self.as_func(base, false)),
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

    /// Return whether unsigned machine arithmetic should be used for the type.
    ///
    /// This includes unsigned integer types and pointer types.
    pub fn uses_unsigned_arith(&self, ty: Type) -> bool {
        ty.is_unsigned_integer()
            || self
                .get(ty)
                .is_some_and(|data| matches!(data.kind, TypeKind::Ptr(_)))
    }

    /// Return whether two types are the same.
    ///
    /// - Trivial types are compared directly.
    /// - Pointer, array, and function types are compared structurally.
    /// - Struct and union types are compared based on identity. In particular,
    ///   two distinctly stored struct/union are treated as different types even
    ///   if their members are all the same.
    pub fn same_type(&self, this: Type, other: Type) -> bool {
        if this == other {
            return true;
        }

        let (Some(this), Some(other)) = (self.get(this), self.get(other)) else {
            return false;
        };

        match (&this.kind, &other.kind) {
            (TypeKind::Ptr(left), TypeKind::Ptr(right)) => self.same_type(*left, *right),
            (TypeKind::Array(left), TypeKind::Array(right)) => {
                left.len == right.len && self.same_type(left.base, right.base)
            },
            (TypeKind::Func(left), TypeKind::Func(right)) => {
                self.same_type(left.return_ty, right.return_ty)
                    && left.is_variadic == right.is_variadic
                    && left.params.len() == right.params.len()
                    && left
                        .params
                        .iter()
                        .zip(right.params.iter())
                        .all(|(this, other)| self.same_type(*this, *other))
            },
            _ => false,
        }
    }

    /// Coerce two operand types for a validated binary/conditional operation.
    ///
    /// This assumes the caller has already decided that [usual arithmetic
    /// conversion][1] is the right operation. It does not try to enforce all
    /// pointer/function compatibility rules by itself.
    ///
    /// [1]: https://en.cppreference.com/cpp/language/usual_arithmetic_conversions
    pub fn coerce(&mut self, lhs: Type, rhs: Type) -> Type {
        if self.is_func(lhs) {
            return self.ptr(lhs);
        }
        if self.is_func(rhs) {
            return self.ptr(rhs);
        }

        if let Some(base) = self.base(lhs).or_else(|| self.base(rhs)) {
            return self.ptr(base);
        }

        if matches!(lhs, Type::Double) || matches!(rhs, Type::Double) {
            return Type::Double;
        }
        if matches!(lhs, Type::Float) || matches!(rhs, Type::Float) {
            return Type::Float;
        }

        let lhs = self.promote_int(lhs).unwrap_or(lhs);
        let rhs = self.promote_int(rhs).unwrap_or(rhs);

        match self.size(lhs).cmp(&self.size(rhs)) {
            Ordering::Less => return rhs,
            Ordering::Greater => return lhs,
            _ => {},
        }

        if rhs.is_unsigned_integer() {
            return rhs;
        }
        lhs
    }

    /// Merge two declarations of the same type.
    ///
    /// This method returns `None` except for the following supported cases:
    ///
    /// - The same type, which will be returned;
    /// - An incomplete array type and a complete array type with the same base,
    ///   where the complete one will be returned.
    pub fn merge(&self, this: Type, other: Type) -> Option<Type> {
        if self.same_type(this, other) {
            return Some(this);
        }

        let this_array = self.as_array(this)?;
        let other_array = self.as_array(other)?;
        if !self.same_type(this_array.base, other_array.base) {
            return None;
        }

        match (this_array.len, other_array.len) {
            (None, Some(_)) => Some(other),
            (Some(_), None) => Some(this),
            _ => None,
        }
    }

    /// Convert a [`Type`] to a [`ConstType`] if applicable.
    pub fn to_const(&self, ty: Type) -> Option<ConstType> {
        match ty {
            Type::Bool => Some(ConstType::Bool),
            Type::Char => Some(ConstType::Char),
            Type::UChar => Some(ConstType::UChar),
            Type::Short => Some(ConstType::Short),
            Type::UShort => Some(ConstType::UShort),
            Type::Int | Type::Enum => Some(ConstType::Int),
            Type::UInt => Some(ConstType::UInt),
            Type::Long => Some(ConstType::Long),
            Type::ULong => Some(ConstType::ULong),
            Type::Float => Some(ConstType::Float),
            Type::Double => Some(ConstType::Double),
            _ if self.is_ptr(ty) => Some(ConstType::Ptr),
            _ => None,
        }
    }

    /// Apply [integer promotions][1] to an integer type.
    ///
    /// If a type is not applicable for integer promotion, this returns `None`.
    ///
    /// [1]: https://en.cppreference.com/w/c/language/conversion.html#Integer_promotions
    pub fn promote_int(&self, ty: Type) -> Option<Type> {
        Some(match ty {
            Type::Bool
            | Type::Char
            | Type::UChar
            | Type::Short
            | Type::UShort
            | Type::Int
            | Type::Enum => Type::Int,
            Type::UInt => Type::UInt,
            Type::Long => Type::Long,
            Type::ULong => Type::ULong,
            _ => return None,
        })
    }
}

impl Type {
    /// Return whether the type is a numeric type.
    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_flonum()
    }

    /// Return whether the type is an integer type.
    pub fn is_integer(&self) -> bool {
        self.is_unsigned_integer()
            || matches!(
                self,
                Type::Bool | Type::Char | Type::Short | Type::Int | Type::Long | Type::Enum
            )
    }

    /// Return whether the type is an unsigned integer type.
    pub fn is_unsigned_integer(&self) -> bool {
        matches!(self, Type::UChar | Type::UShort | Type::UInt | Type::ULong)
    }

    /// Return whether the type is a floating-point type.
    pub fn is_flonum(&self) -> bool {
        matches!(self, Type::Float | Type::Double)
    }
}

impl From<ConstType> for Type {
    fn from(value: ConstType) -> Self {
        match value {
            ConstType::Bool => Type::Bool,
            ConstType::Char => Type::Char,
            ConstType::UChar => Type::UChar,
            ConstType::Short => Type::Short,
            ConstType::UShort => Type::UShort,
            ConstType::Int => Type::Int,
            ConstType::UInt => Type::UInt,
            ConstType::Long => Type::Long,
            ConstType::ULong | ConstType::Ptr => Type::ULong,
            ConstType::Float => Type::Float,
            ConstType::Double => Type::Double,
        }
    }
}
