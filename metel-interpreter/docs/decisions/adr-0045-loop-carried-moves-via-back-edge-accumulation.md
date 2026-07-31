---
id: adr-0045
title: "Loop-Carried Moves via Back-Edge Accumulation, and a Standalone Place Abstraction"
date: '2026-07-31'
status: accepted
relates: adr-0035
implements: issue #291
---

## Context

Issue #291 (RFC-0071 2/4) carries five acceptance criteria for move checking. An
acceptance audit found two of them unmet.

**Criterion 3 — "move tracking per binding through control flow (both branches of an
`if`, loop bodies)".** Every loop form walked its body exactly once:

```rust
let mut body_state = state.clone();
self.check_block(&while_stmt.body, current_module, &mut body_state);
state.union_from(&body_state);
```

The body's exit state was unioned *outwards*, into the code after the loop, but never
fed back *inwards*. A move inside a loop body was therefore invisible to the next
iteration, and this exited `0` under `--move-check`:

```metel
let s = "hello";
var i = 0;
loop {
    i += 1;
    let moved = s;         // moves `s` again on every iteration
    if (i == 2) { break; }
}
```

The same move observed *after* the loop was rejected correctly, which is what the one
existing loop fixture covered. A false negative in the core analysis, and the more
serious direction of error.

**RFC-0071 §9b — the place abstraction.** §9b requires that whatever represents `x`,
`x.f`, `x.f.g`, and "reached through a dynamic index" be a standalone, reusable
component with no move-specific assumptions, so borrow checking can later run a second
analysis over the *same* places with no rework — otherwise the borrow checker rebuilds
them and the two analyses disagree about partial moves. `Place`, `Projection` and
`is_prefix_of` were genuinely analysis-neutral, and move state was held separately in
`FlowState`/`MoveRecord`. But the module was `move_check::place`, so a borrow checker
would import from the move checker's namespace; and `from_typed_place` ended
`TypedPlace::Deref { .. } => None`, with `from_expr`'s `_ => None` dropping the
expression-side deref too. It could not represent `*p` at all — and extending it later
is exactly the rework §9b exists to prevent.

## Alternatives considered

**A naive fixed point** — widen the body's entry state with its whole exit state and
re-walk. This is the obvious reading of "iterate to a fixed point", and it is wrong
here: it rejects

```metel
loop { let moved = s; break; }
```

because the move reaches the widened entry state even though no second iteration ever
observes it. Rust accepts this program. Trading a false negative for a false positive
is a bad deal for a checker users opt into, and it would have rejected the existing
`09_move_in_loop_body_observed_after_loop` fixture's shape.

**Build a CFG for the move checker.** The precise answer, and disproportionate. Every
pass in this interpreter is an AST walker; introducing a CFG for one analysis means
either a second lowering or rewriting the walker. The only reachability fact the fixed
point actually needs is "does this path reach the back edge", which is one bit.

**A `suppress_reporting` flag on the checker** instead of rewinding the report. Workable,
but every recording site would have to consult it, and the report also carries counters
(`skipped_generic_bodies_user`, `unchecked_generic_bodies`) that would each need the same
guard. A mark-and-rewind cannot get out of sync with a site that forgot to check a flag.

**Deduplicate violations by `(place, span)` at the end** rather than not producing
duplicates. Rejected: it would also collapse genuinely distinct repeated diagnostics, and
it treats a symptom of walking the body N times as if it were a property of the report.

**A new `MoveViolationKind` for loop-carried moves.** The kind says *which rule was
broken*; whether the move arrived round a back edge is orthogonal to that and applies
equally to `UseAfterMove` and `PartialMoveUsedAsWhole`.

## Decision

### 1. Loop bodies are analysed to a fixed point over the back edge, not the exit state

`Checker::check_loop_body` drives every loop form — `while`, C-style `for`, `for-in`,
and the `loop` expression — through a closure that performs one pass over the body. The
`while` condition and the `for` step are inside that closure, because the back edge
returns to them; `for`'s `init` and `for-in`'s iterable stay outside, because they run
once.

Each pass collects the state that reaches the loop's **back edge**, which is not the same
as the body's exit state:

- the bottom of the body, but only when control falls through it;
- plus every `continue` site.

Two accumulator stacks on `Checker` carry this, innermost loop last:

- `loop_back_edges` — a `continue` merges its state here (`Checker::reach_back_edge`);
- `loop_exits` — a `break` merges its state here.

`FlowState` gains a `diverged: bool`, set by `break`, `continue` and `return`, meaning
"this path has left the iteration". It is not part of the moved-state lattice, and
`union_from` deliberately leaves it alone. `observe_if_expr` and `observe_match_expr`
join it the only way that is sound — an `if` diverts control only if *both* branches
diverge, a `match` only if every arm does — and, critically, **omit a diverging branch's
moves from the join**, because control never reaches the following code that way.

The driver widens the entry state with the back edge until the moved state stops growing
(compared through `moved_fingerprint`, an order-independent `BTreeSet`, since `moved` is
a `HashMap` of `Vec`s whose iteration order is not stable). After the loop, `state`
receives the body's exit state (unless every path diverged) plus the accumulated
`loop_exits`, so a move that breaks out is still visible afterwards.

`MAX_LOOP_PASSES = 8` caps the iteration. Widening is monotone, so a body converges in
one extra pass unless moves cascade through several bindings; stopping at the cap can
only lose a violation the next pass would have found, never invent one.

### 2. Only the last pass reports; earlier passes are rewound

`MoveCheckReport::mark` captures all four accumulators (violations, both skip counters,
unchecked bodies) and `rewind_to` restores them. The driver marks before each pass and
rewinds only when it is going to widen and walk again.

The passes are ordered so the common case is unchanged in cost: the *first* pass reports,
and its diagnostics are kept if widening produced nothing new. A loop whose body moves
nothing is therefore walked exactly once, as before. A loop that does widen is walked once
more per widening step, and nesting multiplies that — bounded by `MAX_LOOP_PASSES` per
level, and unobservable in the suite's runtime.

### 3. A loop-carried move says which iteration it means

A loop-carried move is usually *its own use*: the same expression, one iteration later.
Reporting it with the existing wording produced

```
[T0019] type error in main.mtl:6:21: use of moved value `s`: `s` was moved at main.mtl:6:21
```

which points the reader back at the line they are already on, and trips the invariant
`move-check-count` asserts (`move site reported as its own use`). `MoveRecord` gains
`from_previous_iteration`, set by `FlowState::mark_moves_as_carried_from` on exactly the
records that were not in the entry state before widening. It surfaces on `MoveViolation`
as `moved_in_previous_iteration`, and `moved_at_clause` phrases the message from where
the reader is standing:

```
`s` was moved here on an earlier iteration                 // same site
`s` was moved at main.mtl:8:21 on an earlier iteration     // different site
`s` was moved at main.mtl:30:14                            // not loop-carried
```

`move-check-count`'s assertion is narrowed to allow that shape and nothing else, rather
than deleted.

### 4. `place` moves to the crate root and gains `Projection::Deref`

`src/move_check/place.rs` becomes `src/place.rs`, declared in both `lib.rs` and
`main.rs`, so neither analysis owns it. `Projection` gains `Deref`, documented as "the
pointee of a reference" alongside `OpaqueIndex`'s "reached through a dynamic index".
`from_typed_place`'s `Deref` arm bridges to `from_expr`, since `TypedPlace::Deref` holds
a `TypedExpr` rather than a nested place (per adr-0035), and `from_expr` handles
`UnaryOp::Deref`, which `typed_ast` already documents as a place per RFC-0110 §6.

Policy stays with each analysis. That a move out of a dynamically indexed element is
rejected, or that a move through a reference needs a reborrow, are facts about *moves*
and remain in `move_check`; `place` only says such a place exists and how it relates to
its prefixes. Rendering moves to `Display for Place` — analysis-neutral, and previously
duplicated between `move_check` and the `move-check-count` binary.

## Consequences

- Criterion 3 is met. A move in a `loop`, `while`, `for` or `for-in` body is now caught
  on the next iteration, including when the use is textually *earlier* than the move (a
  `while` condition reading a binding the body moves).
- `loop { let moved = s; break; }` stays accepted, and so does a move on a branch that
  breaks or returns. Omitting a diverging branch from the join also removed a
  **pre-existing false positive outside loops**: a move in a returning `if` branch was
  being joined into the code after the `if`.
- §9b is met. A borrow checker can depend on `crate::place` without depending on
  `move_check`, and `*p` is representable, so adding that analysis needs no change here.
- Making a dereference nameable is a behaviour change in its own right, not only
  plumbing: moving the same value out of a reference twice is now caught rather than
  ignored.
- `move-check-count` over the pre-change corpus is byte-identical — 30 fixtures, 32 user
  violations, 4590 embedded-std, same spans and places. Neither change moves an existing
  diagnostic.
- Divergence does not propagate outward through a nested loop: a `return` inside an inner
  loop does not mark the outer body's path as diverged, so the outer loop's back edge may
  include moves that only happen on a returning path. This is conservative in the
  false-positive direction and is the price of not building a CFG. No corpus program hits
  it.
- The join itself remains conservative: a branch's moves are unioned without asking
  whether the branch can be taken. Divergence is the only reachability fact the checker
  uses.
- `MoveViolation` gains a public field, so any consumer constructing one exhaustively
  must supply it. The two in-tree consumers are `move_check` itself and
  `move-check-count`.
