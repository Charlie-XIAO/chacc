//! Compile-time constant evaluation.

use crate::error::Result;

/// The semantic types that can participate in compile-time evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstType {
    Bool,
    Char,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    Ptr,
}

impl ConstType {
    /// Return the width of the value domain in bits.
    pub fn width(&self) -> u32 {
        match self {
            Self::Bool | Self::Char | Self::UChar => 8,
            Self::Short | Self::UShort => 16,
            Self::Int | Self::UInt => 32,
            Self::Long | Self::ULong | Self::Ptr => 64,
        }
    }

    /// Return whether values of this type need to sign-extend.
    pub fn is_signed(&self) -> bool {
        matches!(self, Self::Char | Self::Short | Self::Int | Self::Long)
    }
}

/// A compile-time constant value.
///
/// All evaluations assume that C's integer promotions and usual arithmetic
/// conversions are already in place, which will not be performed again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConstValue {
    /// The raw bit-pattern of the value, masked to the width of the type.
    bits: u64,
    ty: ConstType,
}

impl ConstValue {
    /// Construct a compile-time constant from its raw bits.
    ///
    /// Note that this will normalize the bits to fit the width of `ty`, so that
    /// every [`ConstValue`] is canonical for this type.
    pub fn raw(mut bits: u64, ty: ConstType) -> Self {
        let width = ty.width();
        if width < 64 {
            bits &= (1u64 << width) - 1;
        }
        Self { bits, ty }
    }

    /// Return the semantic type of the constant.
    pub fn ty(self) -> ConstType {
        self.ty
    }

    /// Return the raw bit-pattern of the constant.
    pub fn bits(self) -> u64 {
        self.bits
    }

    /// Reinterpret the raw bit-pattern of the constant as a signed integer.
    ///
    /// For signed integer types, this sign-extends the raw bits from the type
    /// width to 64 bits, so that e.g. "0xff" in signed char becomes -1. For
    /// the other types, this directly reinterprets the raw bits as i64 so
    /// values beyond [`i64::MAX`] will be wrapped around.
    pub fn bits_as_signed(self) -> i64 {
        if !self.ty.is_signed() {
            return self.bits as i64;
        }

        let shift = 64 - self.ty.width();
        if shift == 0 {
            self.bits as i64
        } else {
            ((self.bits << shift) as i64) >> shift
        }
    }

    /// Return whether the constant evaluates to true (non-zero).
    pub fn is_true(self) -> bool {
        self.bits != 0
    }

    /// Cast to another [`ConstType`].
    pub fn cast(self, ty: ConstType) -> Self {
        if ty == ConstType::Bool {
            return Self::raw((self.bits != 0) as _, ty);
        }

        let bits = if self.ty.is_signed() {
            self.bits_as_signed() as _
        } else {
            self.bits
        };
        Self::raw(bits, ty)
    }

    pub fn neg(self, ty: ConstType) -> Self {
        Self::raw(self.bits.wrapping_neg(), ty)
    }

    pub fn not(self, ty: ConstType) -> Self {
        Self::raw((self.bits == 0) as _, ty)
    }

    pub fn bit_not(self, ty: ConstType) -> Self {
        Self::raw(!self.bits, ty)
    }

    pub fn and(self, other: impl FnOnce() -> Result<ConstValue>, ty: ConstType) -> Result<Self> {
        Ok(Self::raw((self.bits != 0 && other()?.bits != 0) as _, ty))
    }

    pub fn or(self, other: impl FnOnce() -> Result<ConstValue>, ty: ConstType) -> Result<Self> {
        Ok(Self::raw((self.bits != 0 || other()?.bits != 0) as _, ty))
    }

    pub fn add(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw(self.bits.wrapping_add(other.bits), ty)
    }

    pub fn sub(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw(self.bits.wrapping_sub(other.bits), ty)
    }

    pub fn mul(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw(self.bits.wrapping_mul(other.bits), ty)
    }

    pub fn div(self, other: ConstValue, ty: ConstType) -> Option<Self> {
        if other.bits == 0 {
            return None;
        }
        Some(Self::raw(
            if self.ty.is_signed() {
                self.bits_as_signed().wrapping_div(other.bits_as_signed()) as _
            } else {
                self.bits.wrapping_div(other.bits)
            },
            ty,
        ))
    }

    pub fn rem(self, other: ConstValue, ty: ConstType) -> Option<Self> {
        if other.bits == 0 {
            return None;
        }
        Some(Self::raw(
            if self.ty.is_signed() {
                self.bits_as_signed().wrapping_rem(other.bits_as_signed()) as _
            } else {
                self.bits.wrapping_rem(other.bits)
            },
            ty,
        ))
    }

    pub fn bit_and(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw(self.bits & other.bits, ty)
    }

    pub fn bit_or(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw(self.bits | other.bits, ty)
    }

    pub fn bit_xor(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw(self.bits ^ other.bits, ty)
    }

    pub fn bit_shl(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw(self.bits.wrapping_shl(other.bits_as_signed() as _), ty)
    }

    pub fn bit_shr(self, other: ConstValue, ty: ConstType) -> Self {
        let bits = if self.ty.is_signed() {
            self.bits_as_signed()
                .wrapping_shr(other.bits_as_signed() as _) as u64
        } else {
            self.bits.wrapping_shr(other.bits_as_signed() as _)
        };
        Self::raw(bits, ty)
    }

    pub fn eq(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw((self.bits == other.bits) as _, ty)
    }

    pub fn ne(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw((self.bits != other.bits) as _, ty)
    }

    pub fn lt(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw(
            if self.ty.is_signed() {
                self.bits_as_signed() < other.bits_as_signed()
            } else {
                self.bits < other.bits
            } as _,
            ty,
        )
    }

    pub fn le(self, other: ConstValue, ty: ConstType) -> Self {
        Self::raw(
            if self.ty.is_signed() {
                self.bits_as_signed() <= other.bits_as_signed()
            } else {
                self.bits <= other.bits
            } as _,
            ty,
        )
    }
}

impl TryFrom<ConstValue> for i64 {
    type Error = ();

    fn try_from(value: ConstValue) -> Result<Self, Self::Error> {
        if value.ty.is_signed() {
            Ok(value.bits_as_signed())
        } else {
            i64::try_from(value.bits).map_err(|_| ())
        }
    }
}

impl TryFrom<ConstValue> for usize {
    type Error = ();

    fn try_from(value: ConstValue) -> Result<Self, Self::Error> {
        if value.ty.is_signed() {
            usize::try_from(value.bits_as_signed()).map_err(|_| ())
        } else {
            usize::try_from(value.bits).map_err(|_| ())
        }
    }
}
