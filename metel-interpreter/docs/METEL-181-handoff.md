# METEL-181 — std::core unification: status & continuation

**Branch:** `sprint/22-stdcore` (isolated worktree; `sprint/22` is untouched and green).
**Suite at HEAD:** 400 integration + 46 lib tests, green. CLI verified.

## Done (committed on this branch, all green)

### Infrastructure & prerequisites
- `stdlib/core.mtl` embedded via `build.rs` → `EMBEDDED_STDLIB` table;
  `src/stdlib.rs` (`lookup` / `module_paths`); `EmbeddedStdlibProvider`.
- `NativeKey` covers the full free-function surface (8 keys) + host impls;
  coverage test over `NativeKey::ALL`.
- **Native impl methods**: `construct_impl_method` lowers `native(@…)` methods
  to `FunBody::Native`; inference registers signatures without bodies; the
  evaluator binds them to host impls through the normal aspect/inherent paths.
- **Generic native functions**: `native_fun_ty` handles type parameters
  (`print<T>`, future `List::new<T>`).
- **Impl blocks on primitive types** (`impl Aspect for i64`) typecheck and run:
  `primitive_type_from_name` makes the four self-type sites build
  `Concrete(prim)` for primitive targets. Fixture 75.

### std::core as a real module
- The loader synthesizes the embedded `std::` modules into the `ModuleGraph`
  ahead of user code (`load_embedded_stdlib`); std::core flows through
  resolver → typechecker → evaluator like any module.
- The name-resolver injection now only EXTENDS std::core's computed pub_surface
  with the still-registry-based type names (Perhaps/Result/Display/Iterable/
  From/List); the free-function surface comes from the real module.
- Native functions are exempt from the T0010 pub-return-type lint (their
  signatures are validated by `native_fun_ty`; omitted return = unit).
- Test harness module-count assertions count user modules only.

### TypeVar collision fix (was: CLI stack overflow)
Exported schemes carry ids from their module's generator (~0+), colliding with
the importing module's generator → cyclic substitution → `Substitution::apply`
recursed forever (`println("hi")` CLI crash). Fixed two ways:
- `check_graph` alpha-renames every exported scheme into a dedicated high range
  (2_000_000+, persistent `export_gen`) before storing in `GlobalExports`.
- `Substitution::bind` drops identity bindings (`?v→?v`); `compose_in_place`
  retains them out. Regression fixture:
  `module_semantics/generic_native_scheme_no_typevar_collision`.
Ranges in use: registry/module gens ~0+, StdPrelude 10_000+,
`construct_generic_body` 1_000_000+, exported schemes 2_000_000+.

### Single source of truth for free functions
The hand-maintained registries are deleted; `core.mtl` + `NativeKey` drive
everything:
- typecheck (single-program path): `StdPrelude::default()` derives schemes by
  parsing embedded core.mtl (`populate_schemes_from_embedded_core`);
- runtime: `register_core_natives_from_embedded` binds each native decl to its
  host impl (replaced the eight `register_core!` bodies + macro);
- deleted: `free_function_names()`, the StdPrelude/evaluator parity test
  (replaced by `prelude_schemes_cover_embedded_core_natives`).
Only `List::new`/`List::from` remain hand-written (List is registry-based).

## Remaining (future work, in rough order)

1. **Move the types/aspects into `core.mtl`**: `Perhaps`, `Result` (plain
   enums), `aspect Display/From<S>/Iterable<T>`, `struct List<T>` + native
   methods (`push/pop/get/len/…`, needs new NativeKeys + a generic-struct
   native-method story), the primitive `Display`/`to_string` impls and the
   numeric `From` cross-product (primitive impls now work — fixture 75; keep
   them in std::core per RFC-0060's orphan rule). As each moves, delete its
   `build_registry` / `register_builtins` counterpart and shrink the resolver's
   pub_surface extension; when the list is empty, delete the extension, the
   `GlobalExports` StdPrelude seed, and `StdPrelude` itself (the single-program
   path would then synthesize std::core as a module or keep a fully derived
   prelude under a different name).
2. **Switch the default provider** to `EmbeddedStdlibProvider` and remove the
   `PathRoot::Std => Ok(None)` bypass, so `import std::foo` resolves embedded
   modules uniformly. (Today std::core is force-synthesized, which covers the
   current surface; the bypass only matters for future multi-file std.)
   `validate_std_namespace` must then gate on provenance (embedded vs user
   file), not just the path prefix.
3. **Callables-only SymbolId dispatch** (the sprint-22 bundled decision):
   builtins + each overload get a `SymbolId`, typed calls carry `CalleeId`,
   the evaluator call path rekeys, and the overload name-mangling
   (`overload::mangle`/`type_mangle` + construction rewriting) is deleted with
   selection repointed to yield `SymbolId`. Struct/type/enum registry rekeying
   stays OUT (METEL-185).
