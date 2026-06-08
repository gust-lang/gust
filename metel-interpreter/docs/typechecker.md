# Typechecker Implementation Notes

> Status: v0.8.1 — elaboration support added (METEL-152): `check_graph` now threads `names.symbols` through `check_impl` → `construct_program` → `ConstructCtx` so that `construct_impl_decl` can populate `TypedImplBlock::aspect_id`.

---

## Pipeline Position

### Single-module (legacy `check`)

```
untyped AST  ──►  check()  ──►  TypedProgram
                    │
                    ├─ Pre-pass: register builtins, enums, hoist names
                    ├─ Pass 1:   infer — emit constraints, solve
                    └─ Pass 2:   construct — re-derive concrete types, build TypedAST
```

### Multi-module (v0.6.0 `check_graph`)

```
NormalizedModuleGraph + ResolvedNames
       │
       ▼  (for each module in topological order)
  ┌─────────────────────────────────────────────────────┐
  │ build_import_schemes — pull pub schemes from         │
  │   GlobalExports for this module's imports            │
  │                                                      │
  │ check_impl(program, imported_schemes, base_registry) │
  │   ├─ Pre-pass: seed imports, register builtins, hoist│
  │   ├─ Pass 1:   infer (same as single-module)         │
  │   └─ Pass 2:   construct (scheme_env includes imports)│
  │                                                      │
  │ filter_pub_schemes — extract pub names → GlobalExports│
  └─────────────────────────────────────────────────────┘
       │
       ▼
  TypedModuleGraph (one TypedModule per input module)
```

Entry points:
- `typechecker::check(program) -> Result<TypedProgram>` — single-module legacy path
- `typechecker::check_graph(graph, names, std_prelude) -> Result<TypedModuleGraph>` — multi-module path (v0.6.0)

---

## Module Structure

| File | Responsibility |
|---|---|
| `mod.rs` | `check()` / `check_graph()` entry points; `StdPrelude`, `GlobalExports`, `check_impl` |
| `registry.rs` | `build_registry`, `register_builtins`, `build_concrete_*_env`; registers aspect declaring modules for elaboration |
| `inference.rs` | Pass 1 — all `infer_*` functions |
| `construction.rs` | Pass 2 — `ConstructCtx`, all `construct_*` functions, exhaustiveness checking; `ConstructCtx` carries `symbols: Option<&HashMap<(Vec<String>, String), SymbolId>>` threaded from `check_graph` so `construct_impl_decl` can set `TypedImplBlock::aspect_id` |
| `conversions.rs` | `type_expr_to_infer`, `infer_type_to_type`, `resolved_to_type`, `type_to_infer` |

The inference engine lives in `src/typeinference/` (type vars, unification, substitution, constraints, schemes). The typechecker modules in `src/typechecker/` walk the AST and drive that engine.

---

## Theory Background

### Type Variables and InferType

During inference, types may be partially unknown. **Concrete types** (`Type` enum) are fully resolved: `Int`, `String`, `fun(Int) -> String`. **Inference types** (`InferType` enum) may contain type variables — placeholders that get unified with concrete types as more information becomes available:

```
?t0, ?t1, ?t2   — type variables (generated fresh for each unknown)
fun(?t0) -> ?t1 — an InferType containing type variables
```

Type variables satisfy an **occurs check**: `?t0` cannot be unified with `Array(?t0)`, which would create an infinite type.

### Unification

Unification makes two types equal by binding type variables:

```
unify(Int, Int)                        → ok, already equal
unify(?t0, Int)                        → ok, bind ?t0 = Int
unify(?t0, ?t1)                        → ok, bind ?t0 = ?t1
unify(Int, String)                     → error, incompatible
unify(fun(?t0) -> ?t0, fun(Int) -> Int) → ok, bind ?t0 = Int
unify(?t0, Array(?t0))                 → error, occurs check
```

### Substitution and Constraints

A **substitution** is a map from type variables to types (`?t0 → Int`, `?t1 → String`). Applying a substitution replaces all variables in a type with their bindings.

Rather than unifying types immediately as the AST is walked, the inference system collects **constraints** (equality relations between `InferType`s, each tagged with a source span) and solves them in batch. Batch solving handles complex interdependencies and produces better error messages.

### Let-Polymorphism and Type Schemes

A **type scheme** is a type with universally quantified variables: `∀α. α → α` (the identity function — works with any type). In code:

```rust
pub struct TypeScheme {
    pub quantified_vars: Vec<TypeVar>,
    pub ty: InferType,
}
```

When `let id = fun(x) { x }` is inferred:
1. Infer the body — parameter `x` gets fresh variable `?t0`, function type is `fun(?t0) -> ?t0`
2. **Generalize**: identify free type variables not shared with the outer environment; `?t0` is free → scheme `∀?t0. fun(?t0) -> ?t0`
3. Bind `id` to this scheme in `poly_env`

When `id` is **used**, the scheme is **instantiated** with fresh type variables — each call site gets an independent copy:

```
id(42)      → instantiate to fun(?t1) -> ?t1, unify ?t1 = Int  → id(42) : Int
id("hello") → instantiate to fun(?t2) -> ?t2, unify ?t2 = String → id("hello") : String
```

### The Generalization Boundary: Why `env_fvs` Matters

Generalization must only quantify type variables that are *truly local* to the function. If a variable is shared with the outer scope, quantifying it is unsound.

```metel
fun f(x) {
    let g = fun(y) { x };   // g's type: fun(?t1) -> ?t0 where ?t0 is x's type
}
```

`?t1` is local to `g` — safe to quantify. `?t0` is shared with `f`'s scope — quantifying it would let different calls to `g` return different types, but `x` has one concrete type per call to `f`. The typechecker snapshots the environment's free variables (`env_fvs`) before entering the function body and only quantifies variables absent from that set:

```
fun_ty   = fun(?t1) -> ?t0
env_fvs  = {?t0}                          ← x's type is already in scope
scheme   = ∀?t1. fun(?t1) -> ?t0          ← ?t0 left free, not quantified
```

The snapshot is taken before pushing the function's parameter scope — the right moment, capturing what the surrounding context has already committed.

### Never Type

`InferType::Never` (the bottom type `!`) unifies with any type. Diverging expressions — `return`, `break`, `continue`, and infinite `loop` with no reachable `break` — produce `Never`. This lets the constraint solver treat dead branches as compatible with any expected type.

### Rank-1 Limitation

The HM algorithm infers types at rank 1: `∀` only at the outermost level. Higher-rank polymorphism (e.g. a function that accepts a polymorphic function as an argument) requires decidability-breaking extensions and is not supported. The practical consequence: function arguments are unified as monotypes; passing a polymorphic function as an argument works only if the call site knows the concrete instantiation.

---

## Pre-Pass

Three hoisting steps run before Pass 1:

1. `register_builtins` — binds built-in function names (`print`, `array_push`, etc.) as `TypeScheme` entries in `ctx.poly_env`, and registers `String.len` in `ctx.method_env`.
2. `build_registry` (via `TypeDefinitionRegistry`) — registers `Perhaps<T>` and `Result<T,E>` with their type params as fresh type variables, user-defined enum variants, struct field types, and method signatures.
3. `hoist_fun_decls` — walks top-level `FunDecl`s and pre-registers each with a fresh type variable in both `ctx.mono_env` and `ctx.poly_env`. Enables forward references, mutual recursion, and shadowing of `std::core` builtin names.

`hoist_fun_decls` is also called at block entry in `infer_block`, so nested functions support forward references within their block.

Struct and enum declarations follow **lexical scope rules** matching Rust's model:

- `build_registry` registers only top-level `struct`/`enum` declarations. These are globally visible for the entire compilation unit.
- Struct/enum declarations inside function bodies (or any nested block) are registered at block entry by `infer_block` using `TypeDefinitionRegistry::push_struct_scope` / `register_struct_fields` / `pop_struct_scope`. On scope exit, all names registered in that scope are removed from the registry.
- `construct_block` in Pass 2 mirrors this: it pushes a new struct scope, builds concrete field types from the substitution, and pops on exit.
- A locally-declared struct is **not visible outside its enclosing block**. Two functions may declare structs with the same name without collision.

---

## Pass 1 — Type Inference

**Modules:** `typeinference/mod.rs` (engine) + `typechecker/inference.rs` (AST walkers)

### Environment Structure

```
InferContext {
    mono_env: Vec<HashMap<String, (InferType, bool)>>  // scope stack, innermost last
    poly_env: Vec<HashMap<String, TypeScheme>>           // scope stack, mirrors mono_env
    constraints: Vec<Constraint>                        // accumulated equality constraints
    var_gen: TypeVarGenerator                           // globally unique TypeVar allocator
    registry: TypeDefinitionRegistry                    // pre-built struct/enum/method/aspect-impl registries
    current_return_type / current_break_type            // context for return/break inference
    current_type_params: HashMap<String, TypeVar>       // active generic param map (see below)
}
```

**`current_type_params` invariant:** set to the enclosing generic function's `name → TypeVar` map for the duration of `infer_fun_decl` / `infer_impl_method` body inference, and restored to the caller's map afterward via `swap_type_params`. Empty at top level and inside non-generic functions. All type annotations inside a function body (`let`, `mut`, `for`-init, closure params) must resolve through `ann_to_infer(ann, ctx)` rather than the bare `type_expr_to_infer(ann)` so that param names resolve to their TypeVars instead of `Type::Named`.

`poly_env` takes precedence over `mono_env` in `ctx.lookup()`. Poly entries are automatically instantiated with fresh type variables on each lookup (let-polymorphism).

### Constraint Emission

Each `infer_expr` call returns an `InferType` and may push zero or more `Constraint`s into `ctx.constraints`. Constraints are not solved inline — they accumulate and are solved in batch.

### Inline Solve-and-Generalize (Functions)

`infer_fun_decl` solves accumulated constraints immediately after inferring the function body, generalizes the function type, and re-binds it as a `TypeScheme` in `poly_env`. This is essential for:
- Let-polymorphism: the function's type scheme can be instantiated fresh at each call site
- Mutual recursion: the pre-hoisted mono binding is unified with the inferred type before generalization

The same constraints remain in `ctx.constraints` after the inline solve; the final `ctx.solve()` at the top level re-solves the same list (idempotent).

### Eager Partial Solves

A few inference cases call `ctx.solve()` eagerly to determine structural type information before emitting further constraints:

- `Expr::ForIn`: resolves the iterable type to decide Array vs Range
- `Expr::FieldAccess`, `Expr::MethodCall`, `Expr::TupleAccess`: resolves the receiver type to look up fields/methods

These partial solves are read-only (they produce a `Substitution` value but don't modify `ctx.constraints`). They are a pragmatic workaround for the fact that field/method lookup requires knowing the concrete type name — a fundamental limitation of constraint-only inference.

### Incremental Constraint Solving (v0.8.2)

`InferContext::solve()` now caches the solved substitution for the already-seen
prefix of `ctx.constraints` and only processes newly appended constraints on the
next call. This is valid because inference only ever appends constraints; it
does not rewrite or remove earlier ones.

This matters because generic-heavy inference triggers many eager partial solves.
Re-solving the full constraint list on every call was the dominant cost in the
`0.8.2` benchmark baseline. The cache keeps the behavior the same while making
repeated solves proportional to the number of new constraints rather than the
entire accumulated list.

The invariant is:

- constraints are append-only during a typecheck run
- cached substitutions may be reused only for the already-solved prefix
- `default_literal_vars(...)` remains a caller-side post-processing step and is
  not stored in the cache

### Mutability Enforcement

Binding mutability is tracked as a boolean flag in `mono_env`: `HashMap<String, (InferType, bool)>`. `bind_mono(name, ty, is_mutable)` stores the flag. `lookup_for_write(name, span)` retrieves the binding and returns `T0006` if `is_mutable` is false.

Three write sites call `lookup_for_write` during Pass 1:

1. **Direct assignment** (`Expr::Assign { target: Ident(x), .. }`) — checked directly.
2. **`&mut x`** (`UnaryOp::RefMut` where the operand is `Ident`) — checked before returning `MutPointer(T)`.
3. **Field assignment** (`Expr::FieldAssign`) — the object is inferred first to resolve its type. If the object type is `MutPointer(T)` (auto-deref field assign), the binding check is skipped entirely. Otherwise, `root_binding_for_write` walks the object chain (`FieldAccess → Index → Ident`) to find the root binding and calls `lookup_for_write` on it. Chains ending in `UnaryOp::Deref` also return `None` from `root_binding_for_write` and are exempt.

Method `self` parameters: `self` in an `&mut self` method is bound with `is_mutable = true` regardless of the `Param::mutable` flag (which the parser always sets to `false` for receivers). This is handled at `infer_impl_method` and `infer_default_aspect_method` with `p.mutable || matches!(p.receiver, Some(ReceiverKind::RefMut))`.

### Type Ascription (`:` Operator)

`e : T` is a pure inference hint. Inference:
1. Infers the inner expression type `inner_ty`.
2. Converts the annotation `T` to an `InferType` via `type_expr_to_infer`.
3. Adds a constraint `inner_ty ~ ascribed_ty`.
4. Returns `inner_ty` (not the annotated type directly).

The constraint propagates the annotation into the solver without changing control flow. In Pass 2, the ascription node is **erased**: `construct_expr` resolves the annotation to a concrete `Type` and constructs the inner expression with that type as the expected-type hint. No `TypedExpr::Ascribe` variant exists — ascription has zero runtime cost.

---

## Pass 2 — Construction

**Module:** `typechecker/construction.rs`

Pass 2 re-walks the untyped AST with:
- `subst: &Substitution` — the final solved substitution from Pass 1
- `scheme_env: &SchemeEnv` — generalized type schemes for user-defined functions
- `ConstructCtx` — a stripped-down context with concrete `Type` values (no inference)

Each `construct_expr` call re-derives the node's concrete type by applying `subst` to the inferred type and converting via `infer_type_to_type`. No constraints are emitted; no unification is performed.

### Polymorphic Call Sites

When a call site resolves to a polymorphic callee (present in `scheme_env` but not in `ConstructCtx.env`), `construct_call` calls `instantiate_scheme_for_call`, which:
1. Instantiates the scheme with fresh type variables
2. Unifies the instantiated param types against the concrete argument types
3. Returns the concrete `Fun` type for the specific call

### Polymorphic Function Bodies

Functions with quantified type variables in their scheme are stored as `FunBody::Generic(untyped_block)` rather than `FunBody::Typed(typed_block)`. At each call site the evaluator re-runs the construction pass on the untyped block at the concrete call-site types, producing a `TypedBlock` that is evaluated normally. This is the monomorphization mechanism.

`let`-bound unannotated closures generalised to polymorphic schemes are stored as `TypedExpr::GenericClosure { params, body: Block, .. }` and evaluated to `ClosureBody::Untyped(block)`. The evaluator re-runs construction per call, mirroring the function case.

### Closure Body Expected Type

`Expr::Closure` construction passes `return_type.as_ref().map(|_| &ret_ty)` as the `expected_tail_ty` for `construct_block`. This is necessary so that enum variant literals with unmentioned type params (e.g. `Result::Ok { value }` in a `fun() -> Result<T,E>`) can resolve the unbound type argument from the annotation hint rather than failing with T0002. Closures without an explicit return annotation pass `None`.

### Exhaustive Match Checking

`check_match_exhaustiveness` runs at the end of `construct_match` once the scrutinee type is known concretely.

- An unguarded `_`, bare binding pattern, or irrefutable tuple `(a, b, ...)` is a catch-all — exhaustive.
- **Guarded arms do not count**: a guard may fail at runtime.
- `Bool`: must cover `true` and `false` (both unguarded).
- `Perhaps(_)`: must cover `Perhaps::Some` and `Perhaps::Nope`.
- `Result(_, _)`: must cover `Result::Ok` and `Result::Err`.
- Named enum: must cover every variant.
- `Never`: vacuously exhaustive.
- All other types (Int, Float, Str, …): value-infinite; only a catch-all satisfies exhaustiveness.

Error: `E0008 Non-exhaustive match`.

---

## Type Registries

All type and impl data is stored in a single `TypeDefinitionRegistry` (owned by `InferContext`, shared with `ConstructCtx` via reference). This replaced the previous design where four separate maps were passed to `ConstructCtx` individually (#133).

| Field | Type | Content |
|---|---|---|
| `struct_env` | `HashMap<String, Vec<(String, InferType, Span)>>` | struct name → field list (name, type, declaration span) |
| `struct_type_params` | `HashMap<String, Vec<TypeVar>>` | generic struct name → ordered type-param TypeVars (absent for non-generic structs) |
| `method_env` | `HashMap<String, HashMap<String, InferType>>` | type name → method name → fun type |
| `enum_env` | `HashMap<String, EnumInfo>` | enum name → variant list + type params |
| `aspect_env` | `HashMap<String, Vec<String>>` | aspect name → required method names |
| `impl_aspect_env` | `HashMap<(target, aspect), Vec<Vec<Type>>>` | aspect impl type-arg lists |

`TypeDefinitionRegistry` is constructed in one pre-pass and injected into `InferContext::new`, consistent with [ADR-0001](decisions/adr-0001-typeregistry-structure-and-location.md).

### Elaboration output: `TypedImplBlock::aspect_id`

`TypedImplBlock` carries `aspect_id: Option<SymbolId>` (v0.8.1, METEL-152). It is populated at the end of `construct_impl_decl` by:

1. Looking up the aspect's declaring module via `ctx.registry.aspect_declaring_module(aspect_name)`.
2. Looking up the `SymbolId` for `(declaring_module, aspect_name)` in `ctx.symbols` (the name resolver's intern table, threaded from `check_graph`).

`ctx.symbols` is `None` on the single-module pipeline (`check` / `check_with_ctx`), so `aspect_id` is `None` in that path. The elaborator (`elaborate`) consumes `aspect_id` and stores it in `RuntimeAspectImpl::aspect_id` for SymbolId-keyed dispatch.

---

## Known Limitations

### `as` Cast — Via `From<S>` Aspect (v0.4)

`as` is now desugared to a `From<S>` aspect check. Built-in impls cover `Int ↔ Float`. User-defined types may implement `From<S>` to enable `as` casts. Casting between types with no `From` impl is a typecheck error.

---

## Extension Points

### v0.4 — Aspects (shipped)

All four v0.4 extension points are done:

1. ~~Add `impl_env` / extend `TypeDefinitionRegistry` with aspect-impl storage~~ — **done**: `TypeDefinitionRegistry` carries `aspect_env: HashMap<String, Vec<String>>` (aspect name → required method names) and `impl_aspect_env: HashMap<(target, aspect), Vec<Vec<Type>>>` (impl type-arg lists).
2. ~~Replace the provisional `as` cast with a `From<S>` aspect check~~ — **done**: `construct_cast` calls `has_from_impl(target, source)` and errors with `cannot cast` if no impl is registered.
3. ~~Replace the provisional `?` error type match with a `From<E>` coercion lookup~~ — **done**: `construction.rs` emits a `PropagateError` node carrying the `from_key` when `E1 ≠ E2`; the evaluator calls the impl at runtime.
4. ~~Upgrade `for-in` from Array/Range only to an `Iterable<T>` aspect check~~ — **done**: inference pass checks `iterable_elem_type` and falls back to Array/Range for built-in types.
