# Partial Follow-Up: Redeclaration Handling Is No Longer Intentionally Permissive

## Summary

The original deferred issue in this file has been addressed in large part.
`chacc` no longer relies on "last declaration wins" for the main same-scope
redeclaration cases that were previously deferred.

In particular, the parser now classifies declarations against the **current
scope frame first** before mutating symbol tables in these areas:

- block-scope object declarations
- typedef declarations
- enum tags and enumerators
- global object declarations
- function declarations and function definitions

This means the original broad statement:

- "same-scope redeclaration handling was intentionally reverted"

is no longer true.

What remains deferred is narrower:

- full function declaration compatibility
- richer structural type compatibility beyond the current limited merge rules
- more diagnostic quality such as previous-declaration notes

## What Is Implemented

Relevant code:

- `src/parse.rs`
  - `ScopeFrame { idents, tags }`
  - `find_ident_current()`
  - `find_tag_current()`
  - `parse_declaration()`
  - `parse_typedef_tail()`
  - `parse_enum_specifier()`
  - `declare_global()`
  - `declare_function()`
  - `parse_function()`

Implemented behavior now includes:

- block-scope duplicate locals are rejected
- block-scope no-linkage declarations are checked before creating locals or
  static-local backing storage
- typedef/typedef same-scope redeclaration is accepted only when the declared
  type matches under the current equality rules
- typedef/object, typedef/function, and other different-kind collisions are
  rejected in the current scope
- enum tag redefinition in the current scope is rejected
- enum tag reuse as the wrong kind of tag is rejected
- duplicate enumerators in the current scope are rejected
- global declarations now classify current-scope collisions before reusing or
  creating symbols
- global declarations reuse linkage-bearing globals from outer scopes when
  appropriate
- repeated function declarations reuse an existing function symbol instead of
  creating duplicate function entries
- a second function body is rejected as `redefinition of function`

This work also tightened the relationship between scope bindings and canonical
tables:

- `locals`, `globals`, and `functions` remain the canonical symbol tables
- scope frames still store only name bindings
- redeclarations may reuse an existing canonical symbol while rebinding the
  current scope to that same symbol

## What Still Remains Deferred

### Function Declaration Compatibility

`declare_function()` currently reuses an existing function declaration by name,
but it does **not** yet check that the new function type is compatible with the
old one.

That means this area is still only partially modeled. In particular, later work
should decide how to handle cases like:

```c
int f(void);
long f(void);   // conflicting
```

and more subtle C function-compatibility rules once old-style declarations,
variadics, and related cases matter.

### Broader Structural Type Compatibility

Current type compatibility is still intentionally narrow:

- `TypeStore::merge()` only supports:
  - exact type identity
  - incomplete/complete array redeclarations with the same base type
- typedef duplicate checks still rely on current type-equality behavior

So while the old redeclaration permissiveness is gone, the compiler still does
not have a full structural declaration-compatibility subsystem.

### Diagnostic Refinement

Current diagnostics are serviceable but still coarse in places. Future work may
want:

- previous-declaration notes
- clearer distinction between:
  - redeclaration
  - redefinition
  - conflicting types
  - different kind of symbol

## Current Design Shape

The refactor settled on these design choices:

1. Keep parser functions aligned with grammar productions.
   - `parse_declaration()` remains the parser for the `<declaration>` BNF.
   - declaration classification happens inside that parser or in non-`parse_*`
     helpers such as `declare_global()` / `declare_function()`.

2. Keep canonical symbol tables separate from scope bindings.
   - symbol tables own the actual objects/functions
   - scope frames only bind names to those symbols

3. Check current scope before mutation.
   - this is the key rule that replaced the old permissive overwrite model

4. Keep tags and ordinary identifiers separate.
   - their redeclaration rules differ and should stay on separate code paths

## Remaining Follow-Up Work

The next realistic extension here is:

1. add a structural type-equality / declaration-compatibility helper
2. use it in `declare_function()`
3. likely reuse it for typedef duplicate checks too

That would extend the current redeclaration framework without undoing the
cleanup that has already landed.
