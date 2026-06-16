---
id: adr-0041
title: "Complete the SymbolId Migration via a Name-Resolution Pass (Resolved AST)"
date: '2026-06-14'
status: accepted
relates: adr-0038, adr-0039, adr-0037, adr-0025
implements: METEL-185, METEL-187
---

## Context

METEL-181/180 (ADR-0038/0039) made *callable* dispatch identity-based:
overloaded free functions carry `Call::callee_id: SymbolId` and the evaluator
dispatches through `RuntimeRegistry::symbol_values`. Everything else still
re-derives identity from surface strings after name resolution:

- **`TypeDefinitionRegistry`** (`typeinference/mod.rs`): ~15 maps keyed by a
  type/aspect *name* `String` (`struct_env`, `enum_env`, `method_env`,
  `method_scheme_env`, `aspect_env`, `impl_aspect_env`, …).
- **`RuntimeRegistry`** (`evaluator/mod.rs`): `types: HashMap<String,
  RuntimeTypeEntry>`; every method call maps the receiver `Value` to a type
  *name* via `runtime_type_name()`, then selects the method by *name*
  (`inherent_methods`, `RuntimeAspectImpl::methods`, …). `get_aspect_method_by_id`
  falls back to a pure string search for builtins seeded with `aspect_id: None`.
- **`Environment`** (`evaluator/mod.rs`): `scopes: Vec<HashMap<String, …>>`.
  `TypedExpr::Ident(name)` resolves via `Environment::get(name)`, which serves
  `let` bindings, parameters, captured locals *and* top-level functions
  indiscriminately, with `std_core_lookup` as a name fallback.

These are tracked as two work items — METEL-185 (rekey the type registries) and
METEL-187 (symbol-key ordinary function values). We have decided to do them as
**one migration**: they are the same underlying change (replace
post-resolution name lookups with `SymbolId`), and splitting them only
duplicates the resolver→consumer plumbing.

An earlier draft split this into two ADRs and proposed an incremental
dual-keyed rollout; this ADR supersedes that framing.

## The governing constraint: two kinds of reference

A migration plan only makes sense once you separate:

- **Scope-resolved references** — top-level functions, types, enum variants,
  module paths, static members (`List::new`), imports/aliases. Determined by
  lexical scope, so resolvable to a `SymbolId` **before typechecking**.
- **Type-dependent references** — instance method calls (`x.foo()`), overload
  selection, aspect dispatch. Need the inferred receiver/argument types, so
  resolvable only **during/after typecheck** (where the current code already
  resolves overloads at construction and methods in elaboration, ADR-0037).

No single pre-typecheck pass can erase type-dependent name selection. The
migration therefore resolves the scope half up front and keeps the
type-dependent half where the types are known — making *its output* an id.

## Decision

**Add a resolution pass that produces a "resolved AST": every reference site
carries a `Res`. The lexical environment then holds only true locals; the type
and runtime registries key by `SymbolId`; type-dependent dispatch stays at
typecheck/elaboration but emits ids.** (Approach 1 — the "renamer" model, as in
GHC's `RdrName → Name` and rustc's `Res`/`DefId`.)

### 1. A resolution pass over references

Extend `name_resolver` — which today only interns *declarations* into
`symbols`/`definitions` — to also walk every *reference* and attach:

```
enum Res {
    Def(SymbolId),   // a top-level declaration (fn, type, enum variant, static member, module value)
    Local,           // a let binding / parameter / captured local — stays name-keyed
}
```

The resolved `Res` reaches the typed AST (carried on the AST reference nodes, or
via a resolution side-table — reference sites need stable identity, so either
add node ids or key the table by reference span). `Res::Local` references stay
name-resolved in the environment; `Res::Def` references never touch the env.

### 2. Environment holds only locals

After the pass, `Ident`/`Path` that refer to top-level declarations resolve to
`Res::Def(SymbolId)`; the evaluator materializes the value from the symbol
registry (extending the existing `callee_id` dispatch to all top-level callees,
and to first-class references like `let f = my_fn`). Locals remain in the
string-keyed `Environment`. Shadowing is automatic: a `let`/param is a
`Res::Local` and never carries a top-level id.

We deliberately keep locals **name-keyed**, not slot/de-Bruijn indexed (see
Considered alternatives). `std_core_lookup`'s name path is removed.

### 3. Registries keyed by SymbolId

`TypeDefinitionRegistry` and `RuntimeRegistry::types` key by type `SymbolId`;
method selection within an entry is by method `SymbolId` (from the RFC-0059
definition index, carried on `MethodCall`/`MethodDispatch`). A `Value` resolves
to its type's `SymbolId` so runtime dispatch skips `runtime_type_name()`.
Builtin aspect impls are seeded with the fixed `SYM_ASPECT_*` ids, so
`get_aspect_method_by_id` becomes a pure id lookup and its string fallback is
deleted. Cross-module references and monomorphization travel as ids.

### 4. Diagnostics keep names

A single `SymbolId → original spelling` side table (from the resolver) renders
type/method/function names in errors. No diagnostic path consults a name-keyed
*lookup* map.

## Considered alternatives

- **Approach 2 — lower to a dedicated IR (HIR).** Introduce an id-keyed IR with
  slot-indexed locals (the rustc `AST → HIR → MIR` model), so names are absent
  by construction and the evaluator runs on a `Vec<Value>` frame. This is the
  clean end-state and is the right home for the work **if** the System F HIR /
  native backend (METEL-171) is near-term — doing the resolved-AST migration
  and *then* an HIR repeats the work. It was deferred because it is a far larger
  change (new IR + lowering + evaluator rewrite) and METEL-171 is not yet
  scheduled. This ADR is the pre-HIR consolidation; a future HIR can take
  `Res::Local` to slots without revisiting global identity.
- **Slot/de-Bruijn environment now.** The local half of the IR idea in
  isolation. Deferred with the HIR for the same reason; keeping locals
  name-keyed is a deliberate, reversible stopping point.

## Implementation order (one effort; staged only to keep tests green)

This is a single migration, not independently shipped phases. Suggested internal
order so `cargo test` stays green throughout:

1. **Resolution pass.** Extend `name_resolver` to emit `Res` for every
   reference; thread it into construction and the elaborator. Behaviour
   unchanged (ids available, not yet consumed).
2. **Callables.** Dispatch all top-level direct calls and first-class function
   references via `Res::Def(SymbolId)`; register every top-level fn in
   `symbol_values`; drop the redundant env bindings and `std_core_lookup` name
   path. (Subsumes METEL-187.)
3. **Runtime type registry.** Resolve receiver → type `SymbolId`; key
   `RuntimeRegistry::types` and method selection by id; carry method ids on
   `MethodCall`/`MethodDispatch`; seed `SYM_ASPECT_*`; delete the aspect string
   fallback.
4. **Typechecker registry.** Rekey the `TypeDefinitionRegistry` maps cluster by
   cluster (structs → enums → methods → aspects → impls).
5. **Cleanup.** Remove name→entry maps; keep only the `id → spelling` diagnostic
   table. Add regressions: two same-named types/functions in different modules
   dispatch independently. (Together with step 3–4, subsumes METEL-185.)

## Progress and refinements (as built)

Steps 1–2 (METEL-187) shipped as designed, with two implementation choices worth
recording:

- The resolution pass is realized as a dedicated `reference_resolver` module run
  *inside* `name_resolver::resolve`, before path normalization. It records only
  `Res::Def` references in a `Span`-keyed side table on `ResolvedNames`
  (`references`); `Res::Local` is the implicit default (absent span). Because it
  runs pre-normalization, it resolves only bare single-segment `Ident`s —
  multi-segment paths still flow through the path normalizer, which already
  stamps `symbol_id` on `Expr::ResolvedPath`. Construction reads ids from both.
- Top-level functions are registered in `symbol_values` under a new
  `TypedFunDecl::def_id` **in addition to** their lexical-env binding (kept for
  first-class uses), and a `Call::callee_id` that misses the registry and is not
  an overload id (`< OVERLOAD_SYM_START`) falls back to name lookup. This makes
  call-site stamping safe even for top-level `let`-bound values and the
  single-program path, and defers the env-binding removal to step 5.

Two prerequisites for steps 3–5 surfaced during implementation, splitting step 3:

- **Step 3a — method identity.** `name_resolver` did not assign `SymbolId`s to
  `impl`/`aspect` methods (the `definitions` index skipped `Decl::Impl`), so the
  "methods already have ids from METEL-184" assumption did not hold. Method ids
  are now interned in the resolver under structured keys (`Target::method`,
  `Target::Aspect::method`, `Aspect::method`) before any consumer needs them.
- **Step 3b — value→id at runtime.** `Value::Struct`/`Value::Enum` carry only a
  type-name `String`. Resolving a receiver directly to a type `SymbolId` (and so
  eliminating `runtime_type_name()`) requires threading a `SymbolId` onto those
  `Value` variants — a wide but mechanical change done as its own step before the
  registry is rekeyed.

### Landed so far

- **3b-i**: the resolver now interns through `crate::symbols::SymbolTable`, so the
  `SYM_TYPE_*`/`SYM_ASPECT_*` constants are the real pipeline ids; embedded-core
  aspect impls are seeded under their builtin ids.
- **3b (aspect)**: `get_aspect_method_by_id` is purely id-based; the string
  fallback and `get_aspect_method` are deleted.
- **3b-ii**: `Value::Struct`/`Value::Enum` carry `type_id`, populated at every
  user/std construction site (struct literals, unit enum variants via struct-
  literal lowering, builtin Perhaps/Result/List/Range). Host-built std data types
  (`EnvVar`/`OsError`/`ProcessOutput`, which have no method dispatch) remain `None`.
- **Single-program path removed**: `typechecker::check*` / `evaluator::evaluate*`
  (the no-resolver path) are gone; the bench and unit tests run the graph
  pipeline. This removes the last surface-name-only consumer and unblocks the cut.

### Remaining (step 4–5)

- **3b-iii (landed).** `RuntimeRegistry::types` is keyed by `SymbolId`, with a
  `type_ids` name→id index as the single resolution step for static members,
  `From` targets, and host-built values. `resolve_value_type_id` drives instance
  dispatch (carried `type_id` first, else name index); `TypedImplBlock` carries
  `target_type_id`; embedded-core/`run_passes` register under the type id. The
  `Call::callee_id`→name fallback stays — it is the deferred first-class-function
  environment question (METEL-187 out-of-scope note), not type/method dispatch.

- **Cross-module dispatch guarantee (landed).** Rather than the full registry
  rekey, struct/enum *reference* nodes now carry the resolver-stamped type id:
  the path normalizer resolves a module-qualified struct/enum literal's type id
  from the symbol table (`Expr::StructLiteral` gained `symbol_id`), and
  construction prefers it over the name-keyed declaring-module index. Two modules
  each declaring `struct Widget` with a different `kind()` now dispatch to their
  own impl (regression `same_named_structs_in_two_modules_dispatch_independently`).

- **Deep `TypeDefinitionRegistry` rekey (remaining cleanup).** The runtime path
  is now correct, but the typechecker's `TypeDefinitionRegistry` clusters
  (`struct_env`, `enum_env`, `method_*`, `aspect_*`, decl-module maps) are still
  name-keyed and accumulate across modules, so two *different-shaped* same-named
  cross-module types would still conflate at type-check time (a rarer case that
  surfaces as a confusing type error, not wrong runtime behaviour). Fully meeting
  the "no surface-name lookup in the typechecker" criterion means rekeying those
  maps by type `SymbolId` (threading the resolver id through inference's type-name
  lookups). This is a large, self-contained follow-up; the cross-module runtime
  guarantee and all of METEL-187 do not depend on it. Residual single name→id
  resolution steps (`RuntimeRegistry::type_ids`, unit enum-variant paths,
  module-defined std data types, the `Call::callee_id`→name first-class-function
  fallback) are deliberate and documented above.

## Consequences

- After step 5, no struct/enum/aspect/method/function *lookup* in the
  typechecker or evaluator is keyed by surface name; identity is the resolver's
  `SymbolId`. Two modules may reuse a name with no collision.
- The environment's key space is explicitly defined: **names for locals, ids for
  top-level declarations** — the written decision METEL-187 required.
- First-class function values keep working (a reference resolves to an id; the
  evaluator yields the registered `Value`).
- Hot-path dispatch drops repeated `runtime_type_name()` / scope-walk string
  hashing.
- Large but cohesive change; the staged order keeps each step green and
  reversible. METEL-185 and METEL-187 are both satisfied by this single effort.
- The single-program path (`check_with_ctx`, no resolver) is now test/bench-only
  (ADR-0040); it keeps name seeding until the bench is migrated, the one
  remaining name path, documented as such.
