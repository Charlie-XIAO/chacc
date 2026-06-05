//! Shared utilities across multiple components.

/// The maximum number of general-purpose argument registers supported.
pub const MAX_GP_ARG_REGS: usize = 6;

/// The maximum number of floating-point argument registers supported.
pub const MAX_FP_ARG_REGS: usize = 8;

/// The byte size of the hidden `__va_area__` for variadic functions.
///
/// - 24 bytes for the `__va_elem` bookkeeping struct;
/// - 8 bytes per general-purpose arg register;
/// - 8 bytes per floating-point arg register.
pub const VA_AREA_SIZE: usize = 24 + MAX_GP_ARG_REGS * 8 + MAX_FP_ARG_REGS * 8;

/// Round `n` up to the nearest multiple of `align`.
pub const fn align_up_to(n: u64, align: u64) -> u64 {
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

/// Round `n` down to the nearest multiple of `align`.
pub const fn align_down_to(n: u64, align: u64) -> u64 {
    debug_assert!(align > 0, "align must be positive");

    if (align & (align - 1)) == 0 {
        // Fast path when align is power of 2, similar to above
        n & !(align - 1)
    } else {
        n / align * align
    }
}

/// Get the current date and time representation.
///
/// This never panics, but if [`libc::localtime_r`] fails, this could produce
/// invalid/useless date and time (Jan 0 1900, 00:00:00).
pub fn datetime() -> (String, String) {
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm = std::mem::MaybeUninit::zeroed();
        libc::localtime_r(&now, tm.as_mut_ptr());
        tm.assume_init()
    };

    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    (
        format!(
            "{} {:>2} {:04}",
            MONTHS[tm.tm_mon as usize],
            tm.tm_mday,
            tm.tm_year + 1900,
        ),
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
    )
}
