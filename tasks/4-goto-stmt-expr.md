# Deferred Issue: Goto Resolution Still Allows Jumping Into Statement Expressions

Introduced in: `0c87b58bcc735bbf7f96fe219852f7b543026760`

## Summary

`chacc` now supports ordinary labels and `goto`, and the current design is
already much cleaner than the earlier parser-global backpatching attempt.

However, one GNU-C semantic corner is still missing:

- `goto` into a GNU statement expression is currently accepted
- GCC rejects it as `jump into statement expression`

Example:

```c
int main() {
  int x = ({ a: 1; 2; });
  goto a;
  return x;
}
```

Current `chacc` behavior:

- accepted
- generated code can jump into the middle of the statement expression's local
  control-flow region
- the resulting program can crash at runtime (e.g. segfault) instead of merely
  being non-portable

GCC behavior:

- rejected

This should be fixed later, but it was deferred because the clean solution
adds a bit of bookkeeping to the goto resolver and was not worth mixing into
the just-landed goto/label batch.

## Current Implementation

Relevant code:

- `src/ast.rs`
  - `StmtKind::Goto`
  - `StmtKind::Label`
- `src/parse.rs`
  - `parse_stmt()`
  - `collect_labels_stmt()`
  - `collect_labels()`
  - `resolve_gotos_stmt()`
  - `resolve_gotos()`

The current implementation performs a post-parse resolution pass over the
function body:

1. collect labels from the statement/expression tree
2. resolve gotos against that collected map

This correctly handles:

- forward goto
- duplicate labels
- labels coexisting with typedef names
- multiple gotos to the same label

But labels found inside `NodeKind::StmtExpr` are currently placed into the same
function-wide label namespace as ordinary labels, so a goto outside the
statement expression can still target them.

## Why This Was Deferred

The obvious fix is not conceptually hard, but the clean version is a little
more involved than a one-line check.

The resolver needs to distinguish:

- labels in ordinary function scope
- labels inside a specific nested statement-expression context

The desired rule is:

- jumping into a statement expression is forbidden
- jumping out of a statement expression is allowed
- jumping within the same statement expression is allowed
- jumping from one statement expression into a sibling one is forbidden

That means plain function-wide `name -> label-id` mapping is not enough.

## Recommended Fix

Keep the existing post-parse AST-walk resolver. Do **not** go back to parser-
global backpatch lists or `Rc<RefCell<...>>` in the AST.

The cleaner resolver shape is:

```rust
struct GotoResolver<'a> {
    source: &'a Source,
    labels: FxHashMap<SmolStr, LabelInfo>,
    stmt_expr_parents: Vec<Option<usize>>,
    current_stmt_expr: Option<usize>,
}

struct LabelInfo {
    id: SmolStr,
    stmt_expr: Option<usize>,
}
```

Interpretation:

- `current_stmt_expr == None` means ordinary function scope
- each entered `StmtExpr` gets a fresh numeric context ID
- `stmt_expr_parents[id]` points to the enclosing statement-expression context

Then:

- when collecting labels, store the current statement-expression context
- when resolving a goto, compare the goto's current context with the target
  label's context

The jump is valid only if the label's statement-expression context is the same
as, or an ancestor of, the goto's current context.

Equivalent rule:

- a goto may jump to the same statement expression
- or to an outer statement expression
- or to ordinary function scope
- but never into a deeper or sibling statement expression

This avoids:

- passing a full `Vec<usize>` path around and cloning it for each label
- leaking parser-time backpatch state into the AST

## Concrete Validation Cases

When revisiting this, verify at least these cases:

Allowed:

```c
int main() {
  ({ goto out; 1; });
out:
  return 3;
}
```

Allowed:

```c
int main() {
  return ({ goto a; 0; a: 7; 8; });
}
```

Rejected:

```c
int main() {
  int x = ({ a: 1; 2; });
  goto a;
  return x;
}
```

Rejected:

```c
int main() {
  int x = ({ a: 1; 2; });
  ({ goto a; 3; });
  return x;
}
```

## Important Constraint

If this area is revisited, keep the current post-parse resolution model:

- parse labels/gotos into AST
- resolve after the function body is fully built

That is the right architecture for this codebase. The missing piece is only the
statement-expression context restriction.
