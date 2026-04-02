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
  - `ScopeFrame { vars, tags }`
  - `push_scope_var()`
  - `push_scope_tag()`
  - `create_local()`
  - `create_global()`
  - `parse_typedef_tail()`

At this commit:

- `push_scope_var()` does a plain `insert()` into the current scope frame and does not report collisions.
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
