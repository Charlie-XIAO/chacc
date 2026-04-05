# Deferred Issue: Incomplete Array Support Is Only Partially Implemented

Introduced in: `ecb877bf817775173e8bba0fa7b17c4af16ad1d3`

## Summary

`chacc` now understands incomplete array declarators such as:

- `int (*)[10]`
- `int (*)[][10]`
- `int a[]`

but only the parsing/type-representation part is implemented.

The full C semantics around incomplete arrays were intentionally deferred,
because they split across multiple later chibicc developments and are easy to
get subtly wrong if implemented too early.

At the current state:

- incomplete local variables are rejected
- incomplete global variables are rejected
- incomplete struct/union members are rejected
- array parameters now decay to pointers in function parameter context
- invalid nested incomplete-array forms are now rejected

This is a deliberate stopgap.

## Current Implementation

Relevant code:

- `src/types.rs`
  - `Type::array(base, Option<usize>)`
  - `Type::is_incomplete()`
- `src/parse.rs`
  - `parse_array_dimensions()`
  - `parse_declaration()`
  - `parse_global_variable()`
  - `parse_struct_or_union_decl()`
  - `parse_func_params()`

The parser now represents an omitted array bound as `Type::array(base, None)`.
That yields an incomplete type, which is enough for type names and pointer-to-
array forms.

However, the rest of the compiler does not yet consistently implement all
places where C requires extra behavior for such types.

## Why This Was Deferred

There are really several separate features here:

1. Parsing incomplete array types
2. Adjusting array/function parameter types
3. Supporting special object/layout rules

Those do not all land in the same upstream chapter.

The first part is needed immediately for this chapter.
Array-parameter adjustment and nested incomplete-array validation have now been
implemented. The remaining items are broader semantic features and are better
deferred until their surrounding machinery is present.

## Completed Part 1: Array Parameter Adjustment

In C, function parameters are adjusted:

- array parameters decay to pointers to their element type
- function parameters decay to pointers to function

So these are equivalent:

```c
int f(int a[]);
int f(int *a);
```

`chacc` now implements the array part of that adjustment. For example:

```c
int f(int a[]);
```

is now treated like `int f(int *a);`.

This was deferred earlier, but has now been completed in the corresponding
later chibicc chapter.

## Deferred Part 1: Function Parameter Adjustment

Function parameters still need the analogous decay rule:

```c
int f(int g());
```

should be treated like a parameter of pointer-to-function type rather than a
raw function type.

Latest chibicc does also add this later in `parse.c:func_params()`.

When revisiting this, the remaining fix should happen in parameter parsing, not
in codegen:

- parse the declarator type normally
- if the parameter type is a function, convert it to `ptr(func_ty)`
- store that adjusted type in the function type and in the parameter locals

## Completed Part 1.5: Validation Of Nested Incomplete Array Forms

The parser now rejects invalid combinations of omitted array bounds, such as:

```c
sizeof(int(*)[][][10])
```

This was another place where chibicc was permissive and `chacc` now chooses to
be stricter.

## Deferred Part 2: File-Scope Incomplete Arrays

The current `chacc` behavior rejects:

```c
int a[];
```

with:

- `variable has incomplete type`

That is stricter than GCC and stricter than what full C eventually wants for
tentative definitions, but it is safer than the previous broken behavior.

Why the guard exists:

- before the guard, `chacc` would emit invalid storage like `.zero -1`
- so allowing the declaration without real tentative-definition support was
  already wrong

Latest chibicc still appears loose here:

- it does not reject file-scope incomplete arrays outright
- but it also does not appear to fully complete them into a sound final object
  size in this path
- a direct run of latest local chibicc on `int a[];` produced an assembler-side
  negative-size warning, so this does not look cleanly solved upstream

So this should be treated as a genuine deferred implementation task for
`chacc`, not something we can count on later chibicc chapters to solve for us.

When revisiting this, decide explicitly whether `chacc` should:

- model tentative definitions more like GCC/chibicc
- or remain stricter and reject file-scope incomplete arrays

But do not silently emit invalid storage again.

## Deferred Part 3: Flexible Array Members

The current `chacc` behavior rejects:

```c
struct S { int a[]; };
```

with:

- `field has incomplete type`

That is a temporary guard because true flexible-array-member support is not yet
implemented.

Latest chibicc **does** eventually support this:

- if the last struct member is an array of incomplete type
- it rewrites that member to a zero-length array
- and marks the containing struct as flexible

In other words, latest chibicc has real flexible-array-member handling for the
last member of a struct, and this is something `chacc` can reasonably adopt
later.

When revisiting this:

1. Only the final member should be eligible.
2. It should be treated specially in layout rather than as an ordinary
   incomplete member.
3. The resulting type/layout behavior should match whatever object model
   `chacc` has by then.

## Recommended Future Split

When this area is revisited, do not try to solve everything in one patch.

Instead split it into:

1. Function parameter adjustment
   - function parameters decay to pointers to function
2. Flexible array members
   - last struct member only
3. File-scope incomplete arrays / tentative definitions
   - make an explicit policy choice

That will keep the work easier to reason about and closer to how the language
rules are actually layered.

## Important Constraint

Until real support exists:

- keep rejecting incomplete globals
- keep rejecting incomplete struct/union members
- do not reintroduce broken codegen paths such as negative-size `.zero`

These guards are intentionally stricter than the eventual goal, but they are
better than silently producing invalid output.
