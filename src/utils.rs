//! Shared utilities across multiple components.

/// The maximum number of function arguments supported.
pub const MAX_FUNC_PARAMS: usize = 6;

/// The byte size of the hidden `__va_area__` for variadic functions.
///
/// - 24 bytes for the `__va_elem` bookkeeping struct;
/// - 6 general-purpose arg registers * 8 bytes;
/// - 8 floating-point arg registers * 8 bytes.
pub const VA_AREA_SIZE: usize = 136;

/// Round `n` up to the nearest multiple of `align`.
pub const fn align_to(n: u64, align: u64) -> u64 {
    debug_assert!(align > 0, "align must be positive");

    if (align & (align - 1)) == 0 {
        // Fast path when align is power of 2; if align is provided as a compile
        // time constant, we can expect the compiler to optimize this branching
        // away so this would effectively be no runtime cost
        (n + align - 1) & !(align - 1)
    } else {
        n.div_ceil(align) * align
    }
}
