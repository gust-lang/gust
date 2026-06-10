# METEL-181 — std::core unification: continuation handoff

**Branch:** `sprint/22-stdcore` (isolated worktree; `sprint/22` is untouched and green).

## What is done (committed on this branch, green)

Embedded-stdlib **infrastructure**, all additive — the virtual `std::core` still
exists and is still the active mechanism:

- `stdlib/core.mtl` — the free-function surface as `native(@…)` declarations.
- `build.rs` → generates `EMBEDDED_STDLIB: &[(&[&str], &str)]` from `stdlib/**/*.mtl`,
  keyed by logical module path (`stdlib/core.mtl` → `["std","core"]`).
- `src/stdlib.rs` — `lookup(module_path)` / `module_paths()` over the table.
- `module_loader::EmbeddedStdlibProvider` — embed first, filesystem fallthrough.
  **Not yet wired** as the default provider.
- `NativeKey` now covers the whole free-function surface, including
  `string_len` / `string_concat` (+ host impls), so `stdlib/core.mtl` lowers
  fully. The coverage test (`NativeKey::ALL`) guards this.

## The remaining cutover (the atomic big-bang)

The virtual `std::core` exists at six layers; all must flip together (see the
sprint-22 implementation guide §"As-built status" / the chat analysis). Suggested
order, expecting a red tree until the end:

1. **Loader: synthesize the embedded `std::` modules into the `ModuleGraph`.**
   In `load_root_with`, after loading user modules, prepend a `LoadedModule` for
   each `stdlib::module_paths()` entry: parse its embedded source (via the
   provider) into a `Program` and push with the right `module_path`. Switch the
   default provider used by `pipeline::run_file` / CLI to `EmbeddedStdlibProvider`.
   Remove the `PathRoot::Std => Ok(None)` bypass in `resolve_import_module`.

2. **Name resolver: delete the virtual `std::core` injection** (`name_resolver.rs`
   ~line 182, the hardcoded `pub_surface` for `["std","core"]`). `std::core` is now
   a real loaded module, so its `pub_surface` is computed from its decls. Keep the
   `GlobTier::Std` auto-glob — it now resolves against the real module. NOTE: the
   `validate_std_namespace` guard (METEL-183) currently rejects any module path
   starting with `std`; it must allow the *embedded* std modules while still
   rejecting *user* `std` files (gate on provenance, not just the path).

3. **Typechecker: delete `StdPrelude`** and the `GlobalExports` std::core seed
   (`typechecker/mod.rs` ~235). `std::core` flows through `check_graph` like any
   module. `register_primitive_type_bindings` must stop seeding StdPrelude
   schemes. The free-function schemes now come from the std::core module's native
   decls (already supported by the METEL-182 native path).

4. **Evaluator: delete `register_builtins`** free-function registrations and the
   `free_function_names()` parity test. The std::core module's native decls
   register host impls via `native_host_impl` (already wired for top-level fns).

5. **Move the TYPES/ASPECTS into `stdlib/core.mtl`** (the hard part). Currently in
   `build_registry` / `register_builtins`:
   - `enum Perhaps<T>`, `enum Result<T,E>` — plain Metel enums.
   - `struct List<T>` + methods — needs **native methods** and a **generic native
     constructor** (`List::new<T>`, `List::from`).
   - `aspect Display / From<S> / Iterable<T>` — Metel aspect decls.
   - Display impls for primitives + `to_string`, the numeric `From` cross-product
     — these are impls on *builtin primitive* types, which the RFC-0060 orphan
     rule will say only `std::core` may write. They need native methods.

## Prerequisites — status

- **Native methods.** ✅ DONE. `construct_impl_method` lowers `native(@…)`
  methods to `FunBody::Native`; `infer_impl_method` skips body inference and
  registers the signature; the evaluator registers native impl methods to their
  host impl. (Additive, green.)
- **`NativeKey` variants + host impls** for the free-function surface. ✅ DONE
  (incl. `string_len`/`string_concat`). Still TODO: keys for each List method /
  `to_string` / numeric `from` when those impls move into `core.mtl`.
- **Generic native functions** (`List::new<T>`). ✅ DONE — `native_fun_ty` now
  builds a generic type-var map.
- **Impl blocks on primitive types.** ✅ DONE. `primitive_type_from_name` is the
  inverse of `primitive_type_name`; the four self-type sites
  (`infer_impl_method` / `infer_default_aspect_method` / `construct_impl_method`
  / `construct_default_aspect_method`) now build `self` as `Concrete(prim)` for
  primitive targets. `impl Aspect for i64` typechecks and runs (fixture
  `evaluator/functions/75_impl_aspect_for_primitive.mtl`).

**All prerequisites are now complete.** What remains is the pure cutover.

## Recommended cutover sequencing (given the blocker)

The primitive-impl blocker splits the migration cleanly:

- **Movable to `core.mtl` now**: the free functions (native), `Perhaps`,
  `Result` (plain enums), and `List<T>` (struct + native generic methods, since
  generic structs use the `method_scheme` path, not primitive impls).
- **Blocked until the primitive-impl fix**: `Display`/`From`/`Iterable` impls on
  primitive types, `to_string`, the numeric `From` cross-product. Keep these in
  `build_registry`/`register_builtins` until the fix lands, OR fix primitive
  impls first (preferred — it's a contained typechecker change and unblocks the
  whole types/aspects migration).

So a realistic order: (1) fix primitive impls; (2) write the full `core.mtl`;
(3) do the multi-layer wiring/deletion described above.

## Bundled decision (from sprint 22): callable SymbolId dispatch

Per the agreed scope, fold the **callables-only** SymbolId dispatch rekeying into
this work: give builtins + each overload a `SymbolId`, carry `CalleeId` on typed
`Call`/`MethodCall`, rekey the evaluator call-dispatch path, and **delete the
overload name-mangling** (`overload::mangle`/`type_mangle` + the construction
name-rewriting), repointing `overload::select`/`build_overload_table` to yield a
`SymbolId`. Struct/type/enum *registry* rekeying is explicitly OUT (that is
METEL-185).

## Known coherence gap to respect

RFC-0060 (METEL-186) defines the orphan rule. The numeric `From`/`Display` impls
moving into `std::core` are exactly the "only std::core may impl a builtin aspect
on a builtin type" case — keep them in `std::core` so they remain coherent.

## Verification target

Full `cargo test` green again with `StdPrelude`, `register_builtins`, and
`free_function_names` deleted, and `grep -rn "StdPrelude\|register_builtins\|
free_function_names" src/` returning nothing.
