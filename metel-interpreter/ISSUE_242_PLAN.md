# Implementation plan: issue #242 — RFC-0082 associated types

Repo: `metel-interpreter` (an interpreter for the "Metel" language: parser (pest) ->
name resolver -> path normalizer -> coherence pass -> two-pass HM typechecker
(inference.rs Pass 1, construction.rs Pass 2) -> elaborator -> evaluator).

## Background

RFC-0082 specifies **associated types**: `type Name;` declarations inside `aspect`
blocks, `type Name = ConcreteType;` definitions inside `impl` blocks, `T::AssocType`
projection syntax in generic contexts, equality-constrained bounds
(`Aspect<AssocType = ConcreteType>`), and an object-safety rule (§6, OUT OF SCOPE,
see below).

A PRIOR issue (#233, already implemented) did AST/grammar/parsing plumbing: it all
parses correctly today. BUT #233 explicitly flagged that **real resolution/enforcement
is NOT done**: `typechecker/conversions.rs::type_expr_to_infer_in_context`'s
`Projection` arm currently just produces a fresh/unconstrained `InferType::Var` (a pure
stub) — meaning `T::AssocType` parses and elaborates today but is completely
unconstrained: nothing ties it to the actual concrete associated type any real impl
defines, nothing checks that impl blocks define all the aspect's declared associated
types, and equality constraints in bounds are parsed but never consulted anywhere in
the typechecker.

**This issue (#242) is: implement the real thing.**

A recent sibling issue (#243, RFC-0072 negative bounds) was implemented with this same
strategy (research → written plan → opencode agent → independent review) earlier this
session. That issue's implementation added parallel storage fields for negative bounds
(`TypeScheme.neg_bounds`, registry `neg_fun_bounds`/`neg_type_param_bounds`), sibling
collection/enforcement functions mirroring the existing positive-bound code exactly,
and reused error code T0012 rather than minting a new one. **Follow the same
conventions here**: add parallel fields/sibling functions, don't restructure existing
code; only mint a new error code where genuinely nothing existing fits.

## RFC-0082 full text (condensed to load-bearing content)

**Summary.** Aspects may declare associated types — type-level output parameters that
each implementing type must specify. Declared with `type Name;` in an aspect block,
defined with `type Name = ConcreteType;` in an impl block.

```metel
aspect Alloc {
    type AllocationError;
}
impl Alloc for BumpAlloc {
    type AllocationError = !;
}
aspect Deref {
    type Target;
    fun deref(self: &Self) -> &Target;
}
impl<T, brand 'b> Deref for Rc<T, 'b> {
    type Target = T;
    fun deref(self: &Rc<T, 'b>) -> &T { ... }
}
```

**§1 Declaration in Aspect Blocks.** `type Name;` inside an aspect. Any impl of the
aspect must define `Name`.

**§1.1 Bounds on the declaration.** `type Item: Display;` — constrains every impl: the
concrete type bound to `Item` must satisfy `Display`. Enforced at the impl site. When
no bound is declared, unconstrained.

**§1.2 Use in method signatures.** Inside an aspect block, the bare name (`Target`) is
sugar for `Self::Target`:
```metel
aspect Iterator {
    type Item;
    fun next(self: &mut Self) -> Perhaps<Item>;   // Item = Self::Item
}
```

**§2 Definition in Impl Blocks.** An impl block must define all associated types
declared by the aspect. `type Name = ConcreteType;` binds it for this impl. The
`ConcreteType` must satisfy any bound declared on the association (§1.1). **A missing
associated type definition is a compile error.** Within the impl body, the bare name
resolves to `Self::Item` — the concrete type this impl defines.

**§3 Projection — `T::AssocType`.** In a generic context where `T: Aspect`, the
associated type is referenced as `T::AssocType`:
```metel
fun deref_display<T: Deref>(x: &T) where T::Target: Display {
    println(x.deref());
}
```
`T::Target` is a projection — resolved to the concrete associated type for the
specific `T` at each instantiation. May appear in: function signatures
(`fun f<T: Deref>(x: &T) -> &T::Target`), where clauses (`where T::Target: Display`),
and type positions in bodies (`let y: T::Target = x.deref();`). `T::AssocType` is only
valid when `T: Aspect` is in scope; writing it without that bound is a compile error.

**§3a Disambiguation — no new syntax; use §4's equality constraint with a fresh
variable.** When `T` is bound to two or more aspects that each declare an associated
type of the same name, the bare projection is ambiguous:
```metel
aspect Deref { type Target; fun deref(self: &Self) -> &Target; }
aspect Convert { type Target; fun convert(self: &Self) -> Target; }
fun f<T: Deref + Convert>(x: &T) -> T::Target { ... }
// error: T::Target is ambiguous — both Deref and Convert declare `Target`
```
**This is a hard error, matching the existing method-name-collision precedent
(T0013 — Ambiguous aspect method resolution) exactly** — not a case needing new
disambiguation syntax. Two candidate bracketed-qualifier spellings
(`<T as Aspect>::AssocType`, `<T:Aspect>::AssocType`) were considered and explicitly
rejected by the RFC (both collide with existing meanings of `as`/`<T: Aspect>`
elsewhere in the grammar) — **do not implement either.** The escape hatch is §4's
equality constraint with a fresh type parameter:
```metel
fun f<T: Deref<Target = U> + Convert, U>(x: &T) -> U {
    x.deref()   // ordinary, unambiguous method dispatch
}
```

**§4 Equality Constraints in Bounds.** `Aspect<AssocType = ConcreteType>` in a bound
asserts `T` implements `Aspect` AND its associated type equals `ConcreteType`:
```metel
fun deref_to_node<T: Deref<Target = Node>>(x: &T) -> &Node { x.deref() }
```
Multiple constraints may combine: `T: Deref<Target = Node> + Iterator<Item = i64>`.

**§5 Associated Types vs Generic Type Parameters** — conceptual distinction only
(an associated type is uniquely determined by the implementing type; a generic
parameter on the aspect allows multiple impls per type). No implementation
consequence beyond what §1-§4 already cover.

**§6 Object Safety — OUT OF SCOPE.** RFC-0008 (`dyn Aspect`) is not implemented at
all in this codebase (Phase 4, deferred behind fat pointers, no consumer exists).
There is nothing for an object-safety rule to check yet. **Do not implement any part
of §6.**

**§7 (RFC-0069 SubRegion amendment) — historical only, not spec content, not
relevant.**

**§8 (Standalone Type Aliases) — explicitly deferred by the RFC itself to a future
RFC. Out of scope.**

**§10 Unresolved Questions** — §10.1 (disambiguation) already resolved via §3a above.
§10.2 (higher-kinded associated types) and §10.3 (standalone aliases) explicitly
deferred by the RFC itself. **Out of scope.**

## Scope confirmation (from research — verify, don't re-derive from scratch)

**In scope**: §1 (declaration + §1.1 bound enforcement + §1.2 bare-name sugar), §2
(impl completeness), §3 (projection resolution, both concrete and symbolic/abstract),
§3a (ambiguity — hard error, no new syntax), §4 (equality constraints in bounds).

**Explicitly out of scope, confirmed by reading the actual code — do not attempt:**
- **§6 object safety** — no `dyn`, no trait-object syntax anywhere outside
  `TypeExpr::ImplAspect` (which is `impl Aspect` in *parameter* position, lowered away
  before inference — unrelated to RFC-0008 object safety).
- **§7, §8, §10.2/§10.3** — historical/deferred per the RFC's own text.
- **RFC-0036 conditional/blanket impls (issue #241, not implemented)** — confirmed:
  any `ImplBlock` with non-empty `generics` or a generic target is already skipped by
  both `inference.rs`'s Pass 1 and `registry.rs`'s method-registration pass, with an
  explicit `TODO(generic-impl)` comment. **All new associated-type work must thread
  through the same gate** (only handle non-generic/concrete impls) — do not attempt to
  resolve associated types for conditional impls; that is #241's job. Flag with a
  `TODO(#241)` comment at each relevant check site, matching the convention #243 used
  for its own equivalent gate.
- **RFC-0060 coherence (`coherence.rs`)** — confirmed: `canonicalize()` already has a
  `TypeExpr::Projection { .. } => CanonicalType::Opaque` arm (added in #233). No
  `coherence.rs` changes are needed for #242. If you find during implementation that
  associated types genuinely need to participate in overlap detection, flag this
  clearly as a new discovery in your final report rather than silently expanding scope
  or making the change without flagging it.

## Confirmed current state (from research — line numbers are approximate pointers,
verify against actual current source, since it may have drifted)

- `src/ast/mod.rs`: `AspectDecl.assoc_types: Vec<AssocTypeDecl>` (`{name, bounds:
  Vec<Bound>, span}`), `ImplBlock.assoc_type_defs: Vec<AssocTypeDef>` (`{name, ty:
  TypeExpr, span}`), `TypeExpr::Projection{base, assoc_name, span}`,
  `Bound.assoc_bindings: Vec<(String, TypeExpr)>` — all already exist from #233, all
  currently `#[allow(dead_code)]` (nothing reads them yet — that's this issue's job).
- `src/parser/mod.rs` (`parse_aspect_decl`, `parse_impl_block`, `parse_bound`):
  confirmed correct already, nothing to fix there.
- `src/typechecker/conversions.rs::type_expr_to_infer_in_context`: the `Projection`
  arm is a pure stub — `InferType::Named(format!("{base_name}::{assoc_name}"),
  vec![])`. This is the primary target of the rewrite.
- `src/coherence.rs`: no change needed (see scope section above).
- `src/error/mod.rs`: `TypeErrorCode` tops out at `T0016`. **T0017 is free** — use it
  for "impl missing required associated type definition." Reuse **T0013** (broaden its
  doc comment from "Ambiguous aspect method resolution" to "Ambiguous aspect
  method/associated-type resolution") for §3a's ambiguity error. Reuse **T0012**
  ("Aspect bound not satisfied") for §1.1's bound-violation and §4's
  equality-constraint-mismatch errors — do not mint additional new codes beyond T0017.
- `src/elaborator/mod.rs::build_aspect_method_map`: this is T0013's real mechanism
  today, but it's keyed on **concrete impl target types** discovered by scanning
  `TypedDecl::Impl` blocks — it has nothing to do with a *generic* type parameter's
  in-scope bound list, so it **cannot be reused directly** for §3a's check (which
  needs to search a type param's bound-aspect list, not scan impl blocks). Reuse
  T0013's error code/message shape, build a parallel check where projections are
  resolved against a type param's bound list (see design below).
- `src/typechecker/inference.rs`:
  - `collect_fun_type_var_bounds` / `collect_negative_fun_type_var_bounds` and
    `registry.rs::collect_type_param_bounds` / `collect_negative_type_param_bounds`:
    confirmed — these only ever pull `TypeExpr::Named(n,_) => n` out of `Bound.aspect`;
    `Bound.assoc_bindings` is silently dropped every time, exactly the same shape of
    gap #243 found and fixed for negative polarity. This is the gap §4 must close —
    add a sibling collector for equality constraints, don't touch the existing ones.
  - The `Decl::Impl(ib)` non-generic arm (gated `ib.polarity == Polarity::Positive`) is
    the existing "impl must provide all required methods" check (walks
    `ctx.aspect_method_defs(aspect_name)`, checks each method provided or has a default
    body). **This is the sibling location for both the §2 completeness check and the
    §1.1 bound check** — same function, same guard, added right after the existing
    method-completeness loop.
  - The bounded-`TypeVar` method dispatch "slow path" (inside `infer_expr`'s
    `MethodCall` arm) is where `T::method()` calls on a still-abstract `T: Aspect` are
    resolved today. It currently substitutes `TypeExpr::Named("Self",_) =>
    InferType::Var(*tv)` for the aspect method's return/param types — a bare
    associated-type name (e.g. `Item` in `fun next(...) -> Perhaps<Item>`) falls into
    an `other => type_expr_to_infer(other)` branch and becomes a bogus
    `InferType::Named("Item", [])`. **This confirms §1.2 is not implemented today** —
    needs the same symbolic-placeholder treatment as `T::AssocType` in a signature.
  - `lower_projections_in_program` is **scope-limited to `FunDecl` params/return
    types today** — body-level `let`/`mut` annotations are not rewritten. RFC-0082 §3
    explicitly requires `let y: T::Target = x.deref();` to work — **this gap must be
    closed**, it is real in-scope work.
- `src/typechecker/construction.rs`:
  - `check_fun_call_bounds` / `check_scheme_bounds` / `check_fun_call_neg_bounds` /
    `check_scheme_neg_bounds` (from #243's work) is the exact call-site
    bound-checking pattern to mirror for §4's equality constraints — called from the
    four call-expression construction arms right after `instantiate_scheme_for_call`/
    `_with_turbofish`/`_with_expected_ret` produce `(concrete: Type, var_map:
    HashMap<TypeVar, Type>)`.
  - **Critical finding**: `instantiate_scheme_for_call` and its two siblings each call
    `infer_type_to_type(&subst.apply(&ret))` (and params), which **errors (T0002) on
    any leftover `InferType::Var`**. Today a `Projection` stub silently "resolves" (a
    `Named`, not a `Var`, so this never trips) — wrong-but-non-crashing. The moment
    `Projection` becomes a real symbolic `InferType::Var` for the abstract case, **any
    function returning `T::AssocType` will start failing instantiation with a spurious
    "add a type annotation" error unless the associated-type backfill happens inside
    these three functions, before the final `infer_type_to_type` conversion.** This is
    the single most important implementation-ordering constraint in this plan.
- `src/typeinference/mod.rs`: `TypeDefinitionRegistry` has no associated-type storage
  yet. `TypeScheme` has no field carrying associated-type/projection metadata across
  `generalize`/`instantiate`. `InferContext` has `current_type_param_bounds:
  HashMap<TypeVar, Vec<String>>` already — the natural sibling for a new per-function
  projection-placeholder table.
- Test fixtures: `tests/integration/sources/evaluator/aspects/71_associated_type_basic.mtl`
  and `.../typechecking/aspects/stage13_01_projection_in_return_type.mtl` both have
  explicit header comments stating they only lock in parse/construct-without-crashing
  behavior and that real resolution is #242's job. Neither exercises: a missing
  `type X = ...;` in an impl, a bound on the declaration, a bare-name aspect-signature
  reference actually being called through a bounded generic, an equality constraint,
  or ambiguity — all need new fixtures.
- No aspect in `stdlib/core.mtl` declares an associated type today (RFC-0063/0074/0080's
  `Alloc`/`Deref` aspects are not implemented yet) — this is greenfield, no stdlib
  migration risk.

## Design

### New error code

Add to `src/error/mod.rs`'s `TypeErrorCode` enum, immediately after `T0016`:
```rust
T0017, // Impl missing a required associated type definition
```
No other new codes — reuse T0013 (ambiguity) and T0012 (bound/equality-constraint
violation), per the reasoning above.

### Registry additions (`src/typeinference/mod.rs`)

Add to `TypeDefinitionRegistry`:
```rust
/// aspect name -> its declared associated-type members (name + optional bound), RFC-0082 §1.
aspect_assoc_type_decls: HashMap<String, Vec<AssocTypeDecl>>,

/// (target_type_id, aspect_name) -> assoc-type-name -> concrete Type, RFC-0082 §2.
/// Mirrors `impl_aspect_env`'s key shape (SymbolId-keyed target, name-keyed aspect).
/// Populated only for concrete (non-generic) impls.
impl_assoc_types: HashMap<(SymbolId, String), HashMap<String, Type>>,
```
New accessors mirroring the existing `aspect_method_defs`/`impl_aspect_env_has` shape:
`register_aspect_assoc_types`, `aspect_assoc_type_decls`, `register_impl_assoc_types`
(resolves target via the existing private `resolve_type_position_id`, same
no-op-on-unresolved convention as `register_aspect_impl`), `impl_assoc_type`.

Update `merge_from` with two more `entry().or_insert_with()` blocks for these new
maps, following the exact pattern already used for every other field. Update
`TypeDefinitionRegistry::new()` to initialize both to empty.

### `TypeScheme` additions (`src/typeinference/mod.rs`)

Two new parallel-per-quantified-var fields, following exactly the shape of
`bounds`/`neg_bounds`:
```rust
/// For a quantified var that is itself a symbolic associated-type projection
/// placeholder (RFC-0082 §3 abstract case), records which OTHER quantified var
/// (by POSITION in `quantified_vars`) is the base type, plus aspect/assoc names.
/// `None` for an ordinary quantified var. Same length as `quantified_vars`.
pub assoc_projections: Vec<Option<(usize, String, String)>>, // (base_var_position, aspect_name, assoc_name)

/// Equality constraints (RFC-0082 §4), carried per quantified var like `bounds`.
/// `expected_ty` may itself reference OTHER quantified vars of this scheme (the §3a
/// escape-hatch case, `Deref<Target = U>`), so it's `InferType`, not `Type`.
pub assoc_eq_constraints: Vec<Vec<(String, String, InferType)>>,
```
Update `TypeScheme::mono`/`generalize`/`generalize_with_names` to initialize both to
empty `Vec`s. Add `with_assoc_projections`/`with_assoc_eq_constraints` builders
mirroring `with_bounds`/`with_neg_bounds`.

**Why position-indexed, not `TypeVar`-indexed**: quantified vars get freshly renamed
at every call site. `assoc_projections` needs to relate two vars *to each other within
the same instantiation* (the projection var to its base var) — position is stable
across renaming, so storing base-by-position and looking up
`renaming[quantified_vars[base_position]]` at each call site is correct.

### `InferContext` additions (`src/typeinference/mod.rs`)

Add alongside `current_type_param_bounds`:
```rust
/// Memo + accumulator for symbolic associated-type projections minted while inferring
/// the CURRENT function/method body. Key: (base_tv, aspect_name, assoc_name) so the
/// same projection requested twice gets the same placeholder. Reset (swapped, like
/// current_type_param_bounds) on entry/exit of each function/method body.
current_assoc_projections: HashMap<(TypeVar, String, String), TypeVar>,
/// Flat log of everything minted above, in insertion order.
recorded_assoc_projections: Vec<(TypeVar, String, String, TypeVar)>, // (base, aspect, assoc, placeholder)
```
New methods: `swap_assoc_projections` (same swap-and-restore pattern as
`swap_type_param_bounds`), `fresh_assoc_projection_var(base_tv, aspect_name,
assoc_name) -> TypeVar` (memo lookup; on miss, mint via `fresh_var()`, insert into the
memo AND push onto `recorded_assoc_projections`), `take_recorded_assoc_projections`
(drain). Also add a read-only accessor exposing `current_type_param_bounds` (needed by
`ann_to_infer` in conversions.rs — it doesn't have one today).

Add a free function `collect_fun_assoc_eq_constraints(fun, generic_map, ctx) ->
HashMap<TypeVar, Vec<(String aspect, String assoc, InferType)>>` in `inference.rs`
(not a new `InferContext` field — read straight off `Bound.assoc_bindings` once per
`hoist_fun_decls`/`infer_fun_decl` call, exactly where `collect_fun_type_var_bounds`
runs today), converting each `(assoc_name, TypeExpr)` pair via
`type_expr_to_infer_with_generics(ty, generic_map)` so a sibling type param like `U`
in `T: Deref<Target = U>` stays a `TypeVar`, not a dangling `Named("U",[])`.

### Extend the lowering pass to cover body-level annotations

`lower_projections_in_program` must also lower `TypeExpr`s inside `Block`/`Stmt`/
`Expr`, not just `FunDecl` params/return type, per RFC-0082 §3's explicit
`let y: T::Target = x.deref();` example. Add a new recursive walker
`lower_projections_in_block(block, generics)` that rewrites `Decl::Let`/`Decl::Mut`'s
`type_ann`, recurses into `While`/`For`/`ForIn`'s nested blocks and every `Block`'s
`tail`/`stmts`, rewrites `Expr::Ascribe.ann`, `Expr::Cast.target_type`,
`Expr::Closure.{params[].type_ann, return_type}` (and recurses into the closure's own
body — Metel has no nested generic scopes, so thread the *same* flat `generics` set
through), and recurses into `If`/`Loop`/`Match` arms for nested `let`s. Wire into
`lower_projections_in_fun` so it also rewrites `fun.body`. **Do not** lower
`Decl::Aspect`'s `AspectMethod` bodies here (they use bare names, §1.2, not dotted
`Base::Assoc` syntax — handled separately).

### `conversions.rs`: real `Projection` resolution + §1.2 bare-name sugar

Replace the stub `Projection` arm in `type_expr_to_infer_in_context`. Add new optional
context as a new parameter (don't change the three existing public wrappers'
signatures — struct/enum-field registration etc. never reference `T::AssocType` per
RFC scope, keep them calling the existing wrappers unchanged):
```rust
pub(super) struct AssocResolveCtx<'a> {
    pub registry: &'a TypeDefinitionRegistry,
    pub current_module: &'a [String],
    pub type_param_bounds: &'a HashMap<TypeVar, Vec<String>>,
    /// Set only when converting an ASPECT's own method signature (§1.2's bare-name
    /// sugar): the aspect currently being processed, so `Item` alone resolves as `Self::Item`.
    pub current_aspect: Option<&'a str>,
}
```
New `Projection` arm handles the **concrete case only** (base resolves to `Self` with
a known target type, or a `Named` type with no entry in `generics` — already concrete
at this point in elaboration): resolve the aspect (known directly if `current_aspect`
is set; otherwise search which in-scope aspect declares this assoc-type name), look up
`registry.impl_assoc_type(current_module, base_type_name, aspect_name, assoc_name)` ->
found: `type_to_infer(concrete_ty)`; not found: fall back to a fresh unconstrained var
(defensive — §2's completeness check is the real guard, this shouldn't be reachable
for well-formed programs). If `assoc_ctx` is `None` (call site didn't pass it), keep
today's stub behavior — preserves back-compat for every other call site.

Bare-name sugar (§1.2): before the `Named` arm's existing `Self`/generics lookups —
when `args.is_empty()` and `assoc_ctx.current_aspect` is `Some(aspect)` and that
aspect declares an assoc type named `name`, treat it as `Projection{base: Self,
assoc_name: name}` and recurse into the same resolution.

**The abstract/symbolic case (base is a real TypeVar) is deliberately NOT handled
inside `type_expr_to_infer_in_context`** — it needs `&mut InferContext` to mint a
placeholder, which this function doesn't have and shouldn't gain (keep it a pure,
testable conversion). Instead, call sites in `inference.rs` that own a live `&mut ctx`
must special-case `TypeExpr::Projection` and the bare-name-sugar shape **before**
delegating to `type_expr_to_infer_with_generics`/`ann_to_infer`, calling
`ctx.fresh_assoc_projection_var(base_tv, aspect_name, assoc_name)` directly when
`base` resolves to an in-scope `TypeVar`.

### `inference.rs`: wiring the symbolic case into signature + body conversion

In `infer_fun_decl` and `infer_impl_method`, the `te_to_infer` closures currently call
`type_expr_to_infer_with_generics(_and_self)` directly. Change them to first
pattern-match `te` for `TypeExpr::Projection{base, assoc_name, ..}` (and, when
`current_aspect` is known, the bare-name-sugar shape too):

- If `base` is `TypeExpr::Named(n,_)` and `generic_map.get(n)` is `Some(&base_tv)`:
  **abstract case**. Determine the aspect: look up `base_tv`'s bound aspect list
  (already computed a few lines earlier in both functions) and filter for aspects
  whose declared assoc types include `assoc_name`. Zero matches -> error ("no
  associated type `assoc_name` in scope for `T`" — RFC §3's own stated error case,
  T0003 is a reasonable reuse). More than one match -> **T0013** (the §3a hard error,
  message naming both aspects). Exactly one match ->
  `ctx.fresh_assoc_projection_var(base_tv, aspect_name, assoc_name)` ->
  `InferType::Var(placeholder)`.
- Otherwise (base already concrete): delegate to `type_expr_to_infer_in_context`
  **with** the new `AssocResolveCtx` populated from the current registry/module/bounds
  map/`ib.aspect_name.as_deref()` (for `infer_impl_method`) or `None` (for
  `infer_fun_decl`).

Apply identical special-casing inside `ann_to_infer` so `let`/`mut`/`Ascribe`
annotations in a generic body (now reachable as lowered `Projection` nodes) resolve
the same way.

**§1.2 bare-name sugar for the aspect-method "slow path"**: apply the same
special-casing to the bounded-`TypeVar` method dispatch's return/param-type
conversions — add a branch before the fallback: if the type expr is
`TypeExpr::Named(name, [])` and the current aspect declares an assoc type named
`name`, resolve via `ctx.fresh_assoc_projection_var(*tv, aspect_name, name)` instead of
the wrong fallback. This is the fix that makes `fun next(self: &mut Self) ->
Perhaps<Item>` actually type-correct for a caller of `T::next()` inside a
still-generic function body.

After both `infer_fun_decl` and `infer_impl_method` finish body inference and call
`ctx.solve()`, drain `ctx.take_recorded_assoc_projections()` and, parallel to the
existing `bounds`/`name_map` post-solve remapping, build the scheme's
`assoc_projections: Vec<Option<(usize,String,String)>>`: for each recorded
`(base_tv, aspect, assoc, placeholder_tv)`, resolve both through the final
substitution; if both are still `InferType::Var` (genuinely still generic), find their
**positions** in the final `quantified_vars` list and record
`Some((base_position, aspect, assoc))` at the placeholder's position; entries that
resolved to concrete drop out naturally. Do the same remapping for
`assoc_eq_constraints`.

### Registry additions for §1.2's concrete-impl case (`registry.rs`)

`register_aspect_decl` must also call `registry.register_aspect_assoc_types(ad.name.clone(),
ad.assoc_types.clone())`.

In the Pass 2 impl loop's non-generic-target branch (gated
`ib.polarity == Polarity::Positive`, matching the existing `register_aspect_impl`
call): for each `AssocTypeDef` in `ib.assoc_type_defs`, convert `def.ty` via
`type_expr_to_infer_with_self(&def.ty, &target_name)` -> `infer_type_to_type`,
accumulate into a `HashMap<String, Type>`, call
`registry.register_impl_assoc_types(current_module_path, &target_name, aspect_name,
bindings)`. Skip silently on conversion failure (matches this registry's established
graceful-degradation convention).

`register_default_aspect_method`/`register_default_aspect_methods` and
`register_impl_methods` need their signature-conversion calls upgraded the same way,
but for the **concrete** case only (registry-build time, before Pass 1, fixed
`target_name` string, no TypeVars involved): use `type_expr_to_infer_in_context` with
`AssocResolveCtx{ current_aspect: Some(aspect_name), .. }` so a default-bodied aspect
method's bare `Item` reference resolves to the impl's concrete binding directly.

### §2 completeness + §1.1 bound enforcement (`inference.rs`, `Decl::Impl` arm)

Immediately after the existing method-completeness loop, inside the same
`if ib.polarity == Polarity::Positive { if let Some(aspect_name) = &ib.aspect_name {
... } }` block, add: for each associated-type decl the aspect declares, check the impl
provides a matching `AssocTypeDef` (missing -> **T0017**); if the decl has a bound,
convert the impl's concrete binding and check it satisfies the bound via
`impl_aspect_env_has` (violated -> **T0012**). This runs at Pass 1 time, after
`build_registry` has already populated `impl_aspect_env` for the whole module, so
lookups here see every impl processed so far in the module (cross-module lookups work
via the registry merge, same as every other check).

### §4 equality-constraint enforcement (`construction.rs`)

Mirror `check_fun_call_bounds`/`check_scheme_bounds` with two new functions,
`check_fun_call_assoc_eq`/`check_scheme_assoc_eq`, called from the exact same four
call sites as `check_fun_call_bounds` today. For each quantified var with non-empty
`assoc_eq_constraints`, once resolved to a concrete type, look up the type's actual
associated-type binding via `registry.impl_assoc_type` and compare against the
constraint's expected type (substituting any OTHER quantified vars of this scheme that
`expected_infer` references, via `var_to_type`, the same renaming-proof lookup as
`assoc_projections`). **If `expected` resolves to a still-free var** (the §3a
escape-hatch case, `U` unconstrained at this call site), skip the comparison — let
ordinary unification elsewhere in the call pin `U` down; only compare when fully
concrete. Mismatch -> **T0012**.

### `instantiate_scheme_for_call` / `_with_turbofish` / `_with_expected_ret` backfill (`construction.rs`)

**This is the load-bearing fix** — without it, any function returning `T::AssocType`
(the RFC's own headline example) fails instantiation with a bogus T0002, since these
three functions call `infer_type_to_type` on the final substituted return/param types,
which errors on any leftover `InferType::Var`. In each of the three functions, after
the arg-unification/turbofish/expected-ret substitution is fully built but **before**
the final `infer_type_to_type` calls: for each `Some((base_pos, aspect, assoc))` in
`scheme.assoc_projections`, resolve the base var (via the renaming map) to a concrete
type, look up `registry.impl_assoc_type(current_module, base_name, aspect, assoc)`,
and if found, extend the substitution to bind the projection's own renamed var to that
concrete type before the final conversion. Requires threading `registry:
&TypeDefinitionRegistry` and `current_module: &[String]` into all three functions (add
as new trailing parameters; both already available at every call site).

## Order of implementation (each step independently buildable + testable — commit
after each)

1. Add `T0017` (`error/mod.rs`). Build + test (no behavior change).
2. Add registry storage (`aspect_assoc_type_decls`/`impl_assoc_types` + accessors +
   `merge_from` entries) and wire `register_aspect_decl`/impl-registration (assoc-type
   registration only, not yet the bare-name conversion upgrade). Build + test — still
   no user-visible change (nothing reads these yet).
3. Implement §2 + §1.1 (`inference.rs` `Decl::Impl` arm). Add fixtures: impl missing
   an assoc type -> T0017; assoc type's concrete binding failing its declared bound ->
   T0012. Fully testable in isolation, no dependency on projection resolution.
4. Implement the **concrete** `Projection`/bare-name resolution path in
   `conversions.rs`, plus registry.rs's bare-name upgrade for default-aspect-method
   registration. Test: a default-bodied aspect method referencing its own bare
   associated-type name, dispatched on a concrete impl.
5. Implement `InferContext`'s projection-placeholder machinery and wire the
   abstract-case special-casing into `infer_fun_decl`/`infer_impl_method`/
   `ann_to_infer`/the bounded-TypeVar dispatch slow path, plus the body-level
   lowering-pass extension. Build and test with the existing
   `stage13_01_projection_in_return_type.mtl` fixture (should now produce a correct
   symbolic type instead of a stubbed `Named` string) plus new fixtures exercising
   `let y: T::Target = ...;` in a body and §3a ambiguity.
6. Implement `TypeScheme.assoc_projections` remapping after solve and the
   `instantiate_scheme_for_call`/`_turbofish`/`_expected_ret` backfill. **This is the
   step that makes calling a generic function returning `T::AssocType` actually work
   end-to-end** — test with a concrete call site called with a concrete impl,
   asserting the call's result really has the right type/runtime behavior.
7. Implement §4: `collect_fun_assoc_eq_constraints`, `TypeScheme.assoc_eq_constraints`,
   `check_fun_call_assoc_eq`/`check_scheme_assoc_eq`. Test with `Deref<Target =
   Node>`-shaped bounds, both a matching and a mismatching case.
8. Implement §3a's ambiguity error explicitly if not already fully covered by step 5 —
   add a dedicated fixture with two aspects both declaring the same assoc-type name,
   both bound on the same `T`, confirm T0013 fires with a message naming both aspects.
9. Full regression pass + clippy.

## Test fixtures to add

All under `tests/integration/sources/typechecking/aspects/` (typecheck-only, expect
specific error codes via inline `// ERROR[Txxxx]` comment markers, matching this
repo's existing convention — check an existing `stage*_neg_*` fixture in
`typechecking/generics/` for the exact marker format before writing these) and
`tests/integration/sources/evaluator/aspects/` (end-to-end, expect a runtime value),
following this repo's existing naming convention:

**Positive:**
- `stage13_02_impl_provides_all_assoc_types.mtl` — baseline: aspect with one assoc
  type + bound, impl providing it correctly, satisfies §1.1.
- `72_projection_call_site_resolution.mtl` (evaluator) — a generic function returning
  `T::Item` called with a concrete impl; asserts the returned value's runtime behavior
  is consistent with the concrete `Item` binding.
- `73_bare_name_sugar_in_default_method.mtl` (evaluator) — aspect method with a
  default body referencing bare `Item`, impl relying on the default, called through
  both a concrete receiver and a generic bound.
- `stage13_03_body_let_projection.mtl` — `let y: T::Target = x.deref();` inside a
  generic function body.
- `stage13_04_equality_constraint_pins_type.mtl` — `fun f<T: Deref<Target = Node>>(x:
  &T) -> &Node { x.deref() }`, concrete call, asserts success.
- `stage13_05_equality_constraint_fresh_var_escape_hatch.mtl` — the RFC's own §3a
  example: `fun f<T: Deref<Target = U> + Convert, U>(x: &T) -> U`.

**Negative (each asserting the specific error code):**
- `stage13_06_impl_missing_assoc_type.mtl` — impl omitting a required `type X =
  ...;` -> expect T0017.
- `stage13_07_assoc_type_bound_violation.mtl` — `type Item: Display;` bound to a type
  with no `Display` impl -> expect T0012.
- `stage13_08_projection_without_bound.mtl` — `T::Target` used where `T` has no
  `Deref`-like bound in scope -> expect a clear undefined-projection error (check
  which existing code fits best given RFC §3's stated error case).
- `stage13_09_ambiguous_projection.mtl` — the RFC's own §3a example verbatim (two
  aspects, same assoc-type name, both bound on `T`) -> expect T0013, message naming
  both aspects.
- `stage13_10_equality_constraint_mismatch.mtl` — `T: Deref<Target = Node>` but the
  actual impl's `Target = OtherType` -> expect T0012.

Update `71_associated_type_basic.mtl` and `stage13_01_projection_in_return_type.mtl`'s
header comments once this lands — they currently say "real resolution is issue #242's
job"; that sentence becomes stale.

## Documentation

Note: this repo (`metel-interpreter`) does not have the `docs` submodule checked out
in an isolated worktree (verify — if it's genuinely absent, skip doc updates outside
this repo and note it in your final report, exactly as was correctly done for issue
#243). Do NOT attempt to edit RFC frontmatter or lifecycle stage yourself — that
belongs to a separate tool in the separate `metel-docs` repo, out of scope for this
handoff.

Do update, inside THIS repo if such files exist here:
- `metel-interpreter/docs/typechecker.md`: document the new `assoc_projections`/
  `assoc_eq_constraints` scheme fields and the `impl_assoc_types` registry addition,
  since this changes both Pass 1 and Pass 2 in a way future contributors need to
  understand.
- Consider a short ADR in `metel-interpreter/docs/decisions/` for the position-indexed
  (not `TypeVar`-indexed) `assoc_projections` design — it's a non-obvious choice with
  a real alternative (a separate side-table) a future contributor could reasonably
  second-guess; write down why position-indexing survives call-site renaming and a
  side-table would not.

## Final verification checklist

1. `cargo build` (from the `metel-interpreter/` crate root) — zero warnings.
2. `cargo test --release` — full suite, zero failures, including every new fixture
   above and the full existing regression suite (typechecking/aspects,
   typechecking/generics, evaluator/aspects in full).
3. `cargo clippy --release --lib -- -W clippy::pedantic` — zero warnings, matching
   this repo's established zero-warnings baseline. Fix anything pedantic flags in new
   code rather than adding blanket allows (check existing precedent, e.g.
   `#[allow(clippy::implicit_hasher)]` on `HashMap`-taking public functions, before
   deciding whether a specific allow is warranted by precedent).
4. Manually re-check the `instantiate_scheme_for_call`/`_turbofish`/`_expected_ret`
   backfill ordering against a test where the projection appears in a *parameter*
   type, not just the return type, since the backfill must run before both
   conversions, not just the return one.
5. Confirm no change was made to `coherence.rs`, RFC-0036 conditional-impl paths, or
   anything `dyn`/object-safety related (this issue's explicit scope boundary) — a
   diff review against the starting commit should show no touches to files unrelated
   to associated types.
6. **Before claiming any test is "pre-existing and unrelated to this work," verify
   this by actually checking out the base commit (or using `git show <base>:<path>`)
   and running that exact test there — do NOT rely on `git stash` for this if any of
   your changes are already committed, since `git stash` only reverts UNCOMMITTED
   changes and will silently produce a false "confirmed pre-existing" result if the
   real cause is in an earlier commit of this same session's work.** This exact
   mistake happened during #243's implementation (an agent left a broken, abandoned
   test edit in place while incorrectly claiming via an invalid git-stash check that
   the failure was pre-existing) and was caught by independent review afterward — do
   not repeat it. If you touch any existing test to make it compile (e.g. adding a new
   required struct field to a test literal), verify you haven't ALSO changed its
   actual test logic/assertions in the process; a mechanical "add the new required
   field" edit should never change what the test is asserting.

## Your task

Implement the "Order of implementation" steps 1 through 9 in order, verifying each
step's own test expectations before moving to the next, committing after each step. Do
NOT push or open a PR. When done (or if you get stuck / find the plan's line-number
pointers have drifted from actual current source — treat them as approximate, not
exact; search the actual current code first), report back a clear summary: what was
implemented, full final test/clippy output, and any deviations from this plan or open
questions you flagged along the way — especially anything under the "Explicitly out of
scope" section that you found yourself needing to touch, any spot where the plan's
assumptions about existing code shape turned out to be wrong, and explicit
confirmation that you followed the verification checklist's item 6 (no false
"pre-existing failure" claims backed only by an invalid git-stash check).
