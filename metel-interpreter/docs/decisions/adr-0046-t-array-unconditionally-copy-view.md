---
id: adr-0046
title: "T[] Is Unconditionally Copy, Hardcoded in the Typechecker Like [T; N]"
date: '2026-08-03'
status: accepted
relates: adr-0043
implements: issue #317
---

## Context

RFC-0126 (`T[]` as a Copy Borrowed View) decided `T[]` becomes a non-owning, immutable
view — a pointer and a length, produced only by borrowing a `List<T>`, a `[T; N]`, or
another slice — and that it is `Copy` **unconditionally**, not conditional on `T: Copy`,
because a view holds a location, not a `T`. Array literals retype from `T[]` to `[T; N]`,
since a literal has a statically known length and owns its elements.

Implementing "unconditionally `Copy`" required deciding *where* that fact lives. Every
other `Copy` rule in the language is either a stdlib blanket impl (`extend<T: Copy> Foo<T>:
Copy;`) or, for `[T; N]`, a hardcoded typechecker case tracked by #299 pending const
generics (`[T; N]`'s arity `N` cannot appear in a stdlib blanket impl signature at all
today — see RFC-0124 Open Question 6a). `T[]` cannot use a stdlib blanket impl either, for
a different reason: `Copy` for a view type must hold regardless of whether the *element*
type is `Copy`, and the aspect system has no way to write "always satisfied, ignore `T`
entirely" as a conditional impl — every existing conditional-impl mechanism exists to
express a dependency on `T`, not its absence.

## Decision

`is_copy`/`type_satisfies_aspect`'s `InferType::Array(elem)` arm
(`src/typeinference/mod.rs:2140-2143`) returns `true` for the `Copy` aspect
unconditionally, before even inspecting `elem`:

```rust
InferType::Array(elem) => {
    if aspect_name == "Copy" {
        return true;
    }
    // ... conditional-impl checks for every other aspect still inspect elem
}
```

This is deliberately the same shape as `[T; N]`'s existing hardcoded `Copy` case
(`InferType::SizedArray`, a few lines above, tracked by #299) — both are typechecker
special cases standing in for a stdlib blanket impl the aspect system cannot express yet,
`[T; N]` for lack of const generics, `T[]` for lack of a way to write "regardless of `T`."
Unlike #299, closing this one does not wait on a language feature: nothing about `T[]`'s
`Copy`-ness depends on `T` at all, so there is no future stdlib impl this hardcoding is a
placeholder for — it is expected to stay hardcoded indefinitely, not migrate out the way
#299 eventually will. All other aspects (`Display`, `Clone`, `Eq`, ...) continue to be
ordinary stdlib blanket impls on `T[]`, conditional on `T`, unaffected by this special case.

`T[]: Clone`'s existing stdlib impl (`stdlib/core.mtl`) was rewritten to a trivial identity
copy:

```metel
extend<T: Clone> T[]: Clone {
    fun clone(&self) -> Self {
        return self;
    }
}
```

Under the pre-RFC-0126 owning-buffer model, `clone` allocated a fresh `List<T>` and
returned `.as_slice()` on it — a view into a local about to go out of scope, safe only
because the evaluator deep-copied at the return boundary regardless. Under the view model
there is nothing to allocate: cloning a `Copy` view is `*self`, identical to what `Copy`
already gives for free. RFC-0126 flagged this rewrite as a required consequence of its own
decision, not a new question to reopen — recorded here because it's the one place the
`Copy`-view decision actually changed shipped stdlib code, not just the typechecker.

## Consequences

- `T[]: Copy` and `[T; N]: Copy` (when `T: Copy`) are now both real, closing #290's
  previously-blocked question for `T[]` specifically (RFC-0126 Consequences).
- `#291`'s move checker gains a real rule for `T[]` instead of treating it as affine by
  default — reuse of a `T[]` binding is legal by construction, not a special case the move
  checker has to know about.
- This hardcoding does not appear anywhere in `stdlib/core.mtl` and cannot be searched for
  as a blanket impl — a future reader grepping the stdlib for `T[]: Copy` will find nothing
  and should look here and at `src/typeinference/mod.rs:2140` instead.
- Unlike #299, there is no follow-up issue tracking migrating this out to stdlib, since no
  language feature would ever let it move — recorded so this isn't mistaken for a
  temporary gap the way #299 explicitly is.
