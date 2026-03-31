//! Shared utilities across multiple components.

/// The width-specific spellings of an integer argument register.
pub struct GpArgReg {
    pub b8: &'static str,
    pub b32: &'static str,
    pub b64: &'static str,
}

/// The first six [`GpArgReg`] in calling convention order.
pub const GP_ARG_REGS: [GpArgReg; 6] = [
    GpArgReg {
        b8: "%dil",
        b32: "%edi",
        b64: "%rdi",
    },
    GpArgReg {
        b8: "%sil",
        b32: "%esi",
        b64: "%rsi",
    },
    GpArgReg {
        b8: "%dl",
        b32: "%edx",
        b64: "%rdx",
    },
    GpArgReg {
        b8: "%cl",
        b32: "%ecx",
        b64: "%rcx",
    },
    GpArgReg {
        b8: "%r8b",
        b32: "%r8d",
        b64: "%r8",
    },
    GpArgReg {
        b8: "%r9b",
        b32: "%r9d",
        b64: "%r9",
    },
];

/// Round `n` up to the nearest multiple of `align`.
pub const fn align_to(n: i64, align: i64) -> i64 {
    assert!(align > 0, "align must be positive");

    if (align & (align - 1)) == 0 {
        // Fast path when align is power of 2; if align is provided as a compile
        // time constant, we can expect the compiler to optimize this branching
        // away so this would effectively be no runtime cost
        (n + align - 1) & !(align - 1)
    } else {
        (n + align - 1) / align * align
    }
}
