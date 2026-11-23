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

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Convert a Unix timestamp to local broken-down time.
fn localtime(secs: libc::time_t) -> Option<libc::tm> {
    let mut tm = std::mem::MaybeUninit::zeroed();
    let res = unsafe { libc::localtime_r(&secs, tm.as_mut_ptr()) };
    if res.is_null() {
        return None;
    }
    Some(unsafe { tm.assume_init() })
}

/// Get the current date and time representation.
///
/// This never panics, but if [`libc::localtime_r`] fails, this could produce
/// invalid/useless date and time (Jan 0 1900, 00:00:00).
pub fn datetime() -> (String, String) {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let tm = localtime(now).unwrap_or_else(|| unsafe { std::mem::zeroed() });
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

/// Format a Unix timestamp as an asctime string.
pub fn format_timestamp(secs: i64) -> Option<String> {
    let tm = localtime(secs)?;
    let day = DAYS.get(usize::try_from(tm.tm_wday).ok()?)?;
    let month = MONTHS.get(usize::try_from(tm.tm_mon).ok()?)?;
    Some(format!(
        "{day} {month} {:>2} {:02}:{:02}:{:02} {:04}",
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        tm.tm_year + 1900,
    ))
}
