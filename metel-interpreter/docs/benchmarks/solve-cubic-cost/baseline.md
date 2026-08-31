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
