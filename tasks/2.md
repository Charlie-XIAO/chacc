# Deferred Issue: Function Declarations And Definitions Are Not Merged

Introduced in: `0f5a6684d710a38c31660c0b4859607454265c9f`

## Summary

`chacc` now tracks functions in its own `functions` table and binds function
names into the ordinary identifier namespace through `EntityRef::Function`.
That is a good long-term model for expression resolution, but the current
implementation still treats each parsed function declaration or definition as a
fresh `Function` entry.

As a result, repeated declarations and later definitions of the same function
name are not merged into one canonical symbol record.

This is not immediately breaking for normal code generation, but it is still a
real model gap that should be revisited later.

## Current Implementation

Relevant code:

- `src/parse.rs`
  - `create_function_decl()`
  - `parse_function()`
  - `parse_func_call()`
- `src/ast.rs`
  - `Program`
  - `Function`
  - `EntityRef::Function`
- `src/codegen.rs`
  - `generate()`
  - `gen_function()`

Current behavior:

1. Every function declaration or definition creates a new `Function` entry.
2. That entry is inserted into the ordinary identifier namespace as
   `EntityRef::Function(id)`.
3. A declaration-only entry has:
   - `body: None`
   - empty locals/params bookkeeping
4. A definition later fills in the newly created entry for that parse event,
   instead of upgrading a pre-existing declaration entry.

So code like:

```c
int foo();
int foo() { return 3; }
```

currently produces two `Function` records:

- one declaration-only record for `foo`
- one separate definition record for `foo`

## Why This Has Not Broken Yet

The current design still works well enough because:

- identifier lookup for calls only needs to know that the name resolves to a
  function entity
- `FuncCall` nodes still carry the callee name directly
- code generation skips `Function` entries with `body: None`

So a forward declaration followed by a definition compiles and links
correctly, even though the internal bookkeeping is duplicated.

Likewise, a declared-but-not-defined function call is not inherently an error:

```c
int foo();
int main() { return foo(); }
```

That is valid C if `foo` is provided by another translation unit, so it is
correct to leave unresolved-symbol handling to the linker.

## Why This Is Still A Problem

The lack of merging means there is no single authoritative function symbol for
a given name.

That creates several future problems:

- repeated declarations accumulate duplicate entries
- a declaration and its later definition are not tied together
- later compatibility checks have no canonical declaration record to compare
  against
- duplicate-definition detection will be awkward
- future storage-class handling (`static`, `extern`) will want one symbol record
  per function name, not one record per syntactic appearance
- later function-type compatibility work will need to answer:
  - is this a compatible redeclaration?
  - is this a conflicting declaration?
  - is this a second definition?

With the current implementation, those questions cannot be answered cleanly
without first finding and consolidating all function entries for the same name.

## Chibicc Comparison

At the corresponding chibicc stage, this is also still loose. chibicc creates
fresh objects for repeated function declarations/definitions instead of
carefully merging them into one canonical symbol.

So this is not a regression relative to the current chapter. It is a deferred
cleanup and future-correctness issue.

## Observable Current Behavior

Currently expected behavior:

- `int foo(); int main() { return foo(); } int foo() { return 3; }`
  - compiles and runs correctly
- `int main() { return foo(); } int foo() { return 3; }`
  - errors with `implicit declaration of a function`
- `int foo(); int main() { return sizeof(foo()); } int foo() { return 0; }`
  - uses the declared return type correctly

But internally, the first example still leaves duplicate `Function` records for
`foo`.

## Recommended Future Fix

When revisiting this, keep the current general architecture:

- separate storage structs for:
  - `Function`
  - `GlobalVar`
  - `LocalVar`
- unified binding/reference layer through `EntityRef`

Do **not** regress to duplicating function metadata in `GlobalVar`, and do not
reintroduce a C-style "fat object" unless there is a much stronger reason.

Instead:

1. Add a lookup path for existing function symbols by ordinary identifier name.
2. When parsing a function declaration/definition:
   - if the name already resolves to a function symbol in the relevant scope,
     reuse that `Function` entry
   - otherwise create a fresh one
3. Upgrade the reused entry in place when a definition body appears.
4. Later, add checks for:
   - conflicting function types
   - repeated incompatible declarations
   - duplicate definitions

## Important Constraint

Do not turn this into "every declared-but-not-defined function must error".
That would be wrong for externally provided functions.

The real future goal is:

- merge declarations and definitions of the same function symbol
- keep unresolved external references as a linker concern
- only diagnose missing definitions when the language rules make that knowable
  within one translation unit (for example, later with `static` functions)
