# Deferred Area: Ignored Qualifiers And Specifiers

Introduced in: `957bf645a998867e83409047c68080a72858c586`

## Summary

This note tracks declaration keywords that `chacc` either:

- parses and ignores,
- parses with light validation and then ignores,
- or has intentionally deferred to a later batch.

The current implementation is pragmatic rather than complete. The parser accepts
useful real-world syntax first, and only models semantics when the type system
or declaration machinery is ready for it.

## `auto`

### Current Status

- parsed
- context-validated
- ignored afterward

### What Is Implemented

- accepted in block-scope declarations
- accepted in `for`-initializer declarations
- rejected in obviously invalid contexts such as file scope

### Why This Is Enough For Now

`auto` adds no semantic value for this compiler. At block scope it is just the
default storage duration already used by ordinary local objects.

### Future Plan

No further implementation is planned unless a later compatibility goal makes it
necessary. It is reasonable for `chacc` to parse `auto` and otherwise ignore it
permanently.

## `register`

### Current Status

- parsed
- context-validated
- ignored afterward

### What Is Implemented

- accepted in block-scope declarations
- accepted in `for`-initializer declarations
- accepted in parameter declarations
- rejected in obviously invalid contexts such as file scope

### Why This Is Enough For Now

`register` is historical manual optimization advice. It does not justify any
backend complexity in this compiler.

### Future Plan

Likely remain parsed-and-ignored permanently. The only realistic future work is
small front-end validation if stricter diagnostics become desirable.

## `const`

### Current Status

- parsed
- ignored

### What Is Implemented

- accepted in declaration specifiers
- accepted after `*` in pointer declarators
- does not affect type identity or semantics

### Why Full Support Is Deferred

`const` only becomes meaningful once qualifiers are part of the type model.
Without that, partial checks would be misleading.

Examples that the current `Type` model cannot distinguish properly:

- `int *`
- `const int *`
- `int *const`

The important point is that qualifier information belongs on each type layer,
not as one flat `is_const` bit on the whole type.

### Future Plan

When qualifiers are modeled in types, `const` should drive:

- assignment constraints for const-qualified lvalues
- qualifier-aware casts and conversions
- better redeclaration/type-compatibility checks

## `volatile`

### Current Status

- parsed
- ignored

### What Is Implemented

- accepted in declaration specifiers
- accepted after `*` in pointer declarators
- does not affect type identity or semantics

### Why Full Support Is Deferred

Like `const`, `volatile` needs qualifier-aware types before any meaningful
front-end or backend behavior can be correct.

### Future Plan

Only after qualifier support exists in the type system:

- represent volatile-qualified types explicitly
- decide what volatile should mean for loads/stores and optimization boundaries

## `restrict`, `__restrict`, `__restrict__`

### Current Status

- parsed
- ignored

### What Is Implemented

- accepted in declaration specifiers
- accepted after `*` in pointer declarators
- alternate spellings `__restrict` and `__restrict__` are tokenized as the same
  keyword

### What Is Still Missing

- stricter placement validation
- any aliasing semantics
- any optimization consequences

### Why Full Support Is Deferred

`restrict` matters only for pointer-qualified types and aliasing assumptions.
That is far beyond what the current `Type` model can express.

### Future Plan

After qualifier-aware types exist:

- validate placement more precisely
- decide whether `restrict` should remain syntax-only or gain semantic meaning

## `_Noreturn`

### Current Status

- intentionally deferred from this batch
- not part of the current support plan here

### Why It Was Deferred

`_Noreturn` is not a type qualifier. It is closer to function metadata.

Its backend impact is minimal, but the front-end policy is not:

- should it be accepted only on functions, or accepted more broadly with a
  warning
- how should redeclarations be checked
- when should returning from a `_Noreturn` function be diagnosed

That is a separate design decision and is cleaner as its own batch.

### Future Plan

If implemented later, the likely shape is:

- store it on `Function`, similar in spirit to `is_static`
- validate declaration contexts and redeclaration consistency
- optionally diagnose functions marked `_Noreturn` that return normally

## Array-Parameter Qualifiers Inside `[]`

### Current Status

- intentionally deferred from this batch
- not part of the current support plan here

### Why It Was Deferred

This is a separate grammar rule from ordinary qualifiers. It only matters in
function parameter declarators, so it is better handled as a focused parser
change rather than being mixed into the ordinary qualifier story.

### Future Plan

Handle this in its own batch if needed:

- parse `static` and qualifiers inside parameter array declarators
- decide whether they should be silently ignored or lightly validated

## Overall Plan

### What Is Done Now

- `auto` and `register`: parse, context-validate, ignore
- `const`, `volatile`, `restrict`: parse and ignore

### What Is Deliberately Not Done Yet

- `_Noreturn`
- array-parameter qualifiers inside `[]`
- any qualifier-aware type semantics

### Next Real Semantic Step

If qualifier semantics are ever implemented, the correct first move is to make
qualifiers part of the type representation. Do not bolt isolated special cases
onto the parser without that foundation.
