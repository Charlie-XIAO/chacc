//! The type system for expressions.

/// Expression types used for semantic analysis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Type {
    Int,
    Ptr(Box<Type>),
    Func(Box<Type>),
}

impl Type {
    /// Return a pointer to the given base type.
    pub fn ptr(base: Type) -> Self {
        Self::Ptr(Box::new(base))
    }

    /// Return a function type with the given return type.
    pub fn func(return_ty: Type) -> Self {
        Self::Func(Box::new(return_ty))
    }

    /// Return whether the type is an integer.
    pub fn is_int(&self) -> bool {
        matches!(self, Self::Int)
    }

    /// Return the base type for pointers.
    ///
    /// This returns `None` if the type is not a pointer.
    pub fn base(&self) -> Option<&Type> {
        match self {
            Self::Ptr(base) => Some(base),
            Self::Int | Self::Func(_) => None,
        }
    }

    /// Return whether the type is a function.
    pub fn is_func(&self) -> bool {
        matches!(self, Self::Func(_))
    }

    /// Return the size of the type in bytes.
    pub fn size(&self) -> i64 {
        match self {
            Self::Int => 8,
            Self::Ptr(_) => 8,
            Self::Func(_) => unreachable!("function types do not have a size"),
        }
    }
}
