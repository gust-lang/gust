# Interpreter Architecture

> Rationale for the tree-walk approach: [decisions/adr-0004-interpreter-architecture.md](decisions/adr-0004-interpreter-architecture.md)

## Pipeline

```
.mln root source file
       │
       ▼
  ┌───────────────┐
  │ Module Loader │  root file → ModuleGraph (topological order); invokes parser per file
  └───────────────┘
       │  module_loader::ModuleGraph
       ▼
  ┌───────────────┐
  │ Name Resolver │  per-module import scopes, pub_surface, re-exports; assigns SymbolIds;
  │               │  internally calls reference_resolver::collect_references to build a
  │               │  ReferenceTable, carried in ResolvedNames for later stages to consume
  └───────────────┘
       │  name_resolver::ResolvedNames  (carries symbols: HashMap<(module, name) → SymbolId>)
       ▼
  ┌─────────────────┐
  │ Path Normalizer │  rewrites qualified Expr::Path nodes to Expr::ResolvedPath
  └─────────────────┘
       │  path_normalizer::NormalizedModuleGraph
       ▼
  ┌────────────┐
  │ Coherence  │  aspect-impl orphan rule (T0014) and overlap detection (T0015); validation
  │            │  only — resolves type/aspect names to their declaring module, nothing more
  └────────────┘
       │  path_normalizer::NormalizedModuleGraph (unchanged; validation gate only)
       ▼
  ┌──────────────┐
  │ Type Checker │  per-module HM inference + construction (errors reported here)
  │              │  also populates TypedImplBlock::aspect_id via names.symbols
  └──────────────┘
       │  typed_ast::TypedModuleGraph
       ▼
  ┌─────────────┐
  │ Move Check  │  optional (--move-check flag): rejects use-after-move (RFC-0071, #291);
  │ (optional)  │  validation only — off by default in v0.12.0, see the changelog
  └─────────────┘
       │  typed_ast::TypedModuleGraph (unchanged; validation gate only)
       ▼
  ┌─────────────┐
  │  Elaborator │  resolves MethodDispatch per call site; wraps graph in ElaboratedModuleGraph
  └─────────────┘
       │  elaborator::ElaboratedModuleGraph
       ▼
  ┌─────────────┐
  │  Evaluator  │  tree-walks ElaboratedModuleGraph → program output
  └─────────────┘
```

Each stage is a separate Rust module. No stage is skipped, though Move Check only runs
when `--move-check` is passed — see `pipeline.rs::run_file`.

---

## Crate Structure

```
metel-interpreter/
├── Cargo.toml
└── src/
    ├── main.rs            — CLI entry point: selects a root .mln file, runs the pipeline
    ├── grammar.pest       — pest PEG grammar for the language
    ├── module_loader.rs   — loads the selected root file and its transitive import graph
    ├── name_resolver.rs   — resolves import scopes, visibility, and re-exports per module
    ├── path_normalizer.rs — rewrites qualified Expr::Path nodes to Expr::ResolvedPath
    ├── reference_resolver.rs — builds the ReferenceTable consumed by name_resolver/typechecker
    ├── coherence.rs       — aspect-impl orphan rule (T0014) and overlap detection (T0015)
    ├── move_check/        — optional use-after-move checker (RFC-0071, --move-check flag)
    ├── place.rs            — addressable lvalue-path representation shared by move_check and the typechecker
    ├── parser/            — drives pest, builds untyped AST from CST
    ├── ast/               — untyped AST node definitions
    ├── types/             — concrete type representation (Type enum)
    ├── typeinference/     — HM inference engine: type vars, unification, constraints, schemes
    ├── typechecker/       — two-pass type checker; produces typed AST
    │   ├── mod.rs         — check() / check_graph() entry points, CorePrelude, GlobalExports
    │   ├── registry.rs    — build_registry (drives populate_schemes_from_embedded_core + register_program_decls), concrete env builders
    │   ├── inference.rs   — Pass 1: all infer_* functions
    │   ├── construction.rs— Pass 2: ConstructCtx, construct_* functions, exhaustiveness
    │   └── conversions.rs — type_expr_to_infer, infer_type_to_type, type_to_infer
    ├── symbols.rs         — SymbolId type; reserved ID constants for builtins; SymbolTable intern
    ├── typed_ast/         — typed AST node definitions (MethodDispatch, TypedImplBlock::aspect_id)
    ├── elaborator/        — post-inference elaboration pass; resolves MethodDispatch, produces ElaboratedModuleGraph
    ├── evaluator/         — tree-walking evaluator, lexical env, runtime registry, runtime values
    │   ├── mod.rs         — core: Value, Signal, Environment, RuntimeRegistry, evaluate(), eval_block/stmt/expr
    │   ├── builtins.rs    — register_builtins/runtime_registry: std::core intrinsic values plus type-owned runtime methods with receiver/signature metadata
    │   ├── call.rs        — call_function and method-call dispatch
    │   ├── display.rs     — format_float, value_to_display_string, format_value
    │   ├── lvalue.rs      — eval_binop, apply_assign_op, lvalue path helpers
    │   └── pattern.rs     — match_pattern
    └── error/             — unified error type covering all pipeline stages
```

---

## Component Boundaries

| Data | Type | Produced by | Consumed by |
|------|------|-------------|-------------|
| Module graph | `module_loader::ModuleGraph` | module loader | name resolver / path normalizer |
| Resolved names | `name_resolver::ResolvedNames` | name resolver | path normalizer / typechecker / elaborator |
| Normalized graph | `path_normalizer::NormalizedModuleGraph` | path normalizer | coherence / typechecker |
| Reference table | `reference_resolver::ReferenceTable` | reference resolver (called from name resolver) | typechecker |
| Typed module graph | `typed_ast::TypedModuleGraph` | typechecker (`check_graph`) | move check (optional) / elaborator (`elaborate`) |
| Elaborated module graph | `elaborator::ElaboratedModuleGraph` | elaborator (`elaborate`) | evaluator (`evaluate_graph`) |
| Untyped program (single-file) | `ast::Program` | `load_program` (single-file shim) | typechecker (`check`) |
| Typed program (single-file) | `typed_ast::TypedProgram` | typechecker (`check`) | evaluator (`evaluate`) |
| Errors | `MetelError` | any stage | caller / CLI |

---

## Error Design

All errors use a unified `MetelError` type:

```rust
enum MetelError {
    ParseError   { code: ErrorCode, message: String, start: usize, end: usize, filename: String },
    TypeError    { code: ErrorCode, message: String, start: usize, end: usize, filename: String },
    RuntimePanic { message: String, start: usize, end: usize, filename: String },
    Internal     { message: String },
}
```

Type error codes: E0001–E0008. Runtime panics (`.yolo()` on `nope`, out-of-bounds, division by zero) terminate with a non-zero exit code.

---

## Component Notes

| Component | Notes |
|-----------|-------|
| Module Loader | `src/module_loader.rs` — `load_root` builds the topological `ModuleGraph`; `load_program` parses a single file (shim for single-file test harnesses) |
| Name Resolver | `src/name_resolver.rs` — `resolve` produces per-module `ModuleScope`, `pub_surface`, and re-exports; also assigns a `SymbolId` to every top-level declaration and stores the intern table in `ResolvedNames::symbols` |
| Path Normalizer | `src/path_normalizer.rs` — `normalize` rewrites qualified `Expr::Path` nodes to `Expr::ResolvedPath`; produces `NormalizedModuleGraph` |
| Symbols | `src/symbols.rs` — `SymbolId` newtype; reserved ID constants for builtin types and aspects; `SymbolTable` intern helper |
| Reference Resolver | `src/reference_resolver.rs` — `collect_references` builds the `ReferenceTable` consumed later by the typechecker; invoked from within `name_resolver::resolve`, not a standalone pipeline call |
| Coherence | `src/coherence.rs` — `check(&NormalizedModuleGraph, &ResolvedNames)`; aspect-impl orphan rule (`T0014`) and overlap detection (`T0015`), RFC-0060/#238; validation only, runs after path normalization and before type-checking |
| Move Check | `src/move_check/` — `check_graph(&TypedModuleGraph)`, opt-in via `--move-check`; rejects use-after-move (RFC-0071, #291); shares `src/place.rs`'s addressable lvalue-path representation with the typechecker |
| Elaborator | `src/elaborator/mod.rs` — `elaborate(TypedModuleGraph, &ResolvedNames) -> ElaboratedModuleGraph`; resolves every `MethodDispatch::Dynamic` site to `Inherent` or `Aspect { aspect_id }`; see [decisions/adr-0037-elaboration-boundary.md](decisions/adr-0037-elaboration-boundary.md) |
| Parser | `src/parser/`, `src/grammar.pest` |
| Type Checker | [typechecker.md](typechecker.md) |
| Evaluator | [evaluator.md](evaluator.md) |
| Testing | [testing.md](testing.md) |
| Design decisions | [decisions/](decisions/) |
