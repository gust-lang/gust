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
