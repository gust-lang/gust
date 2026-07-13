# Issue #244: RFC-0060 (Aspect Impl Coherence) remaining scope

## Background

RFC-0060 is already `3-integrated` in metel-docs (`impl_status: in-progress`). Three
pieces are already implemented and must NOT be re-touched or re-derived:
- Orphan rule (issue #238)
- Concrete-impl overlap detection, i.e. two non-generic impls of the same aspect for
  the exact same concrete type (issue #238)
- Negative-vs-concrete-positive impl conflict/finality (issue #264) — an explicit
  `impl !Aspect for Type {}` colliding with an explicit `impl Aspect for Type {}` for
  the same concrete `Type` is already correctly rejected.

What's left, confirmed by direct tracing of the current `sprint/26` HEAD (post-#241)
against RFC-0060's actual text — not by assumption — is two independent gaps, both of
which existed before #241 but were explicitly deferred to it (three `TODO(#241)`
comments in the source say so verbatim) and were never picked back up when #241
landed:

1. **Blanket-impl-aware negative-bound discharge** (RFC-0060 §3) — struct/enum
   literal construction and RFC-0082 associated-type-completeness checking don't
   consult conditional impls at all, for either bound polarity.
2. **Blanket-impl disjointness/priority** (RFC-0060 §2/§5) — coherence overlap
   detection can't compare a blanket impl's canonical target shape against a
   concrete impl's, and there is no mechanism anywhere for an explicit negative impl
   to override a blanket positive impl the way RFC-0060 §5 requires.

Everything below was verified against the actual current source (this plan's author
independently re-checked the two highest-stakes claims — the raw `impl_aspect_env_has`
calls at the literal-construction sites, and the polarity-gated registration in
`registry.rs` with no negative-impl table anywhere — by reading the code directly, not
by trusting the research pass alone).

## Research findings

### 1a/1b. Struct and enum literal construction never see conditional impls

`construction.rs`'s struct-literal path (~1686-1750: positive bounds then negative
bounds) and enum-literal path (~2460-2520, same shape) both call
`ctx.registry.impl_aspect_env_has(module, type_arg_name, aspect)` **directly** on a
bare type name string. Contrast with the generic-function-call path
(`check_type_satisfies_bounds` / `check_type_does_not_satisfy_bound`, ~3146-3211,
used by `check_fun_call_neg_bounds`/`check_scheme_neg_bounds`), which was fixed during
#241's review to call `registry.type_satisfies_aspect(module, concrete_type, aspect)`
instead — the one function that actually consults `conditional_impl_bounds` in
addition to `impl_aspect_env`.

Net effect, for a struct/enum whose generic param has a bound satisfied only via a
conditional impl (e.g. `Pair<A, B>` conditionally implementing `Drop` when both `A`
and `B` do):
- **Positive bound false-rejection**: `Arena<T: Printable>` rejects a `T` that's only
  conditionally `Printable` (undocumented bug — no TODO comment marks this half at
  all, only the negative half is commented).
- **Negative bound false-acceptance (the actual soundness gap)**: `Arena<T: !Drop>`
  silently *accepts* a `T` that DOES implement `Drop` via a conditional impl, because
  `impl_aspect_env_has` reports "no impl" for anything registered only in
  `conditional_impl_bounds`.

Confirm before coding — this repro should silently succeed today when it must reject:
```metel
aspect Drop { fun drop(self); }
struct Arena<T: !Drop> { items: T[] }
struct Pair<A, B> { first: A, second: B }
impl<A: Drop, B: Drop> Drop for Pair<A, B> { fun drop(self) {} }
struct Resource { x: i64 }
impl Drop for Resource { fun drop(self) {} }
fun bad(r: Resource) -> Arena<Pair<Resource, Resource>> {
    Arena { items: [Pair { first: r, second: r }] }  // should be T0012, isn't today
}
fun main() {}
```

### 1c. RFC-0082 associated-type-completeness check can't see the impl's own bounds

`inference.rs`'s `Decl::Impl` handling (~481-599) runs the assoc-type-bound check
(~524-580) unconditionally, with no gate on `ib.generics`. When an aspect declares
`type Target: Bound;` and an impl binds `type Target = T;` where `T` is the impl's
OWN generic parameter (not a concrete type), `type_expr_to_infer_with_self` resolves
`T` as a bare `InferType::Named("T", [])` (it has no visibility into `ib.generics`),
and the completeness check then does `impl_aspect_env_has(module, "T", bound_aspect)`
— "T" is never a real registered type, so this is unconditionally `false`, and the
impl is rejected with T0012 **even when the impl's own bound on `T` already
guarantees the aspect's requirement**. This is a false-positive rejection of legal
code, not merely a missing check — confirmed unambiguous from the code (no
runtime-dependent branch).

Confirm before coding — this should be rejected today when it must be accepted:
```metel
aspect Deref { type Target: Display; fun deref(self: &Self) -> &Target; }
aspect Display { fun display(self) -> String; }
struct Wrapper<T> { value: T }
impl<T: Display> Deref for Wrapper<T> {
    type Target = T;
    fun deref(self: &Wrapper<T>) -> &T { &self.value }
}
fun main() {}
```

### 2a. Coherence overlap detection can't compare a blanket's canonical target shape against a concrete impl's

`coherence::check`'s grouping (~313-412) buckets impls by an exact-equality key
`(aspect_id, canonical_args, canonical_target)`. A blanket impl's canonical target has
`CanonicalType::TypeParam(i)` at generic positions; a concrete impl's has
`CanonicalType::Resolved(id, args)` at the same position. These never compare equal
(different enum variants), so a blanket impl and a concrete impl of the same aspect,
where the concrete impl's type is actually covered by the blanket, are **never even
grouped together** — `provably_disjoint` is never invoked for that pair, and no
conflict is ever raised, regardless of polarity. All existing conditional-impl-overlap
fixtures only test blanket-vs-blanket pairs (identically-shaped keys); none test
blanket-vs-concrete.

Confirm before coding — this should be T0015 today and isn't:
```metel
aspect Marker { fun mark(self) -> String; }
struct Foo<T> { value: T }
impl<T> Marker for Foo<T> { fun mark(self) -> String { return "blanket"; } }
impl Marker for Foo<i64> { fun mark(self) -> String { return "concrete"; } } // should be T0015
fun main() {}
```

### 2b. No mechanism lets a negative impl override a blanket positive impl

Exhaustive grep of every `ib.polarity`/`Polarity::Negative` use in the tree: every
registry-writing site is gated `if ib.polarity == Polarity::Positive { register... }`
— **a negative impl is never recorded anywhere as data**; it only ever prevents its
own positive-registration branch from running. There is no `neg_impl_env` table.
`type_satisfies_aspect` has exactly two ways to return `true` — `impl_aspect_env_has`
or `conditional_impl_bounds` — and neither ever checks "does an explicit negative
impl apply here and override this." `registry.rs`'s own comment at the negative-impl
gate (~468-471) says this is "still deferred... need RFC-0036/RFC-0072 to have real
semantics first" — both landed; the deferred work itself was simply never picked up.

This directly contradicts RFC-0060 §5's priority order: `1. explicit negative impl →
no aspect` must win over `4. blanket positive impl → has aspect`. Confirm before
coding — `f.mark()` dispatches via the blanket today, and `U: !Marker` would
(backwards) be REJECTED for `Foo<i64>` rather than accepted:
```metel
aspect Marker { fun mark(self) -> String; }
struct Foo<T> { value: T }
impl<T> Marker for Foo<T> { fun mark(self) -> String { return "blanket"; } }
impl !Marker for Foo<i64> {}  // should override the blanket for Foo<i64>
fun needs_not_marker<U: !Marker>(x: U) {}
fun main() {
    let f = Foo { value: 5 };
    needs_not_marker(f);  // should be ACCEPTED; today, rejected
}
```

### Explicitly out of scope (confirmed by re-reading RFC-0060 in full)

- **Auto-impl aspects** (RFC-0060 §4) — no `AspectDecl` field marking auto-impl
  exists yet; RFC-0096 (the mechanism-formalizing RFC) is still draft. Unrelated to
  #241, on its own timeline.
- **RFC-0097** (bare-parameter blanket impl orphan rule, `impl<T: Bound> Aspect for
  T`) — separate draft RFC, not RFC-0060's own scope. `coherence.rs`'s `outermost_id`
  already happens to produce the RFC-0097-recommended answer for this shape "by
  accident" (falls through to `None`/requiring aspect-locality) — leave this alone,
  don't try to certify or harden it as part of #244.
- **Negative impls with their own conditional bounds** (`impl<T: !Copy> !Aspect for
  T`) — genuinely unspecified by any RFC (flagged as "OPEN QUESTION (decision 9)" in
  `registry.rs` ~494-504 already). Scope #244's negative-impl work to **concrete
  (non-generic-impl) negative impls only** — every RFC-0060 §5 example is exactly
  this shape. Do not attempt to derive semantics for the generic/conditional case.
- Two negative impls of the same aspect for overlapping targets — not discussed by
  RFC-0060, not touched here.

## Design decisions

### Fix for 1a/1b: route literal construction through the existing type-satisfying helpers

Don't duplicate `impl_aspect_env_has` calls — replace the four call sites (struct
positive ~1699, struct negative ~1731/1738, enum positive ~2473, enum negative
~2500/2504) so they call the SAME `check_type_satisfies_bounds` /
`check_type_does_not_satisfy_bound` helpers (~3146-3211) the generic-function-call
path already uses. Those two helpers already: take a full `Type` (not a bare name),
call `type_satisfies_aspect` (conditional-impl-aware), and implement the
Copy-implies-!Drop override for the negative case. This is a refactor toward a single
source of truth, not new logic — the risk is entirely in making sure the call
signatures line up (these helpers currently take `fun_name: &str` for the error
message; the struct/enum call sites will need to pass the struct/enum's own name
instead, adjusting the error message text but not its error code).

### Fix for 1c: thread the impl's own generic bounds into assoc-type completeness

When `concrete_ty_expr` resolves to `TypeExpr::Named(name, [])` where `name` matches
one of `ib.generics`' own parameter names, don't attempt
`impl_aspect_env_has(module, name, bound_aspect)` (guaranteed false). Instead, look up
that generic parameter's own declared bound set from `ib.generics` (or the
`where_clause`) — reuse `collect_type_param_bounds`/`synth_generics_for_impl`
(`registry.rs`, already built for #241) rather than writing new bound-extraction
logic — and check whether `bound_aspect` is directly present among that parameter's
positive bounds. Present → satisfied by construction, accept. Absent → still reject
with T0012 (unchanged behavior for the case that's actually a bug in the user's impl).
When the binding resolves to an actual concrete type (the common, non-generic case),
keep the existing check but route it through `type_satisfies_aspect` too (same
conditional-impl-awareness fix as 1a/1b, since `type Target = Pair<i64,i64>` should
also account for `Pair`'s own conditional impl).

### Fix for 2a: shape-crossing overlap comparison

Replace the exact-equality `HashMap` bucketing for overlap detection with a pairwise
scan (across ALL impls of a given aspect — the counts here are small, no need for a
cleverer structure) using a **unification-style compatibility test** between two
canonical targets, where `CanonicalType::TypeParam(_)` is compatible with anything at
that position (it represents an as-yet-unconstrained slot) and two
`CanonicalType::Resolved` nodes are compatible only if their ids match and their args
are recursively compatible. This subsumes the existing exact-equality behavior
(same-shaped keys are always compatible with themselves) so it should not regress any
currently-passing fixture — that's the main regression risk to verify.

Once two impls are found target-compatible, decide conflict/no-conflict by polarity
FIRST, then by bound-disjointness only within a polarity class:
- **Both positive**: this is where 1a/1b's fix pays off directly — for a
  blanket-vs-concrete pair, "are they actually simultaneously satisfiable" reduces to
  "does the concrete impl's specific type argument at each bound position satisfy the
  blanket's bound requirement there" — i.e. call `type_satisfies_aspect` per position,
  reusing the exact function just made conditional-impl-aware in 1a/1b. If it's
  satisfied at every position, this is a real conflict (T0015). If not, the blanket
  simply doesn't apply to that concrete instantiation — no conflict. For a
  blanket-vs-blanket pair, keep using the existing `provably_disjoint` bound-list
  comparison (~252-272) unchanged — that logic is already correct and tested.
- **One positive, one negative** (either order): per RFC-0060 §5, this is always
  **permitted, no conflict** — the negative impl wins, full stop, no bound-disjointness
  check needed. This is the case that must NOT start false-positiving once the
  shape-crossing compatibility test above stops treating blanket/concrete pairs as
  automatically non-overlapping — be deliberate about ordering the polarity check
  before the disjointness check, not after.
- **Both negative**: out of scope (see above) — leave whatever happens today
  unchanged; don't add new logic for this combination.

### Fix for 2b: a real negative-impl registry table, consulted with priority

Add `neg_impl_env: HashMap<(SymbolId, String), Vec<Vec<Type>>>` to
`TypeDefinitionRegistry` (same shape as `impl_aspect_env`), populated ONLY for
negative impls with `ib.generics.is_empty()` (the concrete-negative-impl scoping
decision above — note this correctly includes `impl !Marker for Foo<i64> {}`, since
that impl itself declares no generics even though `Foo` is a generic struct; it
excludes `impl<T> !Marker for Foo<T> {}`, which stays out of scope). Wire population
into `registry.rs`'s existing polarity-gated block — add an `else if
ib.polarity == Polarity::Negative && !impl_has_generics` branch alongside the current
`if ib.polarity == Polarity::Positive` one.

Consult it in `type_satisfies_aspect`: before returning `true` from either
`impl_aspect_env_has` or the `conditional_impl_bounds` fallback, check whether
`neg_impl_env` has a matching `(target_id, aspect_name)` entry whose stored type-arg
list matches the query's concrete type args — if so, return `false` immediately
(negative impl wins over everything, implementing priority rule 1 > 4). This ordering
matters: check negative-impl override BEFORE consulting the positive paths, not after.

## Explicitly out of scope (repeated for the "your task" instructions to see directly)

- Auto-impl aspects (RFC-0080/RFC-0096) — unrelated, own timeline.
- RFC-0097 (bare-parameter blanket impl orphan rule) — separate RFC, don't touch.
- Negative impls with their own conditional/generic bounds — explicitly unscoped per
  decision 9; do not invent semantics for this.
- Two negative impls overlapping — not discussed by RFC-0060, don't add logic.
- The stale "not yet implemented" banner at
  `metel-docs/public/reference/spec/declarations.md` around the coherence section —
  this is a metel-docs correction, not a metel-core code change; leave a note in your
  final summary but do not edit metel-docs from this worktree.

## Order of implementation

1. **Struct/enum literal construction fix** (1a/1b) — lowest risk, most mechanical:
   route all four call sites through `check_type_satisfies_bounds` /
   `check_type_does_not_satisfy_bound`. Commit with both the false-negative-rejection
   fixture (positive bound satisfied only via conditional impl, must now accept) and
   the soundness-gap fixture (negative bound violated only via conditional impl, must
   now reject) from the Research findings above.
2. **Assoc-type completeness fix** (1c) — thread `ib.generics`/bound lookup into the
   `inference.rs` check. Commit with the `Deref`/`Display` repro fixture (must now
   accept) plus a negative counterpart (impl's own bound does NOT cover the aspect's
   requirement — must still correctly reject).
3. **Negative-impl registry table + priority consultation** (2b) — add
   `neg_impl_env`, wire registration, wire the priority check into
   `type_satisfies_aspect`. Commit with the `Foo<T>`/`Marker` repro fixture (must now
   accept) plus a fixture confirming an *unrelated* type without a negative impl still
   correctly resolves the blanket (regression guard).
4. **Coherence shape-crossing overlap + polarity-aware conflict decision** (2a) —
   the highest-risk step; budget the most review/test time here since it's a real
   algorithmic change to `coherence::check`'s grouping, not a call-site swap. Commit
   with: the blanket-vs-concrete-positive conflict fixture (must now be T0015), the
   blanket-vs-concrete-negative permitted fixture (must NOT be T0015 — this is the
   case most likely to regress if step 4 is implemented carelessly after step 3), and
   re-run every existing `conditional_impl_*`/`orphan_impl_*` fixture to confirm zero
   regressions in already-passing blanket-vs-blanket and concrete-vs-concrete cases.
5. Full-suite final verification (see checklist below).

## Test fixtures to add

Following this repo's two established conventions exactly (do not invent a third):

- `tests/integration/sources/typechecking/generics/` (numbered `stageN_`/`stageN_negM_`
  flat `.mtl` files, continuing the `stage16`(negative bounds)/`stage17`(conditional
  impls) numbering established by #243/#241):
  - `stage16_08_struct_negative_bound_satisfied_via_conditional_impl.mtl` (positive:
    `!Drop` bound correctly holds because the conditional impl's condition ISN'T met)
    + `stage16_neg_06_struct_negative_bound_violated_via_conditional_impl.mtl` (the
    `Arena<Pair<Resource,Resource>>` repro above — must now reject).
  - `stage17_06_conditional_impl_bound_satisfied_at_struct_literal.mtl` (positive
    bound on a struct literal's type param, satisfied only via a conditional impl —
    must now accept) + `stage17_neg_04_conditional_impl_bound_not_satisfied_at_struct_literal.mtl`.
  - `stage17_07_assoc_type_bound_satisfied_via_impl_own_generic_bound.mtl` (the
    `Deref`/`Display` repro above) + `stage17_neg_05_assoc_type_bound_not_covered_by_impl_own_bound.mtl`.
- `tests/integration/sources/typechecking/aspects/` (named directories, one `main.mtl`
  each, `// ERROR[T0NNN]` inline comment for expected-failure cases):
  - `blanket_vs_concrete_impl_conflict/` (the `Foo<T>`/`Marker` overlap repro above —
    must now be `// ERROR[T0015]`).
  - `negative_impl_overrides_blanket_impl_permitted/` (same shape, but the concrete
    impl is `impl !Marker for Foo<i64> {}` — must NOT error; also assert `f.mark()`
    still dispatches to the blanket impl's body for a `Foo<i64>` value where the
    negative impl doesn't apply to method dispatch itself, only bound satisfaction —
    check this distinction against RFC-0060's text and flag if it's ambiguous rather
    than guessing).
  - `blanket_positive_still_applies_unrelated_concrete_type/` (regression guard:
    `Foo<bool>` with no negative impl of its own must still resolve `Marker` via the
    blanket, confirming step 3/4 didn't break the unconflicted case).

## Final verification checklist

1. `cargo build` and `cargo build --release` clean.
2. Re-run every existing fixture under `typechecking/aspects/` and
   `typechecking/generics/stage16_*`/`stage17_*` — confirm zero regressions,
   especially the existing blanket-vs-blanket conditional-impl-overlap fixtures from
   #241 (`conditional_impl_non_disjoint_rejected`, `conditional_vs_unconditional_impl_conflict`,
   `conditional_impl_different_letters_overlap`) and the existing RFC-0072 negative-bound
   fixtures from #243.
3. **Run `cargo test --release` as a single, full-suite invocation** — not `--lib` or
   `--test integration` separately; those silently skip the `tests/unit.rs` binary
   target (this exact gap caused a real undetected break during #242's work).
4. `cargo clippy --release --lib -- -W clippy::pedantic` clean — zero warnings, no
   exceptions. (An earlier automated implementation on this exact codebase, #241,
   self-reported "zero warnings" when there were actually 8; independent review will
   re-run this, so don't rely on your own summary being trusted verbatim.)
5. All 12 new fixtures above pass with their expected status (success or the specific
   `T0NNN` code), confirmed by actually running the full suite, not by inspection.
6. Do not claim any pre-existing test failure via `git stash` — if a test fails,
   diagnose it against the code you just wrote; a prior session's implementation
   (#243) made exactly this mistake and it was only caught by independent review.
7. Correct the stale banner text at
   `metel-docs/public/reference/spec/declarations.md` — **do not edit this yourself**
   (it's a different repository); just note in your final summary that it needs
   correcting, quoting the current stale sentence, so whoever does the metel-docs
   lifecycle update afterward doesn't need to re-discover it.

## Your task

Implement the plan above end to end in this worktree, following "Order of
implementation" steps 1 through 5 exactly as written, including verification and git
commits after each step. Do not push or open a PR. Do NOT delegate to a subagent/task
tool for this — work through it directly yourself, step by step. Pay special
attention to the Final verification checklist, especially running the FULL
`cargo test --release` (both test binaries) and not falsely claiming a test failure
is pre-existing based on an invalid `git stash` check. Be aware this codebase's
registry/inference/construction/coherence layers were recently touched by issue #241
— re-read the CURRENT state of any file you modify rather than assuming it matches
this plan's line-number references verbatim (they were accurate at the time this plan
was written, but re-confirm before editing). Step 4 (coherence shape-crossing overlap)
is the highest-risk step in this plan — take extra care there, and do not skip any of
its three required fixtures. Report back a full summary at the end, including the
stale-banner note from the verification checklist.
