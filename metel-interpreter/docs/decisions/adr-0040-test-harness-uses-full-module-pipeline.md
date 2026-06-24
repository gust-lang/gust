---
id: adr-0040
title: "Integration Test Harness Runs the Full Module Pipeline"
date: '2026-06-14'
status: active
relates: adr-0039
---

## Context

ADR-0039 left two pipelines that consume the same parsed std::core:

- **Graph path** (`load_root` → `resolve` → `normalize` → `check_graph` →
  `elaborate` → `evaluate_graph`) — what the shipped binary (`pipeline::run_file`)
  actually runs. std::core is a real embedded module.
- **Single-program path** (`check_with_ctx` / `evaluate_with_ctx`) — skips module
  loading, name resolution, normalization, and **elaboration**, and hand-seeds
  std::core three ways: `populate_schemes_from_embedded_core` (typecheck prelude),
  `register_core_natives_from_embedded` (runtime natives), and
  `seed_core_bodied_methods` (runtime Metel-bodied core *methods*).

The integration harness's `evaluator` and `typechecking` fixtures ran on the
single-program path; only `module_loading` / `module_semantics` used the graph
path. So the bulk of the test suite exercised a path the product never uses.

This bit METEL-192. Fixing `print`/`println` to dispatch a user `Display` impl is
cleanest in-language: make them thin Metel wrappers (`pub fun println<T: Display>(x)
{ println_str(x.to_string()); }`) over a String-only host helper. That works in the
product (graph) path, but the single-program seeding registers core *free-function
bodies* nowhere — `seed_core_bodied_methods` handles impl methods only — so every
`println` fixture failed with `undefined name println` / unseeded body. Closing that
gap would mean adding a *fourth* hand-seeding site, deepening the very divergence
METEL-185 already flags.

## Decision

The integration harness's `run_typecheck` and `run_evaluate` now drive the full
module pipeline — the same phases as `pipeline::run_file` — instead of the
single-program path:

```rust
// run_evaluate
let graph = module_loader::load_root(main_source_path(path))?;
let names = name_resolver::resolve(&graph)?;
let normalized = path_normalizer::normalize(graph, &names)?;
let typed = typechecker::check_graph(normalized, &names, CorePrelude::default())?;
let elaborated = elaborator::elaborate(typed, &names)?;
evaluator::evaluate_graph(elaborated)
```

A single fixture file loads as the root module alongside the embedded std::
modules; with no imports it behaves like any leaf program. Tests now assert
against the path users actually run, and the in-language print/println wrappers
need no extra seeding.

The single-program path and its seeding are **not** deleted: `check_with_ctx` /
`evaluate_with_ctx` remain a public API and are still used by the benchmark binary
(`metel-bench` via `run_evaluator_fixture`), which deliberately measures the
typecheck/evaluate phases in isolation. This ADR removes the *test suite's*
dependence on the divergent path, not the path itself.

## Consequences

- The evaluator/typechecking fixtures now exercise elaboration and real std::core
  module loading. METEL-192's fix is a plain stdlib change with no harness-specific
  seeding.
- The regression suite now has product-path coverage for user-defined `Display`
  values flowing through `print`/`println`, instead of only checking the divergent
  single-program shortcut.
- Surfaced one latent fixture divergence: `int_07_pub_declarations.mtl` used
  `pub fun main()` with no return type, which the lax single-program path accepted
  but the product path rejects with T0010 (pub declarations require an explicit
  return type). The fixture was corrected to `pub fun main() -> ()`, matching what
  the shipped binary enforces — the test now reflects real behavior.
- Typecheck fixtures pay module-load + normalization cost they previously skipped;
  acceptable, and the std::core parse is `OnceLock`-cached per process.
- Fully retiring the single-program path (so the seeding in ADR-0039 can be
  deleted outright) now only requires migrating `metel-bench`. That is the natural
  endpoint of METEL-185's "no divergent std::core seeding" goal and is left as
  follow-up.
