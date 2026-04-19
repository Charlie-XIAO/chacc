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

- parsed
- stored on function declarations/definitions
- accepted on some non-function declarations with warnings
- rejected in contexts where GCC also rejects it, such as member declarations
  and typenames

### What Is Implemented

- tokenized as a declaration keyword
- accepted in:
  - file-scope declarations
  - block-scope declarations
  - `for`-initializer declarations
  - parameter declarations
- attached to `Function` metadata
- merged across redeclarations with OR semantics
- warned on non-function uses such as:
  - variables
  - typedefs
  - parameters

### Why It Is Modeled Separately From Qualifiers

`_Noreturn` is not a type qualifier. It is function metadata, closer in shape
to `is_static` than to `const` or `volatile`.

That is why it could be implemented now without waiting for qualifier-aware
types.

### Remaining Limitations

- there is currently no diagnostic for `return` statements inside a
  `_Noreturn` function
- `_Noreturn` is not part of any deeper declaration-compatibility subsystem
- there is no downstream optimization or control-flow effect in codegen

That missing `return` diagnostic is intentional for now. It is not just a
local parser check; the real question is whether the function satisfies its
non-returning contract as a whole. That belongs more naturally in a later
whole-function/control-flow analysis pass, alongside diagnostics such as
"function may fall through the end" or "not all paths return".

### Future Plan

Keep the current behavior unless later chapters create a reason to tighten
compatibility or diagnostics further.

## Array-Parameter Qualifiers Inside `[]`

### Current Status

- parsed in function parameter array declarators
- lightly validated
- otherwise ignored

### What Is Implemented

- accepted only in function parameter array declarators
- accepted forms include:
  - `int a[restrict 3]`
  - `int a[static 3]`
  - `int a[restrict static 3]`
- rejected outside parameter declarators
- `static` currently requires an explicit bound
- qualifiers and `static` are ignored after parsing and do not affect the
  resulting parameter type

### Why It Is Handled Separately

This syntax is not the same thing as ordinary type qualifiers. It is a special
array-parameter grammar rule, so it is clearer to parse it only in parameter
declarators rather than relaxing all array declarators globally.

### Future Plan

The future path here likely splits in two:

- `const` / `volatile` / `restrict`
  - once qualifier-aware types exist, these may be carried onto the adjusted
    pointer type of the parameter, since array parameters already decay to
    pointers in `parse_func_params()`

- `static`
  - this is not a type qualifier; it is a parameter contract about the minimum
    accessible number of elements
  - if implemented later, it will likely need dedicated parameter metadata
    rather than being stored on ordinary array types

So this area is not expected to remain parser-only forever. It is deferred
mainly because the current type and parameter models are not yet rich enough to
support the semantics cleanly.

## Overall Plan

### What Is Done Now

- `auto` and `register`: parse, context-validate, ignore
- `const`, `volatile`, `restrict`: parse and ignore
- `_Noreturn`: parse, warn on broad non-function uses, and store on functions
- array-parameter qualifiers inside `[]`: parse in parameter declarators,
  lightly validate, ignore

### What Is Deliberately Not Done Yet

- any qualifier-aware type semantics

### Next Real Semantic Step

If qualifier semantics are ever implemented, the correct first move is to make
qualifiers part of the type representation. Do not bolt isolated special cases
onto the parser without that foundation.
