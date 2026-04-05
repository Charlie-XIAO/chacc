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
- incomplete array/function parameters are temporarily rejected instead of
  being adjusted to pointer types
- some invalid nested incomplete-array forms are still under-validated

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

There are really three separate features here:

1. Parsing incomplete array types
2. Adjusting function parameter types
3. Supporting special object/layout rules

Those do not all land in the same upstream chapter.

The first part is needed immediately for this chapter.
The later two parts are broader semantic features and are better deferred until
their surrounding machinery is present.

## Deferred Part 1: Function Parameter Adjustment

In C, function parameters are adjusted:

- array parameters decay to pointers to their element type
- function parameters decay to pointers to function

So these are equivalent:

```c
int f(int a[]);
int f(int *a);
```

At the current state, `chacc` does **not** implement that adjustment yet.

Instead, parameter declarations such as:

```c
int f(int a[]);
```

are temporarily rejected as:

- `parameter has incomplete type`

That is intentionally stricter than the eventual goal, but it is still better
than letting such types flow through unchanged and later panic in codegen.

This is intentionally deferred because latest chibicc does add this later in
`parse.c:func_params()`, and we do not want to mix that later semantic step
into the current chapter.

When revisiting this, the fix should happen in parameter parsing, not in
codegen:

- parse the declarator type normally
- if the parameter type is an array, convert it to `ptr(base)`
- if the parameter type is a function, convert it to `ptr(func_ty)`
- store that adjusted type in the function type and in the parameter locals

## Deferred Part 1.5: Validation Of Nested Incomplete Array Forms

The current parser is also still too permissive for some invalid combinations
of omitted array bounds.

Example:

```c
sizeof(int(*)[][][10])
```

This is not a valid type, but chibicc also accepts similar forms, so this is
another place where blindly following upstream would keep a known hole.

The general issue is that an incomplete array bound should not be accepted in
arbitrary nested positions just because `type_suffix` can mechanically build a
tree of `array(base, None)`.

When revisiting this, add an explicit semantic validation pass for array
declarators so that:

- allowed incomplete-array forms remain accepted where C permits them
- invalid nested/stacked omitted bounds are rejected
- the rule is enforced deliberately rather than falling out accidentally from
  later codegen or size computations

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

1. Parameter adjustment
   - arrays/functions in parameter position decay to pointers
2. Validation of nested incomplete-array forms
   - reject invalid omitted-bound combinations such as `int(*)[][][10]`
3. Flexible array members
   - last struct member only
4. File-scope incomplete arrays / tentative definitions
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
