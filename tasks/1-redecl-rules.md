# Deferred Issue: Same-Scope Redeclaration Handling Was Intentionally Reverted

Introduced in: `42d144f95e7954ae0d86864933b1f50907504a8e`

## Summary

`chacc` currently does **not** implement correct same-scope redeclaration rules for:

- ordinary identifiers in the variable/typedef namespace
- struct/union tags in the tag namespace

Instead, the current parser behavior is intentionally permissive: when a name is
declared again in the same scope frame, the new binding simply overwrites the
old one in the scope map.

This is a deliberate rollback from an earlier attempt to model C redeclaration
rules more accurately. That attempt was heading in the right direction, but it
added a lot of machinery early and risked drifting too far from the
chapter-by-chapter chibicc learning path.

The current priority is:

1. keep the implementation simple
2. avoid rejecting correct real-world code due to an incomplete redeclaration model
3. defer full declaration compatibility checking until later

## Current Implementation

Relevant code:

- `src/parse.rs`
  - `ScopeFrame { idents, tags }`
  - `push_scope_ident()`
  - `push_scope_tag()`
  - `create_local()`
  - `create_global()`
  - `declare_global()`
  - `declare_func_decl()`
  - `parse_typedef_tail()`

At this commit:

- `push_scope_ident()` does a plain `insert()` into the current scope frame and does not report collisions.
- `push_scope_tag()` also does a plain `insert()` and does not report collisions.
- variable creation and typedef insertion do not perform same-scope compatibility checks before mutating state.

As a result, the most recent declaration in the same scope silently wins for
lookup purposes.

## Why This Is Wrong

C has non-trivial redeclaration rules. A few examples:

### Ordinary objects at file scope

These are valid:

```c
int x;
int x;
```

These are invalid:

```c
int x = 1;
int x = 1;   // redefinition
```

```c
int x;
short x;     // conflicting types
```

### Ordinary objects at block scope

This is invalid even though the type is the same:

```c
int main() {
  int x;
  int x;     // redeclaration of variable
}
```

### Typedefs

These are valid:

```c
typedef int x;
typedef int x;
```

These are invalid:

```c
typedef int x;
typedef short x;   // conflicting types
```

### Typedef vs object collision

These are invalid:

```c
typedef int x;
int x;             // different kind of symbol
```

```c
int x;
typedef int x;     // different kind of symbol
```

### Tags

Same-scope tag redeclarations are also not generally "last declaration wins".
Later chapters will need to distinguish cases like:

- incomplete declaration followed by completion
- incompatible same-scope redefinition
- nested-scope shadowing

## Current Observable Misbehavior

Because same-scope collisions are silently overwritten, `chacc` may:

- accept invalid C programs that should fail
- resolve later references to the wrong declaration
- overwrite a typedef with an object, or vice versa, in the same scope
- overwrite a tag in the same scope

This is especially risky for:

- block-scope duplicate locals
- conflicting typedefs
- typedef/object collisions
- tag redefinitions

There is also an implementation detail to keep in mind:

- `create_local()` and `create_global()` append to `locals` / `globals` first
  and only then update the scope map
- when collisions are silently allowed, old entries remain in those tables but
  are no longer reachable by name

That is not immediately fatal, but it means the symbol tables can accumulate
dead entries and makes later cleanup harder.

## Why This Was Deferred

A more correct implementation was prototyped, but it needed significantly more
information in scope entries, such as:

- identifier kind
  - object
  - typedef
  - later: function
- declared type
- for file-scope objects:
  - whether the declaration is tentative
  - whether there is an actual definition
- later:
  - storage-class information
  - function declaration compatibility

That is a real subsystem, not just a small parser tweak.

Doing it early would increase divergence from chibicc before later chapters
(function declarations, storage classes, incomplete types, etc.) are in place.

## Updated Refactor Plan

The current codebase is in a better position to revisit this than it was when
this task was first written:

- globals now have explicit storage state via `GlobalStorage`
- global redeclaration already has a narrow merge point in `declare_global()`
- incomplete-array redeclarations already have a narrow type merge point in
  `TypeStore::merge()`
- block-scope `extern` and block-scope function declarations are now modeled
  explicitly enough that the remaining weakness is mostly same-scope checking

The right refactor is still **not** "make `push_scope_ident()` return an error".
That would be too late in the pipeline and would not have enough information to
classify the redeclaration correctly.

The better plan is:

1. Add current-scope lookup helpers.
   - `find_ident_current(name) -> Option<OrdinaryIdent>`
   - `find_tag_current(name) -> Option<Type>`
   - keep the existing outward-search helpers for normal lookup

2. Classify a declaration against the **current scope frame first**.
   - do this before mutating `locals`, `globals`, or the scope maps
   - distinguish:
     - current-scope compatible redeclaration
     - current-scope conflicting redeclaration
     - current-scope different-kind collision
     - no current-scope binding, but reusable outer binding
     - no binding at all

3. Keep canonical symbol tables separate from scope bindings.
   - `locals`, `globals`, and `functions` remain the canonical objects
   - the scope maps remain only name-to-symbol bindings
   - a redeclaration may reuse an existing canonical symbol while still
     rebinding the current scope to that same symbol

4. Handle ordinary identifiers and tags separately.
   - ordinary identifiers: object / typedef / function namespace
   - tags: struct / union / enum namespace
   - both need current-scope checks, but the compatibility rules differ

5. Keep the implementation staged.
   - do **not** try to solve full function declaration compatibility in the
     same patch
   - fix globals/typedefs/tags first
   - fold functions into the same framework only after function
     declaration/definition compatibility is modeled properly

## Suggested Implementation Order

### Stage 1: Add Current-Scope APIs

Add small helpers in `parse.rs`:

- `find_ident_current()`
- `find_tag_current()`

These should only inspect `self.scopes.last()`.

This is the key mechanical step. It lets declaration code distinguish:

- "same scope redeclaration"
- from "outer scope reuse/shadowing"

without changing how ordinary identifier lookup works elsewhere.

### Stage 2: Ordinary Identifiers, But Not Functions Yet

Introduce a narrow declaration classifier for non-function ordinary
identifiers. Conceptually, it should answer:

- is there a current-scope typedef/object/function of the same name?
- if so, is this redeclaration allowed?
- if not, should we reuse an outer global symbol or create a fresh local/global?

At this stage, functions can remain on the current stopgap path.

The first targets should be:

- block-scope duplicate locals
- typedef/typedef compatibility
- typedef/object collisions
- file-scope object/object compatibility and redefinition

This is where `create_local()` and `create_global()` should stop being called
eagerly. The declaration should be classified first, and only then should a new
canonical symbol be created if needed.

### Stage 3: Tags

Do the same current-scope split for tags:

- allow completion of an existing incomplete tag in the same scope when valid
- reject conflicting same-scope tag redefinitions
- still allow inner-scope shadowing

This should reuse the same "current scope first, then outer scopes" pattern as
ordinary identifiers, but keep the logic separate because the rules are
different.

### Stage 4: Functions

Only after function redeclaration/definition compatibility is modeled properly
should `declare_func_decl()` be pulled into the same stricter subsystem.

Until then, `declare_func_decl()` should be treated as a stopgap:

- enough for block-scope function declarations and `extern`
- not yet the final shape for redeclaration rules

## Design Constraint

The parser should continue to map cleanly to grammar productions.

That means:

- one `parse_declaration()` for the `<declaration>` BNF
- semantic classification inside that parser or in non-`parse_*` helpers
- avoid proliferating new `parse_*` functions that do not correspond to
  distinct grammar productions

The good abstraction boundary here is not "more parser entry points". It is:

- parser functions for grammar
- non-parser helpers for declaration classification and symbol reuse

## Recommended Future Fix

When revisiting this, do **not** try to paper over the problem with a slightly
different error string. The real fix should:

1. Enrich ordinary-identifier scope entries so they carry at least:
   - kind (`object`, `typedef`, later `function`)
   - declared `Type`
   - enough metadata for file-scope object redefinitions
2. Check the **current scope frame only** before inserting a new declaration.
3. Distinguish these cases:
   - same-scope object/object, compatible
   - same-scope object/object, incompatible
   - same-scope typedef/typedef, compatible
   - same-scope typedef/typedef, incompatible
   - same-scope typedef/object or object/typedef
4. Avoid mutating `locals`, `globals`, or scope maps until the declaration has
   been classified.
5. Treat tags separately but with analogous current-scope checks.

## Suggested Behavior To Aim For

Minimum target:

- file-scope `int x; int x;` should be accepted
- file-scope `int x=1; int x=1;` should fail
- block-scope `int x; int x;` should fail
- `typedef int x; typedef int x;` should be accepted
- `typedef int x; typedef short x;` should fail
- `typedef int x; int x;` should fail
- same-scope tag redefinition should not silently overwrite

Nice-to-have later:

- diagnostics that distinguish:
  - redeclaration
  - redefinition
  - conflicting types
  - different kind of symbol
- previous-declaration notes

## Important Constraint

This should probably be revisited **after** more of chibicc has been completed,
not immediately. In particular, function declarations/definitions and additional
storage-class/type-system work will change what "correct" redeclaration
handling needs to know.
