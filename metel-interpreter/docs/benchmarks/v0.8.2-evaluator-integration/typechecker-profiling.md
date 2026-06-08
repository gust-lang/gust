# Typechecker Profiling Notes

This note summarizes the first `METEL-177` pass over the evaluator integration
benchmark suite after adding typechecker sub-phase timings.

## Main Finding

The original `0.8.2` baseline showed that generic-heavy programs were spending
most of their time in `typecheck`, not `evaluate`.

Sub-phase profiling then showed that the dominant cost inside typechecking was
repeated constraint solving triggered by eager partial solves during inference.

The first optimization implemented was:

- make `InferContext::solve()` incremental for append-only constraints instead
  of cloning and re-solving the full constraint list on every call

## Before / After

Measured with the release benchmark harness on the evaluator integration suite.

| Fixture | Before typecheck (ms) | After typecheck (ms) | Before total (ms) | After total (ms) |
|---|---:|---:|---:|---:|
| `int_04_generic_algorithms.mtl` | 1526.251 | 23.165 | 1662.887 | 160.724 |
| `int_01_statistics.mtl` | 599.684 | 11.796 | 675.241 | 87.376 |
| `int_03_generic_option_chain.mtl` | 362.841 | 9.107 | 431.502 | 76.467 |
| `int_05_generic_data_pipeline.mtl` | 298.318 | 7.538 | 357.644 | 66.298 |
| `int_11_generic_sized.mtl` | 135.220 | 4.580 | 157.804 | 27.107 |

## Current Hotspot Shape

Representative sub-phase results after the optimization:

### `int_04_generic_algorithms.mtl`

- registry: `0.065 ms`
- inference: `4.610 ms`
- solve: `16.844 ms`
- construction: `0.527 ms`
- counters: `solve_calls=295`, `constraints_processed=658`

### `int_01_statistics.mtl`

- registry: `0.052 ms`
- inference: `2.357 ms`
- solve: `8.481 ms`
- construction: `0.350 ms`
- counters: `solve_calls=197`, `constraints_processed=506`

### `int_03_generic_option_chain.mtl`

- registry: `0.055 ms`
- inference: `1.962 ms`
- solve: `6.254 ms`
- construction: `0.367 ms`
- counters: `solve_calls=194`, `constraints_processed=380`

### `int_05_generic_data_pipeline.mtl`

- registry: `0.052 ms`
- inference: `1.413 ms`
- solve: `5.304 ms`
- construction: `0.296 ms`
- counters: `solve_calls=183`, `constraints_processed=390`

## Interpretation

- `solve` is still the largest typechecker sub-phase on the slowest fixtures,
  but it is now in the low-millisecond range rather than the hundreds-to-thousands.
- `parse` is now the dominant end-to-end cost on several fixtures.
- Runtime evaluation remains a secondary cost on this suite.

## Recommended Next Step

If more `0.8.2` optimization work is desired, the next candidate should be a
small targeted reduction in parse cost or a second pass on inference/solve
allocation churn only where the new sub-phase data still shows a meaningful
payoff.
