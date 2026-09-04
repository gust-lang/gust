# Interpreter Testing

The interpreter test suite is split into two Cargo integration-test crates:

- `tests/integration.rs` for source-fixture-driven end-to-end tests
- `tests/unit.rs` for explicit Rust tests that assert internal behavior directly

The integration harness exists to keep source-based tests uniform across parser, typechecker, evaluator, and module-system coverage.

## Integration Harness

`tests/integration.rs` includes generated `#[test]` registrations from `build.rs`. The build script walks these fixture roots:

- `tests/integration/sources/parsing`
- `tests/integration/sources/typechecking`
- `tests/integration/sources/evaluator`
- `tests/integration/sources/module_loading`
- `tests/integration/sources/module_semantics`

Discovery rules:

- A `.mtl` file is a single-file fixture.
- A directory containing `main.mtl` is a multi-module fixture.
- Test names are derived from the fixture path and generated at build time.

The shared harness lives under `tests/integration/harness/`.

## Fixture Forms

Single-file fixtures:

```text
tests/integration/sources/typechecking/functions/example.mtl
tests/integration/sources/typechecking/functions/example.toml
```

Multi-module fixtures:

```text
tests/integration/sources/module_semantics/diamond_dependency/
  main.mtl
  left.mtl
  right.mtl
  base.mtl
  test.toml
```

The sidecar is optional. If it is absent, suite defaults and legacy inline annotations are used.

## Harness Configuration

The harness resolves each fixture to:

- a runner
- a std-prelude mode
- an expected result
- optional program-structure checks
- optional module-graph checks

Supported runners:

- `parse`
- `typecheck`
- `evaluate`
- `load_program`
- `load_graph`
- `full_pipeline`

Supported prelude modes:

- `empty`
- `default`

`empty` means `typechecker::CorePrelude::empty()`. `default` means `typechecker::CorePrelude::default()`.

## Sidecar Format

Single-file fixtures use `<name>.toml`. Directory fixtures use `test.toml`.

Example:

```toml
runner = "full_pipeline"
prelude = "empty"

[expect]
status = "success"

[graph]
module_count = 4
has_module_paths = ["main", "main::left", "main::right", "main::base"]
```

Recognized top-level keys:

- `runner`
- `prelude`

Recognized `[expect]` keys:

- `status`
- `code`
- `contains`
- `line`
- `col`

Recognized `[program]` keys:

- `imports`
- `decls`

Recognized `[graph]` keys:

- `module_count`
- `has_module_paths`

Supported expectation statuses:

- `success`
- `parse_error`
- `typecheck_error`
- `runtime_error`
- `load_error`

`has_module_paths` uses `::`-separated module paths in string form.

## `[options]` Keys

Recognized `[options]` keys, beyond `runner`/`prelude` above:

- `move_check` (bool) -- run under `--move-check`.
- `rfc` (list of strings) -- RFC-section citations this fixture demonstrates
  (ADR-0049). Legacy: superseded by `spec =` for any RFC that has reached
  `3-integrated` (PROCESS.md).
- `spec` (list of strings) -- spec-block citations, e.g.
  `spec.declarations.structs.instantiation-and-field-access.legality-1`
  (ADR-0050).
- `error` (list of strings) -- error-code citations, e.g. `["T0003"]`: this
  fixture's own `expect.code` demonstrates that documented code
  (metel-core#981). Deliberately a separate axis from `spec =`, not a widened
  form of it.
- `skip` (string) -- when set, `run_fixture` reports the fixture skipped
  instead of running it, with the value as the reason (e.g. a tracking
  issue). For a fixture checked in ahead of the feature it exercises.
- `spec_title` (string) -- human-readable label shown in place of the `.mtl`
  filename by the rendered spec's inline fixture viewer (metel-core#944/#974).
  Purely presentational.

`rfc`, `spec`, `skip`, and `spec_title` are parsed and validated by the
harness but not otherwise acted on here -- they exist for `rfc.py`
(`docs/rfcs/tools/rfc.py`, in the `docs` submodule/metel-docs) to read when
rendering the spec's Formal-rules blocks and `error-codes.md`'s fixture
viewers.

**This sidecar format has two independent parsers, in two repos, that do not
check each other.** `harness/fixture.rs` (here) parses it to run the fixture;
`rfc.py` (metel-docs) parses the same files to render documentation. Neither
generates the other, and the Rust harness rejects any `[options]` key it
doesn't recognize (`panic!("unknown options sidecar key ...")`) rather than
ignoring it. Concretely: adding or renaming a sidecar key needs a matching
change on both sides in the same PR set, or `cargo test -p metel --test
integration` fails on every fixture that used the new key even though
`rfc.py check` passed cleanly (metel-core#981 hit exactly this: `error =`
landed in `rfc.py` first, and every fixture citing it panicked in the harness
until `FixtureOptions`/`PartialConfig`/`merge_config` and a new sidecar-key
match arm were added here). When adding a key, grep both `fixture.rs`'s
`"options" => match key` block and `rfc.py`'s sidecar-scanning functions
before considering the change done.

## Legacy Annotations

The harness still supports the legacy fixture conventions so older suites do not need an immediate rewrite.

- `parsing`: files with a `neg_` prefix are treated as parse-failure fixtures
- `typechecking`: `// ERROR[CODE]` marks the expected type error and source line
- `evaluator`: `// PARSE_ERROR[...]`, `// TYPECHECK_ERROR[...]`, and `// RUNTIME_ERROR[...]` mark the expected failing stage

Resolution order is:

1. sidecar TOML
2. legacy inline annotation
3. suite default success

New fixtures should prefer sidecars when they need non-default behavior or assertions that cannot be expressed cleanly inline.

## Unit Tests

`tests/unit.rs` keeps tests that do not fit the fixture harness well, especially:

- type inference tests
- parser AST and error-format checks
- typechecker tests that assert exact internal details

These tests are still integration-test crates from Cargo's perspective, but they remain explicit Rust code instead of discovered source fixtures.

## When To Add Which Test

Use the integration harness when the test is primarily about language behavior expressed as source files:

- parser acceptance and rejection
- typechecking behavior
- evaluator behavior
- module loading and multi-module semantics

Use `tests/unit.rs` when the test needs to inspect interpreter internals directly or would become awkward as a source fixture.
