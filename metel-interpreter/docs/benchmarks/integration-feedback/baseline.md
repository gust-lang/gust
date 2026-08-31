# Integration feedback cost — metel-core#873

## Hardware / profile

Measured on the CI-class two-core Linux dev host, `--release`, warm build
(`cargo build --release -p metel --tests` already run). `cargo` 1.96.1.

## Benchmark procedure (reproducible)

```sh
# One exact fixture, warm build:
cargo test --release -p metel --test integration -- --exact evaluator__control_flow__14_match

# Full integration suite (warm):
BIN=$(ls -t target/release/deps/integration-* | grep -v '\.d$' | head -1)
time "$BIN"

# Per-phase split for one fixture (parse / typecheck / evaluate):
cp metel-interpreter/tests/integration/sources/evaluator/control_flow/14_match.mtl /tmp/f873/
./target/release/metel-bench --fixtures-dir /tmp/f873 --output-dir /tmp/f873out \
  --warmups 3 --iterations 30
grep -E '^\| (parse|typecheck|evaluate|total) \|' /tmp/f873out/summary.md
```

## Baseline

| measurement | value |
|---|---|
| full integration suite (945 tests, warm) | **85.9 s** wall / 2 m 30 s user |
| one exact fixture (`14_match`, warm) | 0.34 s |
| `14_match` per phase: parse | **134 ms** |
| `14_match` per phase: typecheck | 12 ms |
| `14_match` per phase: evaluate | **125 ms** |
| `14_match` per phase: total | 271 ms |

Every fixture re-parses (`134 ms`) and re-evaluates (`125 ms`) the entire
embedded `std::core` (854 lines) from scratch — that repeated stdlib work,
×945, is the bulk of the warm suite.

## Change 1 — process cache for embedded-stdlib parsing

`module_loader::load_embedded_stdlib` now serves the parsed `Program` for each
`std::` module from a process-global memo keyed by `(module path, source hash)`
(`STDLIB_PARSE_CACHE`). Safe to share between test cases: a `Program` AST is
pure owned data — spans are byte offsets, there are no `SymbolId`s,
`TypeVar`s, diagnostics, or `Rc`/`RefCell` — so a `clone` per load is
identical to a fresh parse and cannot leak. An LSP overlay that shadows a
stdlib module with different text hashes differently and is a clean miss.
Parallel test threads share the memo under a `Mutex` (poison-tolerant); each
still gets its own `Program`.

## Change 2 — build.rs writes `integration_generated.rs` only on change

`metel-interpreter/build.rs` compares the freshly-generated test list against
the on-disk `integration_generated.rs` and skips the write when byte-identical.
This stops an unrelated fixture edit from touching the file `integration.rs`
`include!`s.

> **Known limitation, not fixed here.** `build.rs` still emits
> `cargo:rerun-if-changed` for the five fixture *directories*, and cargo
> recurses those, so editing any fixture's contents still re-runs the build
> script and — because the `RunCustomBuild` fingerprint tracks those file
> mtimes — recompiles `lib metel` and the test crate (~30 s here). Removing
> the directory watch is the fix, but it trades away zero-touch discovery of
> newly-added fixtures. Options for a follow-up: (a) move the integration
> harness + `build.rs` into its own workspace member so a fixture edit never
> invalidates `lib metel`; (b) watch a checked-in `fixtures.manifest`
> (structure only) instead of the dirs, with a `cargo xtask` / CI check that
> the manifest matches disk. Both are larger than this PR.

## Results

| measurement | before | after | delta |
|---|---:|---:|---:|
| full integration suite (945, warm) | 85.9 s | **35.1 s** | **−59 %** |
| suite user time | 2 m 30 s | 1 m 00 s | −60 % |
| `14_match` parse phase | 134 ms | **9 ms** | −93 % |
| `14_match` total | 271 ms | **150 ms** | −45 % |

945 tests pass in both `--release` and debug.

## Remaining dominant cost

`evaluate` — every fixture still re-runs `std::core`'s decls (`extend` blocks,
`native` bindings, Metel-bodied `fun`s) into a fresh runtime environment
(`~125 ms`). Caching that safely is harder: the evaluated environment holds
`Rc<RefCell<Value>>` and closures capturing it, and the issue's non-goals bar
sharing mutable evaluator state between tests. A snapshot-and-clone of the
post-stdlib environment, or a cached typed+elaborated stdlib graph re-run
cheaply, would be the next step — out of scope here.

The `cargo test` compile cost on a fixture edit (Change 2's known limitation
above) is the other outstanding item.
