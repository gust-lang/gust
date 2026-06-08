# v0.8.2 Optimization Shortlist

This note ranks low-cost optimization candidates from the evaluator integration
benchmark baseline in `summary.md`.

The benchmark runner executes the existing evaluator integration fixtures through
the same single-file `parse -> typecheck -> evaluate` path used by the test
suite. The resulting timings therefore represent current interpreter user
experience more accurately than a runtime-only microbenchmark.

## Baseline Summary

The slowest fixtures are:

| Fixture | Total (ms) | Parse (ms) | Typecheck (ms) | Evaluate (ms) |
|---|---:|---:|---:|---:|
| `int_04_generic_algorithms.mtl` | 1662.887 | 132.087 | 1526.251 | 4.549 |
| `int_01_statistics.mtl` | 675.241 | 74.096 | 599.684 | 1.461 |
| `int_02_battle.mtl` | 442.262 | 96.210 | 344.430 | 1.621 |
| `int_03_generic_option_chain.mtl` | 431.502 | 65.887 | 362.841 | 2.774 |
| `int_05_generic_data_pipeline.mtl` | 357.644 | 57.010 | 298.318 | 2.316 |
| `int_11_generic_sized.mtl` | 157.804 | 21.880 | 135.220 | 0.703 |

Observed pattern:

- `typecheck` is the dominant phase on every slow fixture.
- `evaluate` stays below `5 ms` even on the worst generic-heavy programs.
- The runtime call graph still matters for later tuning, but it is not the
  primary bottleneck in the current end-to-end flow.

Conclusion:

- The highest-ROI `0.8.2` work is in generic-heavy front-end costs, especially
  typechecking and construction.
- Runtime micro-optimizations should be treated as secondary unless they are
  nearly free.

## Ranked Candidates

### 1. Add finer-grained typechecker profiling and attack generic-heavy passes first

Priority: `Highest`

Why:

- `int_04_generic_algorithms.mtl` spends about `91.8%` of total time in
  `typecheck`.
- `int_01_statistics.mtl` spends about `88.8%` in `typecheck`.
- `int_03_generic_option_chain.mtl` spends about `84.1%` in `typecheck`.
- `int_05_generic_data_pipeline.mtl` spends about `83.4%` in `typecheck`.

Low-cost action:

- Extend the current benchmark/profiling workflow with typechecker sub-phase
  timings, at minimum:
  - registry build
  - inference
  - constraint solve
  - construction
  - module-level graph glue

Expected payoff:

- Highest, because this is where nearly all current time is spent.
- Also de-risks later optimization work by showing whether inference,
  construction, or generic registry lookups dominate.

Risk:

- Low. This is measurement work first, not a semantic change.

### 2. Cache construction-at-call-time for repeated generic closures and methods

Priority: `High`, but only after candidate 1 confirms runtime construction is
material inside hot programs

Why:

- Generic-heavy fixtures show many repeated calls into the same generic helpers:
  - `int_04_generic_algorithms.mtl`: `fold`, `map_arr`, `filter`, `zip_with`,
    recursive `rsum`, and `160` closure calls
  - `int_05_generic_data_pipeline.mtl`: `filter_array`, `map_array`,
    `zip_with`, `any`, `all`, and `105` closure calls
  - `int_03_generic_option_chain.mtl`: repeated `option_*` combinators and
    `68` closure calls
- The evaluator currently reconstructs generic bodies at call time via
  `construct_generic_body(...)` in [src/evaluator/call.rs](/mnt/c/Users/Vladastos/Projects/metel-lang/metel-core/metel-interpreter/src/evaluator/call.rs:58) and [src/typechecker/construction.rs](/mnt/c/Users/Vladastos/Projects/metel-lang/metel-core/metel-interpreter/src/typechecker/construction.rs:200).

Low-cost action:

- Memoize typed generic bodies by a stable key such as:
  - callable identity
  - receiver identity for methods if needed
  - concrete argument type vector

Expected payoff:

- Medium for runtime.
- Possibly higher if the same machinery is reused often enough during generic
  helper-heavy programs.

Risk:

- Medium. The cache key must respect type context and receiver shape.
- This should stay scoped to exact repeated instantiations, not speculative
  sharing.

### 3. Reduce repeated typechecker work around generic instantiation and method lookup

Priority: `High`

Why:

- The slowest fixtures are all generic-heavy.
- Current construction and inference paths repeatedly instantiate schemes and
  perform method/type lookups for generic calls.
- The existing code clearly has hot generic paths:
  - `instantiate_scheme_for_call(...)`
  - generic method slow paths in construction/inference
  - repeated conversion between `Type` and `InferType`

Low-cost action:

- Target allocation and lookup churn around:
  - repeated scheme instantiation for the same call shapes inside a fixture
  - repeated method lookup on the same receiver type/method pair
  - repeated rebuilding of small generic substitution maps

Expected payoff:

- Potentially high, but this needs candidate 1's sub-phase timings before code
  changes start.

Risk:

- Medium. This touches sensitive typechecker code and needs tight regression
  coverage.

### 4. Trim evaluator call overhead for closures and intrinsics

Priority: `Medium`

Why:

- Runtime graphs show closure-heavy workloads:
  - `160` closure calls in `int_04_generic_algorithms.mtl`
  - `105` closure calls in `int_05_generic_data_pipeline.mtl`
  - `68` closure calls in `int_03_generic_option_chain.mtl`
- `call_runtime_callable(...)` clones captured environments and binds arguments
  on every invocation in [src/evaluator/call.rs](/mnt/c/Users/Vladastos/Projects/metel-lang/metel-core/metel-interpreter/src/evaluator/call.rs:18).

Low-cost action:

- Measure and then reduce obvious per-call overhead:
  - avoid unnecessary cloning in trivial intrinsic paths
  - reduce temporary allocations while binding arguments
  - avoid repeated string work on profiler-disabled runs if any remains

Expected payoff:

- Low to medium.
- Useful only after front-end hotspots are addressed or if this can be done very
  cheaply.

Risk:

- Low, if kept local to call setup and not closure semantics.

### 5. Defer string/display and builtin micro-optimizations

Priority: `Low`

Why:

- `int_06_display.mtl`, `int_08_std_core_paths.mtl`, `int_09_numeric_pipeline.mtl`,
  and `int_10_char_processing.mtl` are already fast.
- Their runtime graphs show builtin-heavy edges, but total fixture time remains
  low compared with the generic/typechecking cases.

Conclusion:

- Do not spend `0.8.2` scope here first.

## Recommendation For METEL-175

Recommended order:

1. Extend profiling to split the typechecker into sub-phases.
2. Re-run the same integration benchmark suite.
3. If construction-at-call-time or generic instantiation stands out, implement:
   - typed generic body memoization
   - small lookup/allocation reductions around generic calls
4. Only then spend time on evaluator call overhead.

Recommended scope adjustment:

- Reword the milestone from "evaluator performance" to "interpreter
  responsiveness on generic-heavy programs".
- If `0.8.2` remains runtime-only by definition, then the benchmark evidence
  says the likely payoff is limited.

## Explicit Non-Recommendations

- Do not start with aspect dispatch optimization. Aspect-heavy fixtures are not
  the dominant cost.
- Do not start with builtin string/display tuning. Those fixtures are already
  cheap.
- Do not do a broad architecture rewrite before typechecker sub-phase data
  exists.
