---
id: ADR-0027
title: std::core as a virtual in-memory module seeded from StdPrelude
status: superseded by adr-0039
date: 2026-05-29
issues: ["#201", "#202"]
---

> **Superseded (sprint 22, METEL-181).** std::core is no longer a virtual
> module: it is a real module embedded from `stdlib/core.mtl` that flows
> through the normal loader → resolver → typechecker → evaluator pipeline.
> See [adr-0039](adr-0039-native-bindings-embedded-stdcore.md). The TypeVar
> offset insight below (the 10000 base) survives in `CorePrelude`.

## Context

Every Metel module needs `Perhaps`, `Result`, `Display`, `Iterable`, `From`, and all built-in functions available without an explicit import. Two implementation strategies were considered:

**Option A — Real `.mln` file:** `std/core.mln` declaring `Perhaps`, `Result`, etc. as user-level Metel code.

**Option B — Virtual in-memory module:** `std::core` has no physical file. Its public surface is seeded into the typechecker's `GlobalExports` at startup from `StdPrelude::default()`.

## Decision

Implement Option B (virtual module). Reasons:

1. **No bootstrapping problem:** `Perhaps` and `Result` are native `Value` variants in the evaluator (ADR-0016). A real file would require the typechecker to process them as user types and the evaluator to treat them as user-defined structs — requiring ADR-0016 to be reversed or a special two-path dispatch.
2. **StdPrelude already exists:** `StdPrelude` is the canonical source for all built-in function type schemes. Seeding `GlobalExports` from it costs one call; maintaining a real file would require keeping the file and `StdPrelude` in sync.
3. **Simpler name resolver integration:** `std::core` is injected into `names.pub_surface` directly after name resolution, making `import std::core::Perhaps` valid without any file loading.

`StdPrelude::default()` calls `registry::populate_std_schemes` — the single source of truth for all built-in function schemes. Both `register_builtins` (Pass 1) and `register_builtin_schemes` (Pass 2) draw from this same function, eliminating the previous divergence.

The `std::core` auto-import is inserted by `resolve_module` as `(GlobTier::Std, vec!["std","core"])` into every module's glob list (ADR-0026).

## Migration path

A future sprint can implement Option A (real stdlib files) without breaking API: replace the `GlobalExports` seeding call with a module load, keep `StdPrelude` as an internal registry seed, and remove the virtual `pub_surface` injection. User code does not change.

## Consequences

- `std::core` cannot be listed, enumerated, or introspected at runtime — it has no file, no `Ast`, no `TypedDecl` list
- Adding a new core type (e.g. `Error<T>`) requires: (a) registering it in `build_registry`, (b) adding it to `pub_surface` injection in `name_resolver.rs`, and (c) adding it to `StdPrelude` if it has associated functions
- This is a known limitation documented in the spec: "std::core is currently a virtual module — it has no physical .mln file and cannot be listed or enumerated"
