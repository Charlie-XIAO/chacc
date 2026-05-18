//! The type system for expressions.

use std::cmp::Ordering;
use std::rc::Rc;

use smol_str::SmolStr;

use crate::utils::align_to;

/// Arithmetic types.
///
/// **Note:** This is not intended for direct use. Use [`Type`] or [`ConstType`]
/// instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithType {
    Bool,
    Char,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    Float,
    Double,
}

impl ArithType {
    const fn size(self) -> u64 {
        match self {
            Self::Bool | Self::Char | Self::UChar => 1,
            Self::Short | Self::UShort => 2,
            Self::Int | Self::UInt | Self::Float => 4,
            Self::Long | Self::ULong | Self::Double => 8,
        }
    }

    const fn is_signed(self) -> bool {
        matches!(self, Self::Char | Self::Short | Self::Int | Self::Long)
    }

    const fn is_unsigned(self) -> bool {
        matches!(self, Self::UChar | Self::UShort | Self::UInt | Self::ULong)
    }

    const fn is_flonum(self) -> bool {
        matches!(self, Self::Float | Self::Double)
    }

    const fn promote_int(self) -> Option<Self> {
        match self {
            Self::Bool | Self::Char | Self::UChar | Self::Short | Self::UShort | Self::Int => {
                Some(Self::Int)
            },
            Self::UInt | Self::Long | Self::ULong => Some(self),
            _ => None,
        }
    }

    fn coerce(self, other: Self) -> Self {
        if matches!(self, Self::Double) || matches!(other, Self::Double) {
            return Self::Double;
        }
        if matches!(self, Self::Float) || matches!(other, Self::Float) {
            return Self::Float;
        }

        let lhs = self.promote_int().unwrap_or(self);
        let rhs = other.promote_int().unwrap_or(other);

        match lhs.size().cmp(&rhs.size()) {
            Ordering::Less => return rhs,
            Ordering::Greater => return lhs,
            Ordering::Equal => {},
        }

        if rhs.is_unsigned() { rhs } else { lhs }
    }
}

/// Dispatch methods on [`ArithType`].
///
/// `enum_ty`, if provided, will be treated the same as [`ArithType::Int`].
macro_rules! impl_scalar_type {
    ($ty:ty $(, enum_ty = $enum_ty:ident)?) => {
        impl $ty {
            pub const BOOL: Self = Self::Arith(ArithType::Bool);
            pub const CHAR: Self = Self::Arith(ArithType::Char);
            pub const UCHAR: Self = Self::Arith(ArithType::UChar);
            pub const SHORT: Self = Self::Arith(ArithType::Short);
            pub const USHORT: Self = Self::Arith(ArithType::UShort);
            pub const INT: Self = Self::Arith(ArithType::Int);
            pub const UINT: Self = Self::Arith(ArithType::UInt);
            pub const LONG: Self = Self::Arith(ArithType::Long);
            pub const ULONG: Self = Self::Arith(ArithType::ULong);
            pub const FLOAT: Self = Self::Arith(ArithType::Float);
            pub const DOUBLE: Self = Self::Arith(ArithType::Double);

            const fn enum_as_int(self) -> Self {
                match self {
                    $(Self::$enum_ty => Self::INT,)?
                    _ => self,
                }
            }

            /// Return whether this is a signed integer type.
            pub const fn is_signed(self) -> bool {
                matches!(self.enum_as_int(), Self::Arith(arith) if arith.is_signed())
            }

            /// Return whether this is an unsigned integer type.
            ///
            /// Note that `_Bool` is intentionally excluded by this method,
            /// because it does not follow the standard unsigned integer
            /// arithmetics.
            pub const fn is_unsigned(self) -> bool {
                matches!(self, Self::Arith(arith) if arith.is_unsigned())
            }

            /// Return whether this is an integer type.
            ///
            /// This includes signed, unsigned, and `_Bool`.
            pub const fn is_integer(self) -> bool {
                self.is_signed() || self.is_unsigned() || matches!(self, Self::BOOL)
            }

            /// Return whether this is a floating-point type.
            pub const fn is_flonum(self) -> bool {
                matches!(self, Self::Arith(arith) if arith.is_flonum())
            }

            /// Return whether this is an arithmetic type.
            ///
            /// This includes both integer and floating-point types.
            pub const fn is_arith(self) -> bool {
                self.is_integer() || self.is_flonum()
            }

            /// Return the resulting type after [integer promotions][1].
            ///
            /// If the type is not an integer type, this returns `None`.
            ///
            /// [1]: https://en.cppreference.com/w/c/language/conversion.html#Integer_promotions
            pub const fn promote_int(self) -> Option<Self> {
                match self.enum_as_int() {
                    Self::Arith(arith) => match arith.promote_int() {
                        Some(promoted) => Some(Self::Arith(promoted)),
                        None => None,
                    },
                    _ => None,
                }
            }

            /// Coerce with another type via [usual arithmetic conversions][1].
            ///
            /// [1]: https://en.cppreference.com/cpp/language/usual_arithmetic_conversions
            pub fn coerce(self, other: Self) -> Option<Self> {
                match (self.enum_as_int(), other.enum_as_int()) {
                    (Self::Arith(lhs), Self::Arith(rhs)) => Some(Self::Arith(lhs.coerce(rhs))),
                    _ => None,
                }
            }
        }
    };
}

/// Expression types that can be evaluated at compile-time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstType {
    Arith(ArithType),
    Ptr,
}

impl_scalar_type!(ConstType);

impl ConstType {
    /// Return the width of this type in bits.
    pub const fn width(self) -> u64 {
        match self {
            Self::Arith(arith) => arith.size() * 8,
            Self::Ptr => 64,
        }
    }
}

impl TryFrom<Type> for ConstType {
    type Error = ();

    fn try_from(ty: Type) -> Result<Self, Self::Error> {
        match ty {
            Type::Arith(arith) => Ok(Self::Arith(arith)),
            Type::Enum => Ok(Self::INT),
            _ => Err(()),
        }
    }
}

/// Expression types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Type {
    #[default]
    Dummy,
    Void,
    Enum,
    Arith(ArithType),
    /// A non-trivial type stored in [`TypeStore`].
    Stored(usize),
}

impl_scalar_type!(Type, enum_ty = Enum);

impl From<ConstType> for Type {
    fn from(ty: ConstType) -> Self {
        match ty {
            ConstType::Arith(arith) => Self::Arith(arith),
            ConstType::Ptr => Self::ULONG,
        }
    }
}

#[derive(Debug, Clone)]
struct TypeData {
    kind: TypeDataKind,
    size: u64,
    align: u64,
}

#[derive(Debug, Clone)]
enum TypeDataKind {
    Ptr(Type),
    Array(ArrayTypeData),
    Func(FuncTypeData),
    StructOrUnion(StructOrUnionTypeData),
}

#[derive(Debug, Clone)]
pub struct ArrayTypeData {
    pub base: Type,
    /// If `None`, this represents an incomplete array type.
    pub len: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct FuncTypeData {
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
pub struct StructOrUnionTypeData {
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

    fn push(&mut self, kind: TypeDataKind, size: u64, align: u64) -> Type {
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
        self.push(TypeDataKind::Ptr(base), 8, 8)
    }

    /// Construct a function type with the given return type and parameters.
    pub fn func(&mut self, return_ty: Type, params: Vec<Type>, is_variadic: bool) -> Type {
        self.push(
            TypeDataKind::Func(FuncTypeData {
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
        self.push(
            TypeDataKind::Array(ArrayTypeData { base, len }),
            size,
            align,
        )
    }

    /// Construct a struct or union type with the given members.
    ///
    /// If `members` is `None`, this represents an incomplete struct or union
    /// type. Otherwise, for a struct, the member offsets will be assigned here
    /// so they do not need to be pre-computed. For a union, the member offsets
    /// must be all 0.
    pub fn struct_or_union(&mut self, is_struct: bool, members: Option<Vec<Member>>) -> Type {
        let ty = self.push(
            TypeDataKind::StructOrUnion(StructOrUnionTypeData {
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
        let TypeDataKind::StructOrUnion(sou) = &mut data.kind else {
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
            Type::Enum => ArithType::Int.size(),
            Type::Arith(arith) => arith.size(),
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
            Type::Enum => ArithType::Int.size(),
            Type::Arith(arith) => arith.size(),
            Type::Stored(_) => self.get(ty).unwrap().size,
        }
    }

    /// Return the base type for arrays and pointers.
    pub fn base(&self, ty: Type) -> Option<Type> {
        let data = self.get(ty)?;
        match &data.kind {
            TypeDataKind::Ptr(base) => Some(*base),
            TypeDataKind::Array(ArrayTypeData { base, .. }) => Some(*base),
            _ => None,
        }
    }

    /// Return the array type if it is one.
    pub fn as_array(&self, ty: Type) -> Option<&ArrayTypeData> {
        let data = self.get(ty)?;
        match &data.kind {
            TypeDataKind::Array(array) => Some(array),
            _ => None,
        }
    }

    /// Return the function type if it is one.
    ///
    /// If `accept_ptr` is true, a function pointer type is also accepted and
    /// the pointed function type is returned.
    pub fn as_func(&self, ty: Type, accept_ptr: bool) -> Option<&FuncTypeData> {
        let data = self.get(ty)?;
        match &data.kind {
            TypeDataKind::Func(func) => Some(func),
            _ if accept_ptr => self.base(ty).and_then(|base| self.as_func(base, false)),
            _ => None,
        }
    }

    /// Return the struct or union type if it is one.
    pub fn as_struct_or_union(&self, ty: Type) -> Option<&StructOrUnionTypeData> {
        let data = self.get(ty)?;
        match &data.kind {
            TypeDataKind::StructOrUnion(sou) => Some(sou),
            _ => None,
        }
    }

    /// Return whether the type is a pointer.
    pub fn is_ptr(&self, ty: Type) -> bool {
        self.get(ty)
            .is_some_and(|data| matches!(data.kind, TypeDataKind::Ptr(_)))
    }

    /// Return whether the type is a function.
    pub fn is_func(&self, ty: Type) -> bool {
        self.get(ty)
            .is_some_and(|data| matches!(data.kind, TypeDataKind::Func(_)))
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
            TypeDataKind::Array(ArrayTypeData { len: None, .. })
                | TypeDataKind::StructOrUnion(StructOrUnionTypeData { members: None, .. })
        )
    }

    /// Return whether the byte chunk `lo..hi` should be passed in fp register.
    ///
    /// A chunk is passed in a floating-point register if and only if the type
    /// is itself a floating-point type, or it is an aggregate type with all
    /// fields overlapping that byte range being floating-point types.
    ///
    /// Callers must pass 0 for `offset`; it is used internally for recursion.
    pub fn is_fp_chunk(&self, ty: Type, lo: u64, hi: u64, offset: u64) -> bool {
        if let Some(sou) = self.as_struct_or_union(ty) {
            let Some(members) = &sou.members else {
                return false;
            };
            return members
                .iter()
                .all(|member| self.is_fp_chunk(member.ty, lo, hi, offset + member.offset as u64));
        }

        if let Some(array) = self.as_array(ty) {
            let Some(len) = array.len else {
                return false;
            };
            let base_size = self.size(array.base);
            return (0..len)
                .all(|i| self.is_fp_chunk(array.base, lo, hi, offset + base_size * i as u64));
        }

        offset < lo || offset >= hi || ty.is_flonum()
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
            (TypeDataKind::Ptr(left), TypeDataKind::Ptr(right)) => self.same_type(*left, *right),
            (TypeDataKind::Array(left), TypeDataKind::Array(right)) => {
                left.len == right.len && self.same_type(left.base, right.base)
            },
            (TypeDataKind::Func(left), TypeDataKind::Func(right)) => {
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

    /// Coerce two operand types via [usual arithmetic conversions][1].
    ///
    /// This method does not try to enforce any pointer/function compatibility
    /// rules. It is the caller's responsibility to decide that [usual
    /// arithmetic conversions][1] is the right thing to do.
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

        lhs.coerce(rhs).unwrap_or(lhs)
    }

    /// Convert a [`Type`] to a [`ConstType`] if applicable.
    pub fn to_const(&self, ty: Type) -> Option<ConstType> {
        ConstType::try_from(ty)
            .ok()
            .or_else(|| self.is_ptr(ty).then_some(ConstType::Ptr))
    }
}
