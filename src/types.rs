//! The type system for expressions.

/// Expression types used for semantic analysis.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub enum Type {
    #[default] // Use Type::default() as a dummy type
    Char,
    Int,
    Ptr(Box<Type>),
    Array {
        base: Box<Type>,
        len: usize,
    },
    Func {
        return_ty: Box<Type>,
        params: Vec<Type>,
    },
}

impl Type {
    /// Construct a pointer type to the given base type.
    pub fn ptr(base: Type) -> Self {
        Self::Ptr(Box::new(base))
    }

    /// Construct a function type with the given return type and parameters.
    pub fn func(return_ty: Type, params: Vec<Type>) -> Self {
        Self::Func {
            return_ty: Box::new(return_ty),
            params,
        }
    }

    /// Construct an array type with the given element type and length.
    pub fn array(base: Type, len: usize) -> Self {
        Self::Array {
            base: Box::new(base),
            len,
        }
    }

    /// Return whether the type is an integer data type.
    pub fn is_int(&self) -> bool {
        matches!(self, Self::Char | Self::Int)
    }

    /// Return the base type for arrays and pointers.
    pub fn base(&self) -> Option<&Type> {
        match self {
            Self::Ptr(base) => Some(base),
            Self::Array { base, .. } => Some(base),
            _ => None,
        }
    }

    /// Return whether the type is a function.
    pub fn is_func(&self) -> bool {
        matches!(self, Self::Func { .. })
    }

    /// Return whether the type is an array.
    pub fn is_array(&self) -> bool {
        matches!(self, Self::Array { .. })
    }

    /// Return the size of the type in bytes.
    pub fn size(&self) -> i64 {
        match self {
            Self::Char => 1,
            Self::Int => 8,
            Self::Ptr(_) => 8,
            Self::Array { base, len } => base.size() * (*len as i64),
            Self::Func { .. } => unreachable!("function types do not have a size"),
        }
    }
}
