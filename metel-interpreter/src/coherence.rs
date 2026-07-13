//! Aspect implementation coherence: the orphan rule (`T0014`) and overlap
//! detection (`T0015`) for concrete impls (RFC-0060, issue #238).
//!
//! Runs after path normalization, before type-checking. It only needs to
//! resolve type and aspect *names* to their declaring module — exactly what
//! `ResolvedNames` already provides — so it works directly over each
//! module's `Decl::Impl` blocks rather than needing inferred types.
//!
//! Scope (ADR-0042 + RFC-0036): concrete impls AND conditional/blanket impls
//! with generic parameters are checked. `CanonicalType::TypeParam` represents
//! impl-scoped type variables during overlap detection. §3.1 (negation
//! disjointness) and §3.2 (unconditional vs. conditional conflict) are
//! handled via `provably_disjoint` checks on `scoped_type_param_bounds`.

use std::collections::HashMap;

use crate::ast::{Decl, ImplBlock, Span, TypeExpr, WhereClause};
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
    Reference(Box<CanonicalType>),
    MutReference(Box<CanonicalType>),
    Fun(Vec<CanonicalType>, Option<Box<CanonicalType>>),
    /// `impl Aspect` in parameter position — not expected in an impl's own
    /// target type, kept only so canonicalization stays total.
    Opaque,
    /// An impl-scoped type parameter (e.g. `T` in `impl<T: Copy> ... for Pair<T, T>`),
    /// keyed by its position in the target type's top-level arguments.
    /// Two conditional impls with different letters (`T` vs `U`) at the same
    /// position canonicalize identically — required for §3.1 overlap detection.
    TypeParam(usize),
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
        TypeExpr::Reference(inner) => CanonicalType::Reference(Box::new(go(inner))),
        TypeExpr::MutReference(inner) => CanonicalType::MutReference(Box::new(go(inner))),
        TypeExpr::Fun(params, ret) => CanonicalType::Fun(
            params.iter().map(go).collect(),
            ret.as_deref().map(go).map(Box::new),
        ),
        // `T::AssocType` (RFC-0082) isn't resolved to a concrete type at this pass
        // either — both stay opaque until issue #242 does that resolution for real.
        TypeExpr::ImplAspect { .. } | TypeExpr::Projection { .. } => CanonicalType::Opaque,
    }
}

/// Canonicalize an impl's target type for overlap detection: top-level unresolved
/// names (impl-scoped type parameters like `T` in `impl<T> ... for Pair<T, T>`)
/// are mapped to `CanonicalType::TypeParam(i)` by position, so `Pair<T, T>` and
/// `Pair<U, U>` canonicalize identically. Non-top-level positions and resolved
/// names use the ordinary `canonicalize` path.
fn canonicalize_impl_target(
    names: &ResolvedNames,
    current_module: &[String],
    ib: &ImplBlock,
) -> CanonicalType {
    // Collect the set of impl-scoped type param names (from ib.generics)
    // so we can identify them in the target type's top-level arguments.
    let impl_param_names: std::collections::HashSet<&str> =
        ib.generics.iter().map(|g| g.name.as_str()).collect();

    match &ib.target_type {
        TypeExpr::Named(target_name, args) if !args.is_empty() => {
            // Top-level args: map each to TypeParam(i) if it's an impl param,
            // else canonicalize normally.
            let cargs: Vec<CanonicalType> = args
                .iter()
                .enumerate()
                .map(|(i, arg)| {
                    if let TypeExpr::Named(n, inner_args) = arg {
                        if inner_args.is_empty() && impl_param_names.contains(n.as_str()) {
                            return CanonicalType::TypeParam(i);
                        }
                    }
                    canonicalize(names, current_module, arg)
                })
                .collect();
            match resolve_id(names, current_module, target_name) {
                Some(id) => CanonicalType::Resolved(id, cargs),
                None => CanonicalType::Unresolved(target_name.clone(), cargs),
            }
        }
        _ => canonicalize(names, current_module, &ib.target_type),
    }
}

/// Collect the scoped type-param bounds for an impl block, indexed by the
/// target type's top-level argument position. Returns `(pos_bounds, neg_bounds)`
/// where `pos_bounds[i]` is the list of positive aspect names required of the
/// type at position `i`, and `neg_bounds[i]` is the list of negative aspect
/// names. Both are empty vectors for unconditional impls, or for blanket/
/// structural impls whose target type is not a named type with top-level args
/// (e.g. `impl<T> Display for T[]`).
fn scoped_type_param_bounds(ib: &ImplBlock) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    // Only Named targets with top-level args have meaningful positions.
    let arg_count = match &ib.target_type {
        TypeExpr::Named(_, args) => args.len(),
        _ => return (vec![], vec![]),
    };

    // Build the set of impl param names.
    let impl_param_names: std::collections::HashSet<&str> =
        ib.generics.iter().map(|g| g.name.as_str()).collect();

    let mut pos_bounds: Vec<Vec<String>> = vec![vec![]; arg_count];
    let mut neg_bounds: Vec<Vec<String>> = vec![vec![]; arg_count];

    // Build a name → target-arg-position map by scanning the target type's
    // top-level arguments. A type param at `ib.generics` position 0 may appear
    // at target arg position 1 (e.g. `impl<T, U> ... for Pair<U, T>`).
    let target_args = match &ib.target_type {
        TypeExpr::Named(_, args) => args,
        _ => return (vec![], vec![]),
    };
    let name_to_target_pos: HashMap<&str, usize> = target_args
        .iter()
        .enumerate()
        .filter_map(|(i, arg)| {
            if let TypeExpr::Named(n, _) = arg {
                if impl_param_names.contains(n.as_str()) {
                    return Some((n.as_str(), i));
                }
            }
            None
        })
        .collect();

    // From inline bounds: `impl<T: Copy> ...` → pos_bounds for T's target position
    for gp in &ib.generics {
        if let Some(&pos) = name_to_target_pos.get(gp.name.as_str()) {
            for bound in &gp.bounds {
                if bound.polarity == crate::ast::Polarity::Positive {
                    if let TypeExpr::Named(aspect, _) = &bound.aspect {
                        pos_bounds[pos].push(aspect.clone());
                    }
                } else {
                    if let TypeExpr::Named(aspect, _) = &bound.aspect {
                        neg_bounds[pos].push(aspect.clone());
                    }
                }
            }
        }
    }

    // From where clause: `where T: Copy` → same merge
    if let Some(wc) = &ib.where_clause {
        merge_where_clause_bounds(&wc, &name_to_target_pos, &impl_param_names, &mut pos_bounds, &mut neg_bounds);
    }

    (pos_bounds, neg_bounds)
}

/// Merge where-clause constraints into the pos/neg bound vectors.
fn merge_where_clause_bounds(
    wc: &WhereClause,
    name_to_pos: &HashMap<&str, usize>,
    impl_param_names: &std::collections::HashSet<&str>,
    pos_bounds: &mut Vec<Vec<String>>,
    neg_bounds: &mut Vec<Vec<String>>,
) {
    for (type_param_name, bounds) in &wc.constraints {
        if !impl_param_names.contains(type_param_name.as_str()) {
            continue;
        }
        if let Some(&pos) = name_to_pos.get(type_param_name.as_str()) {
            for bound in bounds {
                if bound.polarity == crate::ast::Polarity::Positive {
                    if let TypeExpr::Named(aspect, _) = &bound.aspect {
                        pos_bounds[pos].push(aspect.clone());
                    }
                } else {
                    if let TypeExpr::Named(aspect, _) = &bound.aspect {
                        neg_bounds[pos].push(aspect.clone());
                    }
                }
            }
        }
    }
}

/// Two sets of conditional impl bounds are provably disjoint iff, at some
/// position `i`, one impl's positive bound set contains an aspect name present
/// in the other's negative bound set at the same position. This is §3.1's
/// disjointness criterion: `impl<T: Copy> ...` and `impl<T: !Copy> ...` have
/// `Copy` in pos[0] of the first and `Copy` in neg[0] of the second.
fn provably_disjoint(
    (a_pos, a_neg): &(Vec<Vec<String>>, Vec<Vec<String>>),
    (b_pos, b_neg): &(Vec<Vec<String>>, Vec<Vec<String>>),
) -> bool {
    let len = a_pos.len().max(b_pos.len());
    for i in 0..len {
        let a_p = a_pos.get(i).map_or(&[] as &[String], |v| v);
        let a_n = a_neg.get(i).map_or(&[] as &[String], |v| v);
        let b_p = b_pos.get(i).map_or(&[] as &[String], |v| v);
        let b_n = b_neg.get(i).map_or(&[] as &[String], |v| v);
        // a's positive intersects b's negative → disjoint
        if a_p.iter().any(|a| b_n.contains(a)) {
            return true;
        }
        // b's positive intersects a's negative → disjoint
        if b_p.iter().any(|b| a_n.contains(b)) {
            return true;
        }
    }
    false
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
    /// Scoped type-param bounds for §3.1/§3.2 overlap checks.
    scoped_bounds: (Vec<Vec<String>>, Vec<Vec<String>>),
    span: &'a Span,
}

fn is_local(declaring: &HashMap<SymbolId, Vec<String>>, id: Option<SymbolId>, module: &[String]) -> bool {
    id.and_then(|id| declaring.get(&id))
        .is_some_and(|m| m.as_slice() == module)
}

/// Check the orphan rule (T0014) and overlap detection (T0015) for every
/// concrete `impl Aspect for Type` block in the program. See RFC-0060 / the
/// "Aspect Implementation Coherence" section of `declarations.md`.
///
/// # Errors
/// Returns an error if any `impl` violates the orphan rule (T0014) or overlaps
/// with another `impl` of the same aspect/type instantiation (T0015).
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
            // Use canonicalize_impl_target so TypeParam(i) is produced for
            // impl-scoped type variables — required for §3.1/§3.2 overlap detection.
            let canonical_target = canonicalize_impl_target(names, &module.module_path, ib);
            let scoped_bounds = scoped_type_param_bounds(ib);
            impls.push(CollectedImpl {
                module: &module.module_path,
                aspect_name,
                aspect_id,
                target_local,
                canonical_key: (canonical_args, canonical_target),
                scoped_bounds,
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
    // covering the same canonicalized target type conflict — unless they are
    // provably disjoint via §3.1 (e.g. `T: Copy` vs. `T: !Copy`).
    // Group impls by canonical key, then scan pairwise within each group.
    let mut groups: HashMap<(SymbolId, Vec<CanonicalType>, CanonicalType), Vec<&CollectedImpl>> =
        HashMap::new();
    for imp in &impls {
        let Some(aspect_id) = imp.aspect_id else {
            continue;
        };
        let key = (aspect_id, imp.canonical_key.0.clone(), imp.canonical_key.1.clone());
        groups.entry(key).or_default().push(imp);
    }

    for group in groups.values() {
        // Pairwise check within the group.
        for i in 0..group.len() {
            for j in (i + 1)..group.len() {
                let a = group[i];
                let b = group[j];
                // §3.1: if the two impls are provably disjoint (one's positive
                // bound is the other's negative bound at some position), skip.
                if provably_disjoint(&a.scoped_bounds, &b.scoped_bounds) {
                    continue;
                }
                return Err(MetelError::type_error(
                    TypeErrorCode::T0015,
                    format!(
                        "conflicting implementation: `{}` is already implemented for this type at {}:{}:{}",
                        a.aspect_name,
                        a.span.filename,
                        a.span.line,
                        a.span.col
                    ),
                    b.span,
                ));
            }
        }
    }

    Ok(())
}
