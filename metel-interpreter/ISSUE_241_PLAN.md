# Implementation plan: issue #241 — RFC-0036 conditional impl blocks

Repo: `metel-interpreter` (an interpreter for the "Metel" language: parser (pest) ->
name resolver -> path normalizer -> coherence pass -> two-pass HM typechecker
(inference.rs Pass 1, construction.rs Pass 2) -> elaborator -> evaluator).

This plan was produced by a research agent that read the actual current source in
full (not just grep) before proposing anything. Follow it in order; each step should
build and its own fixtures should pass before moving to the next.

**Note**: issue #240 (RFC-0037, return-position `impl Aspect`) is being implemented
in PARALLEL by a separate agent on a separate branch/worktree, based on the same
`sprint/26` tip. The two issues are independent (no shared file regions expected to
conflict beyond incidental proximity in shared files like `inference.rs`), but if you
notice yourself needing to touch the exact same function #240's plan also touches,
note it explicitly in your final report rather than guessing at how to merge intent.

## Background

RFC-0036 ("Conditional Impl Blocks") was just integrated into the spec
(`internal/rfcs/3-integrated/rfc-0036-conditional-impl-blocks.md` in the metel-docs
repo, including integration-time corrections at the top and a new §3.3 note about
bare-parameter blanket impls being explicitly out of scope). An `impl` block for a
generic type may be CONDITIONAL on its own type parameters satisfying additional
bounds:
```metel
impl Printable for Pair<A, B> where A: Printable, B: Printable { ... }
// equivalent inline form:
impl<A: Printable, B: Printable> Printable for Pair<A, B> { ... }
```
`Pair<i64, String>` is `Printable`; `Pair<i64, NonPrintable>` is not -- but both
remain CONSTRUCTABLE (the struct's own unconditional bounds, RFC-0034, and the
impl's conditional bounds are independent checks). The compiler checks a conditional
impl's bounds at every point the aspect is REQUIRED (method call, bound check, impl
selection) -- NOT at the impl's own declaration site.

Key RFC rules:
- **§2.1**: use-site checking, not declaration-site.
- **§2.2**: struct bounds (construction-time, RFC-0034, already implemented) and
  impl bounds (aspect-availability, this RFC) are independent, checked separately.
- **§2.3**: a generic function propagates a conditional impl to its OWN callers only
  by stating the bound explicitly in its own signature -- no inference.
- **§3.1 coherence**: two conditional impls of the same aspect for the same type are
  a coherence error UNLESS provably disjoint via **syntactic negation only** (one
  impl has an explicit negative bound, RFC-0072, directly negating a positive bound
  in the other -- e.g. `T: !Copy` directly negates `T: Copy`). No general
  inference-based disjointness proof.
- **§3.2**: a conditional impl and an UNCONDITIONAL impl for the same type
  constructor always conflict.
- **§3.3**: ordinary orphan rule applies, BUT bare-parameter blanket impls
  (`impl<T: Bound> Aspect for T`, no named wrapping type) are explicitly OUT OF
  SCOPE -- deferred to RFC-0097 (draft, not accepted). Every real example targets a
  genuinely named type.
- **§4**: error reporting reuses **T0012** ("Aspect bound not satisfied") -- NOT a
  new code (the RFC's original text mistakenly said T0013, already claimed
  elsewhere; corrected during integration, matching how issue #243 already reused
  T0012 for the negative-bound direction).
- The integration pass's own worked example additionally established: a conditional
  impl's `where`-clause bounds and issue #242's already-implemented
  equality-constrained bounds (`Aspect<AssocType = Concrete>`) are stored/checked as
  the EXACT SAME `Bound` AST structure, so they should compose for free if you reuse
  the existing bound-collection/checking machinery rather than inventing
  impl-specific versions.

## Research findings (already done -- do not re-derive, verify then build on this)

### §0. Grounding: what already exists vs. what's a stub (verified by reading current
code, not assumed)

**Parser/AST (issue #233) -- genuinely complete, reusable as-is:**
- `src/ast/mod.rs` `ImplBlock` (line 179) has `generics: Vec<GenericParam>` and
  `where_clause: Option<WhereClause>` -- the EXACT SAME types `StructDecl`/
  `EnumDecl`/`FunDecl` use. `GenericParam { name, bounds: Vec<Bound> }`,
  `WhereClause { constraints: Vec<(String, Vec<Bound>)> }`, `Bound { polarity,
  aspect, assoc_bindings }` (`ast/mod.rs:250-268`).
- `src/parser/mod.rs::parse_impl_block` (line 464) genuinely parses both
  `impl<T: Bound> Aspect for Type<T>` (via `Rule::generic_params`) and `impl Aspect
  for Type<T> where T: Bound` (via `Rule::where_clause`). Both delegate to the SAME
  `parse_bound_list`/`parse_bound` (line 2586), which already handles `Rule::bang`
  for `Polarity::Negative` -- so `impl<T: !Copy> Aspect for Wrapper<T>` already
  parses correctly today. **No parser work is needed for this issue.**
- `src/grammar.pest:85-86` (`impl_block`) and lines 98-115 confirm the grammar
  shares these productions with struct/fun declarations.

**Registry (`src/typechecker/registry.rs`) -- real gaps, precisely located:**
- `collect_type_param_bounds`/`collect_negative_type_param_bounds` (lines 21-99) are
  generic over `&[GenericParam]`/`Option<&WhereClause>` -- reusable verbatim for
  impl blocks once given the right generics/where-clause slice. Currently
  module-private `fn`; make `pub(super)`.
- `is_generic_target` (line 406): `!ib.generics.is_empty() ||
  registry.struct_generic_names_for(target_name).is_some_and(|n| !n.is_empty())`.
  When true, `register_generic_impl_method_schemes` (line 532) runs instead of
  `register_impl_methods`. Read in full: it builds a `TypeScheme` with **`bounds:
  vec![]`, `neg_bounds: vec![]`, `assoc_projections: vec![]`,
  `assoc_eq_constraints: vec![]` hardcoded** (lines 592-598) -- registers the method
  as unconditionally callable, zero where-clause tracking. Confirmed baseline gap
  for the INLINE-generics impl form.
- **A real, confirmed live bug, not just a stub**, sitting right next to the
  `TODO(generic-impl)` comment (registry.rs lines 428-451): the "track which
  aspects this type implements" block (`registry.register_aspect_impl(...)`, line
  ~462) is claimed by its own comment to be gated by `is_generic_target`, but the
  actual control flow has NO such gate -- the only `continue` in this loop is for
  non-`Named` (structural) targets. **Today, once a conditional impl like
  `impl Printable for Pair<A, B> where A: Printable, B: Printable` type-checks,
  `registry.register_aspect_impl(module, "Pair", "Printable", [])` fires
  unconditionally, permanently marking `impl_aspect_env_has(module, "Pair",
  "Printable")` TRUE regardless of `A`/`B`** -- the struct is silently treated as
  unconditionally `Printable`. Must be fixed (skip this registration path for impls
  carrying impl-level bounds; route through new conditional-aware storage instead --
  see §1 decision 6).
- `impl_aspect_env_has` (typeinference/mod.rs:1183) and its backing store
  `impl_aspect_env: HashMap<(SymbolId, String), Vec<Vec<Type>>>` are a pure
  existence boolean keyed by `(type_id, aspect_name)` -- no notion of "the target's
  own type arguments," so cannot express "implements Printable WHEN A: Printable."
  A new conditional-aware query is required; cannot be retrofitted onto
  `impl_aspect_env_has` without changing its meaning for every unconditional caller.
- `method_scheme_env: HashMap<String, HashMap<String, (TypeScheme, Vec<TypeVar>)>>`
  (typeinference/mod.rs:935) stores AT MOST ONE scheme per `(type_name,
  method_name)`. Two conditional impls of the same aspect for the same struct
  providing a same-named method (§3.1's own worked example, `Wrapper<T>::serialize`
  from both `T: Copy` and `T: !Copy` impls) would have the second
  `register_method_scheme` call silently OVERWRITE the first via `.insert()` (lines
  1145-1148). **Confirmed to be the single largest net-new piece of work.**

**Inference (Pass 1, `src/typechecker/inference.rs`):**
- `Decl::Impl(ib) if !ib.generics.is_empty()` (line 481) short-circuits to
  `Ok(InferType::unit())` -- **it checks only `ib.generics`, not whether the struct
  itself is generic.** Consequence, verified against the two existing fixtures:
  - The INLINE form (`impl<T: Display> Greet for Box1<T>`, fixture
    `69_conditional_impl_inline_bound.mtl`) has non-empty `ib.generics`, so it takes
    this early return -- `infer_impl_method` is NEVER called for it. Its only
    registered scheme is the unbounded one from `register_generic_impl_method_schemes`.
  - The WHERE-CLAUSE form (`impl Farewell for Box2<T> where T: Display`, fixture
    `69b_conditional_impl_where_clause.mtl`) has EMPTY `ib.generics` (the parser only
    puts params into `ib.generics` for the `<...>` syntax; `T` here is implicitly
    the target's own already-registered struct type param) -- so it falls through to
    the `Decl::Impl(ib) =>` branch and DOES call `infer_impl_method`, like any
    ordinary generic-struct impl method already did before RFC-0036 existed. Not a
    special conditional-impl code path; the pre-existing generic-impl-method-inference
    path, unmodified.
  - `infer_impl_method` (line 877) seeds `struct_bounds: HashMap<TypeVar,
    Vec<String>>` ONLY from the struct's own unconditional bounds
    (`ctx.get_type_param_bounds(target_name)`, RFC-0034) -- NEVER from the impl
    block's own `ib.generics`/`ib.where_clause`. The resulting `TypeScheme` gets
    `.with_assoc_projections()` conditionally but NEVER `.with_bounds()`/
    `.with_neg_bounds()`/`.with_assoc_eq_constraints()` -- confirms method schemes
    today carry zero impl-level bound metadata, for EITHER form.

**Construction (Pass 2, `src/typechecker/construction.rs`):**
- `construct_impl_decl`/`construct_impl_method` (lines 676-794): `is_generic_target
  = impl_has_generics || struct_generic_names_for(target_name).is_some_and(non-empty)`
  -- this condition (unlike inference.rs's) correctly covers BOTH conditional-impl
  forms, since it also checks whether the STRUCT itself is generic. When true, the
  method body is stored as `FunBody::Generic(method.body.clone())` -- deferred,
  dynamically interpreted at runtime, never re-typechecked. Same mechanism every
  generic function body already uses -- not conditional-impl-specific.
- **The method-call dispatch/bound-check gap -- the biggest functional hole**:
  `Expr::MethodCall` handling (line 1439-1588). The "slow path" (generic
  struct/enum method, lines 1507-1576) looks up EXACTLY ONE scheme via
  `ctx.registry.method_scheme_for(&struct_name, method)`, builds a substitution
  from `struct_tvars.zip(receiver_type_args)`, instantiates, returns -- **it never
  calls `check_scheme_bounds`/`check_scheme_neg_bounds`/`check_scheme_assoc_eq`,
  unlike every function-call branch above it** (lines 2676-2855, which call all
  four `check_*` pairs after every scheme instantiation). This asymmetry is the
  precise, confirmed reason the existing fixtures document "no real bound-satisfaction
  checking... issue #241's job": the checking primitives exist and work (proven by
  #242/#243's own call-site tests), they're simply never invoked from the
  method-call path.
- `check_fun_call_bounds`/`check_scheme_bounds`/`check_fun_call_neg_bounds`/
  `check_scheme_neg_bounds`/`check_fun_call_assoc_eq`/`check_scheme_assoc_eq` (lines
  2935-3239) are the exact reusable primitives. `check_scheme_bounds`/
  `check_scheme_neg_bounds` take `(fun_name, scheme, var_to_type: &HashMap<TypeVar,
  Type>, span, registry, current_module)` and iterate `scheme.quantified_vars.zip(
  &scheme.bounds)` -- DIRECTLY usable for the method-call path once the method's
  `TypeScheme` carries the impl's own bounds and once a `var_to_type` map is built
  from `struct_tvars.iter().copied().zip(receiver_type_args.iter().cloned())` (data
  already available at the call site).
- `check_type_satisfies_bounds` (line 3113) reduces a concrete `Type` to a bare name
  and calls `impl_aspect_env_has(module, type_name, aspect)` -- IGNORES the type's
  own type arguments entirely. Fine for checking a FUNCTION's own generic-param
  bounds, but NOT sufficient, unmodified, for "does `Pair<i64, SomeNonPrintable>`
  (a whole instantiated generic type) implement `Printable`" -- needs the new
  conditional-aware registry query (§1 decision 6), invoked recursively per type
  argument.

**`FunGeneralization`/`scheme_env` rebuild (`src/typechecker/mod.rs`) -- the exact
risk class from the #242 precedent, precisely scoped here:**
- The `FunGeneralization` struct (line 74) and `scheme_env` rebuild loop (lines
  806-821, with all four `.with_bounds()`/`.with_neg_bounds()`/
  `.with_assoc_projections()`/`.with_assoc_eq_constraints()` calls present, matching
  commit `fd3c9af`'s fix) is CONFIRMED already fixed. This rebuild loop only
  concerns TOP-LEVEL FUNCTION schemes (`scheme_env`) -- a DIFFERENT mechanism from
  method schemes.
- **Method schemes take a different, LOWER-RISK path than the one that broke in
  #242**: `infer_impl_method` calls `ctx.register_method_scheme(...)` directly into
  the SAME `TypeDefinitionRegistry` that `construction::construct_program` is later
  handed as `ctx.registry()` -- there is NO intermediate "collect into a Vec, then
  rebuild a fresh scheme from scratch" step for method schemes analogous to
  `fun_generalizations`/`scheme_env`. **If bounds are attached directly onto the
  `TypeScheme` at the point `infer_impl_method` builds it (and at the point
  `register_generic_impl_method_schemes` builds its own), there is no separate
  rebuild step later that could silently drop it** -- the #242 failure mode
  structurally cannot recur here, PROVIDED bounds are attached at the single point
  of construction and never rebuilt afterward. **Nonetheless, explicitly verify the
  cross-module merge path**: `TypeDefinitionRegistry::merge_from`
  (typeinference/mod.rs:1458-1470) merges `method_scheme_env` PER-METHOD via
  `.or_insert_with(|| scheme.clone())` -- a full `TypeScheme` clone, not a rebuild,
  so whatever fields are set survive the merge intact. **Action item: after
  implementing the bound-attachment step, add a cross-module regression fixture (a
  struct+conditional-impl defined in one module, called with both a satisfying and
  a non-satisfying concrete type from an importing module) specifically to catch a
  #242-style regression at the merge boundary** -- the single highest-value
  regression test given the stated risk.

**Coherence (`src/coherence.rs`, issue #238) -- module doc is stale, confirmed:**
- The file's own module doc (lines 9-14) says `ImplBlock` has no generics/
  where-clause field "today" -- FALSE since #233, needs fixing. `canonicalize` (line
  88) resolves every `TypeExpr::Named(name, _)` via `resolve_id`; for an unbound
  type-parameter name like `T` in `Type<T>`, `resolve_id` returns `None`, producing
  `CanonicalType::Unresolved("T", [])`. **Two conditional impls using different
  parameter letters for the same position (`Type<T>` vs. `Type<U>`) would NOT
  canonicalize identically today, silently missing an overlap that should be
  flagged.** Must be fixed as part of extending coherence for §3.1/§3.2, not
  pre-existing-but-fine.
- The overlap check (`seen: HashMap<(SymbolId, Vec<CanonicalType>, CanonicalType),
  &Span>`, lines 209-226) is an exact-key-match-conflicts scan; §3.1's
  syntactic-negation escape hatch needs to intercept BEFORE this insertion.

**Test fixture baseline (confirmed by reading, not assumed):**
- `tests/integration/sources/evaluator/aspects/69_conditional_impl_inline_bound.mtl`
  and `69b_conditional_impl_where_clause.mtl` both exist and both EXPLICITLY
  disclaim real bound checking in their own comments. Both currently pass
  end-to-end (parse, construct, dispatch, evaluate) with a single conditional impl
  each, no bound violation exercised. **These are the exact regression baseline --
  must keep passing unmodified.**
- No existing fixture exercises: bound violation (T0012), two conditional impls for
  the same aspect/target, or §3.1's negation-disjointness. All net-new.

**Error codes**: T0012 = "Aspect bound not satisfied" (confirmed reuse target),
T0013 = "Ambiguous aspect method resolution" (confirmed already claimed, matches the
RFC's own correction -- do NOT use T0013 for this issue's errors), T0015 =
"Conflicting implementation" (confirmed, already used by `coherence.rs` for exactly
this class -- reuse for §3.1/§3.2, no new code needed anywhere in this issue).

**Architectural finding that resolves §2.3 "propagation through generic functions"
(non-obvious, worth stating explicitly):** every existing bound-check primitive
lives in `construction.rs` (Pass 2) and is only ever reached from a call site being
EAGERLY, CONCRETELY constructed (`FunBody::Typed`). Pass 1's own `Expr::MethodCall`/
`Expr::Call` handling performs pure type unification -- it does NOT check aspect
bounds anywhere, for ANY bound class (ordinary RFC-0034 bounds included). Since a
generic function's body is ALWAYS deferred to `FunBody::Generic` in Pass 2 (never
constructed, dynamically interpreted at runtime instead), there is NO point in the
existing pipeline, for any bound class, where a call made from inside a
still-generic function's body is verified against the callee's bound requirements.
This is a pre-existing, deliberate architectural characteristic of the whole
generics system, not specific to conditional impls. **Therefore: §2.3's propagation
composes "for free" in the same limited sense ordinary bound propagation already
does today -- no new Pass-1 wiring is needed or should be added**, since adding it
only for conditional impls would be an inconsistent, scope-creeping special case.
The positive §2.3 fixture should be written as a CONCRETE, non-generic call site
invoking a generic function that itself calls the bounded method -- this exercises
the real, existing enforcement point (the generic function's own declared-bound
check against the concrete instantiation, via `check_fun_call_bounds`/
`check_scheme_bounds`, already implemented) end-to-end, which is the honest scope
of what "propagation" means operationally in this codebase.

## §1. Design decisions (grounded in the above)

1. **Bound collection**: reuse `collect_type_param_bounds`/
   `collect_negative_type_param_bounds` (registry.rs) verbatim -- change visibility
   to `pub(super)`. For the impl-block case, since `ib.generics` is empty for the
   where-clause-only form, SYNTHESIZE a `Vec<GenericParam>` from the struct's own
   canonical generic names (`struct_generic_names_for`) before calling these
   functions:
   ```rust
   let synth: Vec<GenericParam> = struct_generic_names.iter().map(|n| GenericParam {
       name: n.clone(),
       bounds: ib.generics.iter().find(|g| &g.name == n).map(|g| g.bounds.clone()).unwrap_or_default(),
   }).collect();
   let impl_bounds = collect_type_param_bounds(&synth, ib.where_clause.as_ref());
   let impl_neg_bounds = collect_negative_type_param_bounds(&synth, ib.where_clause.as_ref());
   ```
   Handles both syntactic forms uniformly, merges inline+where-clause bounds on the
   same param (matching the merge behavior these functions already implement for
   structs), requires zero new bound-parsing logic.

2. **Struct-bounds vs. impl-bounds independence (§2.2)**: keep them in SEPARATE
   `HashMap<TypeVar, Vec<String>>`s. `struct_bounds` (existing, RFC-0034) continues
   to be used only to make aspect methods callable INSIDE a method body during
   inference -- never attached to the resulting `TypeScheme.bounds`. `impl_bounds`/
   `impl_neg_bounds` (new) get attached via `.with_bounds()`/`.with_neg_bounds()` to
   the method's own `TypeScheme`, so `check_scheme_bounds`/`check_scheme_neg_bounds`
   at the call site check ONLY the impl's own conditional requirement, keeping
   error messages correctly attributable to "required by the conditional impl"
   rather than conflating with a (structurally already-guaranteed-true) struct
   bound.

3. **Where to attach bounds** (two symmetric sites, both needed):
   - `register_generic_impl_method_schemes` (registry.rs) -- covers the
     inline-generics form, which `inference.rs` currently skips entirely. Compute
     `impl_bounds`/`impl_neg_bounds` there (registry.rs already has `ib`,
     `target_name`, and `registry.struct_generic_names_for`/`type_params` in scope)
     and call `.with_bounds(&by_var)`/`.with_neg_bounds(&by_var)` on the
     `TypeScheme` before `register_method_scheme`.
   - `infer_impl_method` (inference.rs) -- covers the where-clause form (already
     reaches this function) AND, per decision 4 below, the inline form once its
     early-return is loosened. Compute the same `impl_bounds`/`impl_neg_bounds`,
     keyed to the RESOLVED `struct_tvars_resolved` (mirroring how
     `assoc_projections` is already resolved post-solve), attach via
     `.with_bounds()`/`.with_neg_bounds()` alongside the existing
     `.with_assoc_projections()` call.
   - Since `infer_impl_method`'s registration OVERWRITES whatever
     `register_generic_impl_method_schemes` put in the registry (both call the same
     `register_method_scheme`, last write wins), attaching bounds in BOTH places is
     required so that (a) the Pass-0 bootstrap scheme already carries bounds
     correctly, and (b) Pass 1's own overwrite doesn't regress it back to
     bounds-free. This directly forestalls a #242-style "collected in one place,
     silently dropped by a later overwrite" failure by making both writers
     responsible for the same invariant.

4. **Loosen `inference.rs`'s early-return** (line 481) from `!ib.generics.is_empty()`
   to a condition matching `construction.rs`'s existing `is_generic_target` check
   inverted: run `infer_impl_method` whenever `ib.target_type` is
   `TypeExpr::Named(name, _)` AND `name` resolves to a real registered struct/enum
   (`struct_generic_names_for(name).is_some()`), regardless of whether `ib.generics`
   is empty. Keep the `Ok(InferType::unit())` short-circuit only for RFC-0061
   structural targets and the (out-of-scope) bare-parameter blanket form, where
   `target_name` isn't a real struct/enum at all and `infer_impl_method` would hit
   its own `_ => Err("generic impl blocks not yet supported")`. This makes the
   INLINE form finally get real Pass-1 body inference (previously skipped
   wholesale) -- required for its `TypeScheme` to gain properly-solved
   `struct_tvars_resolved` for the bound attachment in decision 3 to be meaningful.

5. **Multi-impl dispatch storage** (confirmed largest net-new piece): add a new
   registry map, additive and non-invasive to every existing caller of
   `method_scheme_for`:
   ```rust
   // typeinference/mod.rs, TypeDefinitionRegistry
   method_scheme_variants: HashMap<(String, String), Vec<(TypeScheme, Vec<TypeVar>)>>,
   ```
   populated by a new `register_method_scheme_variant(type_name, method_name,
   scheme, struct_tvars)` that PUSHES rather than overwrites, called from BOTH
   `register_generic_impl_method_schemes` and `infer_impl_method` IN ADDITION TO
   the existing single-slot `register_method_scheme` call (kept unchanged for full
   backward compatibility with every other reader). `method_scheme_for` (singular)
   keeps returning whatever was last registered -- fine for the common
   single-conditional-impl case (the two existing 69/69b fixtures keep working via
   the unmodified path). The NEW `method_scheme_variants_for(type_name,
   method_name) -> Option<&Vec<(TypeScheme, Vec<TypeVar>)>>` is consulted first,
   from construction.rs's `MethodCall` slow path, ONLY when it has more than one
   entry; with 0 or 1 entries the existing single-scheme path is used unchanged --
   keeps the change surgical and low-risk for the overwhelming majority of impls
   (unconditional generic-struct methods, single-conditional-impl cases) while
   adding real dispatch only where genuinely needed. `merge_from` gets a parallel
   per-`(type,method)` `Vec`-append merge arm (concatenate rather than
   `or_insert_with`, since cross-module conditional impls for the same method name
   are a legitimate, if rare, scenario the RFC doesn't forbid).

6. **Conditional-aware "does this instantiation satisfy this aspect" query** -- new
   registry storage and method, additive next to (not replacing) `impl_aspect_env`:
   ```rust
   // key: (target_type_id, aspect_name) -> one entry per registered conditional impl for that pair
   conditional_impl_bounds: HashMap<(SymbolId, String), Vec<(Vec<Vec<String>>, Vec<Vec<String>>)>>,
   //                                                        ^positive-by-position ^negative-by-position
   ```
   New method `fn aspect_satisfied_by(&self, current_module, type_name: &str,
   type_args: &[Type], aspect_name: &str) -> bool`:
   - If `conditional_impl_bounds` has NO entry for `(target_id, aspect_name)`, fall
     back to the existing `impl_aspect_env_has` (covers unconditional impls -- zero
     behavior change for every non-generic/non-conditional case).
   - Otherwise, for each registered `(pos_bounds, neg_bounds)` entry, check that for
     every position `i`: every aspect name in `pos_bounds[i]` is satisfied by
     `type_args[i]` (RECURSING into `aspect_satisfied_by` again if `type_args[i]` is
     itself `Type::Named(inner, inner_args)`, so nested generic instantiations --
     e.g. `Pair<Pair<i64,String>, bool>` -- are handled correctly, not just
     single-level primitives), and every aspect name in `neg_bounds[i]` is NOT
     satisfied (mirroring `check_type_does_not_satisfy_bound`'s existing
     Copy-implies-!Drop special case for consistency). Return true on the first
     fully-satisfied entry (coherence guarantees at most one can be, in a
     well-formed program); false if none match -- this is the T0012 case.
   - Populate `conditional_impl_bounds` at the EXACT SITE of the confirmed bug in
     registry.rs (the `register_aspect_impl` call around line 462): when
     `is_generic_target` is true AND the impl carries non-empty `impl_bounds`/
     `impl_neg_bounds` (from decision 1), register into `conditional_impl_bounds`
     INSTEAD OF (unconditionally, buggily) into `impl_aspect_env` via
     `register_aspect_impl`. When `is_generic_target` is true but bounds are EMPTY
     (an ordinary, unconditional generic impl, e.g. plain `impl Iterable for
     List<T>` with no where clause) -- this is §3.2's "unconditional impl for a
     generic type constructor" case -- still register into `impl_aspect_env` as
     today (unconditional membership), so `aspect_satisfied_by`'s fallback path
     continues to work for it, and so the §3.2 conflict is detected correctly at
     the coherence layer (decision 8) by seeing BOTH an entry in
     `conditional_impl_bounds` (from the conditional impl) AND the plain
     unconditional registration for the same `(target_id, aspect)` key.

7. **Use-site checking wiring (§2.1)** -- the concrete, minimal change to
   `construction.rs`'s `MethodCall` slow path (around line 1507-1576): after
   building `struct_tvars`/`receiver_type_args` (already computed there) and before
   or after instantiating the scheme, build `let var_to_type: HashMap<TypeVar,
   Type> = struct_tvars.iter().copied().zip(receiver_type_args.iter().cloned()).collect();`
   and call `check_scheme_bounds(method, &scheme, &var_to_type, span, ctx.registry,
   ctx.current_module)?` and `check_scheme_neg_bounds(...)` (and
   `check_scheme_assoc_eq` for completeness with #242's composition requirement) --
   exactly mirroring the four calls already made after every function-call scheme
   instantiation elsewhere in this file. When `method_scheme_variants_for` returns
   >1 entries, iterate variants and pick the first whose bounds+neg_bounds are both
   satisfied (using the same check functions, but non-erroring -- i.e. a boolean
   "would this succeed" helper, or catch-and-continue over `check_scheme_bounds`'s
   `Result`), falling through to a T0012 error (using the RFC's own error-message
   shape, "`Type<Args>` does not implement `Aspect` because `X` does not implement
   `Y` (required by: impl...)") if none match -- this is the only place genuinely
   new dispatch-selection logic is needed; everywhere else reuses the four existing
   `check_*` functions unmodified.

8. **Coherence (§3.1/§3.2)** -- extend `coherence.rs`:
   - Fix the stale module doc comment.
   - Add a helper `fn scoped_type_param_bounds(ib: &ImplBlock) -> Vec<(Vec<String>,
     Vec<String>)>` (positive, negative), indexed by `ib.target_type`'s own
     TOP-LEVEL type-argument position (not by struct lookup -- coherence runs
     before the registry exists, but position-in-target-type-args is exactly
     equivalent to struct-declaration-order by construction). For each top-level
     arg that is a bare, unresolvable `Named(name, [])` (i.e., an impl-scoped type
     variable, not a real declared type), look up matching bounds from
     `ib.generics` (by name) and `ib.where_clause.constraints` (by name), merged.
   - Extend `canonicalize`'s handling specifically for an impl's own `target_type`
     (a NEW `canonicalize_impl_target` wrapper, not a change to the general
     `canonicalize` used elsewhere): map each top-level unresolved-name argument to
     `CanonicalType::TypeParam(i)` (a NEW enum variant) keyed by position, so
     `Type<T>` and `Type<U>` canonicalize identically. Fixes the confirmed
     stale-comment gap; required for §3.1/§3.2 to fire correctly at all (without
     it, two conditional impls using different letters would never even reach the
     overlap check).
   - In the overlap-detection loop (replacing the flat `seen` HashMap lookup with a
     pairwise scan WITHIN each exact-canonical-key group, since groups are always
     small in practice): before reporting T0015 for two impls with the same
     canonical key, check `provably_disjoint(scoped_type_param_bounds(a),
     scoped_type_param_bounds(b))` -- true iff, at some position `i`, one impl's
     positive bound set contains an aspect name present in the other's negative
     bound set at the same position `i`. If disjoint, skip (§3.1's accept case);
     otherwise report T0015 as today.
   - §3.2 falls out for free from the above: an unconditional impl's
     `scoped_type_param_bounds` is all-empty at every position, so
     `provably_disjoint` can never return true against it (nothing to negate), so
     the ordinary T0015 conflict path fires whenever an unconditional and a
     conditional impl share the same (now-correctly-canonicalized) target -- no
     separate code path needed, only requires verifying this behavior with a
     dedicated test.

9. **Negative-polarity conditional impls (`impl<T: !Copy> !Serialize for
   Wrapper<T>`)** -- confirmed via direct reading of RFC-0072 §4 (only discusses
   negative bounds AS CONDITIONS inside a POSITIVE conditional impl, e.g.
   `impl<T: !Drop> BulkMove for Arena<T>`, which decisions 1-7 above already
   handle) and RFC-0081 (no mention of generics/conditional/where at all). **This
   exact combination is not specified anywhere.** Recommendation: explicitly detect
   `ib.polarity == Polarity::Negative && (!ib.generics.is_empty() ||
   ib.where_clause.is_some())` in the new registration code and route it through
   the EXISTING, minimal, already-shipped issue #264 negative-impl handling
   UNCHANGED (do not attempt conditional semantics for it -- do not crash, do not
   silently apply bounds). **Flag this explicitly as an open question for
   reviewers, not a guessed behavior** -- do not implement a guess.

10. **Explicitly out of scope, confirmed and not touched**: bare-parameter blanket
    impls (`impl<T: Bound> Aspect for T`) -- confirmed `is_generic_target`'s
    registry.rs branch and construction.rs's `_ if impl_has_generics =>
    String::new()` fallback already tolerate (don't crash on) this shape without
    giving it real semantics; leave as-is, deferred to RFC-0097. RFC-0061
    structural targets (`T[]`, tuples, fn-types) -- same tolerate-don't-crash
    posture, issue #245's job. RFC-0060 §3/§4/§5 auto-impl/blanket-priority rules
    beyond what §3.1/§3.2 require -- issue #244's job; note (do not implement) that
    this issue's `conditional_impl_bounds` storage and `aspect_satisfied_by` query
    will likely be directly reusable by #244's closed-world negative-bound
    discharge work.

## Order of implementation (each step builds + `cargo test --release` before the
next)

**Step 1 -- Registry visibility + bound-collection helper (registry.rs).**
Change `collect_type_param_bounds`/`collect_negative_type_param_bounds` from `fn`
to `pub(super)`. Add a small new helper (registry.rs, near them) `pub(super) fn
synth_generics_for_impl(struct_generic_names: &[String], ib_generics:
&[GenericParam]) -> Vec<GenericParam>` implementing the synthesis from decision 1.
No behavior change yet -- build, `cargo test --release` (must be a no-op diff in
test results).

**Step 2 -- Attach bounds in `register_generic_impl_method_schemes` (registry.rs).**
Inside the existing per-method loop (line ~576), compute `impl_bounds`/
`impl_neg_bounds` (via step 1's helpers, using `generic_names`/`type_params`
already in scope) and build `by_var: HashMap<TypeVar, Vec<String>>` from
`type_params.iter().zip(&impl_bounds)`. Call `.with_bounds(&by_var)`/
`.with_neg_bounds(&by_neg_var)` on the `TypeScheme` before
`registry.register_method_scheme(...)`. Also call the new
`register_method_scheme_variant` (decision 5) alongside the existing call. Build.
`cargo test --release` -- fixtures 69/69b must still pass unmodified (no checking
is wired up to CONSUME these bounds yet, so this step is inert but must not
regress construction).

**Step 3 -- Fix the `is_generic_target`/`register_aspect_impl` bug + add
`conditional_impl_bounds` storage (registry.rs + typeinference/mod.rs).**
Add `conditional_impl_bounds` field and `register_conditional_impl_bounds`/
`aspect_satisfied_by` to `TypeDefinitionRegistry` (typeinference/mod.rs, near
`impl_aspect_env`), implementing decision 6 (with recursion into nested
`Type::Named` args). In registry.rs's `register_program_decls`, at the
confirmed-buggy call site (~line 462), branch: if `is_generic_target && (impl_bounds
or impl_neg_bounds non-empty)`, call `register_conditional_impl_bounds` instead of
`register_aspect_impl`; else keep the existing `register_aspect_impl` call
unchanged (covers non-generic impls and unconditional generic impls, §3.2's
"unconditional" side). Add `merge_from` support for `conditional_impl_bounds`
(append-merge, mirroring `method_scheme_env`'s per-key merge pattern). Build.
`cargo test --release` -- this step CHANGES observable behavior (a conditional impl
no longer silently marks the aspect as unconditionally implemented) -- expect no
regressions since nothing currently depends on that buggy registration being true
(confirmed no test exercises it), but re-run full suite to be certain.

**Step 4 -- Loosen `inference.rs`'s early-return + attach bounds in
`infer_impl_method` (inference.rs).**
Apply decision 4's condition change to the `Decl::Impl` dispatch (line 481). Inside
`infer_impl_method`, alongside the existing `struct_bounds` seeding loop (lines
893-913), add a parallel `impl_bounds`/`impl_neg_bounds` computation (step 1's
helpers, using `struct_tvars_ordered` before resolution) and, after
`struct_tvars_resolved` is computed (line ~1035), build `by_var`/`by_neg_var` keyed
by the RESOLVED tvars and call `.with_bounds()`/`.with_neg_bounds()` on the scheme
built at line ~1052, before `ctx.register_method_scheme(...)`. Also call the new
`register_method_scheme_variant`. Build. `cargo test --release` -- this is the step
most likely to surface a latent issue, since it's the first time the inline form
(`Box1<T>`/fixture 69) gets real Pass-1 body inference; run the two 69/69b fixtures
explicitly first, then the full suite.

**Step 5 -- Use-site checking in construction.rs's `MethodCall` (construction.rs).**
Implement decision 7: build `var_to_type` in the slow path (lines ~1507-1576), call
`check_scheme_bounds`/`check_scheme_neg_bounds`/`check_scheme_assoc_eq` on the
single-scheme path. Add the multi-variant dispatch loop using
`method_scheme_variants_for` (decision 5) when >1 entries exist, with the T0012
fallback error using the RFC's own message shape. Build. `cargo test --release`.

**Step 6 -- Coherence extension (coherence.rs).**
Implement decision 8: fix stale doc, add `CanonicalType::TypeParam(usize)`,
`canonicalize_impl_target`, `scoped_type_param_bounds`, `provably_disjoint`, and the
pairwise-within-group overlap scan. Build. `cargo test --release`.

**Step 7 -- Negative-polarity guard (registry.rs/inference.rs/construction.rs,
wherever the new conditional paths were added in steps 2-5).**
Add the explicit `Polarity::Negative && has bounds` detection from decision 9 at
each new registration/checking site added in steps 2-5, routing to existing #264
behavior unchanged, with a code comment flagging it as an open question. No test
fixture required to PASS new behavior here since none is specified -- add one test
confirming it doesn't crash (parses/registers without panicking).

**Step 8 -- Regression fixture for the #242-style merge-boundary risk
(multi-module).**
Add the cross-module fixture flagged in the `FunGeneralization` analysis above: a
struct + conditional impl in one module, imported and called (both a satisfying and
a violating concrete instantiation) from another module. This is the single
highest-value test for catching a silent metadata-drop at the `merge_from`
boundary.

**Step 9 -- Full fixture suite (see list below) + verification checklist.**

## Test fixtures to add

All negative typechecking fixtures use the `// ERROR[T00xx]` inline annotation
convention. Multi-file coherence fixtures follow the existing
`conflicting_impl_same_target`/`orphan_impl_cross_module_violation` directory-with-
`main.mtl` pattern.

**Positive -- `tests/integration/sources/typechecking/generics/` (check the highest
existing `stageNN` prefix at implementation time and continue from there; note in
your final report exactly which prefix you used, since issue #240 may also be
adding fixtures to this same directory in a parallel worktree -- check for a
collision before merging, don't just assume you have the number to yourself):**
- `..._conditional_impl_inline_bound_satisfied.mtl` -- `impl<T: Printable> Aspect
  for Pair<T, T>`-shaped, called with a satisfying concrete type; typechecks and
  constructs.
- `..._conditional_impl_where_clause_satisfied.mtl` -- where-clause form, same
  shape.
- `..._conditional_impl_two_params_both_satisfied.mtl` -- `Pair<A, B> where A:
  Printable, B: Printable`, both satisfied.
- `..._conditional_impl_negative_bound_disjoint_dispatch.mtl` -- **the §3.1
  positive case**: two conditional impls of the same aspect for the same struct,
  `T: Copy` vs. `T: !Copy`, both providing the same method name; call it with a
  `Copy` type and a `!Copy` type, confirm each dispatches to the correct impl
  (exercises the new multi-variant registry + dispatch-selection end to end).
- `..._conditional_impl_propagation_through_generic_function.mtl` -- the §2.3
  fixture: a `fun print_pair<A: Printable, B: Printable>(p: Pair<A, B>) {
  p.print(); }` called from `main()` with concrete args (exercising the existing,
  already-correct outer-call bound check per the architectural-precedent finding,
  not new Pass-1 machinery).
- `..._struct_bounds_vs_impl_bounds_independent.mtl` -- the §2.2 worked example
  verbatim: `SortedList<T: Comparable>` constructable with any `Comparable` `T`;
  `impl<T: Comparable + Printable> Printable for SortedList<T>` -- construct with a
  `Comparable`-but-not-`Printable` `T` (succeeds), then confirm calling `.print()`
  on it is the failure case (paired with the negative fixture below), and succeeds
  when `T` is also `Printable`.

**Negative -- same directory, `..._neg_*`:**
- `..._neg_conditional_impl_bound_not_satisfied.mtl` -- `Pair<i64,
  NonPrintable>.print()` -> `// ERROR[T0012]`, message should name the unsatisfied
  inner type per RFC §4's example shape.
- `..._neg_struct_bound_satisfied_impl_bound_violated.mtl` -- the §2.2 negative
  half: `SortedList<Comparable-but-not-Printable>` constructs fine, `.print()`
  fails `T0012`.
- `..._neg_conditional_impl_multi_bound_one_violated.mtl` -- two-param conditional
  impl, one satisfied, one not -> `T0012`.

**Coherence -- `tests/integration/sources/typechecking/aspects/` (directory-style,
next after the existing `stage13_*` series):**
- `conditional_impl_negation_disjoint_accepted/` -- §3.1's accepted example
  verbatim (`impl<T: Copy> Serialize for Wrapper<T>` + `impl<T: !Copy> Serialize
  for Wrapper<T>`) -- must typecheck successfully (no coherence error).
- `conditional_impl_non_disjoint_rejected/` -- §3.1's rejected example verbatim
  (`Clone` vs. `Display`, no negation) -> `main.mtl` with `// ERROR[T0015]`.
- `conditional_impl_explicit_negation_added_accepted/` -- the RFC's own fix-up
  example (`T: Clone, T: !Display` vs. `T: Display`) -- must typecheck
  successfully.
- `conditional_vs_unconditional_impl_conflict/` -- §3.2 -> `// ERROR[T0015]`.
- `conditional_impl_different_letters_still_detected_as_overlap/` -- two
  conditional impls of the same aspect for the same struct using DIFFERENT
  type-param letters (`Type<T>` vs `Type<U>`), same (non-disjoint) bounds -> `//
  ERROR[T0015]`, specifically to pin the `CanonicalType::TypeParam` fix from step
  6.

**Cross-module (step 8's merge-boundary regression):**
- `tests/integration/sources/typechecking/generics/..._cross_module_conditional_impl/`
  (directory, two `.mtl` files) -- struct + conditional impl defined and
  Pass-1-inferred in a dependency module, called with both a satisfying and a
  violating concrete type from an importing module's `main.mtl`. This is the
  fixture most likely to catch a silent regression at `merge_from`.

**Evaluator (end-to-end runtime, `tests/integration/sources/evaluator/aspects/`,
continuing from the highest existing prefix -- check at implementation time):**
- `..._conditional_impl_negation_dispatch_runtime.mtl` -- runtime confirmation that
  the §3.1 dispatch fixture actually calls the CORRECT method body (not just
  typechecks), by asserting on distinguishable return values from each impl.

**Negative-polarity open-question guard (step 7):**
- `..._negative_conditional_impl_does_not_crash.mtl` (evaluator/aspects) --
  `impl<T: !Copy> !Serialize for Wrapper<T>` parses/registers without panicking;
  not asserting any particular semantic behavior since none is specified.

## Final verification checklist

1. `cargo build` (and `cargo build --release`).
2. **Run `cargo test --release` as a SINGLE, FULL-SUITE invocation** -- explicitly
   NOT `cargo test --release --lib` or `cargo test --release --test integration`
   run separately, since those two commands silently skip the separate
   `tests/unit.rs` binary target -- this exact gap is what let issue #242's
   automated implementation ship a broken commit undetected. Confirm both
   `Running tests/integration.rs` AND `Running tests/unit.rs` appear in the output.
3. `cargo clippy --release --lib -- -W clippy::pedantic` -- zero new warnings.
4. Explicitly re-run the two pre-existing fixtures (`69_conditional_impl_inline_bound.mtl`,
   `69b_conditional_impl_where_clause.mtl`) and confirm unchanged pass/output --
   they are the only real regression baseline for this feature today.
5. Explicitly re-run the full `typechecking/generics/` and `typechecking/aspects/`
   suites (RFC-0034/#242/#243's own regression coverage) -- the changes in steps
   3/4 touch shared registry/inference code paths those suites exercise.
6. Before claiming any pre-existing test "fails identically on the unmodified
   baseline," verify by actually checking out/testing the base commit directly --
   do NOT rely on `git stash` if any of your changes are already committed, since
   `git stash` only reverts uncommitted changes and will silently produce a false
   "confirmed pre-existing" result if the real cause is in an earlier commit of
   this same session's work. (This exact mistake happened during issue #243's
   implementation and was caught by independent review afterward -- do not repeat
   it.)

## Your task

Implement the "Order of implementation" steps 1 through 9 in order, verifying each
step's own test expectations before moving to the next, committing after each step.
Do NOT push or open a PR. When done (or if you get stuck / find this plan's
line-number pointers have drifted from actual current source -- treat them as
approximate, not exact; search the actual current code first), report back a clear
summary: what was implemented, full final test/clippy output (confirming BOTH test
binaries ran, per the verification checklist), any deviations from this plan or open
questions you flagged along the way (especially decision 9's negative-polarity open
question -- do not guess at behavior there, just confirm it doesn't crash), and
explicit confirmation that you followed the verification checklist's last item (no
false "pre-existing failure" claims backed only by an invalid git-stash check).
