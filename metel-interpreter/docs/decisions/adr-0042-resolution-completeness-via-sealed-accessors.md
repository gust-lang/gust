---
id: adr-0042
title: "Resolution Completeness via Sealed Accessors, Not Ad Hoc Optional Fields"
date: '2026-07-11'
status: accepted
relates: adr-0041, adr-0037, adr-0025
implements: METEL-185, METEL-187
---

## Context

ADR-0041 shipped steps 1–3b-iii of the `SymbolId` migration: the reference-resolution
pass, `SymbolId`-based dispatch for callables and runtime type/method lookup, and the
cross-module dispatch guarantee. It did this by adding `Option<SymbolId>` fields
directly to the existing shared AST/value types (`TypedFunDecl::def_id`,
`TypedFunDecl::symbol_id`, `TypedExpr::Call::callee_id`, `Value::Struct::type_id`,
`Value::Enum::type_id`, `TypedImplBlock::target_type_id`), populated by the resolver
and stamped on during construction/elaboration.

That approach mixes two genuinely different reasons a field can be `None`:

- **Structural non-applicability** — the field doesn't apply to this node, permanently
  and correctly. A nested/local function has no top-level `def_id` because it isn't a
  top-level declaration; a host-built value (`EnvVar`, `OsError`, `ProcessOutput`) has
  no `type_id` because it has no user-facing type identity to carry. `None` here is not
  a gap — it's the right answer, forever, for that kind of node.
- **Resolution incompleteness** — the field *should* have a value by the time anything
  downstream reads it, but nothing currently guarantees it does. `TypedExpr::Call`'s
  `callee_id` is the concrete instance: a call site that misses the registry and isn't
  an overload id falls back to name lookup — the exact string-keyed path this whole
  migration exists to delete, still reachable for first-class function values. ADR-0041
  named this explicitly and deferred it ("the deferred first-class-function environment
  question").

Collapsing both into the same `Option<SymbolId>` shape means a caller checking
`if let Some(id) = ...` can't tell, from the type alone, whether a `None` is expected
and permanent or a bug. The remaining migration work — ADR-0041's "step 4–5", the deep
`TypeDefinitionRegistry` rekey now tracked as issue #239 (ImplKey refactor) and part of
#237 (SymbolId stabilization) — was scoped to extend the same pattern to the
typechecker's `struct_env`/`enum_env`/`method_env`/`aspect_env`/`impl_aspect_env` maps.
This ADR revises that plan before it starts, for the reason above, not because
steps 1–3b-iii were wrong — those fields are overwhelmingly the structural-
non-applicability kind, and are not being reworked.

**Considered and rejected: a fully phase-typed AST (duplicate or generic-parameterized
node types per phase).** The stronger, more literal fix — separate types for
"pre-resolution" and "post-resolution" nodes, so a `SymbolId` is a mandatory field by
construction rather than a checked optional one — was considered directly. Rejected for
this migration specifically: identity isn't fully known at a single clean boundary
(construction stamps `callee_id`, elaboration stamps method ids across several passes),
so "post-resolution" would have to mean "post-elaboration," and duplicating (or
generic-parameterizing) every node kind that carries identity is a large, invasive
change for a codebase whose AST/typed-AST is explicitly a pre-HIR representation —
METEL-171 (System-F HIR / native backend, tracked as issue #259) is the actual right
home for that scale of change, and it remains unscheduled. Building a second full
phase-typed representation now risks being reworked or discarded once METEL-171
happens; a narrower fix that doesn't touch node shapes at all does not carry that risk.

## Decision

**For fields where `None` means resolution incompleteness (a bug, not a valid state),
stop exposing the raw `Option<SymbolId>` to downstream code. Introduce a completeness-
checked wrapper, constructed only after every such reference has actually resolved, and
give it accessors that return `SymbolId` outright — never `Option<SymbolId>` — for
those specific fields.**

Concretely, for the two pieces of remaining migration work:

1. **`Call::callee_id`'s fallback.** Finish the deferred first-class-function
   environment work so every `Call` site that should carry an id actually gets one
   during construction/elaboration — no exceptions left needing the name-lookup
   fallback. A dedicated exhaustiveness check (a debug-assertion pass, or a checked
   constructor) verifies this once, at the elaboration boundary (ADR-0037), before the
   evaluator ever sees the tree. The evaluator's `Call` handling reads a `SymbolId`
   through an accessor that cannot yield `None` — the string-lookup branch is deleted,
   not merely deprioritized.
2. **The typechecker's `TypeDefinitionRegistry`.** Rather than rekeying
   `struct_env`/`enum_env`/`method_env`/`aspect_env`/`impl_aspect_env` as
   `HashMap<SymbolId, _>` maps that individual call sites still query with
   `.get(&id)` → `Option<&T>` (the same ad hoc-optionality problem one level up), wrap
   the fully-populated registry in a sealed type (working name: `ResolvedTypeRegistry`)
   produced by a single completeness-checked construction step once all modules are
   processed. Its lookup methods for identities that must exist by that point
   (`struct_def(id: SymbolId) -> &StructDef`, not `-> Option<&StructDef>`) panic (or
   return a structured internal error, not a silent `None`) on a genuine miss, because a
   miss at that point is a resolver bug, not a valid runtime state to propagate. This is
   the `ImplKey`/coherence-pipeline foundation issue #239 and #238 actually need — and
   it should be built this way from the start rather than bolted on as more
   `Option<SymbolId>` fields and reworked later.

**What does not change:** `TypedFunDecl::def_id`/`symbol_id` for nested/local
functions, `Value::Struct`/`Value::Enum::type_id` for host-built types, and any other
field whose `None` is structurally correct stay exactly as ADR-0041 left them —
plain `Option<SymbolId>`, checked normally. This decision is scoped to the two
resolution-incompleteness cases above, not a blanket relitigation of every optional
identity field the prior migration introduced.

**Not extended to monomorphization or any other future pass yet.** A parallel proposal
to introduce a dedicated monomorphization pass under the same discipline was raised
alongside this one and deliberately held back — see
`reports/strategy/OBJECTIVES.md` (metel-docs) for the reasoning. This ADR's scope is
symbol resolution only.

## Consequences

- `wip-symbolid-migration` (ADR-0041 steps 1–3b-iii) merges as-is; nothing in it is
  reworked by this decision.
- The remaining migration work (issues #237, #238, #239) is built against this ADR's
  sealed-accessor discipline instead of extending the `Option<SymbolId>`-on-shared-
  structs pattern to the typechecker's registries.
- Downstream code (typechecker, evaluator, coherence pipeline) that needs an identity
  from a sealed accessor can treat it as always present — no `if let Some`, no
  `.unwrap()`-and-hope. A miss surfaces immediately, at construction of the sealed type,
  as a resolver bug — not later, as a silently wrong dispatch.
- This is deliberately smaller than a fully phase-typed AST (Option A, considered and
  rejected above). It does not preclude a future move to a real phase-typed IR under
  METEL-171; it also does not require one to get the correctness guarantee that
  actually matters here (no silent fallback to name lookup past the point where an id
  should exist).

## Implementation note, 2026-07-11 — two real bugs closing the fallback surfaced

The `Call::callee_id` piece (item 1) is done: `TypedLetDecl`/`TypedMutDecl` gained a
`def_id`, registered in `RuntimeRegistry` the moment their Pass 2 initializer runs (the
same moment they're bound in `env`) — closing the specific gap this ADR named. A
`let_mut_def_ids` set distinguishes a legitimate "called before its `let` executed" miss
(falls back to the existing, correct name-lookup error) from a genuine bug (anything
else missing its runtime value is now `MetelError::internal`, not a silent
re-evaluation by name).

Turning that miss into a hard error immediately surfaced two pre-existing bugs in
`SymbolId` resolution that the old blanket fallback had silently absorbed for as long
as the migration has existed — neither is specific to `let`/`mut`:

1. **Overloaded names resolved to a stale id.** `reference_resolver::resolve_name`'s
   "same-module declared name" check didn't exclude overloaded names — a bare
   reference to an overloaded name (e.g. a call that falls through to an outer/generic
   binding because no local overload matches) got whatever id the initial interning
   pass happened to assign the name, never the id anything actually registers a value
   under. Fixed: `resolve_name` now skips this check entirely for a name declared via
   more than one `fn` in the module (a new `overloaded_names` input, purely syntactic),
   falling through to imports/globs — which correctly finds the outer binding's real,
   registered id.
2. **Re-exported names got an orphaned id, not their real declaration's.** Importing a
   re-exported name (`export inner::name;` then `import facade::name;`) minted a fresh
   `SymbolId` under `(facade_module, name)` — a key nothing ever declares anything
   under, since `name` isn't actually declared in `facade`, just re-exported. Fixed:
   `name_resolver` now keeps every module's full re-export bindings (previously only
   their key names survived, into `pub_surface`) and an item import checks whether the
   source module re-exports the name before minting an id, reusing the re-export's own
   binding (already chased to its real declaring module) instead.

Both were latent in ADR-0041's migration itself, invisible only because the fallback
this ADR closes was silently and correctly re-resolving these exact cases by name every
time. One pre-existing unit test (`facade_re_exports_item_for_callers`) asserted the
first bug's *symptom* directly (`source_module == ["parser"]` for a name actually
declared in `["parser", "ast"]`) — corrected to assert the real declaring module.

Still open: the `TypeDefinitionRegistry` → `ResolvedTypeRegistry` piece (item 2, issues
#237/#239) — not started.

## Implementation note, 2026-07-11 — item 2 scoped down to `impl_aspect_env`, done

The full `ResolvedTypeRegistry` sealed wrapper (struct/enum/method definitions) was
reconsidered before starting: its only forcing function is issue #238's coherence
pipeline, which doesn't exist yet, so building it now risked exactly the speculative
rework this ADR exists to avoid. Scoped down to what issue #239 names concretely:
`impl_aspect_env` (`(target, aspect) -> type_args`), rekeyed so the **target type** is
a `SymbolId` rather than a bare string — fixing the real, concrete bug this enables:
two modules each declaring a type with the same surface name (e.g. `struct Item`)
would otherwise conflate their aspect impls, the same collision class ADR-0041 already
fixed for runtime dispatch but left open here.

**The aspect half deliberately stays name-keyed, not id-keyed** — this is the one
place this ADR's own instinct (rekey everything to ids) was wrong, caught by an actual
test failure, not by inspection. `From`/`Iterable`/aspect names generally are treated
as shared, program-wide protocol slots for this bookkeeping: a module declaring its
own `aspect From<T>` for a domain conversion still needs the *built-in* numeric `From`
cross-product to resolve in the same file (`evaluator/types/60_from_cast.mtl`), and a
module declaring its own `aspect Iterable` still needs that binding to work with its
own targets without needing the builtin's specific id
(`evaluator/aspects/59_iterable_aspect.mtl`). Resolving the aspect half through the
same shadowing-aware lookup as the target made a local declaration invisibly shadow
the builtin one for this specific bookkeeping — a real regression, not a hypothetical,
caught by 10 failing tests on the first attempt.

**Target resolution reuses `ModuleScope`, not new infrastructure.** An impl's target
type is very often imported, not locally declared, so knowing its real declaring
module needs the same lookup `reference_resolver` already does for expression
`Ident`s — local declaration, then explicit import, then glob (user tier before std).
`TypeDefinitionRegistry` now carries `Rc`-shared copies of the global symbol table and
every module's import scope (set once when built) and a `resolve_type_position_id`
helper mirroring that precedence, so `register_aspect_impl`/`impl_aspect_env_has`/
`has_from_impl`/`iterable_elem_type`'s public signatures gained a `current_module`
parameter but stay otherwise name-based — no ripple into the typechecker's
construction/inference call sites beyond passing that one extra parameter through
context that was already available at every call site (`ConstructCtx::current_module`,
`InferContext::current_module_path()`).

Regression test: `typechecking/cross_module_same_named_type_impl_isolation` — two
modules each declare their own `struct Item`, only one implements `Labelled`; a bound
check against the other must fail (`T0012`), not incorrectly succeed via a bare-string
match against the wrong module's impl.

Full suite green (638 tests, up from 636). The `struct_env`/`enum_env`/`method_env`
sealed-accessor piece remains genuinely deferred, per the reasoning above — revisit
once #238 has a concrete consumer for it.
