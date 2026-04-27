//! Compile-time constant evaluation.

use crate::error::Result;
use crate::types::ConstType;

/// A raw compile-time constant value representation.
#[derive(Clone, Copy, Debug)]
enum RawConstValue {
    /// An integer-like constant value.
    ///
    /// This stores the raw bits because C integer semantics are fundamentally
    /// width-based.
    Int(u64),
    /// A floating-point constant value.
    ///
    /// This stores the semantic numeric value and converts back to IEEE-754
    /// storage bits only at serialization boundaries.
    Float(f64),
}

impl PartialEq for RawConstValue {
    fn eq(&self, other: &Self) -> bool {
        match (*self, *other) {
            (Self::Int(lhs), Self::Int(rhs)) => lhs == rhs,
            (Self::Float(lhs), Self::Float(rhs)) => lhs.to_bits() == rhs.to_bits(),
            _ => false,
        }
    }
}

impl Eq for RawConstValue {}

/// A compile-time constant value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConstValue {
    raw: RawConstValue,
    pub ty: ConstType,
}

impl ConstValue {
    /// Construct a compile-time integer constant from its raw bits.
    ///
    /// Note that this will normalize the bits to fit the width of `ty`, so that
    /// every [`ConstValue`] is canonical for this type.
    pub fn int(mut bits: u64, ty: ConstType) -> Self {
        debug_assert!(!ty.is_flonum());

        let width = ty.width();
        if width < 64 {
            bits &= (1u64 << width) - 1;
        }
        Self {
            raw: RawConstValue::Int(bits),
            ty,
        }
    }

    /// Construct a compile-time boolean const.
    fn bool(value: bool, ty: ConstType) -> Self {
        Self::int(value as _, ty)
    }

    /// Construct a compile-time floating-point constant.
    ///
    /// If the type is float, the value is rounded to the nearest representable
    /// f32 immediately so that the stored semantic value matches the precision
    /// of the source type.
    pub fn float(mut value: f64, ty: ConstType) -> Self {
        debug_assert!(ty.is_flonum());

        if ty == ConstType::FLOAT {
            value = (value as f32) as f64;
        }
        Self {
            raw: RawConstValue::Float(value),
            ty,
        }
    }

    /// Return the storage bit-pattern of the constant.
    pub fn bits(self) -> u64 {
        match self.raw {
            RawConstValue::Int(bits) => bits,
            RawConstValue::Float(value) => match self.ty {
                ConstType::FLOAT => (value as f32).to_bits() as u64,
                ConstType::DOUBLE => value.to_bits(),
                _ => unreachable!(),
            },
        }
    }

    /// Return the storage bit-pattern of the integer constant.
    ///
    /// This panics if called on a floating-point constant.
    fn int_bits(self) -> u64 {
        match self.raw {
            RawConstValue::Int(bits) => bits,
            RawConstValue::Float(_) => {
                unreachable!("expected integer constant but got floating-point constant")
            },
        }
    }

    /// Reinterpret the storage bit-pattern of an integer constant as signed.
    ///
    /// For signed integer types, this sign-extends the raw bits from the type
    /// width to 64 bits, so that e.g. "0xff" in signed char becomes -1. For
    /// the other integer-like types, this directly reinterprets the raw bits as
    /// i64 so values beyond [`i64::MAX`] will be wrapped around. This panics if
    /// called on a floating-point constant.
    pub fn int_bits_as_signed(self) -> i64 {
        debug_assert_ne!(self.ty, ConstType::Ptr);

        let bits = self.int_bits();
        if !self.ty.is_signed() {
            return bits as i64;
        }

        let shift = 64 - self.ty.width();
        if shift == 0 {
            bits as i64
        } else {
            ((bits << shift) as i64) >> shift
        }
    }

    /// Cast to another [`ConstType`].
    pub fn cast(self, ty: ConstType) -> Self {
        if ty == ConstType::BOOL {
            return Self::bool(self.into(), ty);
        }
        if ty.is_flonum() {
            return Self::float(self.into(), ty);
        }

        let bits = match self.raw {
            RawConstValue::Int(_) if self.ty.is_signed() => self.int_bits_as_signed() as u64,
            RawConstValue::Int(bits) => bits,
            RawConstValue::Float(value) if ty.is_signed() => value as i64 as u64,
            RawConstValue::Float(value) => value as u64,
        };
        Self::int(bits, ty)
    }

    pub fn neg(self, ty: ConstType) -> Self {
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        if ty.is_flonum() {
            Self::float(-f64::from(self), ty)
        } else {
            Self::int(self.int_bits().wrapping_neg(), ty)
        }
    }

    pub fn not(self, ty: ConstType) -> Self {
        Self::bool(!bool::from(self), ty)
    }

    pub fn bit_not(self, ty: ConstType) -> Self {
        debug_assert!(!self.ty.is_flonum());
        debug_assert!(!ty.is_flonum());
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        Self::int(!self.int_bits(), ty)
    }

    pub fn and(self, other: impl FnOnce() -> Result<ConstValue>, ty: ConstType) -> Result<Self> {
        Ok(Self::bool(self.into() && other()?.into(), ty))
    }

    pub fn or(self, other: impl FnOnce() -> Result<ConstValue>, ty: ConstType) -> Result<Self> {
        Ok(Self::bool(self.into() || other()?.into(), ty))
    }

    pub fn add(self, other: ConstValue, ty: ConstType) -> Self {
        if ty.is_flonum() {
            Self::float(f64::from(self) + f64::from(other), ty)
        } else {
            Self::int(self.int_bits().wrapping_add(other.int_bits()), ty)
        }
    }

    pub fn sub(self, other: ConstValue, ty: ConstType) -> Self {
        if ty.is_flonum() {
            Self::float(f64::from(self) - f64::from(other), ty)
        } else {
            Self::int(self.int_bits().wrapping_sub(other.int_bits()), ty)
        }
    }

    pub fn mul(self, other: ConstValue, ty: ConstType) -> Self {
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(other.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        if ty.is_flonum() {
            Self::float(f64::from(self) * f64::from(other), ty)
        } else {
            Self::int(self.int_bits().wrapping_mul(other.int_bits()), ty)
        }
    }

    pub fn div(self, other: ConstValue, ty: ConstType) -> Option<Self> {
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(other.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        if ty.is_flonum() {
            return Some(Self::float(f64::from(self) / f64::from(other), ty));
        }

        let other_bits = other.int_bits();
        if other_bits == 0 {
            return None;
        }

        Some(Self::int(
            if self.ty.is_signed() {
                self.int_bits_as_signed()
                    .wrapping_div(other.int_bits_as_signed()) as u64
            } else {
                self.int_bits().wrapping_div(other_bits)
            },
            ty,
        ))
    }

    pub fn rem(self, other: ConstValue, ty: ConstType) -> Option<Self> {
        debug_assert!(!self.ty.is_flonum());
        debug_assert!(!other.ty.is_flonum());
        debug_assert!(!ty.is_flonum());
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(other.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        let other_bits = other.int_bits();
        if other_bits == 0 {
            return None;
        }

        Some(Self::int(
            if self.ty.is_signed() {
                self.int_bits_as_signed()
                    .wrapping_rem(other.int_bits_as_signed()) as u64
            } else {
                self.int_bits().wrapping_rem(other_bits)
            },
            ty,
        ))
    }

    pub fn bit_and(self, other: ConstValue, ty: ConstType) -> Self {
        debug_assert!(!self.ty.is_flonum());
        debug_assert!(!other.ty.is_flonum());
        debug_assert!(!ty.is_flonum());
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(other.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        Self::int(self.int_bits() & other.int_bits(), ty)
    }

    pub fn bit_or(self, other: ConstValue, ty: ConstType) -> Self {
        debug_assert!(!self.ty.is_flonum());
        debug_assert!(!other.ty.is_flonum());
        debug_assert!(!ty.is_flonum());
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(other.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        Self::int(self.int_bits() | other.int_bits(), ty)
    }

    pub fn bit_xor(self, other: ConstValue, ty: ConstType) -> Self {
        debug_assert!(!self.ty.is_flonum());
        debug_assert!(!other.ty.is_flonum());
        debug_assert!(!ty.is_flonum());
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(other.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        Self::int(self.int_bits() ^ other.int_bits(), ty)
    }

    pub fn bit_shl(self, other: ConstValue, ty: ConstType) -> Self {
        debug_assert!(!self.ty.is_flonum());
        debug_assert!(!other.ty.is_flonum());
        debug_assert!(!ty.is_flonum());
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(other.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        Self::int(
            self.int_bits()
                .wrapping_shl(other.int_bits_as_signed() as _),
            ty,
        )
    }

    pub fn bit_shr(self, other: ConstValue, ty: ConstType) -> Self {
        debug_assert!(!self.ty.is_flonum());
        debug_assert!(!other.ty.is_flonum());
        debug_assert!(!ty.is_flonum());
        debug_assert_ne!(self.ty, ConstType::Ptr);
        debug_assert_ne!(other.ty, ConstType::Ptr);
        debug_assert_ne!(ty, ConstType::Ptr);

        let bits = if self.ty.is_signed() {
            self.int_bits_as_signed()
                .wrapping_shr(other.int_bits_as_signed() as _) as u64
        } else {
            self.int_bits()
                .wrapping_shr(other.int_bits_as_signed() as _)
        };
        Self::int(bits, ty)
    }

    pub fn eq(self, other: ConstValue, ty: ConstType) -> Self {
        Self::bool(
            if self.ty.is_flonum() || other.ty.is_flonum() {
                f64::from(self) == f64::from(other)
            } else {
                self.int_bits() == other.int_bits()
            },
            ty,
        )
    }

    pub fn ne(self, other: ConstValue, ty: ConstType) -> Self {
        Self::bool(
            if self.ty.is_flonum() || other.ty.is_flonum() {
                f64::from(self) != f64::from(other)
            } else {
                self.int_bits() != other.int_bits()
            },
            ty,
        )
    }

    pub fn lt(self, other: ConstValue, ty: ConstType) -> Self {
        Self::bool(
            if self.ty.is_flonum() || other.ty.is_flonum() {
                f64::from(self) < f64::from(other)
            } else if self.ty.is_signed() {
                self.int_bits_as_signed() < other.int_bits_as_signed()
            } else {
                self.int_bits() < other.int_bits()
            },
            ty,
        )
    }

    pub fn le(self, other: ConstValue, ty: ConstType) -> Self {
        Self::bool(
            if self.ty.is_flonum() || other.ty.is_flonum() {
                f64::from(self) <= f64::from(other)
            } else if self.ty.is_signed() {
                self.int_bits_as_signed() <= other.int_bits_as_signed()
            } else {
                self.int_bits() <= other.int_bits()
            },
            ty,
        )
    }
}

impl From<ConstValue> for bool {
    fn from(value: ConstValue) -> Self {
        match value.raw {
            RawConstValue::Int(bits) => bits != 0,
            RawConstValue::Float(value) => value != 0.0,
        }
    }
}

impl From<ConstValue> for f64 {
    fn from(value: ConstValue) -> Self {
        debug_assert_ne!(value.ty, ConstType::Ptr);

        match value.raw {
            RawConstValue::Float(value) => value,
            RawConstValue::Int(bits) => {
                if value.ty.is_signed() {
                    value.int_bits_as_signed() as f64
                } else {
                    bits as f64
                }
            },
        }
    }
}

impl TryFrom<ConstValue> for i64 {
    type Error = ();

    fn try_from(value: ConstValue) -> Result<Self, Self::Error> {
        match value.raw {
            RawConstValue::Float(_) => Err(()),
            RawConstValue::Int(_) if value.ty.is_signed() => Ok(value.int_bits_as_signed()),
            RawConstValue::Int(_) => i64::try_from(value.bits()).map_err(|_| ()),
        }
    }
}

impl TryFrom<ConstValue> for u64 {
    type Error = ();

    fn try_from(value: ConstValue) -> Result<Self, Self::Error> {
        match value.raw {
            RawConstValue::Float(_) => Err(()),
            RawConstValue::Int(_) if value.ty.is_signed() => {
                u64::try_from(value.int_bits_as_signed()).map_err(|_| ())
            },
            RawConstValue::Int(_) => Ok(value.bits()),
        }
    }
}

impl TryFrom<ConstValue> for usize {
    type Error = ();

    fn try_from(value: ConstValue) -> Result<Self, Self::Error> {
        match value.raw {
            RawConstValue::Float(_) => Err(()),
            RawConstValue::Int(_) if value.ty.is_signed() => {
                usize::try_from(value.int_bits_as_signed()).map_err(|_| ())
            },
            RawConstValue::Int(_) => usize::try_from(value.bits()).map_err(|_| ()),
        }
    }
}
