---
id: adr-0043
title: "Recovering Generic Type Arguments from Field Values, Not Tagging Value::Struct/Enum"
date: '2026-07-12'
status: accepted
relates: adr-0041
implements: issue #267
---

## Context

`Value::Struct`/`Value::Enum` (`src/evaluator/mod.rs`) carry a `name` and a `fields:
HashMap<String, Value>`, but no record of what a generic type's parameters were
instantiated to for that particular instance — `Wrapper { value: 5 }`'s only
intrinsic type tag is bare `Named("Wrapper", [])`. `type_of::value_to_type`, used to
derive argument types for `construct_generic_body` (construction-at-call-time for
deferred generic method/function bodies, `ClosureBody::Untyped`), inherited this
erasure and returned the same bare `Named(name, [])` for any struct/enum value,
regardless of what `T` actually was.

This was mostly latent — a generic method body that never inspects a `T`-typed
value's own capabilities never needed the parameter resolved to anything concrete.
It surfaced as issue #267: a method on `Wrapper<T>` calling `self.value.to_string()`
(a `Display`-bounded method on the `T`-typed field) failed to construct, because
`construct_generic_body`'s unification of the receiver's declared generic type
(`Named("Wrapper", [TypeVar])`, from the scheme) against the erased runtime-derived
type (`Named("Wrapper", [])`, zero args) fails outright on an arity mismatch — the two
`Named` shapes don't even have the same number of type arguments to unify pairwise.
Unification failures here are tolerated by design (see `construct_generic_body`'s own
comment: "the typechecker already validated the program; here we only need a 'good
enough' substitution for construction"), so the miss was silently absorbed and the
type parameter defaulted to `Unit` — the field then had no `to_string` method,
because `Unit` doesn't have one.

## Alternatives considered

**Tag `Value::Struct`/`Value::Enum` with their own `type_args: Vec<Type>`.** The
direct fix: compute the concrete type args once, at construction time (the typed AST
node building the struct/enum literal already knows its own fully-resolved
`Type::Named(name, args)` from Pass 2), and carry them on the runtime value from then
on. Rejected for this issue: `Value` is the single most pattern-matched type in the
evaluator — pervasive exhaustive matches in cloning, display/`to_string` formatting,
pattern matching, equality, and every runtime dispatch path would all need a new
field threaded through, for a problem that, in practice, only matters at the one
call-time-reconstruction seam. A localized fix that doesn't touch `Value`'s shape at
all carries much less risk of an unrelated regression elsewhere in the evaluator.

## Decision

**Recover type arguments on demand, from field values, only at the point
`construct_generic_body` actually needs them — do not change `Value`'s shape.**

`typechecker::infer_named_type_args` (`src/typechecker/mod.rs`) takes a struct/enum's
name (and variant, for enums), the already-computed `Type` of each of its fields (the
evaluator recurses over the live value to get these), and the registry's declared
field-type templates (`FieldEntry.ty: InferType`, which is `Var(tv)` for a field whose
declared type is a bare generic parameter) plus the type's own quantified `TypeVar`s.
It unifies each field's declared template against that field's actual type — the same
`unify`/`Substitution` machinery `construct_generic_body` itself already uses, applied
one level down, per field, instead of once against the whole erased receiver type —
then reads each quantified type parameter back out of the resulting substitution.
Best-effort, same tolerance as `construct_generic_body`'s own top-level attempt: a
field that doesn't mention a given type param at all (e.g. `Perhaps::None`, no
payload) leaves it unresolved, still defaulted to `Unit`, exactly as before this fix
for that specific case — this only fixes the case where *some* field's actual value
does carry the information.

`type_of::value_to_type` (`src/evaluator/type_of.rs`) gained `registry: &
TypeDefinitionRegistry` and `span: &Span` parameters to call this for `Value::Struct`/
`Value::Enum`; its three call sites in `src/evaluator/call.rs` pass the `type_ctx`'s
registry already available there. One call site (`call_method_function`'s early
receiver-type capture, needed before `closure.body` is even matched on) doesn't always
have a real `type_ctx` yet — a synthetic empty `TypeDefinitionRegistry::default()`
covers that fallback, which is harmless because the only consumer of that particular
value (`ClosureBody::Typed`, non-generic) never actually reads the recovered type args.

## Consequences

- Calling a bounded method (e.g. `to_string()` via `Display`) on a generic-typed
  struct/enum field, from inside a deferred generic method body, now resolves
  correctly instead of seeing the field as `Unit`.
- `Value::Struct`/`Value::Enum`'s shape is unchanged — no ripple into the many
  exhaustive matches over `Value` elsewhere in the evaluator.
- The recovery is still best-effort and per-call-site: it does not make runtime
  values type-argument-aware in general, only at this one reconstruction seam. A
  future need to know a struct/enum instance's own type arguments somewhere else in
  the evaluator (rather than only here) would still need the fuller `Value` tagging
  approach this ADR rejected for the narrower problem at hand.
