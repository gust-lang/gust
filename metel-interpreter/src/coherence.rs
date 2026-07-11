//! Aspect implementation coherence: the orphan rule (`T0014`) and overlap
//! detection (`T0015`) for concrete impls (RFC-0060, issue #238).
//!
//! Runs after path normalization, before type-checking. It only needs to
//! resolve type and aspect *names* to their declaring module — exactly what
//! `ResolvedNames` already provides — so it works directly over each
//! module's `Decl::Impl` blocks rather than needing inferred types.
//!
//! Scope (ADR-0042): only concrete impls are checked. `ImplBlock` has no
//! generics/where-clause field today, so conditional/blanket impls
//! (RFC-0036) aren't parseable yet; `AspectDecl` has no auto-impl marker, so
//! auto-derived aspects (RFC-0080) don't exist yet either. Both are deferred
//! until those RFCs land — see the "Aspect Implementation Coherence" section
//! of `declarations.md`.

use std::collections::HashMap;

use crate::ast::{Decl, Span, TypeExpr};
use crate::error::{MetelError, TypeErrorCode};
use crate::name_resolver::{GlobTier, ResolvedNames};
use crate::path_normalizer::NormalizedModuleGraph;
use crate::symbols::SymbolId;

/// Resolve a bare type- or aspect-position name to its declaring `SymbolId`,
/// from the perspective of `current_module`. Mirrors the precedence used by
/// `reference_resolver::resolve_name` and
/// `typeinference::TypeDefinitionRegistry::resolve_type_position_id` (local
/// declaration -> explicit import -> glob, user tier before std) — duplicated
/// here in miniature because coherence runs before `TypeDefinitionRegistry`
/// exists.
fn resolve_id(names: &ResolvedNames, current_module: &[String], name: &str) -> Option<SymbolId> {
    if let Some(id) = names
        .symbols
        .get(&(current_module.to_vec(), name.to_string()))
    {
        return Some(*id);
    }
    let scope = names.scopes.get(current_module)?;
    if let Some(binding) = scope.explicit.get(name) {
        return Some(binding.symbol_id);
    }
    let mut std_hit = None;
    for (tier, glob_module) in &scope.globs {
        if let Some(id) = names.symbols.get(&(glob_module.clone(), name.to_string())) {
            match tier {
                GlobTier::User => return Some(*id),
                GlobTier::Std => std_hit = std_hit.or(Some(*id)),
            }
        }
    }
    std_hit
}

/// `SymbolId -> declaring module`, inverted from `names.symbols`'
/// `(module, name) -> id` map. Every id has exactly one canonical declaring
/// entry: `SymbolTable::intern` dedups on the full `(module, name)` key, and
/// import bindings reuse the source declaration's id rather than minting a
/// second one under the importing module — so the inversion is unambiguous.
fn declaring_modules(names: &ResolvedNames) -> HashMap<SymbolId, Vec<String>> {
    names
        .symbols
        .iter()
        .map(|((module, _name), id)| (*id, module.clone()))
        .collect()
}

/// A structurally comparable form of `TypeExpr`, with `Named` constructors
/// resolved to their declaring `SymbolId` so that two spellings of the same
/// type (a local name vs. an imported alias) compare equal for overlap
/// detection. Only concrete impls exist today (no generics on `ImplBlock`),
/// so canonicalization never needs to account for bound type variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CanonicalType {
    Resolved(SymbolId, Vec<CanonicalType>),
    Unresolved(String, Vec<CanonicalType>),
    Unit,
    Tuple(Vec<CanonicalType>),
    Array(Box<CanonicalType>),
    SizedArray(Box<CanonicalType>, u64),
    Pointer(Box<CanonicalType>),
    MutPointer(Box<CanonicalType>),
    Fun(Vec<CanonicalType>, Option<Box<CanonicalType>>),
    /// `impl Aspect` in parameter position — not expected in an impl's own
    /// target type, kept only so canonicalization stays total.
    Opaque,
}

fn canonicalize(names: &ResolvedNames, current_module: &[String], ty: &TypeExpr) -> CanonicalType {
    let go = |t: &TypeExpr| canonicalize(names, current_module, t);
    match ty {
        TypeExpr::Named(name, args) => {
            let cargs: Vec<_> = args.iter().map(go).collect();
            match resolve_id(names, current_module, name) {
                Some(id) => CanonicalType::Resolved(id, cargs),
                None => CanonicalType::Unresolved(name.clone(), cargs),
            }
        }
        TypeExpr::Unit => CanonicalType::Unit,
        TypeExpr::Tuple(items) => CanonicalType::Tuple(items.iter().map(go).collect()),
        TypeExpr::Array(inner) => CanonicalType::Array(Box::new(go(inner))),
        TypeExpr::SizedArray(inner, n) => CanonicalType::SizedArray(Box::new(go(inner)), *n),
        TypeExpr::Pointer(inner) => CanonicalType::Pointer(Box::new(go(inner))),
        TypeExpr::MutPointer(inner) => CanonicalType::MutPointer(Box::new(go(inner))),
        TypeExpr::Fun(params, ret) => CanonicalType::Fun(
            params.iter().map(go).collect(),
            ret.as_deref().map(go).map(Box::new),
        ),
        TypeExpr::ImplAspect { .. } => CanonicalType::Opaque,
    }
}

/// The declaring `SymbolId` of a type expression's own outermost constructor,
/// if it names one at all. Structural types (tuples, arrays, pointers,
/// function types) have no owning module, so they can never satisfy the
/// orphan rule's "local" half on their own — only `Named` types can.
fn outermost_id(names: &ResolvedNames, current_module: &[String], ty: &TypeExpr) -> Option<SymbolId> {
    match ty {
        TypeExpr::Named(name, _) => resolve_id(names, current_module, name),
        _ => None,
    }
}

struct CollectedImpl<'a> {
    module: &'a [String],
    aspect_name: &'a str,
    aspect_id: Option<SymbolId>,
    target_local: bool,
    /// The aspect's own type arguments (e.g. `i64` in `impl From<i64> for f64`)
    /// plus the target type, canonicalized. `From<i64> for f64` and
    /// `From<u8> for f64` both target `f64` but are different impls of the
    /// aspect — overlap is about the *whole* instantiation, not the target
    /// type alone.
    canonical_key: (Vec<CanonicalType>, CanonicalType),
    span: &'a Span,
}

fn is_local(declaring: &HashMap<SymbolId, Vec<String>>, id: Option<SymbolId>, module: &[String]) -> bool {
    id.and_then(|id| declaring.get(&id))
        .map(|m| m.as_slice() == module)
        .unwrap_or(false)
}

/// Check the orphan rule (T0014) and overlap detection (T0015) for every
/// concrete `impl Aspect for Type` block in the program. See RFC-0060 / the
/// "Aspect Implementation Coherence" section of `declarations.md`.
pub fn check(graph: &NormalizedModuleGraph, names: &ResolvedNames) -> Result<(), MetelError> {
    let declaring = declaring_modules(names);

    let mut impls: Vec<CollectedImpl> = Vec::new();
    for module in graph.modules() {
        for decl in &module.program.decls {
            let Decl::Impl(ib) = decl else { continue };
            let Some(aspect_name) = ib.aspect_name.as_deref() else {
                continue; // inherent impl (no aspect) — nothing to check
            };
            let aspect_id = resolve_id(names, &module.module_path, aspect_name);
            let target_local = is_local(
                &declaring,
                outermost_id(names, &module.module_path, &ib.target_type),
                &module.module_path,
            );
            let canonical_args = ib
                .aspect_type_args
                .iter()
                .map(|a| canonicalize(names, &module.module_path, a))
                .collect();
            let canonical_target = canonicalize(names, &module.module_path, &ib.target_type);
            impls.push(CollectedImpl {
                module: &module.module_path,
                aspect_name,
                aspect_id,
                target_local,
                canonical_key: (canonical_args, canonical_target),
                span: &ib.span,
            });
        }
    }

    // Orphan rule (T0014). Skipped when the aspect name itself doesn't
    // resolve — an undefined aspect is a more fundamental error the
    // typechecker will report on its own (T0003), and layering a coherence
    // error on top of it would only obscure the real problem.
    for imp in &impls {
        let Some(aspect_id) = imp.aspect_id else {
            continue;
        };
        let aspect_local = is_local(&declaring, Some(aspect_id), imp.module);
        if !aspect_local && !imp.target_local {
            return Err(MetelError::type_error(
                TypeErrorCode::T0014,
                format!(
                    "orphan implementation: neither `{}` nor the target type is local to this module",
                    imp.aspect_name
                ),
                imp.span,
            ));
        }
    }

    // Overlap detection (T0015): two impls of the same resolved aspect
    // covering the same canonicalized concrete target type conflict. The
    // orphan rule above already confines any possible overlap to a single
    // module (or a module and `std::core`), so a flat global scan is enough.
    let mut seen: HashMap<(SymbolId, Vec<CanonicalType>, CanonicalType), &Span> = HashMap::new();
    for imp in &impls {
        let Some(aspect_id) = imp.aspect_id else {
            continue;
        };
        let key = (aspect_id, imp.canonical_key.0.clone(), imp.canonical_key.1.clone());
        if let Some(prior_span) = seen.get(&key) {
            return Err(MetelError::type_error(
                TypeErrorCode::T0015,
                format!(
                    "conflicting implementation: `{}` is already implemented for this type at {}:{}:{}",
                    imp.aspect_name, prior_span.filename, prior_span.line, prior_span.col
                ),
                imp.span,
            ));
        }
        seen.insert(key, imp.span);
    }

    Ok(())
}
