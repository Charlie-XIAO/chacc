# Deferred Issue: Anonymous Globals Are Emitted As Public Symbols

## Summary

`chacc` currently emits `.globl` for every entry in the global variable table.
That includes compiler-generated anonymous globals such as string literals.

So code like:

```c
char *p = "foo";
```

currently produces assembly shaped like:

```asm
  .globl p
  .data
p:
  .quad .L..0
  .globl .L..0
  .data
.L..0:
  .byte 102
  .byte 111
  .byte 111
  .byte 0
```

The code works, but the `.globl .L..0` part is odd. Those anonymous labels
should stay local implementation details rather than exported symbols.

## Current Implementation

Relevant code:

- `src/parse.rs`
  - `create_global()`
  - `create_anon_global()`
- `src/codegen.rs`
  - `gen_globals()`

The parser represents both user-declared globals and compiler-generated
anonymous globals with the same `GlobalVar` struct. Codegen then emits
`.globl` unconditionally for every global symbol.

## Why This Was Deferred

The obvious quick fix would be to special-case names that start with `.L`.
That would work today, but it bakes naming conventions into visibility logic.

The cleaner fix is to add explicit linkage/visibility information to
`GlobalVar`, but that overlaps with later work on file-scope `static` and
other global-linkage behavior. It is better to solve those together instead of
adding a one-off flag now and reshaping it again shortly afterward.

## Recommended Fix

When global linkage semantics are revisited, make symbol visibility explicit in
the AST rather than inferring it from the emitted name.

For example, `GlobalVar` could eventually carry something like:

```rust
pub enum GlobalVisibility {
    Public,
    Local,
}
```

Then:

- normal global definitions use `Public`
- compiler-generated anonymous globals use `Local`
- future file-scope `static` globals also use `Local`

and `gen_globals()` emits `.globl` only for `Public` symbols.

## Important Constraint

When fixing this later:

- do not key visibility off a `.L` prefix
- keep anonymous globals working through the normal global table
- align the fix with eventual file-scope `static` support rather than treating
  string literals as a special one-off case
