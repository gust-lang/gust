---
id: adr-0038
title: "Free-Function Overloading: Exact-Match Selection, SymbolId Dispatch"
date: '2026-06-11'
status: active
---

## Context

Sprint 22 (METEL-180) added free-function overloading: a module may declare
more than one `fun` with the same name, distinguished by parameter types. This
forced a decision the interpreter had avoided until now — what identifies a
callable after typechecking? Every dispatch surface (scheme env, lexical
environment, GlobalExports) was keyed by surface name, which cannot identify
one definition out of an overload set.

Two sub-decisions were made, one about *selection* (which candidate a call
site picks) and one about *identity* (how the picked candidate is referenced
through the rest of the pipeline).

An intermediate name-mangling design (`print$i32`, `print$i64` — construction
rewrote declaration names and call sites) shipped first and was deleted within
the same sprint once SymbolId dispatch landed; it is documented here only so
the history of `overload.rs` makes sense.

## Decision

### Selection: exact match only

A candidate matches a call site only when the argument types equal its
parameter types exactly. Implicit numeric `From` coercion does **not**
participate in overload selection (it still applies to non-overloaded calls).

Rationale: coercion makes multiple candidates viable for one call (`1i32`
matches `i32` exactly *and* `i64` via coercion), which forces a specificity
ranking with all its edge cases. Exact match keeps resolution a one-line
predicate (`overload::select`) and keeps diagnostics trivial: the
no-match error lists every candidate signature verbatim.

Constraints that follow from this: overloaded functions must be non-generic
and fully parameter-annotated (each definition needs a distinct concrete
signature), and duplicate signatures under one name are a `T0011` error.
The overload table is per module; user overloads are not exportable.

Bare numeric literals are defaulted before selection (a bare `42` is `i64`),
and the selected candidate's parameter types are constrained back onto the
arguments — so literals participate in selection the same way they type
everywhere else, and a no-match call reports the full candidate list.

**Fallback to outer bindings.** A local overload set EXTENDS a non-overload
binding of the same name from an outer source (prelude/imports) rather than
replacing it: exact-match candidates win; when none matches, the call falls
back to the outer binding (e.g. a module overloading `print(i32)`/`print(i64)`
still reaches the generic std::core `print<T>` for an `i8`). A name with no
outer binding reports the candidate list. Note the asymmetry with single
declarations: a lone local `fun print(...)` shadows the outer binding fully
(normal scoping); only *overload sets* get fallback dispatch. This n=1 → n=2
discontinuity is a known wart — shadow-vs-extend intent is currently inferred
from declaration count. A single coherent rule (unify, consistent hiding, or
Julia-style explicit extension syntax) is part of METEL-188's RFC scope,
where the same question recurs for imported overload sets.

**std::core exception (seeding, not export).** std::core declares the first
overloaded stdlib function (`assert(cond)` / `assert(cond, msg)`), and every
module must see it. Since overload sets do not flow through
GlobalExports/imports, the embedded core's overload groups are seeded into
every module's table by `build_overload_table` — the same derive-from-
`core_program()` pattern the registry, prelude, and runtime already use
(ADR-0039). The canonical entries live in a process-wide
`core_overload_table()` so call sites in every module and the runtime host
registration agree on each definition's `SymbolId`. A module declaring its
own `fun` with a seeded name shadows the std::core group entirely. A general
"exportable overload sets" mechanism (overloads in GlobalExports, resolved
through normal imports) is future work — METEL-188; if it lands, the
graph-path seeding becomes an ordinary import while the single-program path
(which performs no imports) keeps the seeding.

### Identity: SymbolId per definition, stamped at the call site

Each overloaded definition gets a unique `SymbolId` from a dedicated range
(`symbols::OVERLOAD_SYM_START = 0x4000_0000`, process-global atomic allocator
in `typechecker::overload`). The range is disjoint from the name-resolver's
user range (1000+) so the two allocators never need to coordinate.

The flow:

1. `overload::build_overload_table` (per module, pre-inference) groups
   same-name `fun` decls, validates them, and assigns each entry a
   `SymbolId` (`OverloadEntry { params, ret, symbol_id }`).
2. Inference checks each overloaded body independently but registers
   **nothing** under the shared name: overloads never enter the name-keyed
   scheme env, the poly env, or the export surface.
3. Construction selects the candidate by exact match against the typed
   argument types and stamps `TypedExpr::Call::callee_id = Some(symbol_id)`.
   The typed declaration carries the same id (`TypedFunDecl::symbol_id`).
4. The evaluator registers overloaded definitions in a SymbolId-keyed map
   (`RuntimeRegistry::symbol_values`) instead of the lexical environment, and
   a `Call` with `callee_id: Some(id)` dispatches through that map without
   evaluating the callee expression.

`callee_id: None` (every non-overloaded call) preserves the existing path:
evaluate the callee expression, which for a named function is a lexical-env
lookup.

### What was deliberately NOT built

- **No `CalleeId` enum.** The sprint guide sketched
  `CalleeId::{Free, Method, AspectMethod}`. Only free functions can be
  overloaded today, so `Option<SymbolId>` on `Call` carries everything needed.
  The enum can be introduced when method dispatch is rekeyed (METEL-185)
  without unwinding anything done here.
- **Method-level SymbolId dispatch.** `MethodCall` still selects methods by
  name within a type entry; the aspect itself is already id-resolved
  (`MethodDispatch::Aspect { aspect_id }`, ADR-0037 era). Folded into
  METEL-185.
- **Symbol-keyed lookup for ordinary functions.** Functions are first-class
  values in a lexical environment; rekeying that environment is an
  architectural question tracked separately (METEL-187).

## Consequences

- Names never disambiguate overloads anywhere in the pipeline. Grepping for
  `mangle` in `src/` returns nothing; the selection logic
  (`build_overload_table`, `select`, `no_match_error`) is the permanent part
  of `typechecker/overload.rs`.
- Because overloads are invisible to the scheme env, a bare reference to an
  overloaded name (`let f = describe;`) is an undefined-name error, and
  overloaded functions cannot be `pub`-exported. Both are acceptable for the
  current scope and would need explicit design (probably expected-type-driven
  selection) to lift.
- Overload SymbolIds are process-unique but not stable across runs. Nothing
  persists them; if a future incremental-compilation layer needs stability,
  allocation must move into the name resolver next to the user range.
- A call site that types correctly always finds its symbol at runtime; a miss
  in `symbol_values` is an internal error, not a user-facing panic.
- Argument types must be known at the call site for selection. Bare numeric
  literals are defaulted (`42` → `i64`) before selecting, so they resolve like
  they do everywhere else; arguments whose type is still a genuine inference
  variable produce a `T0002` "cannot resolve argument types for overloaded
  call" error asking for an annotation — the documented cost of exact-match
  selection.
