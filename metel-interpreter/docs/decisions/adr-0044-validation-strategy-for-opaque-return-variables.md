# ADR-0044: Validation Strategy for Opaque Return Variables

## Status

Accepted (revised after independent review found the originally-implemented
strategy below was never actually wired in, and — separately — didn't work
correctly when tested)

## Context

RFC-0037 introduces return-position `impl Aspect` types, which allow functions to return opaque types that implement an aspect interface. The caller sees an abstract type that satisfies the aspect bound, without knowing the concrete implementation.

A key requirement is preventing the caller from naming or observing the concrete type of the opaque return value while still allowing normal usage patterns like:
- Passing the value to functions that accept `impl Aspect` parameters
- Calling aspect methods on the value
- Storing the value in variables declared with `impl Aspect` types

However, the type system must prevent problematic usage patterns like:
- Explicitly naming the concrete type (`let x: ConcreteType = f();`)
- Casting to the concrete type
- Passing to non-generic parameters declared with the exact concrete type

## Decision

Validation happens **incrementally, inside `apply_constraint_with_coercion`**,
immediately after each individual constraint's own unification result is
composed into the substitution — not as a single pass over the fully-solved
substitution at the end of `solve()` (see "Superseded approach" below for why
that doesn't work).

1. **Per-quantified-var metadata**: `TypeScheme.opaque_returns` tracks which
   quantified variables represent opaque returns and (for the unlinked case)
   their concrete types, exactly as originally designed.
2. **Call-site marking**: when instantiating an opaque-returning function at a
   call site, each opaque quantified var's fresh renamed copy is registered in
   `InferContext.opaque_return_vars` (`mark_opaque_return_var`) and given its
   aspect bound (`register_type_var_bound`) — also as originally designed.
3. **Incremental check**: after `apply_constraint_with_coercion` composes a
   constraint's unification result into `subst`, it iterates
   `opaque_return_vars` and checks whether any of them, applied through the
   *current* `subst`, has become a type other than `Var`/`Never`. If so, that
   is the exact point a concrete type got named — reject with `T0018`
   immediately, using that constraint's own span.

### Superseded approach: single validation at the end of `solve()`

The first implementation added `validate_opaque_return_bindings()`, called
once after the whole batch of constraints in a `solve()` call was processed,
checking every opaque return var against the fully-solved substitution. This
is the natural reading of "mirrors `validate_literal_bindings()`" and is what
the rest of this document originally described — **but it doesn't work**:

- It was never actually wired in. The one call site was commented out with a
  `// TODO: Fix validation logic for specific test cases` note, and
  `cargo build` had been flagging both `validate_opaque_return_bindings` and
  `validate_opaque_return_bindings_for_constraint` as dead code the entire
  time. Independent review caught this by simply reading the build's own
  warning output — a check that should have been part of verifying this ADR
  was actually implemented before marking it "Accepted".
- Once wired in and tested directly, it also produced false positives for the
  legitimate "linked" case (an opaque return passed to *another* function's
  own `impl Aspect` parameter). By the time a whole `solve()` batch — often
  covering an entire function body's worth of constraints, sometimes more —
  has fully closed over every transitive unification, a legitimate
  opaque-marker-to-another-function's-generic-parameter chain has typically
  been resolved down to a concrete type too, the same as an actual violation.
  There is no way to distinguish "resolved via legitimate indirection" from
  "the concrete type was directly named" from the end state alone.

  Checking right after *each* constraint's own composition avoids this: at
  the moment a marker unifies with another function's own parameter
  placeholder (itself just a fresh, unresolved `Var` from the *caller's*
  point of view — that other function's body is solved separately, in its
  own `solve()` call), the check sees `Var`, not a concrete type, and passes.
  Only a constraint that directly forces the marker to a concrete shape
  (`Named`, `Concrete`, `Tuple`, a function type, etc.) trips the check.

A second, real bug compounded the confusion while this was being debugged:
call-site instantiation used a disposable `TypeVarGenerator`
(`ctx.fresh_var_generator()`) that snapshotted `ctx`'s real counter without
ever advancing it, so every ordinary `ctx.fresh_var()` call later in the same
function body reissued the exact same ids — aliasing an opaque marker from
one call with an unrelated `TypeVar` from a later expression once three or
more opaque-returning calls appeared in the same scope. This made some
failures look like validation-logic bugs when they were actually a TypeVar
identity collision unrelated to validation at all. Fixed by minting the
renaming vars from `ctx.fresh_type_var_raw()` (the context's own live
generator) instead.

## Rationale

### Why incremental, not a single end-of-solve pass?

See "Superseded approach" above — the end-of-solve version cannot tell a
legitimate resolved-via-indirection chain apart from a real violation, since
both look identical (a concrete type) by the time the whole batch is solved.
Checking at each constraint's own composition point catches the violation at
the moment it happens, before further unification could make it
indistinguishable from a legitimate chain.

### Why not scattered per-AST-site guards?

Still avoided, for the same reasons as originally reasoned: per-site guards
(at `let` declarations, ascriptions, argument passing, etc.) would be
fragile and easy to miss. Constraint-level checking covers every site that
ultimately produces a constraint, which is all of them.

### Why not deeper `unify()` engine changes?

Still avoided for the same reason: `unify()` itself stays opacity-agnostic;
the check lives in the wrapper (`apply_constraint_with_coercion`) that
already exists specifically to layer additional validation
(`validate_literal_bindings`) on top of `unify()`'s raw result.

## Testing Strategy

1. **Positive test cases**: basic opaque returns, method calls on opaque
   returns, passing to `impl Aspect` parameters, linked vs. unlinked cases,
   and — added after the TypeVar-aliasing bug above — three or more
   independent opaque-returning calls in one scope with method calls on more
   than one before the last.
2. **Negative test cases**: explicit concrete type naming, casting to
   concrete types, passing to non-generic concrete parameters. (Three of the
   original negative fixtures for these cases were previously "passing" only
   because opacity wasn't enforced at all *and* the fixtures independently
   lacked the test harness's actual `// ERROR[T0NNN]` annotation — two
   unrelated bugs that happened to cancel out. Both are fixed.)
3. **Cross-module testing**: opacity preserved across module boundaries.

## Related Work

- **RFC-0037**: Return-Position `impl Aspect` - the original specification
- **ADR-0043**: Generic Type Arg Recovery from Field Values - related monomorphization patterns
- **Existing `validate_literal_bindings()`**: the pattern this approach still follows, just invoked at a finer grain (per-constraint rather than per-solve-batch)
