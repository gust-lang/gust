# `ctx.solve()` stress fixtures

Generated Metel programs that isolate the cost of type inference / constraint
solving (`InferContext::solve`, `Substitution::apply`, `unify`). Each is a valid,
evaluable program whose `main` returns `i64`, so `metel-bench` can run it and
report `typecheck_detail` (`solve_ns`, `inference_ns`, `solve_calls`,
`constraints_processed`).

These live **outside** `tests/integration/sources/`, so `cargo test` does not
pick them up.

| fixture | stresses |
|---|---|
| `deep_type.mtl` | one generic type nested `DEEP_N` levels (`List<List<…<i64>>>`) — `apply`/`unify` recursion + eager re-apply over deep structure |
| `solve_storm.mtl` | `STORM_N` sequential generic `List` method calls — per-expression eager `solve()` + full substitution clone |
| `id_chain.mtl` | `CHAIN_N` generic `id()` calls — instantiation-var / constraint accumulation |

## Regenerate (tune sizes)

```
python3 metel-interpreter/benches/stress/generate.py metel-interpreter/benches/stress
```

Edit `STORM_N` / `DEEP_N` / `CHAIN_N` at the top of `generate.py` first. Keep them
below the point where the program fails to evaluate (integer overflow) or the
run wall-clocks out.

`generate_deep.py <path> <depth>` emits a single `deep_type` at an arbitrary
depth, for the depth-scaling sweep in `../../docs/benchmarks/solve-cubic-cost/baseline.md`.

## Run the baseline

```
cargo build --release -p metel --bin metel-bench
./target/release/metel-bench \
  --fixtures-dir metel-interpreter/benches/stress \
  --output-dir /tmp/solve-baseline --warmups 2 --iterations 15
cat /tmp/solve-baseline/summary.md
```
