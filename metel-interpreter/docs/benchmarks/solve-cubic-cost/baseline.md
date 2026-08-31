# `ctx.solve()` cost — baseline before optimization

Follow-up to the METEL-177 pass (`../v0.8.2-evaluator-integration/typechecker-profiling.md`).
That pass made `InferContext::solve` incremental (stop cloning + re-solving the
whole constraint list every call). It was a large win but a **symptom fix**:
`solve` is still the dominant typecheck sub-phase on generic-heavy programs, and
the underlying cost model is unchanged.

This note establishes the baseline the next optimization pass measures against.

## Stress fixtures

`metel-interpreter/benches/stress/` — `deep_type.mtl`, `solve_storm.mtl`,
`id_chain.mtl`. See that dir's README. Run with `metel-bench --fixtures-dir`.

## Headline: inference cost is ~cubic in generic-type nesting depth

`deep_type` at depth `n` is `let v0 := 0i64; let vk := wrap(v{k-1});` — the type
of `vn` is `List` nested `n` deep. Nothing else varies. `metel-bench`, release,
5 iterations:

| depth | parse ms | inference ms | solve ms | typecheck ms | solve_calls | constraints |
|------:|---------:|-------------:|---------:|-------------:|------------:|------------:|
|  40   |  132.8   |      13.7    |   12.7   |     41.7     |    314      |    379      |
|  80   |  145.5   |      53.3    |   67.9   |    147.6     |    354      |    419      |
| 120   |  162.4   |     186.9    |  226.3   |    453.2     |    394      |    459      |
| 160   |  163.9   |     802.4    |  640.5   |   1510.2     |    434      |    499      |
| 200   |  184.9   |    1871.3    | 1255.5   |   3237.4     |    474      |    539      |
| 300   |  259.3   |    7068.7    | 4215.8   |  11483.6     |    574      |    639      |

- **`solve_calls` and `constraints` grow *linearly*** with depth (≈ +1 call, +0.67
  constraints per level). The blow-up is **not** call volume.
- **`inference` and `solve` each grow ≈ O(depth³).** Doubling depth 80→160:
  `solve` ×9.4, `inference` ×15. 120→300 (×2.5): `solve` ×18.6, `inference` ×38.
- `inference ns` here is *inference minus solve_ns* (harness definition), so the
  eager `apply`/`unify`/scheme-instantiation work outside `solve()`'s body is
  cubic too — same root cause, wider blast radius.
- `parse` grows ~linearly and is not the target.

End-to-end wall time, single `parse→typecheck→eval` run (`deep_type`, eval is
trivial here):

| depth | 40 | 80 | 120 | 160 | 200 | 250 | 300 | 400 | 500 | 700 | 1000 |
|------:|---:|---:|----:|----:|----:|----:|----:|----:|----:|----:|-----:|
| ms    | 410 | 621 | 1064 | 1961 | 3529 | 6660 | 11742 | 27637 | 53317 | 154463 | >180000 (timeout) |

## Default-size fixtures (release, 15 iterations)

| fixture | typecheck ms | solve ms | solve % of tc | inference ms | solve_calls | constraints | µs / solve call |
|---|---:|---:|---:|---:|---:|---:|---:|
| `deep_type` (nest 90)        | 165 |  80 | 48% |  59 | 364 |  429 | 220 |
| `solve_storm` (250 methods)  | 108 |  41 | 38% |  46 | 858 | 1595 |  48 |
| `id_chain` (400 `id` calls)  |  50 |  13 | 27% |  21 | 711 |  817 |  19 |

`deep_type` is the clearest: 220 µs per `solve()` call, ~half of typecheck,
driven entirely by type-structure depth.

## Root cause (why it is cubic)

`Substitution` is a bare `HashMap<TypeVar, InferType>` with **no path
compression** (`typeinference/mod.rs`):

1. `Substitution::apply` (mod.rs:418) recurses structurally over the type *and*
   down binding chains (`Var(v) => self.apply(resolved)`). Depth of one `apply`
   = type nesting + longest var chain, and chains only grow within a solve.
2. `unify` (mod.rs:542) eagerly re-applies the whole substitution to every
   subterm (`unify(&subst.apply(p1), &subst.apply(p2))` per element), then
   `compose_in_place` walks every binding — O(n²) per unify over an n-ary type.
3. Each of the linearly-many `solve()` / eager-inference steps pays that growing
   O(n²) apply cost → O(n³) overall.

This is also the stack-overflow vector (`apply`/`occurs_in`/`unify` recursion
depth is unbounded; `ulimit -s` was not enforced in the measurement sandbox, so
the exact crash depth is not pinned here — the cubic time on the same code path
is the signal).

## Optimization candidates (measure against this)

1. **Union-find `Substitution`** — `find(var)` path-compresses; only
   representatives carry a structured binding. Collapses (1) and the O(n²) in
   (2). Primary root-cause fix.
2. Iterative `apply` / `occurs_in` / `unify` (heap worklist) — removes the
   structural-recursion stack term that survives (1).
3. Stop cloning `cached_subst` in `solve()` — mutate in place, clone only on the
   error path (or return `&Substitution` / `Rc`; ~half the `solve()?` sites just
   `.apply()` the result and drop it).
4. `Rc<InferType>` subterms — deep `clone()` → refcount bump (larger refactor).

Recommended: prototype **1 + 3** on a branch, re-run `metel-bench --fixtures-dir
metel-interpreter/benches/stress` plus the depth sweep, compare `solve_ns` /
`inference_ns` / wall-time-vs-depth against the tables above.

## Prototype v1 (this branch) — `#3` + partial `#1`

Changes in `typeinference/mod.rs` (+ one caller):

- **`#3`**: `cached_subst: Rc<Substitution>`; `solve()` mutates in place via
  `Rc::make_mut` and returns an `Rc` handle — was two full deep clones per call.
  The one speculative `solve()` caller checkpoints/restores.
- **partial `#1`**: `unify`'s sub-term recursion goes through `unify_seq`, which
  skips `acc.apply(x)`/`acc.apply(y)` while the running accumulator is empty
  (the deep-nested-equal-type case). Empty-substitution / empty-delta fast paths
  on `apply` / `compose` / `compose_in_place`.
- **not done**: path compression / union-find `find`. `Substitution` is still a
  raw chain map.

Results — `945` integration + `139` + `139` unit tests green, no regressions.

Default fixtures (typecheck ms, before → after):

| fixture | typecheck | inference | solve |
|---|---|---|---|
| `deep_type` (90) | 165 → **70** (−58%) | 59 → **7.7** (−87%) | 80 → 37 |
| `solve_storm` (250) | 108 → **56** (−48%) | 46 → 18 | 41 → 19 |
| `id_chain` (400) | 50 → **29** (−42%) | 21 → 9 | 13 → 7 |

Depth sweep (`deep_type` wall ms, before → after):

| depth | 40 | 80 | 120 | 160 | 200 | 300 | 400 | 500 |
|------:|---:|---:|----:|----:|----:|----:|----:|----:|
| before | 410 | 621 | 1064 | 1961 | 3529 | 11742 | 27637 | 53317 |
| after  | 347 | 447 |  713 | 1182 | 1951 |  6438 | 15005 | 29287 |
| speedup | 1.18× | 1.39× | 1.49× | 1.66× | 1.81× | 1.82× | 1.84× | 1.82× |

**Verdict: ~1.8× constant-factor win, cubic unchanged.** The speedup plateaus:
200→400 is still 7.7× time for 2× depth (baseline was 7.8×) → still O(depth³).

The residual cubic is where the prototype deliberately didn't touch:
`solve()` → `apply_constraint_with_coercion` → `subst.apply(&constraint.lhs)`
against the **accumulated** `cached_subst` (not empty — ~`depth` chained
bindings), plus `compose_in_place`'s `values_mut()` loop applying each
constraint's non-empty delta over all existing bindings. Both are O(depth) work
× O(depth) bindings × O(depth) constraints. Only **path compression** (`find`
with link-rewriting, needs `&mut`/`RefCell` on the lookup path) or union links
(don't store nested types at all) collapse that. That is prototype v2.

## Prototype v2 (this branch) — reverse index in `Substitution`

`Substitution` gains `rev: HashMap<TypeVar, HashSet<TypeVar>>` — `rev[u]` = the
keys whose *value* mentions `u`, maintained by every mutation. `compose` /
`compose_in_place` then rewrite **only** the bindings a delta can actually
change (`affected_by`), instead of re-applying and deep-cloning every binding.

Instrumenting first (temporary `apply` call-site counters, since reverted)
**corrected the diagnosis**:

- Of ~57k `apply` calls on `deep_type` d200, `apply_constraint` (the `solve()`
  path) makes exactly **2 per constraint**, O(1) each. The bulk come from Pass 2
  **construction** — `typechecker/mod.rs` `infer_type_args_for_construction`
  (`:929`/`:937`, ~40k), `construction.rs:79/54/188`, `construction/calls.rs`.
- After v2, `compose_in_place` touches a **constant ~85 bindings** regardless of
  depth (was O(depth)).

Results — `945` integration + `139` frontend + `2` interpreter tests green.

Default fixtures (typecheck ms / solve ms, baseline → v1 → v2):

| fixture | typecheck | solve |
|---|---|---|
| `deep_type` (90) | 165 → 70 → **34** | 80 → 37 → **4.6** |
| `solve_storm` (250) | 108 → 56 → **43** | 41 → 19 → **5.6** |
| `id_chain` (400) | 50 → 29 → **26** | 13 → 7 → **1.1** |

`deep_type` typecheck-phase scaling (metel-bench, `typecheck_ns` only):

| depth | 100 | 200 | 400 | 200→400 |
|------:|----:|----:|----:|--------:|
| baseline | 81 | 657 | 7070 | 10.8× (≈O(n³·⁴)) |
| **v2** | 41 | 131 | **470** | **3.6× (≈O(n^1.8))** |

**Verdict: v2 breaks the typecheck cubic.** typecheck is ~15× faster at d400 and
now scales ~quadratically. `solve_ns` is near-linear (reverse index) with an
O(n²) residual from `apply_constraint`'s per-constraint `subst.apply(rhs)`.

The remaining `apply_constraint` O(n²) is the next in-solver target (§2 below).
Pass 2 construction was the other suspect — instrumentation flagged
`construct_generic_body` as the top `apply` caller — but memoising it was
prototyped and did **not** pay off on realistic code (§1 below). Separately,
`deep_type`'s **wall** time is now **evaluator-bound** (eval ≈ O(n³): the
interpreter deep-clones the O(depth) nested runtime value at each of `depth`
levels) — a distinct concern from type-checking.

## Remaining optimization opportunities (ranked)

Measure every change against `metel-bench --fixtures-dir
metel-interpreter/benches/stress` + the depth sweep, comparing `typecheck_ns`
/ `inference_ns` / `solve_ns` and wall-time-vs-depth against the tables above.

### 1. Pass 2 generic-body construction memoization — *prototyped, rejected*

`construct_generic_body` is called by the **evaluator** (`evaluator/call.rs`, the
`ClosureBody::Untyped` arms) on every invocation of a generic `fun`/method — it
re-runs the whole typechecker construction pass on the body each call. It is the
dominant `apply` *caller* in the instrumentation (~40k of 57k calls on
`deep_type` d200) — but that is call *count*, not wall-time: post-#913 those
`apply`s are on shallow types and cheap.

Prototyped: a thread-local cache of `Rc<TypedBlock>` keyed by
`(TypeCtx pointer, name, {arg types, expected return})`, cleared per program run
in `reset_runtime_state`. Correct and effective at its job — `solve_storm`
248/251 calls, `id_chain` 399/400, `generic_body_reuse` 799/800 served from
cache; all 945 + 139 + 2 tests green.

**But not worth landing.** Measured (median of 9 `metel` runs, `MEMO_OFF` toggle):

| fixture | off | on | speedup |
|---|---:|---:|---:|
| `generic_body_reuse` (synthetic worst case) | 474 ms | 378 ms | 1.25× |
| `int_04_generic_algorithms` | 986 | 1052 | **0.94×** |
| `int_05_generic_data_pipeline` | 624 | 634 | 0.98× |
| `int_03_generic_option_chain` | 684 | 693 | 0.99× |
| `int_01_statistics` | 647 | 618 | 1.05× |
| `solve_storm` | 959 | 928 | 1.03× |

On the realistic generic-heavy fixtures the per-call `format!` key + `HashMap`
lookup **costs as much as the construction it saves** (METEL-177 already made
construction cheap for these). Only a large body called many times at one
monomorphisation wins, and even then modestly. `optimization-shortlist.md`
candidate #2 gated itself on "candidate 1 confirms runtime construction is
material inside hot programs" — it isn't. `generic_body_reuse.mtl` is kept as
the witness fixture.

### 2. Path compression / lazy resolution in constraint solving — *medium*

`apply_constraint_with_coercion` does `subst.apply(&constraint.lhs/rhs)` per
constraint against the accumulated `cached_subst`; on a chained-var-deep type
that is O(depth) per constraint → O(n²) total (the residual after v2). Two ways
to collapse it:

- **Path compression**: a `find(var)` that chases `Var` links and rewrites each
  to point at the representative, so the second resolution of a chain is
  O(1) amortized. Needs `&mut`/`RefCell` on the lookup path (`apply` is `&self`
  in many call contexts).
- **Lazy unification**: stop pre-`apply`ing the constraint sides; give `unify`
  access to the substitution so it resolves vars shallowly on demand. Cleaner
  asymptotically but architectural — `unify` currently takes bare `&InferType`
  and returns a delta, and has 8 external callers in `construction*`.

### 3. Iterative `apply` / `occurs_in` / `unify` — *medium, safety*

Still structurally recursive over `InferType`. A genuinely deep type (or a long
var chain, pre-#2) can overflow the stack — the original report behind this
work. Convert the three to an explicit heap worklist. No asymptotic time change;
removes the crash vector. Contained to `typeinference/mod.rs`.

### 4. `Rc<InferType>` subterms — *large, broad payoff*

`InferType::{Fun,Array,…}` hold `Box<InferType>` / `Vec<InferType>`; every
`apply` / `compose` / `type_to_infer` deep-clones. `Rc`-sharing subterms turns
those into refcount bumps across the whole typechecker *and* evaluator. Do only
if 1–3 don't close the gap; it is a module-wide type change.

### 5. Evaluator value sharing — *separate area*

`deep_type`'s wall time is now eval-bound: the interpreter deep-clones the
O(depth) nested runtime `List` value at each of `depth` construction levels
(eval ≈ O(n³)). `Rc`/COW value subterms in the evaluator, or not cloning on
pass-by-value where a borrow suffices. Distinct from type-checking; own
investigation.

### 6. Parser on long expressions — *separate area, unmeasured*

`id_chain`'s parse phase (350 ms+) dominates its total — long `+` chains / large
flat block bodies are slow in the pest grammar path. Not profiled here; noted
because it surfaced while building the fixtures.
