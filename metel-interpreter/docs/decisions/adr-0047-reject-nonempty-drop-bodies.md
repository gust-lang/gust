---
id: adr-0047
title: "Reject a Non-Empty drop() Body Rather Than Ship It Inert"
date: '2026-08-03'
status: accepted
relates: adr-0045
implements: issue #345
---

## Context

RFC-0071 §9c recorded a release gate: **"#290 must not ship without #292."** #290
declares the `Drop` aspect and enforces its eligibility rules (`Copy`/`Drop` mutual
exclusion, `!Drop` bounds, the partial-move ban on `Drop` types); #292 (drop order and
explicit destructor invocation) is what actually *runs* a `drop` method when a value goes
out of scope. Shipping #290 without #292 means `extend Handle: Drop { fun drop(self) {
close_the_handle(); } }` compiles, looks like a working destructor, and the body **never
runs** — a feature that appears functional and silently does nothing, the exact failure
mode this gate exists to catch (already hit twice elsewhere in this project).

#292 moved to v0.13.0 along with the rest of the ownership block (#293, #310, #328, #330,
#338, #342) on 2026-07-31, while #290 had already shipped in v0.12.0. The gate fired. Its
literal remedy — reject `Drop` impls outright until #292 lands — was rejected as too broad:
it conflates two things that are actually separate. Declaring `Drop` has real, immediately
useful type-level effects today (the `Copy`/`Drop` exclusion, `!Drop` bounds, the
partial-move ban) that have nothing to do with whether a body runs. Rejecting the
*declaration* would take away a working feature to guard against a body that silently
doesn't run — throwing out the correct 90% to guard the broken 10%.

## Decision

Reject a **non-empty `drop` method body** specifically, not the `Drop` impl itself.
`check_impl_decl` (`src/typechecker/construction.rs:1023-1049`) matches the aspect by its
*declaring module* (`std::core`), not by name — so a user module's own unrelated `Drop`
aspect is unaffected, the same discipline `coherence.rs` already uses for the `Copy`/`Drop`
exclusion check. For every `drop` method on an `extend T: Drop` block whose declaring
aspect is `std::core::Drop`:

```rust
let body_is_empty = method.body.stmts.is_empty() && method.body.tail.is_none();
if body_is_empty {
    continue;
}
return Err(MetelError::type_error(
    TypeErrorCode::T0001,
    "a `drop` body cannot run yet: destructor invocation is not implemented \
     (metel-core#292), so this cleanup would silently never happen. Leave the \
     body empty to declare the type `Drop` for its type-level effects — \
     `Copy` exclusion, `!Drop` bounds, and the partial-move ban — or move the \
     cleanup into an ordinary method the caller invokes"
        .to_string(),
    &method.span,
));
```

`extend T: Drop { fun drop(self) {} }` (empty body) remains legal and is the documented
way to opt a type into `Drop`'s type-level effects in v0.12.0. Any non-empty body is a
compile error naming the exact blocking issue, not a silent no-op.

## Consequences

- The release gate is discharged by a narrower rejection than its own wording proposed —
  recorded directly in RFC-0071 §9c ("Discharged 2026-07-31 (#345), by a narrower
  rejection than this section's wording") and here, so a future reader finds the same
  reasoning from either document.
- A user who writes real cleanup code in `drop` gets a compile-time error pointing at
  #292, not a runtime program that quietly leaks/never cleans up — the failure mode the
  gate exists to prevent is closed for v0.12.0.
- `getting-started/structs-and-methods.mdx`'s Copy/Drop tutorial section (added the same
  day this ADR was written, for issue #375) demonstrates exactly this: an empty-body
  `Drop` impl, with prose explaining why a non-empty body is rejected. Written from the
  same source (this restriction, and RFC-0071 §9c's discharge note) independently — no
  discrepancy found between what the tutorial teaches and what the typechecker enforces.
- When #292 lands, this restriction should be removed in the same change that makes
  destructor invocation real — a `drop` body would no longer be silently-never-run, so the
  restriction's own justification disappears with it.
