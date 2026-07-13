# Implementation plan: issue #240 — RFC-0037 return-position `impl Aspect`

Repo: `metel-interpreter` (an interpreter for the "Metel" language: parser (pest) ->
name resolver -> path normalizer -> coherence pass -> two-pass HM typechecker
(inference.rs Pass 1, construction.rs Pass 2) -> elaborator -> evaluator).

This plan was produced by a research agent that read the actual current source in
full (not just grep) before proposing anything. Follow it in order; each step should
build and its own fixtures should pass before moving to the next.

## Background

RFC-0037 ("Return-Position impl Aspect") was just integrated into the spec
(`internal/rfcs/3-integrated/rfc-0037-return-position-impl-aspect.md` in the
metel-docs repo). A function may declare `fun f(...) -> impl Aspect` instead of a
named return type. The caller sees an opaque type known only to satisfy `Aspect` --
no boxing, no vtable, no heap allocation, since the concrete type is fixed by the
function's own body (monomorphised, one concrete type per function definition, not
per call).

Key RFC rules:
- **§1.1**: the function body must produce the SAME concrete type on every code path
  (an `if`/`else` returning different concrete types on different branches is a type
  error).
- **§1.2 caller rights**: may call aspect methods, store/pass the value to anything
  accepting the same opaque type or aspect bound; may NOT name the concrete type,
  cast it, or call non-aspect methods.
- **§1.3 / §2 independence**: each `impl Aspect` occurrence (parameter or return) is
  an independent fresh type variable UNLESS the body's own dataflow unifies them
  (e.g. `fun transform(x: impl Display) -> impl Display { x }` unifies the two
  because the body literally returns `x`).
- **§3**: ownership/Copy/Drop apply to the concrete type normally -- not yet
  load-bearing since RFC-0071 ownership isn't implemented (everything is
  deep-cloned on bind today).
- **§4.1/§4.2**: single concrete type per function; the opaque type is not nameable
  by the caller.

## Research findings (already done -- do not re-derive, verify then build on this)

### Verified: today's behavior is worse than "best-effort," it's non-functional

`TypeExpr::ImplAspect { bound, source_spell, span }` (`src/ast/mod.rs:636-643`)
already parses in both parameter and return position via the general `type_expr`
grammar rule -- no grammar work needed.

`lower_impl_aspect` (`src/typechecker/inference.rs:3012-3064`) desugars a
PARAMETER-position `impl Aspect` into a fresh `GenericParam` + `Bound`, rewriting the
param's type annotation. **It only rewrites `fun.params` -- `fun.return_type` passes
through completely untouched.**

`infer_fun_decl`'s return-type conversion (`inference.rs:728-732`) calls
`te_to_infer`, which bottoms out in `type_expr_to_infer_in_context`
(`conversions.rs:124-126`):
```rust
TypeExpr::ImplAspect { bound, .. } => {
    type_expr_to_infer_in_context(bound, generics, self_ty_name, assoc_ctx)
}
```
`bound` is the aspect name itself (`TypeExpr::Named("Display", [])`), so `impl
Display` in return position is converted to **`InferType::Named("Display", [])`** --
literally treated as if the user wrote a concrete nominal type named `Display`. The
body's actual return value then fails to unify against this for any real body.
**`fun make_pair() -> impl Display { 42 }` fails to typecheck TODAY with T0001**
("cannot unify i64 with Display") -- this breaks the RFC's own primary examples, not
just edge cases.

No existing test fixture exercises return position at all (`grep -rn -- "-> impl "
tests/` returns nothing).

### Error codes

T0001-T0017 are taken. **T0018 is free.** An ordinary `if`/`else` with mismatched
branch types against a declared NAMED return type already produces plain T0001 via
`constrain_with_read_copy` -> `unify()` -- confirmed by tracing `Expr::If`'s handling
(`inference.rs:1561-1578`). **§1.1's divergent-branch rejection reuses T0001 for
free -- no new code needed for that part.** T0018 should be reserved for the
genuinely NEW failure class this issue introduces: a caller attempting to
name/observe the concrete type of an opaque return value (a different failure class
from an ordinary type mismatch, so it earns its own code per this session's
established "reuse existing codes only when it's the same failure class"
convention).

### RFC-0008 (`dyn Aspect`) overlap

Confirmed zero overlap -- `dyn Aspect`/vtable/aspect-object machinery doesn't exist
anywhere in the codebase, matching RFC-0037 §4.1's own text ("not dynamic
dispatch"). Nothing to scope in.

### The #242 bug-class precedent, and where it applies here

`FunGeneralization` (`typechecker/mod.rs:74-94`) and the scheme rebuild
(`mod.rs:806-812`) is exactly the site issue #242's fix had to thread
`assoc_projections`/`assoc_eq` through, because a function's LOCAL `bind_poly` call
and the CROSS-MODULE-EXPORTED scheme built at this rebuild site are two SEPARATE
`TypeScheme` values built from the same `FunGeneralization` data -- attaching new
metadata to only one silently breaks cross-module calls. **This exact risk applies
identically to whatever opaque-return identity this issue adds** -- it must be a new
field on `FunGeneralization`, threaded through both the local `ctx.bind_poly` call
(`inference.rs:803`) and this re-generalization site, mirroring
`.with_assoc_projections(...)`.

There is a SECOND, related landmine already present: `refresh_scheme_for_export`
(`typechecker/mod.rs:177-200`) **unconditionally zeroes `assoc_projections`/
`assoc_eq_constraints` on export** (`assoc_projections: vec![]`, line 196).
**Whatever new opaque-return field is added must NOT be dropped there**, or
cross-module calls to an opaque-returning `pub fun` will silently lose the
aspect-bound metadata exactly the way #242 lost projections before its own fix. This
is a concrete, must-fix site -- flagged explicitly so it isn't missed.

### How return-type-from-body inference already works (the "for free" part)

`infer_fun_decl` unifies the body's tail type against `ret_ty` inside the defining
function's OWN inference, before generalization (`inference.rs:748-750`). **If a
return-position `impl Aspect` occurrence is represented as a plain `ctx.fresh_var()`
(identical to the existing "no annotation" fallback), ordinary unification already
forces it to collapse to whatever concrete type the body produces, or fails with
T0001 if branches disagree.** This is genuinely free -- no bespoke divergent-branch
detection is needed.

What is NOT free: stopping that collapse from leaking the concrete type name into
the exported scheme, and stopping a caller from naming it (see Design §2c below).

### Instantiation/call-site mechanics

`instantiate_scheme_for_call`/`_with_turbofish`/`_with_expected_ret`
(`construction.rs:3241-3412`) all call `infer_type_to_type(&subst.apply(&ret),
span)?` to get a concrete `Type` for the call's return -- **this errors T0002 if
that slot is still an unresolved `Var`**. All three already contain an "RFC-0082
backfill" block that special-cases one quantified-var position using side-channel
metadata carried on the scheme (`scheme.assoc_projections`) rather than requiring
ordinary substitution to have resolved it -- **this is the exact precedent to reuse
for opaque returns**.

Also critical: Pass 1's own identifier/call-name resolution (`InferContext::lookup`)
uses the plain `instantiate()`, which discards `bounds`/`assoc_projections`/
`assoc_eq_constraints` entirely -- bound-satisfaction checking (T0012) is NOT done in
Pass 1 at all, only in `construction.rs` using the fully-solved `var_to_type` map.
**This means nothing today ever needs an abstractly-bound-typed value to remain
method-callable OUTSIDE its own defining function** -- parameter-position `impl
Aspect`/generic bounds are only ever "live" for method dispatch inside the one
function that declares them (`current_type_param_bounds`, swapped wholesale at
function entry/exit, never touched at a call site). **Return-position `impl Aspect`
is the FIRST feature where an abstractly-typed value escapes into a CALLER's scope
and must still support aspect-method dispatch there** -- this is genuinely new
machinery.

Confirmed the payoff, though: `Expr::MethodCall`'s "slow path" (`inference.rs:
1820-1907`) already does exactly what's needed for ANY `InferType::Var` receiver --
`ctx.bounds_for_type_var(*tv)` -- with zero awareness of WHY that var is bound. It
looks up aspect method defs and dispatches purely from the bound list. The "fast
path" (concrete named type) is skipped entirely for a bare `Var` receiver, which is
WHY "cannot call non-aspect methods" (§1.2) also falls out for free once the
receiver's static type is kept as an unresolved bound `Var`. Casting is rejected for
free too (`Expr::Cast` requires a `Concrete`/`Named` source; a bare `Var` fails,
producing T0007 naturally).

**What is NOT free**: preventing a caller from writing the correct concrete type as
an explicit annotation (`let x: Label = f();`) or passing the value to a
non-generic parameter declared with the exact concrete type. Both funnel through
`constrain_with_read_copy`/`ctx.add_constraint`, solved by plain `unify()`'s
`(Var(v), _) => bind_var(*v, b)` -- which ALWAYS succeeds. Nothing stops this today,
and nothing has ever needed to (this is the first feature where a `Var`'s concrete
resolution must be HIDDEN, not just left flexible). This is the one genuinely
bespoke piece of engineering this issue requires -- see Design §2c.

### Body construction / evaluator mechanics -- a real design correction

Functions whose scheme has non-empty `quantified_vars` are stored as
`FunBody::Generic` and reconstructed LAZILY AT RUNTIME, per call, using RUNTIME
ARGUMENT/RECEIVER VALUES to recover concrete type args (ADR-0043's whole subject). A
zero-argument, no-receiver function like `make_pair()` has NOTHING for that
runtime-recovery mechanism to grab onto. **Treating an opaque-return function as
"generic" in the ordinary `FunBody::Generic` sense would silently break exactly this
case.** The correct design (below) recognizes that an opaque-return function is not
actually polymorphic in the traditional sense -- its return type is fixed once, at
definition time -- so its body should be built EAGERLY (`FunBody::Typed`), exactly
like an ordinary monomorphic function, using the concrete type recorded during Pass
1.

## Design

### §2a. What "opaque identity" needs to be -- two cases requiring opposite treatment

**Linked case** (`fun transform(x: impl Display) -> impl Display { x }`): if the
return-position occurrence is an ordinary `ctx.fresh_var()`,
`constrain_with_read_copy` unifies it with `body_ty` = the parameter's own
already-bound generic var (from the EXISTING `lower_impl_aspect` pass). The two vars
simply become one; since the shared var never resolves to concrete within
`transform`'s own body, it survives the local solve as a genuine free variable.
`generalize()` quantifies it once; instantiation at each call site gives both
positions the SAME fresh copy. **This is correct and needs zero new machinery** --
and it's correct for the caller to be able to name the concrete type here, since the
callee's body returns the same value the caller handed in; there's no internal
choice being hidden.

**Unlinked case** (`make_pair`, `make_adder`): nothing else in the signature
constrains the return-position var, so ordinary unification collapses it to a fully
concrete type DURING `infer_fun_decl`'s own local solve, before generalization ever
runs. **This is the case that needs new opacity machinery**, because here the callee
genuinely hides an internal choice.

**Discriminator (simple, mechanical)**: after `infer_fun_decl`'s own local
`ctx.solve()`, check whether the return-position marker var is still a free `Var` in
`resolved_ty` (linked case -> leave alone) or has collapsed to `Concrete`/`Named`
(unlinked case -> needs opacity handling below).

### §2b. Mechanism for the unlinked case

1. **New `TypeScheme` field** (`src/typeinference/mod.rs`, near `assoc_projections`):
   ```rust
   /// Per-quantified-var opaque-return metadata (RFC-0037). Some((aspect_name,
   /// concrete_ty)) means the i-th quantified var is a return-position `impl Aspect`
   /// occurrence whose concrete type is fixed by the function's own body (not chosen
   /// per call, unlike an ordinary generic). The caller never sees concrete_ty
   /// directly -- used only to (a) verify the aspect bound once at definition time,
   /// (b) let construction build a concrete Type for the call expression and the
   /// function's own eagerly-built body.
   pub opaque_returns: Vec<Option<(String, Type)>>,
   ```
   Default to `vec![]` in `TypeScheme::mono`/`generalize`/`generalize_with_names`.
   Add `.with_opaque_returns(&HashMap<TypeVar, (String, Type)>)` mirroring
   `.with_bounds`.

2. **In `infer_fun_decl`** (`inference.rs`, ~lines 616-871), when `fun.return_type`
   contains `TypeExpr::ImplAspect { bound, .. }` (top-level OR nested in
   `Tuple`/`Array` -- recurse the whole `TypeExpr` tree, don't just match a top-level
   node, per §2e below):
   - Extract the aspect name from `bound` (expect `TypeExpr::Named(aspect_name, _)`
     -- note: `collect_fun_type_var_bounds` already drops any type args like
     `Callable<i64,i64>`'s `<i64,i64>` for PARAMETER-position bounds too; this is a
     pre-existing limitation, not something to fix here -- keep test fixtures to
     simple no-arg aspects like `Display`).
   - Set `ret_ty = ctx.fresh_var()` (identical to the existing no-annotation
     fallback) and record `(ret_ty's TypeVar, aspect_name)` in a local
     `pending_opaque_returns: Vec<(TypeVar, String)>`.
   - Proceed with body inference exactly as today.
   - AFTER the function's own local solve (`let solved = ctx.solve()?; let
     partial_subst = ctx.default_literal_vars(&solved); let resolved_ty =
     partial_subst.apply(&fun_ty);`), for each pending `(marker_tv, aspect_name)`:
     apply `partial_subst` to `InferType::Var(marker_tv)`.
     - Still `Var(_)` (linked case): do nothing further -- ordinary
       `generalize`/`bind_poly` handles it.
     - Resolved to `Concrete`/`Named` (unlinked case): convert to a concrete `Type`
       (`infer_type_to_type`), then:
       a. **Verify the bound** by reusing the same `impl_aspect_env_has` check
          `check_type_satisfies_bounds` already does (factor a small shared helper
          into `typeinference/mod.rs` callable from both `inference.rs` and
          `construction.rs` if convenient) -- this check happens HERE, at
          definition time, since it's a property of the declaration, not a
          specific call. Emit **T0012** on failure (reused code, same failure class
          as an ordinary unsatisfied bound, just checked at a different point).
       b. **Re-abstract**: replace this concrete occurrence in `resolved_ty` with a
          FRESH placeholder `TypeVar` (so it becomes a genuine free var again, gets
          quantified by `generalize()`, gets a fresh instantiation at every call
          site -- exactly like an ordinary generic param). Record `(placeholder_tv,
          aspect_name, concrete_ty)`.
   - Thread the recorded list into `FunGeneralization.opaque_returns:
     HashMap<TypeVar, (String, Type)>`, pushed at BOTH the local `ctx.bind_poly`
     call (~line 803) AND the `fun_generalizations.push(...)` (~lines 860-869) --
     matching the #242 precedent so it survives to both the module-local poly env
     and the cross-module-exported scheme.

3. **In `typechecker/mod.rs`**:
   - Add `.with_opaque_returns(&fg.opaque_returns)` to the scheme rebuild
     (~lines 806-812).
   - **Fix `refresh_scheme_for_export`** (~lines 177-200) to thread `opaque_returns`
     through the renaming (map each `(orig_tv, (aspect, ty))` through `renaming` the
     same way `quantified_vars` are remapped), rather than dropping it the way
     `assoc_projections`/`assoc_eq_constraints` were unconditionally zeroed before
     their own fix. **This is the single most important "don't repeat #242" fix in
     this whole plan** -- untested, it will silently work in same-module fixtures
     and silently break cross-module ones.

4. **In construction.rs's three instantiate functions**: add a backfill block
   structurally identical to the existing "RFC-0082 backfill" blocks, but for
   `opaque_returns`: for each `(orig_tv, (_aspect, concrete_ty))` in
   `scheme.opaque_returns`, find the renamed fresh var (via the `renaming` map
   `instantiate_with_renaming` already produces) and `subst.bind(fresh_var,
   InferType::Concrete(concrete_ty.clone()))` BEFORE the final `infer_type_to_type`
   call. This makes the existing call succeed using the KNOWN concrete type rather
   than requiring ordinary substitution to have resolved it.

5. **In `construct_fun_decl`** (construction.rs, the `scheme.quantified_vars.is_empty()`
   branch): extend the eager-construction condition to ALSO fire when every
   quantified var is accounted for by `scheme.opaque_returns` (the function isn't
   actually polymorphic in the traditional sense -- see the body-construction
   finding above). Build a substitution binding each `opaque_returns` var to its
   recorded concrete `Type`, apply to `scheme.ty`, treat the result as if
   `quantified_vars` were empty (reusing the existing eager-build path). A function
   mixing real generics AND an opaque return (rare, not in the RFC's examples) still
   falls into `FunBody::Generic` as before -- not worth optimizing further here.

### §2c. The one genuinely bespoke guard: preventing the caller from naming the concrete type

Because §2b intentionally leaves the call-site's instantiated var UNBOUND (so method
dispatch/storage/passing-to-aspect-bound-positions route through the existing `Var`
"slow path" for free), ordinary `unify()` permissiveness means nothing stops that
var from being bound to an explicitly-named concrete type via a `let`/`mut`
annotation, `Expr::Ascribe`, or passing it as an argument to a concretely-typed
parameter. All of these funnel through two choke points: `constrain_with_read_copy`
-> `ctx.add_constraint`, and `Expr::Call`'s ordinary argument constraint -- both
solved by `ctx.solve()` -> `apply_constraint_with_coercion`.

Rather than scattered per-call-site checks (fragile -- easy to miss a leak path),
add ONE centralized, post-hoc validation, mirroring the EXISTING
`validate_literal_bindings` pattern (`typeinference/mod.rs:595-628`, already invoked
from `apply_constraint_with_coercion` right after `subst.compose_in_place`):

1. **New `InferContext` field**: `opaque_return_vars: HashSet<TypeVar>` --
   deliberately SEPARATE from `current_type_param_bounds` (do not reuse those keys;
   `current_type_param_bounds` also holds ordinary parameter-position bound vars
   during their own defining function's body-check, and retroactively restricting
   THOSE too would be an unrelated, untested behavior change to already-shipped
   generic-bound semantics).
2. **Populate at the call site** (§2d below).
3. **New `validate_opaque_return_bindings(subst, opaque_return_vars, span) ->
   Result<(), MetelError>`**, structurally identical to `validate_literal_bindings`:
   for each tracked var, if `subst.apply(&InferType::Var(var))` resolves to
   anything OTHER than `InferType::Var(_)`, return **T0018** ("cannot name the
   concrete type of an opaque `impl Aspect` return value; use `impl Aspect` or a
   generic bound instead").
4. Wire into `apply_constraint_with_coercion` (thread `opaque_return_vars: &HashSet<TypeVar>`
   through as one more parameter, exactly like the literal-var sets) and into
   `InferContext::solve()` (already passes literal-var sets through -- add the new
   set the same way).

This single choke point catches every leak path uniformly (annotations, ascription,
non-generic argument passing, struct-literal field assignment, array element
typing) with one new function, without touching `unify()`/`bind_var` and without
needing to enumerate every call site by hand.

**This decision deserves its own ADR** (next available: `adr-0044`) documenting the
central-validation choice vs. scattered per-site guards vs. a deeper `unify()`-engine
change, and the linked/unlinked discriminator from §2a -- following the same
discipline as ADR-0043.

### §2d. Wiring the aspect bound into `current_type_param_bounds` at the call site

New, additive `InferContext` method:
```rust
pub fn register_type_var_bound(&mut self, tv: TypeVar, aspect: String) {
    self.current_type_param_bounds.entry(tv).or_default().push(aspect);
}
```
(insert, not swap -- leaves everything else untouched, and naturally goes out of
scope for the caller the same way the rest of the map already does whenever the
caller's own function boundary is crossed via `swap_type_param_bounds`).

In `Expr::Call`'s inference (`inference.rs:1429-1543`): when `callee` is a bare
`Expr::Ident(name)`/`Expr::ResolvedPath` naming a poly-bound function with non-empty
`opaque_returns`, do a DEDICATED instantiation instead of falling through to the
generic arm: call `instantiate_with_renaming(&scheme, gen)` (already exists) to get
`(instantiated_ty, renaming)`; for each `(orig_tv, (aspect, _))` in
`scheme.opaque_returns`, look up `renaming[&orig_tv]` and call BOTH
`ctx.register_type_var_bound(fresh_tv, aspect.clone())` (for method dispatch) AND
insert `fresh_tv` into `ctx.opaque_return_vars` (for the naming guard). Use
`instantiated_ty` as `callee_ty` for the rest of `Expr::Call`'s existing logic,
unchanged. Add a small read-only `ctx.poly_scheme(name) -> Option<TypeScheme>`
accessor to find the scheme without instantiating (mirrors `ctx.lookup`'s internal
search).

**Scope note**: only direct-by-name calls (`f(...)`) are covered, matching every
existing `impl Aspect` fixture's convention. Calling through a stored function value
(`let g = make_pair; g()`) won't get bound/opacity tracking -- flag as a known,
explicitly out-of-scope limitation (no fixture in the corpus exercises this pattern
for ANY generic function today either, so this is consistent with existing
coverage).

**Known, worth-documenting limitation**: a call to an opaque-returning function that
appears in the source BEFORE that function's own declaration sees only the hoisted
mono placeholder (no `opaque_returns` metadata yet) -- chaining an aspect-method
call directly off such a forward-referenced call may not get bound-registered
correctly. Pre-existing general characteristic of the hoist-then-infer pipeline, not
unique to this feature -- not this issue's job to fix. Declare opaque-returning
helper functions before their callers, matching every existing fixture's convention.

### §2e. Does independence (§1.3/§2) need anything extra?

No. `fun pair() -> (impl Display, impl Display) { (42, "hello") }`: each occurrence
inside the `Tuple` return-type gets its own independently-lowered `ctx.fresh_var()`
(the lowering in §2b must recurse into `TypeExpr::Tuple`/`TypeExpr::Array`/etc.
wherever `ImplAspect` can nest, not just match a top-level node) -- two separate
marker vars, two separate `opaque_returns` entries, independently instantiated at
each call site. Falls out with no special-casing, provided the lowering walks the
whole `TypeExpr` tree.

## Explicitly out of scope

- **RFC-0008** (`dyn Aspect`/aspect objects) -- zero consumer, Phase 4, not touched.
- **RFC-0037 §5's own three deferred items**: named linkage syntax (`impl(x)
  Display`), `impl Aspect` in struct fields (RFC-0038), multiple aspect bounds in
  return position (`impl Aspect + Other`). Do not implement any of these -- the
  design deliberately doesn't need "same opaque identity across two calls" to be
  provable, since there's no syntax requiring two occurrences to share an identity
  without named linkage; a fresh-per-call-site marker var (ordinary generic
  instantiation) is fully sufficient for every observable behavior the RFC actually
  specifies.
- **RFC-0071 ownership/Copy/Drop** -- not implemented at all; §3's text stays
  aspirational, no new work beyond today's deep-clone-on-bind semantics.
- **Aspect bound type-argument checking** (e.g. verifying `impl Callable<i64,i64>`'s
  `<i64,i64>` actually matches) -- confirmed pre-existing gap in parameter-position
  `impl Aspect` too; not this issue's job. Keep test fixtures to simple no-arg
  aspects.
- Calling an opaque-returning function through an indirect/stored function value --
  bound/opacity tracking not guaranteed; document as a known limitation, do not
  attempt to fix.

## Order of implementation (each step independently buildable + testable -- commit
after each)

1. **Add `TypeScheme.opaque_returns` field + `FunGeneralization` plumbing.**
   `typeinference/mod.rs`: add the field, default to `vec![]` everywhere a
   `TypeScheme` is built, add `.with_opaque_returns(...)`. `typechecker/mod.rs`: add
   `opaque_returns: HashMap<TypeVar, (String, Type)>` to `FunGeneralization`, wire
   `.with_opaque_returns(&fg.opaque_returns)` into the scheme rebuild, update every
   other `FunGeneralization { .. }` literal (grep for all of them) to include
   `opaque_returns: HashMap::new()`. **Fix `refresh_scheme_for_export`** to thread
   `opaque_returns` through `renaming` instead of dropping it. No behavior change
   yet (field always empty) -- `cargo build && cargo test` must be green.

2. **Lower return-position `impl Aspect` into a fresh marker var + definition-time
   bound check.** `inference.rs`'s `infer_fun_decl`: replace the flat return-type
   conversion with logic that recurses into `TypeExpr` finding `ImplAspect` nodes
   (top-level and nested), producing marker vars; after the local solve, check
   each marker, verify the bound (T0012 on failure), re-abstract into a fresh
   placeholder, record into `FunGeneralization.opaque_returns` at both push sites.
   Add `T0018` to `error/mod.rs` (reserved now, raised in step 4). Test: a
   same-module fixture with no method call yet should typecheck without error
   (proves the pipeline doesn't crash and doesn't wrongly reject).

3. **Backfill opaque-return concrete types in Pass 2 instantiate + eager body
   construction.** Add the "RFC-0037 backfill" block (mirroring the existing
   "RFC-0082 backfill" blocks) to all three `instantiate_scheme_for_call`/
   `_with_turbofish`/`_with_expected_ret` functions. Extend `construct_fun_decl`'s
   eager-vs-generic condition to also eager-build when every quantified var is
   accounted for by `opaque_returns`. Test: same fixture, plus one that calls an
   aspect method on the returned value -- expect this to still fail until step 4
   (confirms you're at the right boundary; don't skip ahead).

4. **Register aspect bound + opacity guard at opaque-return call sites.**
   `typeinference/mod.rs`: add `opaque_return_vars: HashSet<TypeVar>` to
   `InferContext`, `register_type_var_bound`, `mark_opaque_return_var`,
   `validate_opaque_return_bindings` (mirroring `validate_literal_bindings`), thread
   through `apply_constraint_with_coercion` and `InferContext::solve()`.
   `inference.rs`'s `Expr::Call`: add the dedicated-instantiation path for
   opaque-returning callees, registering both the bound and the opacity guard on
   the fresh renamed var. Add the small `ctx.poly_scheme(name)` accessor. Test: the
   method-dispatch fixture from step 3 should now pass; add negative fixtures for
   naming/casting.

5. **Test fixtures** -- see full list below.

6. **Clippy + full test pass.**

7. **Docs**: update `public/reference/spec/declarations.md`'s return-position
   section if anything drifted from the integration text, changelog entry, `rfc.py
   transition rfc-0037 --to implemented` (do this yourself only if you have access
   to the metel-docs repo from this worktree; otherwise note it in your final
   report for the orchestrating session to handle), an ADR for §2c's design
   decision (`docs/decisions/adr-00xx-...md`, check the next free ADR number
   yourself rather than assuming), `metel-interpreter/docs/typechecker.md` update.

## Test fixtures to add

Follow the existing `tests/integration/sources/typechecking/generics/stage12_*`/
`stage14_*` naming convention; use the next free `stageNN_*` prefix (check what's
free at implementation time, since issue #241 may also be adding fixtures to this
same directory in parallel -- coordinate by checking `ls` immediately before
picking a number, and note in your final report exactly which prefix you used, in
case of a collision with the parallel issue #241 work when both get merged).

**Positive:**
- `..._return_impl_aspect_basic.mtl` -- `fun make_pair() -> impl Display { 42 }`
  called with no method call yet, typechecks.
- `..._return_impl_aspect_method_dispatch.mtl` -- an aspect + implementing struct +
  a function returning `impl Aspect` that constructs/returns it + a caller calling
  the aspect method on the result. Exercises the full call-site bound-registration
  path.
- `..._return_impl_aspect_two_calls_independent.mtl` -- two separate calls to the
  same opaque-returning function, each passed onward to something taking `impl
  Aspect`/`T: Aspect`, proving independent instantiation.
- `..._return_impl_aspect_linked_to_param.mtl` -- the RFC §2 `transform` example
  verbatim, including a caller doing `let y: Concrete = transform(x);` to prove the
  LINKED case correctly allows naming.
- `..._return_impl_aspect_pass_to_bound_param.mtl` -- pass the opaque return value
  into a function declared with an `impl Aspect`/`T: Aspect` parameter.
- `..._return_impl_aspect_tuple_independent.mtl` -- RFC §1.3's `pair() -> (impl
  Display, impl Display)` example, both positions used independently.
- An evaluator fixture (`tests/integration/sources/evaluator/functions/` or
  `.../aspects/`) proving RUNTIME correctness end-to-end -- the only way to confirm
  the eager `FunBody::Typed` construction path (step 3) actually executes
  correctly, not just typechecks.

**Negative (each asserting the specific error code):**
- `..._return_impl_aspect_divergent_branches.mtl` -- RFC §1.1's `bad(flag)` example
  -> **T0001** (reused code, confirms the "for free" claim).
- `..._return_impl_aspect_bound_not_satisfied.mtl` -- body returns a concrete type
  that does NOT implement the declared aspect -> **T0012** (at the definition site).
- `..._return_impl_aspect_caller_cannot_name.mtl` -- `let x: Concrete = f();` where
  `Concrete` happens to be the actual concrete type -> **T0018**. This is the single
  most important negative fixture -- it's the one case that isn't free and would
  otherwise silently pass.
- `..._return_impl_aspect_cannot_cast.mtl` -- `f() as SomeType` -> **T0007** (reused
  code, confirms cast rejection is free).
- `..._return_impl_aspect_non_aspect_method.mtl` -- the concrete type has some
  OTHER inherent method not declared by the aspect; calling it on the opaque value
  -> **T0003** ("no method on type parameter"), reused code.
- `..._return_impl_aspect_non_generic_param.mtl` -- passing the opaque return value
  to a function whose parameter is declared with the exact concrete (non-generic)
  type -> **T0018** (same guard as the naming fixture, different leak path --
  proves the centralized validator, not a per-site check, is what's catching it).

**Cross-module** (matching this repo's existing module-semantics test directory
convention): a `pub fun` in one module returning `impl Aspect`, called from another
module, with an aspect-method call on the result. This specifically exercises the
`refresh_scheme_for_export` fix and must fail loudly (not silently produce wrong
types) if that fix is skipped.

## Final verification checklist

1. `cargo build` from `metel-interpreter/` -- zero warnings.
2. **Run the FULL `cargo test --release` -- not `--lib` and `--test integration` run
   separately.** Confirm both `Running tests/integration.rs` AND `Running
   tests/unit.rs` actually appear in the output. Issue #242's own prior automated
   implementation attempt silently skipped the separate `tests/unit.rs` target this
   way and shipped a genuinely broken commit as a result -- do not repeat this.
3. `cargo clippy --release --lib -- -W clippy::pedantic` -- zero new warnings.
4. Every new/changed `unify()`/`add_constraint` call: verify expected-vs-actual
   argument order and substitution composition direction is preserved.
5. Every new `infer_type_to_type` call in the three instantiate-family backfills:
   verify the opaque-returns binding happens BEFORE the call.
6. Confirm the cross-module fixture actually exercises the `refresh_scheme_for_export`
   fix -- do not consider the issue done without it passing.
7. Before claiming any pre-existing test "fails identically on the unmodified
   baseline," verify by actually checking out/testing the base commit directly --
   do NOT rely on `git stash` if any of your changes are already committed, since
   `git stash` only reverts uncommitted changes and will silently produce a false
   "confirmed pre-existing" result if the real cause is in an earlier commit of
   this same session's work. (This exact mistake happened during issue #243's
   implementation and was caught by independent review afterward -- do not repeat
   it.)

## Your task

Implement the "Order of implementation" steps 1 through 7 in order, verifying each
step's own test expectations before moving to the next, committing after each step.
Do NOT push or open a PR. When done (or if you get stuck / find this plan's
line-number pointers have drifted from actual current source -- treat them as
approximate, not exact; search the actual current code first), report back a clear
summary: what was implemented, full final test/clippy output (confirming BOTH test
binaries ran, per the verification checklist), any deviations from this plan or open
questions you flagged along the way, and explicit confirmation that you followed
the verification checklist's last item (no false "pre-existing failure" claims
backed only by an invalid git-stash check).
