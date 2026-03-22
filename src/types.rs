//! The type system for expressions.

/// Expression types used for semantic analysis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Type {
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
    /// Return a pointer to the given base type.
    pub fn ptr(base: Type) -> Self {
        Self::Ptr(Box::new(base))
    }

    /// Return a function type with the given return type.
    pub fn func(return_ty: Type, params: Vec<Type>) -> Self {
        Self::Func {
            return_ty: Box::new(return_ty),
            params,
        }
    }

    /// Return an array type with the given element type and length.
    pub fn array(base: Type, len: usize) -> Self {
        Self::Array {
            base: Box::new(base),
            len,
        }
    }

    /// Return whether the type is an integer.
    pub fn is_int(&self) -> bool {
        matches!(self, Self::Int)
    }

    /// Return the element type for pointers and arrays.
    ///
    /// This returns `None` if the type has no element type.
    pub fn base(&self) -> Option<&Type> {
        match self {
            Self::Ptr(base) => Some(base),
            Self::Array { base, .. } => Some(base),
            Self::Int | Self::Func { .. } => None,
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
            Self::Int => 8,
            Self::Ptr(_) => 8,
            Self::Array { base, len } => base.size() * (*len as i64),
            Self::Func { .. } => unreachable!("function types do not have a size"),
        }
    }
}
