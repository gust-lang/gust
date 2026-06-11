---
id: adr-0039
title: "Native Host Bindings and std::core as a Real Embedded Module"
date: '2026-06-11'
status: active
supersedes: adr-0027
---

## Context

Until sprint 22 the standard library existed twice: `StdPrelude` /
`populate_std_schemes` gave the typechecker hand-written schemes, and
`evaluator::builtins::register_builtins` gave the runtime hand-written
implementations, kept in sync by a parity test. ADR-0027 papered over this by
injecting `std::core` as a *virtual* module: the resolver was given a
hard-coded export list and the loader short-circuited every `std::` import.
Each new builtin had to be added in three or four places, and the std::core
surface visible to users was whatever the injection list said, not what any
source file declared.

Sprint 22 (METEL-181/182/183) replaces this with one mechanism: stdlib source
files that declare their host-backed items explicitly, compiled into the
binary, flowing through the normal module pipeline.

## Decision

### `native(@…)` declarations bind stdlib items to host code

A stdlib declaration can be marked native and bodiless:

```metel
native(@std.core.println) pub fun println<T>(x: T);

impl Display for i64 {
    native(@std.core.to_string) fun to_string(&self) -> String;
}
```

- The surface id (`@std.core.println`) is lowered to a closed `NativeKey`
  enum variant (`src/native_keys.rs`). The enum is the single source of truth
  for what the host provides: an unknown id is a hard error at construction,
  and `native_host_impl` is a **total `match`** over the enum, so coverage is
  compile-time (plus a `NativeKey::ALL` round-trip test). There is no
  string-keyed native registry.
- `native` is stdlib-only, enforced at the typechecker
  (`enforce_native_stdlib_only`), not the parser — the grammar stays uniform,
  the restriction is one check with a clear diagnostic.
- Native functions are exempt from the T0010 pub-annotation lint; their
  signatures are validated by `native_fun_ty` (parameters must be annotated;
  omitted return type means unit).
- Generic natives carry aspect bounds (`print<T: Display>`): `TypeScheme`
  has a positional `bounds` field that survives prelude derivation and export
  alpha-renaming, and construction enforces it at scheme-instantiating call
  sites — `println` of a type without a Display impl is a compile-time T0012,
  not a runtime panic. Structural types (arrays, tuples, closures) have no
  named impls and skip the static check; the runtime stays the backstop there.
- Keys are deliberately coarse where the host can be value-driven: all 13
  primitive `Display` impls share one `@std.core.to_string` key (the host
  formats by runtime value), and the numeric `From` cross-product uses one
  key per *target* type (the source type travels in the value).

### std::core is a real module, embedded in the binary

- `build.rs` scans `stdlib/**/*.mtl` into an `EMBEDDED_STDLIB` table of
  `(module_path, source)` pairs; `stdlib::lookup` serves it and
  `stdlib::core_program()` is the parsed-once `std::core` AST.
- The module loader synthesizes the embedded `std::` modules into the
  `ModuleGraph` ahead of user code; std::core then flows through resolver →
  typechecker → evaluator like any module. The resolver injection from
  ADR-0027 is deleted — std::core's public surface is computed from its
  declarations.
- `EmbeddedStdlibProvider` is the default `SourceProvider` (RFC-0058 /
  METEL-183): embedded lookup first, filesystem fallthrough.
  `validate_std_namespace` rejects user files under `std::`, so the stdlib
  cannot be shadowed from disk.

### One source of truth, two pipelines

`stdlib/core.mtl` + `NativeKey` drive everything; nothing else registers
builtins by hand. The two pipeline paths consume the same parsed program:

- **Graph path** (`load_root` → `check_graph` → evaluate): std::core is
  checked and evaluated as a module. Additionally, every module's
  `build_registry` runs `register_program_decls` over the embedded core decls
  so builtin types/aspects/impls are present in each module's registry
  (skipped when the module being checked *is* std::core).
- **Single-program path** (`check` / `evaluate_with_ctx`, no module loading):
  `CorePrelude` (the renamed remnant of `StdPrelude`) derives its schemes by
  walking the embedded core decls (`populate_schemes_from_embedded_core`),
  and the runtime seeds host bindings the same way
  (`register_core_natives_from_embedded`). These are *derivations*, not
  duplicate lists — adding a declaration to core.mtl plus its `NativeKey`
  arm is the entire recipe for a new builtin.

Intentionally still hand-registered (not expressible as named-type decls):
Range/RangeInclusive `Iterable` impls (runtime ranges are intrinsic) and the
String / array `len` pattern methods (keyed by runtime value shape).

### Supporting fixes this forced

- **Exported-scheme TypeVar collision.** Once std::core exported real schemes,
  their low TypeVar ids could collide with an importing module's generator and
  produce a cyclic substitution (`Substitution::apply` recursed forever).
  `check_graph` now alpha-renames every exported scheme into a dedicated
  2_000_000+ range (`refresh_scheme_for_export`), and `Substitution::bind`
  drops identity bindings. Ranges in use: module/registry gens ~0+,
  `CorePrelude` 10_000+, `construct_generic_body` 1_000_000+, exports
  2_000_000+, overload SymbolIds 0x4000_0000+ (ADR-0038).
- **Primitive impl targets.** `impl Display for i64` requires the method's
  self type to be `Concrete(I64)`, not `Named("i64")`; `primitive_type_from_name`
  is applied at every self-type construction site.
- **Generic-struct native methods.** Metel-bodied methods on generic structs
  are inferred per defining module, but native methods have no body to infer;
  their annotated signatures are registered as polymorphic schemes over the
  struct's type params (`register_generic_native_impl_methods`). Static native
  methods (`List::new`) become joined-key prelude schemes.

## Consequences

- `StdPrelude` (as a hand-maintained list), `register_core!`,
  `free_function_names()`, the resolver injection, and the GlobalExports
  std::core seed are gone. ADR-0027 is superseded; its TypeVar-offset insight
  (the 10000 base) survives in `CorePrelude`.
- The stdlib surface users see is exactly what `stdlib/core.mtl` declares —
  the embedding is invisible (`import std::core::println` behaves like any
  module import).
- Every program now parses/checks/evaluates std::core's ~530 lines. The parse
  is `OnceLock`-cached per process; per-module registry seeding is the
  remaining cost, acceptable for a tree-walk interpreter and a natural target
  if registry sharing ever matters.
- The single-program path's runtime seeding registers aspect methods with
  `aspect_id: None`, which is why the evaluator keeps a string fallback after
  id-based aspect lookup. Deleting that fallback (seed the well-known
  `SYM_ASPECT_*` ids instead) is folded into METEL-185.
- Multi-file stdlib growth (`std::io`, `std::collections`, …) is now just
  adding `.mtl` files under `stdlib/` — the loader walks the embedded
  manifest like a directory tree.
