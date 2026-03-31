//! Shared utilities across multiple components.

/// The maximum number of function arguments supported.
pub const MAX_FUNC_PARAMS: usize = 6;

/// Round `n` up to the nearest multiple of `align`.
pub const fn align_to(n: i64, align: i64) -> i64 {
    debug_assert!(align > 0, "align must be positive");

    if (align & (align - 1)) == 0 {
        // Fast path when align is power of 2; if align is provided as a compile
        // time constant, we can expect the compiler to optimize this branching
        // away so this would effectively be no runtime cost
        (n + align - 1) & !(align - 1)
    } else {
        (n + align - 1) / align * align
    }
}
