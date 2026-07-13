use std::collections::HashMap;

use crate::ast::{
    AspectMethod, AssignOp, AssignTarget, BinOp, Block, Bound, Decl, Expr, ForInit, FunDecl,
    GenericParam, ImplBlock, Literal, MatchExpr, Param, Pattern, Polarity, Program, Span, Stmt,
    TypeExpr, UnaryOp, Visibility,
};
use crate::error::{MetelError, TypeErrorCode};
use crate::typeinference::{
    free_vars, generalize, EnumInfo, FieldEntry, InferContext, InferType, Substitution, TypeScheme,
    TypeVar, VariantInfo,
};
use crate::types::Type;

use super::conversions::{
    infer_type_to_type, type_expr_to_infer, type_expr_to_infer_with_generics,
    type_expr_to_infer_with_generics_and_self, type_expr_to_infer_with_self, type_to_infer,
};
use super::FunGeneralization;

/// Build the per-quantified-var `assoc_projections` map from the body's recorded
/// projection log and the post-solve substitution. Each entry
/// `(base_tv, aspect, assoc, placeholder)` is resolved through `subst`; if
/// `base_tv` is still a free `Var` after resolution, it's a quantified var in
/// the scheme and gets its projection info attached.
fn build_assoc_projection_map(
    body_assoc_log: &[(TypeVar, String, String, TypeVar)],
    subst: &crate::typeinference::Substitution,
    scheme: &crate::typeinference::TypeScheme,
) -> std::collections::HashMap<TypeVar, (usize, String, String, TypeVar)> {
    use std::collections::HashMap;
    let mut map: HashMap<TypeVar, (usize, String, String, TypeVar)> = HashMap::new();
    for (base_tv, aspect, assoc, placeholder) in body_assoc_log {
        let InferType::Var(resolved) = subst.apply(&InferType::Var(*base_tv)) else {
            continue;
        };
        if let Some(pos) = scheme.quantified_vars.iter().position(|&v| v == resolved) {
            map.entry(resolved)
                .or_insert_with(|| (pos, aspect.clone(), assoc.clone(), *placeholder));
        }
    }
    map
}

/// Resolve a type annotation, substituting any name that matches the current
/// function's generic type params with the corresponding `TypeVar` rather than
/// producing a Named type.  Must be used for all annotations inside function
/// bodies; bare `type_expr_to_infer` ignores the param map.
fn ann_to_infer(te: &TypeExpr, ctx: &InferContext) -> InferType {
    let params = ctx.type_params();
    if params.is_empty() {
        type_expr_to_infer(te)
    } else {
        // Check for abstract-case projection first.
        if let TypeExpr::Projection {
            base,
            ref assoc_name,
            ..
        } = te
        {
            if let TypeExpr::Named(ref n, _) = **base {
                if let Some(&base_tv) = params.get(n.as_str()) {
                    if let Some(bounds) = ctx.bounds_for_type_var(base_tv) {
                        for aspect in bounds {
                            if let Some(decls) = ctx.registry().aspect_assoc_type_decls(aspect) {
                                if decls.iter().any(|d| d.name == *assoc_name) {
                                    // Cannot mint a new var here (need &mut ctx).
                                    // Return a Named placeholder; the caller that has
                                    // &mut ctx (infer_fun_decl/infer_impl_method) handles
                                    // the real projection var minting.
                                    return InferType::Named(format!("{n}::{assoc_name}"), vec![]);
                                }
                            }
                        }
                    }
                }
            }
        }
        type_expr_to_infer_with_generics(te, params)
    }
}

/// Register the names of all direct `FunDecl`s in `decls` with fresh type
/// variables so that forward references and mutual recursion work.
/// The function type of a `native` declaration, built from its annotations,
/// plus the aspect bounds of its generic params keyed by their `TypeVars` (so
/// the caller can attach them to the generalized scheme).
struct NativeFunTyResult {
    fun_ty: InferType,
    bounds: HashMap<TypeVar, Vec<String>>,
    neg_bounds: HashMap<TypeVar, Vec<String>>,
    assoc_eq: HashMap<TypeVar, Vec<(String, String, InferType)>>,
}

fn native_fun_ty(
    fun: &FunDecl,
    ctx: &mut InferContext,
) -> Result<NativeFunTyResult, MetelError> {
    // Generic native functions (e.g. `print<T: Display>`) map each type
    // parameter to a fresh TypeVar; the caller generalizes the result into a
    // polymorphic scheme carrying the bounds.
    let generic_map = fun_generic_map(fun, ctx);
    let bounds_by_var = collect_fun_type_var_bounds(fun, &generic_map);
    let neg_bounds_by_var = collect_negative_fun_type_var_bounds(fun, &generic_map);
    let assoc_eq_by_var = collect_fun_assoc_eq_constraints(fun, &generic_map);
    let te_to_infer = |te: &TypeExpr| -> InferType {
        if generic_map.is_empty() {
            type_expr_to_infer(te)
        } else {
            type_expr_to_infer_with_generics(te, &generic_map)
        }
    };
    let mut param_types = Vec::with_capacity(fun.params.len());
    for p in &fun.params {
        let ann = p.type_ann.as_ref().ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0002,
                format!(
                    "native function `{}` requires a type annotation on every parameter",
                    fun.name
                ),
                &p.span,
            )
        })?;
        param_types.push(te_to_infer(ann));
    }
    let ret_ty = match &fun.return_type {
        Some(te) => te_to_infer(te),
        None => InferType::unit(),
    };
    Ok(NativeFunTyResult {
        fun_ty: InferType::Fun(param_types, Box::new(ret_ty)),
        bounds: bounds_by_var,
        neg_bounds: neg_bounds_by_var,
        assoc_eq: assoc_eq_by_var,
    })
}

pub(super) fn hoist_fun_decls(decls: &[Decl], ctx: &mut InferContext) {
    for decl in decls {
        if let Decl::Fun(fun) = decl {
            // Overloaded names are not bound to a single shared type — each
            // definition is checked independently and dispatched by SymbolId
            // (METEL-180). Binding here would unify their distinct signatures.
            // This applies to native overloads too (std::core's assert pair).
            if ctx.is_overloaded(&fun.name) {
                continue;
            }
            // Native functions have a concrete annotated signature and no body;
            // bind it eagerly so forward references resolve. Errors here surface
            // again (deterministically) in infer_fun_decl.
            if fun.native.is_some() {
                if let Ok(result) = native_fun_ty(fun, ctx) {
                    let env_fvs = ctx.env_free_vars();
                    ctx.bind_poly(&fun.name, generalize(result.fun_ty, &env_fvs).with_bounds(&result.bounds).with_neg_bounds(&result.neg_bounds).with_assoc_eq_constraints(&result.assoc_eq));
                }
                continue;
            }
            if fun.generics.is_empty() {
                let fresh = ctx.fresh_var();
                ctx.bind_mono(&fun.name, fresh.clone(), false);
                // Also bind in poly_env so user declarations shadow any imported binding
                // (poly_env lookup takes precedence over mono_env regardless of scope level).
                ctx.bind_poly(&fun.name, TypeScheme::mono(fresh));
            } else {
                let generic_map = fun_generic_map(fun, ctx);
                let type_var_bounds = collect_fun_type_var_bounds(fun, &generic_map);
                let neg_type_var_bounds = collect_negative_fun_type_var_bounds(fun, &generic_map);
                if !type_var_bounds.is_empty() {
                    ctx.register_fun_bounds(fun.name.clone(), type_var_bounds.clone());
                }
                if !neg_type_var_bounds.is_empty() {
                    ctx.register_neg_fun_bounds(fun.name.clone(), neg_type_var_bounds.clone());
                }
                let assoc_eq_by_var = collect_fun_assoc_eq_constraints(fun, &generic_map);
                if !assoc_eq_by_var.is_empty() {
                    ctx.register_fun_assoc_eq_constraints(fun.name.clone(), assoc_eq_by_var);
                }

                let te_to_infer = |te: &TypeExpr, ctx: &mut InferContext| -> InferType {
        if let TypeExpr::Projection {
            base,
            ref assoc_name,
            ..
        } = te
        {
                        if let TypeExpr::Named(ref n, _) = **base {
                            if let Some(&base_tv) = generic_map.get(n.as_str()) {
                                // NOTE: use the locally-computed `type_var_bounds` map, not
                                // `ctx.bounds_for_type_var` -- that reads `current_type_param_bounds`,
                                // which is only populated by `swap_type_param_bounds` during body
                                // inference (`infer_fun_decl`), which hasn't run yet at hoist time.
                                // Using it here always finds no bounds and silently produces a
                                // stale, unresolved `Named("T::Item", [])` that then fails to unify
                                // with the correctly-resolved type computed later.
                                if let Some(bounds) = type_var_bounds.get(&base_tv) {
                                    for aspect in bounds {
                                        if let Some(decls) =
                                            ctx.registry().aspect_assoc_type_decls(aspect)
                                        {
                                            if decls.iter().any(|d| d.name == *assoc_name) {
                                                return InferType::Var(
                                                    ctx.fresh_assoc_projection_var(
                                                        base_tv,
                                                        aspect.clone(),
                                                        assoc_name.clone(),
                                                    ),
                                                );
                                            }
                                        }
                                    }
                                }
                                return InferType::Named(format!("{n}::{assoc_name}"), vec![]);
                            }
                        }
                    }
                    type_expr_to_infer_with_generics(te, &generic_map)
                };

                let param_types: Vec<InferType> = fun
                    .params
                    .iter()
                    .map(|p| {
                        if let Some(ann) = &p.type_ann {
                            te_to_infer(ann, ctx)
                        } else {
                            ctx.fresh_var()
                        }
                    })
                    .collect();

                let ret_ty = if let Some(ann) = &fun.return_type {
                    te_to_infer(ann, ctx)
                } else {
                    ctx.fresh_var()
                };

                let env_fvs = ctx.env_free_vars();
                let provisional_fun_ty = InferType::Fun(param_types, Box::new(ret_ty));
                let provisional_scheme = generalize(provisional_fun_ty, &env_fvs);
                ctx.bind_poly(&fun.name, provisional_scheme);
            }
        }
    }
}

fn fun_generic_map(fun: &FunDecl, ctx: &mut InferContext) -> HashMap<String, TypeVar> {
    fun.generics
        .iter()
        .map(|g| (g.name.clone(), ctx.fresh_type_var_raw()))
        .collect()
}

pub(super) fn collect_fun_type_var_bounds(
    fun: &FunDecl,
    generic_map: &HashMap<String, TypeVar>,
) -> HashMap<TypeVar, Vec<String>> {
    let mut map: HashMap<TypeVar, Vec<String>> = HashMap::new();
    for gp in &fun.generics {
        if let Some(&tv) = generic_map.get(&gp.name) {
            let names: Vec<String> = gp
                .bounds
                .iter()
                .filter_map(|b| {
                    // Negative bounds (`T: !Drop`) are dropped from this positive
                    // aspect-name list for now — their satisfaction checking is
                    // issue #243's job, not this one's.
                    if b.polarity != crate::ast::Polarity::Positive {
                        return None;
                    }
                    if let TypeExpr::Named(n, _) = &b.aspect {
                        Some(n.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if !names.is_empty() {
                map.entry(tv).or_default().extend(names);
            }
        }
    }
    if let Some(wc) = &fun.where_clause {
        for (param_name, bounds) in &wc.constraints {
            if let Some(&tv) = generic_map.get(param_name.as_str()) {
                let names: Vec<String> = bounds
                    .iter()
                    .filter(|b| b.polarity == Polarity::Positive)
                    .filter_map(|b| {
                        if let TypeExpr::Named(n, _) = &b.aspect {
                            Some(n.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                for name in names {
                    let entry = map.entry(tv).or_default();
                    if !entry.contains(&name) {
                        entry.push(name);
                    }
                }
            }
        }
    }
    map
}

/// Collect **negative** aspect-name bounds per generic type variable (RFC-0072,
/// issue #243). Mirrors `collect_fun_type_var_bounds` but filters for
/// `Polarity::Negative`.
pub(super) fn collect_negative_fun_type_var_bounds(
    fun: &FunDecl,
    generic_map: &HashMap<String, TypeVar>,
) -> HashMap<TypeVar, Vec<String>> {
    let mut map: HashMap<TypeVar, Vec<String>> = HashMap::new();
    for gp in &fun.generics {
        if let Some(&tv) = generic_map.get(&gp.name) {
            let names: Vec<String> = gp
                .bounds
                .iter()
                .filter_map(|b| {
                    if b.polarity != crate::ast::Polarity::Negative {
                        return None;
                    }
                    if let TypeExpr::Named(n, _) = &b.aspect {
                        Some(n.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if !names.is_empty() {
                map.entry(tv).or_default().extend(names);
            }
        }
    }
    if let Some(wc) = &fun.where_clause {
        for (param_name, bounds) in &wc.constraints {
            if let Some(&tv) = generic_map.get(param_name.as_str()) {
                let names: Vec<String> = bounds
                    .iter()
                    .filter(|b| b.polarity == Polarity::Negative)
                    .filter_map(|b| {
                        if let TypeExpr::Named(n, _) = &b.aspect {
                            Some(n.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                for name in names {
                    let entry = map.entry(tv).or_default();
                    if !entry.contains(&name) {
                        entry.push(name);
                    }
                }
            }
        }
    }
    map
}

/// Collect equality constraints (`Aspect<AssocType = ConcreteType>`, RFC-0082 §4)
/// per generic type variable. Mirrors `collect_fun_type_var_bounds`'s shape, but
/// reads `Bound.assoc_bindings` instead of just the bound's aspect name. Each
/// entry is `(aspect_name, assoc_name, expected_infer_type)` — `expected_infer_type`
/// is converted via `type_expr_to_infer_with_generics` so a sibling type param
/// (the `U` in `Deref<Target = U>`, RFC-0082 §3a's escape hatch) stays a `TypeVar`
/// rather than becoming a dangling `Named("U", [])`.
pub(super) fn collect_fun_assoc_eq_constraints(
    fun: &FunDecl,
    generic_map: &HashMap<String, TypeVar>,
) -> HashMap<TypeVar, Vec<(String, String, InferType)>> {
    let mut map: HashMap<TypeVar, Vec<(String, String, InferType)>> = HashMap::new();
    let mut collect_from_bounds = |tv: TypeVar, bounds: &[Bound]| {
        for b in bounds {
            if b.polarity != Polarity::Positive || b.assoc_bindings.is_empty() {
                continue;
            }
            let TypeExpr::Named(aspect_name, _) = &b.aspect else {
                continue;
            };
            for (assoc_name, assoc_ty) in &b.assoc_bindings {
                let expected = type_expr_to_infer_with_generics(assoc_ty, generic_map);
                map.entry(tv).or_default().push((
                    aspect_name.clone(),
                    assoc_name.clone(),
                    expected,
                ));
            }
        }
    };
    for gp in &fun.generics {
        if let Some(&tv) = generic_map.get(&gp.name) {
            collect_from_bounds(tv, &gp.bounds);
        }
    }
    if let Some(wc) = &fun.where_clause {
        for (param_name, bounds) in &wc.constraints {
            if let Some(&tv) = generic_map.get(param_name.as_str()) {
                collect_from_bounds(tv, bounds);
            }
        }
    }
    map
}

pub(super) fn infer_program(
    program: &Program,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<(), MetelError> {
    for decl in &program.decls {
        infer_decl(decl, ctx, fun_generalizations)?;
    }
    Ok(())
}

// Exhaustive match over every AST/type-system variant; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
fn infer_decl(
    decl: &Decl,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    match decl {
        Decl::Let(ld) => {
            let env_fvs = ctx.env_free_vars();
            let val_ty = infer_expr(&ld.value, ctx, fun_generalizations)?;
            let bound_ty = if let Some(ann) = &ld.type_ann {
                let declared = ann_to_infer(ann, ctx);
                constrain_with_read_copy(ctx, val_ty.clone(), declared, ld.span.clone())
            } else {
                val_ty.clone()
            };
            // Let-polymorphism: generalize unannotated closure-valued let bindings.
            // If the resolved type still has free variables, they are quantified into a
            // polymorphic scheme so each call site gets a fresh instantiation.
            if matches!(&ld.value, Expr::Closure { .. }) && ld.type_ann.is_none() {
                let solved = ctx.solve()?;
                let partial_subst = ctx.default_literal_vars(&solved);
                let resolved_ty = partial_subst.apply(&val_ty);
                let scheme = generalize(resolved_ty.clone(), &env_fvs);
                if !scheme.quantified_vars.is_empty() {
                    ctx.bind_poly(&ld.name, scheme);
                    fun_generalizations.push(FunGeneralization {
                        name: ld.name.clone(),
                        fun_ty: resolved_ty,
                        env_fvs,
                        name_map: HashMap::new(),
                        bounds: HashMap::new(),
                        neg_bounds: HashMap::new(),
                        assoc_projections: HashMap::new(),
                        assoc_eq: HashMap::new(),
                    });
                    return Ok(InferType::unit());
                }
            }
            ctx.bind_mono(&ld.name, bound_ty, false);
            Ok(InferType::unit())
        }
        Decl::Mut(md) => {
            let val_ty = infer_expr(&md.value, ctx, fun_generalizations)?;
            let bound_ty = if let Some(ann) = &md.type_ann {
                let declared = ann_to_infer(ann, ctx);
                constrain_with_read_copy(ctx, val_ty, declared, md.span.clone())
            } else {
                val_ty
            };
            ctx.bind_mono(&md.name, bound_ty, true);
            Ok(InferType::unit())
        }
        Decl::Fun(fd) => {
            infer_fun_decl(fd, ctx, fun_generalizations)?;
            Ok(InferType::unit())
        }
        Decl::Struct(_) | Decl::Enum(_) | Decl::Aspect(_) => Ok(InferType::unit()),
        Decl::Impl(ib) => {
            // Extract the target type name; bail for non-named targets (structural
            // blanket impls are still out of scope).
            let target_name = match &ib.target_type {
                TypeExpr::Named(name, _) => name.rsplit("::").next().unwrap_or(name).to_string(),
                _ => {
                    return Err(MetelError::internal(
                        "generic impl blocks not yet supported",
                    ))
                }
            };
            let mut inherited_defaults = vec![];
            // A negative impl (RFC-0081, issue #264) is a declaration of
            // non-implementation, not a real impl with missing overrides — it must
            // not be required to provide the aspect's methods, nor inherit its
            // default bodies. The parser already enforces `ib.methods.is_empty()`
            // for a negative impl; without this guard that empty method list would
            // otherwise look exactly like every required method being missing.
            if ib.polarity == Polarity::Positive {
                if let Some(aspect_name) = &ib.aspect_name {
                    if let Some(methods) = ctx.aspect_method_defs(aspect_name).cloned() {
                        let provided: std::collections::HashSet<&str> =
                            ib.methods.iter().map(|m| m.name.as_str()).collect();
                        for method in methods {
                            if provided.contains(method.name.as_str()) {
                                continue;
                            }
                            if method.default_body.is_none() {
                                return Err(MetelError::type_error(
                                    TypeErrorCode::T0003,
                                    format!(
                                        "`{}` does not implement `{}::{}` required by aspect `{}`",
                                        target_name, target_name, method.name, aspect_name
                                    ),
                                    &ib.span,
                                ));
                            }
                            inherited_defaults.push(method);
                        }
                    }
                    // RFC-0082 §2: check that the impl defines all associated types
                    // declared by the aspect. §1.1: if the declaration has a bound,
                    // the concrete binding must satisfy it.
                    // TODO(#241): generic impls are skipped above; assoc-type
                    // completeness for blanket impls is #241's job.
                    if let Some(assoc_decls) = ctx.registry().aspect_assoc_type_decls(aspect_name).cloned() {
                        let provided_assoc: std::collections::HashMap<&str, &TypeExpr> =
                            ib.assoc_type_defs.iter().map(|d| (d.name.as_str(), &d.ty)).collect();
                        for decl in &assoc_decls {
                            if let Some(concrete_ty_expr) = provided_assoc.get(decl.name.as_str()) {
                                // §1.1: if the declaration has a bound, check the
                                // concrete binding satisfies it.
                                for bound in &decl.bounds {
                                    if let TypeExpr::Named(bound_aspect, _) = &bound.aspect {
                                        if bound.polarity == Polarity::Positive {
                                            let concrete_infer = type_expr_to_infer_with_self(
                                                concrete_ty_expr,
                                                &target_name,
                                            );
                                            // Check that the concrete type satisfies the
                                            // bound aspect. For concrete target types the
                                            // concrete binding is also concrete, so we can
                                            // check via the registry's impl_aspect_env.
                                            let concrete_name = match &concrete_infer {
                                                InferType::Concrete(t) => Some(format!("{t}")),
                                                InferType::Named(n, _) => Some(n.clone()),
                                                _ => None,
                                            };
                                            if let Some(name) = concrete_name {
                                                if !ctx.registry().impl_aspect_env_has(
                                                    ctx.current_module_path(),
                                                    &name,
                                                    bound_aspect,
                                                ) {
                                                    return Err(MetelError::type_error(
                                                        TypeErrorCode::T0012,
                                                        format!(
                                                            "associated type `{}` bound `{}` is not satisfied by `{}`",
                                                            decl.name, bound_aspect, name
                                                        ),
                                                        &ib.span,
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                // Missing associated type definition → T0017.
                                return Err(MetelError::type_error(
                                    TypeErrorCode::T0017,
                                    format!(
                                        "`{}` does not define associated type `{}` required by aspect `{}`",
                                        target_name, decl.name, aspect_name
                                    ),
                                    &ib.span,
                                ));
                            }
                        }
                    }
                }
            }
            for method in &ib.methods {
                infer_impl_method(method, ib, &target_name, ctx, fun_generalizations)?;
            }
            // `inherited_defaults` is only ever populated inside the `Some(aspect_name)`
            // branch above, so this is always `Some` when the loop body runs.
            if let Some(aspect_name) = ib.aspect_name.as_deref() {
                for method in &inherited_defaults {
                    infer_default_aspect_method(
                        method,
                        &target_name,
                        aspect_name,
                        ctx,
                        fun_generalizations,
                    )?;
                }
            }
            Ok(InferType::unit())
        }
        Decl::Stmt(stmt) => infer_stmt(stmt, ctx, fun_generalizations),
    }
}

// Exhaustive match over every AST/type-system variant; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
fn infer_fun_decl(
    fun: &FunDecl,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<(), MetelError> {
    // Native functions have no Metel body to infer. Validate and record their
    // annotated signature for the construction pass; dispatch is by NativeKey.
    if fun.native.is_some() {
        let NativeFunTyResult { fun_ty, bounds, neg_bounds, assoc_eq } = native_fun_ty(fun, ctx)?;
        // Overloaded native definitions (std::core's assert pair) are
        // dispatched by SymbolId and never enter the name-keyed scheme env.
        if ctx.is_overloaded(&fun.name) {
            return Ok(());
        }
        let env_fvs = ctx.env_free_vars();
        ctx.bind_poly(
            &fun.name,
            generalize(fun_ty.clone(), &env_fvs).with_bounds(&bounds).with_neg_bounds(&neg_bounds).with_assoc_eq_constraints(&assoc_eq),
        );
        fun_generalizations.push(FunGeneralization {
            name: fun.name.clone(),
            fun_ty,
            env_fvs,
            name_map: HashMap::new(),
            bounds,
            neg_bounds,
            assoc_projections: HashMap::new(),
            assoc_eq,
        });
        return Ok(());
    }

    // For generic functions, create fresh type variables for each parameter name.
    let generic_map = fun_generic_map(fun, ctx);

    // Collect merged bounds (inline + where clause) per TypeVar, register for call-site checking.
    let type_var_bounds = collect_fun_type_var_bounds(fun, &generic_map);
    if !type_var_bounds.is_empty() {
        ctx.register_fun_bounds(fun.name.clone(), type_var_bounds.clone());
    }
    let neg_type_var_bounds = collect_negative_fun_type_var_bounds(fun, &generic_map);
    if !neg_type_var_bounds.is_empty() {
        ctx.register_neg_fun_bounds(fun.name.clone(), neg_type_var_bounds.clone());
    }
    let assoc_eq_by_var = collect_fun_assoc_eq_constraints(fun, &generic_map);
    if !assoc_eq_by_var.is_empty() {
        ctx.register_fun_assoc_eq_constraints(fun.name.clone(), assoc_eq_by_var.clone());
    }

    let te_to_infer = |te: &TypeExpr, ctx: &mut InferContext| -> Result<InferType, MetelError> {
        // RFC-0082 §2 abstract-case: T::AssocType where T is a generic param.
        if let TypeExpr::Projection {
            base,
            ref assoc_name,
            span: proj_span,
            ..
        } = te
        {
            if let TypeExpr::Named(ref n, _) = **base {
                if let Some(&base_tv) = generic_map.get(n.as_str()) {
                    // Find the aspect(s) that declare this assoc type.
                    let mut matching_aspects: Vec<String> = Vec::new();
                    if let Some(bounds) = type_var_bounds.get(&base_tv) {
                        for aspect in bounds {
                            if let Some(decls) = ctx.registry().aspect_assoc_type_decls(aspect) {
                                if decls.iter().any(|d| d.name == *assoc_name) {
                                    matching_aspects.push(aspect.clone());
                                }
                            }
                        }
                    }
                    if matching_aspects.len() > 1 {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0013,
                            format!(
                                "ambiguous associated type `{assoc_name}`: multiple aspects declare it: {}",
                                matching_aspects.join(", ")
                            ),
                            proj_span,
                        ));
                    }
                    if let Some(aspect) = matching_aspects.into_iter().next() {
                        return Ok(InferType::Var(ctx.fresh_assoc_projection_var(
                            base_tv,
                            aspect,
                            assoc_name.clone(),
                        )));
                    }
                    // Fallback: named placeholder
                    return Ok(InferType::Named(format!("{n}::{assoc_name}"), vec![]));
                }
            }
        }
        Ok(if generic_map.is_empty() {
            type_expr_to_infer(te)
        } else {
            type_expr_to_infer_with_generics(te, &generic_map)
        })
    };

    let param_types: Vec<InferType> = fun
        .params
        .iter()
        .map(|p| {
            if let Some(ann) = &p.type_ann {
                te_to_infer(ann, ctx)
            } else {
                Ok(ctx.fresh_var())
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let ret_ty = if let Some(ann) = &fun.return_type {
        te_to_infer(ann, ctx)?
    } else {
        ctx.fresh_var()
    };

    let env_fvs = ctx.env_free_vars();

    ctx.push_scope();
    for (param, pt) in fun.params.iter().zip(param_types.iter()) {
        ctx.bind_mono(&param.name, pt.clone(), false);
    }

    // Build initial name_map from original TypeVars; will be resolved post-solve below.
    let orig_name_map: HashMap<TypeVar, String> =
        generic_map.iter().map(|(n, &tv)| (tv, n.clone())).collect();
    let saved_type_params = ctx.swap_type_params(generic_map);
    let saved_tp_bounds = ctx.swap_type_param_bounds(type_var_bounds.clone());
    let (saved_assoc_memo, saved_assoc_log) = ctx.swap_assoc_projections();
    let saved_ret = ctx.push_return_type(ret_ty.clone());
    let body_ty = infer_block(&fun.body, ctx, fun_generalizations)?;

    constrain_with_read_copy(ctx, body_ty, ret_ty.clone(), fun.body.span.clone());

    ctx.pop_return_type(saved_ret);
    ctx.swap_type_param_bounds(saved_tp_bounds);
    ctx.swap_type_params(saved_type_params);
    // Capture the projection log recorded during this function's body BEFORE restoring.
    let body_assoc_log = ctx.take_recorded_assoc_projections();
    ctx.restore_assoc_projections(saved_assoc_memo, saved_assoc_log);
    ctx.pop_scope();

    let fun_ty = InferType::Fun(param_types, Box::new(ret_ty));

    // Overloaded functions have no single shared binding to constrain; each
    // definition stands alone (its concrete signature lives in the overload
    // table, keyed by SymbolId) and never enters the name-keyed scheme env.
    let is_overloaded = ctx.is_overloaded(&fun.name);

    if !is_overloaded {
        if let Some(pre_reg) = ctx.lookup(&fun.name) {
            ctx.add_constraint(pre_reg, fun_ty.clone(), fun.span.clone());
        }
    }

    // Inline solve-and-generalize: future call sites look up this function via the
    // poly_env and get a fresh instantiation per call, avoiding constraint conflicts
    // when the same polymorphic function is called at different types.
    let solved = ctx.solve()?;
    let partial_subst = ctx.default_literal_vars(&solved);
    let resolved_ty = partial_subst.apply(&fun_ty);

    // Overloaded definitions are dispatched by SymbolId, never by name: the
    // scheme env and the export surface know nothing about them. The body was
    // still inferred and solved above, so type errors inside it are reported.
    if is_overloaded {
        return Ok(());
    }

    let scheme = generalize(resolved_ty.clone(), &env_fvs);
    // Attach assoc_projections if any projections were recorded during body inference.
    // `proj_map` is already keyed by the FINAL (post-`partial_subst`) TypeVar (see
    // `build_assoc_projection_map`), so it's carried into `FunGeneralization` as-is,
    // with no further remapping needed -- unlike `bounds`/`neg_bounds` below, which
    // are collected pre-solve and must still be remapped through `partial_subst`.
    let proj_map = if body_assoc_log.is_empty() {
        HashMap::new()
    } else {
        build_assoc_projection_map(&body_assoc_log, &partial_subst, &scheme)
    };
    let scheme = if proj_map.is_empty() {
        scheme
    } else {
        scheme.with_assoc_projections(&proj_map)
    };
    ctx.bind_poly(fun.name.clone(), scheme);

    // After solving, the original TypeVars may have been unified with others.
    // Remap name_map through partial_subst so quantified_vars (which are in the
    // resolved type) have correct names.
    let name_map: HashMap<TypeVar, String> = orig_name_map
        .into_iter()
        .filter_map(|(orig_tv, name)| {
            match partial_subst.apply(&InferType::Var(orig_tv)) {
                InferType::Var(final_tv) => Some((final_tv, name)),
                _ => None, // var was solved to a concrete type; no longer generic
            }
        })
        .collect();
    let bounds: HashMap<TypeVar, Vec<String>> = type_var_bounds
        .iter()
        .filter_map(
            |(orig_tv, b)| match partial_subst.apply(&InferType::Var(*orig_tv)) {
                InferType::Var(final_tv) => Some((final_tv, b.clone())),
                _ => None,
            },
        )
        .collect();
    let neg_bounds: HashMap<TypeVar, Vec<String>> = neg_type_var_bounds
        .iter()
        .filter_map(
            |(orig_tv, b)| match partial_subst.apply(&InferType::Var(*orig_tv)) {
                InferType::Var(final_tv) => Some((final_tv, b.clone())),
                _ => None,
            },
        )
        .collect();
    // Remap equality constraints (RFC-0082 §4) the same way as bounds/neg_bounds:
    // keyed by the ORIGINAL declared TypeVar (from collect_fun_assoc_eq_constraints),
    // re-keyed to the FINAL post-solve TypeVar so it lines up with `resolved_ty`'s
    // free vars (what `generalize` re-quantifies over). Each constraint's expected
    // `InferType` is also substituted, since it may itself reference another
    // generic param (the `U` in `Deref<Target = U>`, RFC-0082 §3a's escape hatch).
    let assoc_eq: HashMap<TypeVar, Vec<(String, String, InferType)>> = assoc_eq_by_var
        .iter()
        .filter_map(
            |(orig_tv, constraints)| match partial_subst.apply(&InferType::Var(*orig_tv)) {
                InferType::Var(final_tv) => Some((
                    final_tv,
                    constraints
                        .iter()
                        .map(|(aspect, assoc, ty)| {
                            (aspect.clone(), assoc.clone(), partial_subst.apply(ty))
                        })
                        .collect(),
                )),
                _ => None,
            },
        )
        .collect();
    // Store resolved_ty (post-solve) so the re-generalization in check_impl uses the
    // already-solved type and is not perturbed by a now-empty final substitution.
    fun_generalizations.push(FunGeneralization {
        name: fun.name.clone(),
        fun_ty: resolved_ty,
        env_fvs,
        name_map,
        bounds,
        neg_bounds,
        assoc_projections: proj_map,
        assoc_eq,
    });
    Ok(())
}

// Exhaustive match over every AST/type-system variant; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
fn infer_impl_method(
    method: &FunDecl,
    ib: &crate::ast::ImplBlock,
    target_name: &str,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<(), MetelError> {
    // Start with the method's own generic params.
    let mut generic_map: HashMap<String, TypeVar> = method
        .generics
        .iter()
        .map(|g| (g.name.clone(), ctx.fresh_type_var_raw()))
        .collect();

    // Seed with the target struct/enum's generic params so that type annotations
    // referencing e.g. `T` in `impl SortedList<T>` resolve to TypeVars and
    // aspect methods on bounded params are available in the body.
    let mut struct_bounds: HashMap<TypeVar, Vec<String>> = HashMap::new();
    // Ordered TypeVars for the struct's generic params (same order as struct type args).
    let mut struct_tvars_ordered: Vec<TypeVar> = Vec::new();
    if let Some(names) = ctx.struct_generic_names_for(target_name).cloned() {
        let bounds_by_pos: Option<Vec<Vec<String>>> =
            ctx.get_type_param_bounds(target_name).cloned();
        for (i, name) in names.iter().enumerate() {
            if !generic_map.contains_key(name) {
                let tv = ctx.fresh_type_var_raw();
                generic_map.insert(name.clone(), tv);
                struct_tvars_ordered.push(tv);
                if let Some(ref bp) = bounds_by_pos {
                    if let Some(b) = bp.get(i) {
                        if !b.is_empty() {
                            struct_bounds.insert(tv, b.clone());
                        }
                    }
                }
            }
        }
    }

    // RFC-0036 §2.2: compute impl-level bounds (from the impl block's own
    // where clause / inline bounds) and merge them into `struct_bounds` so that
    // method dispatch and type annotations inside the body can see impl-level
    // constraints (e.g. `impl<T: Display> Greet for Box1<T>` needs `T: Display`
    // visible when resolving `self.value.to_string()`).
    let generic_names_for_impl: Vec<String> = ctx
        .struct_generic_names_for(target_name)
        .cloned()
        .unwrap_or_default();
    let synth = super::registry::synth_generics_for_impl(&generic_names_for_impl, &ib.generics);
    let impl_bounds: Vec<Vec<String>> =
        super::registry::collect_type_param_bounds(&synth, ib.where_clause.as_ref());
    let impl_neg_bounds: Vec<Vec<String>> =
        super::registry::collect_negative_type_param_bounds(&synth, ib.where_clause.as_ref());

    // Merge impl-level bounds into struct_bounds (union: keep any existing
    // struct-level bounds and add the impl's).
    for (i, tv) in struct_tvars_ordered.iter().enumerate() {
        if let Some(ib_bounds) = impl_bounds.get(i) {
            if !ib_bounds.is_empty() {
                struct_bounds
                    .entry(*tv)
                    .or_default()
                    .extend(ib_bounds.iter().cloned());
            }
        }
    }

    let te_to_infer = |te: &TypeExpr, ctx: &mut InferContext| -> Result<InferType, MetelError> {
        // RFC-0082 §2 abstract-case: T::AssocType where T is a generic param.
        if let TypeExpr::Projection {
            base,
            ref assoc_name,
            span: proj_span,
            ..
        } = te
        {
            if let TypeExpr::Named(ref n, _) = **base {
                if let Some(&base_tv) = generic_map.get(n.as_str()) {
                    let mut matching_aspects: Vec<String> = Vec::new();
                    if let Some(bounds) = struct_bounds.get(&base_tv) {
                        for aspect in bounds {
                            if let Some(decls) = ctx.registry().aspect_assoc_type_decls(aspect) {
                                if decls.iter().any(|d| d.name == *assoc_name) {
                                    matching_aspects.push(aspect.clone());
                                }
                            }
                        }
                    }
                    if matching_aspects.len() > 1 {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0013,
                            format!(
                                "ambiguous associated type `{assoc_name}`: multiple aspects declare it: {}",
                                matching_aspects.join(", ")
                            ),
                            proj_span,
                        ));
                    }
                    if let Some(aspect) = matching_aspects.into_iter().next() {
                        return Ok(InferType::Var(ctx.fresh_assoc_projection_var(
                            base_tv,
                            aspect,
                            assoc_name.clone(),
                        )));
                    }
                    return Ok(InferType::Named(format!("{n}::{assoc_name}"), vec![]));
                }
            }
        }
        Ok(if generic_map.is_empty() {
            type_expr_to_infer_with_self(te, target_name)
        } else {
            type_expr_to_infer_with_generics_and_self(te, &generic_map, target_name)
        })
    };

    // Include struct TypeVars in self type so call-site unification resolves correctly.
    // For a primitive target (`impl Display for i64`) the self type must be the
    // concrete primitive, since call sites produce `Concrete(Type::I64)` and the
    // unifier has no Named↔Concrete bridge (METEL-181).
    let self_ty = if let Some(prim) = primitive_type_from_name(target_name) {
        InferType::Concrete(prim)
    } else if struct_tvars_ordered.is_empty() {
        InferType::Named(target_name.to_string(), vec![])
    } else {
        InferType::Named(
            target_name.to_string(),
            struct_tvars_ordered
                .iter()
                .map(|&tv| InferType::Var(tv))
                .collect(),
        )
    };
    let param_types: Vec<InferType> = method
        .params
        .iter()
        .map(|p| {
            if p.name == "self" {
                Ok(self_ty.clone())
            } else if let Some(ann) = &p.type_ann {
                te_to_infer(ann, ctx)
            } else {
                Ok(ctx.fresh_var())
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ret_ty = method
        .return_type
        .as_ref()
        .map_or_else(|| Ok(InferType::unit()), |t| te_to_infer(t, ctx))?;

    // Native methods have no Metel body; their signature comes entirely from
    // annotations (METEL-181). Skip body inference but still register the
    // method scheme below so call sites resolve.
    let mut body_assoc_log = Vec::new();
    if method.native.is_none() {
        ctx.push_scope();
        for (p, pt) in method.params.iter().zip(param_types.iter()) {
            let is_mutable =
                p.mutable || matches!(p.receiver, Some(crate::ast::ReceiverKind::RefMut));
            ctx.bind_mono(&p.name, pt.clone(), is_mutable);
        }
        let saved_type_params = ctx.swap_type_params(generic_map);
        let saved_tp_bounds = ctx.swap_type_param_bounds(struct_bounds);
        let (saved_assoc_memo, saved_assoc_log) = ctx.swap_assoc_projections();
        let saved_ret = ctx.push_return_type(ret_ty.clone());
        let body_ty = infer_block(&method.body, ctx, fun_generalizations)?;
        constrain_with_read_copy(ctx, body_ty, ret_ty.clone(), method.body.span.clone());
        ctx.pop_return_type(saved_ret);
        ctx.swap_type_param_bounds(saved_tp_bounds);
        ctx.swap_type_params(saved_type_params);
        let body_assoc_log_inner = ctx.take_recorded_assoc_projections();
        ctx.restore_assoc_projections(saved_assoc_memo, saved_assoc_log);
        ctx.pop_scope();
        body_assoc_log = body_assoc_log_inner;
    }

    let solved = ctx.solve()?;
    let partial_subst = ctx.default_literal_vars(&solved);
    let fun_ty = InferType::Fun(param_types, Box::new(ret_ty));
    let resolved_fun_ty = partial_subst.apply(&fun_ty);

    // Map each struct type-param TypeVar through the solution: body inference
    // (e.g. a `self.value` field access) may have unified the original param var
    // with a fresh representative, so `struct_tvars_ordered` can be stale. The
    // scheme is generalized from `resolved_fun_ty`, so its quantified vars and the
    // `struct_tvars` we hand to the call site must use the *resolved* representatives.
    let struct_tvars_resolved: Vec<TypeVar> = struct_tvars_ordered
        .iter()
        .map(|&tv| match partial_subst.apply(&InferType::Var(tv)) {
            InferType::Var(v) => v,
            _ => tv,
        })
        .collect();

    // If the resolved method type still has free TypeVars from the struct's generic params,
    // store it as a polymorphic scheme so Pass 2 can instantiate it per call site.
    let struct_tvars_free: std::collections::HashSet<TypeVar> =
        struct_tvars_resolved.iter().copied().collect();
    if !struct_tvars_free.is_empty()
        && free_vars(&resolved_fun_ty)
            .iter()
            .any(|v| struct_tvars_free.contains(v))
    {
        let mut scheme = generalize(resolved_fun_ty, &std::collections::HashSet::new());
        // RFC-0036 §2.2: attach impl-level bounds keyed by resolved tvars so
        // use-site checking can verify the concrete receiver satisfies them.
        let by_var: std::collections::HashMap<TypeVar, Vec<String>> = impl_bounds
            .iter()
            .enumerate()
            .filter_map(|(i, bounds)| {
                if bounds.is_empty() { return None; }
                let resolved_tv = struct_tvars_resolved.get(i)?;
                Some((*resolved_tv, bounds.clone()))
            })
            .collect();
        let by_neg_var: std::collections::HashMap<TypeVar, Vec<String>> = impl_neg_bounds
            .iter()
            .enumerate()
            .filter_map(|(i, bounds)| {
                if bounds.is_empty() { return None; }
                let resolved_tv = struct_tvars_resolved.get(i)?;
                Some((*resolved_tv, bounds.clone()))
            })
            .collect();
        scheme = scheme.with_bounds(&by_var).with_neg_bounds(&by_neg_var);
        let scheme = if body_assoc_log.is_empty() {
            scheme
        } else {
            let proj_map = build_assoc_projection_map(&body_assoc_log, &partial_subst, &scheme);
            scheme.with_assoc_projections(&proj_map)
        };
        ctx.register_method_scheme(
            target_name.to_string(),
            method.name.clone(),
            scheme.clone(),
            struct_tvars_resolved.clone(),
        );
        ctx.register_method_scheme_variant(
            target_name.to_string(),
            method.name.clone(),
            scheme,
            struct_tvars_resolved,
        );
    } else {
        ctx.register_method(
            target_name.to_string(),
            method.name.clone(),
            resolved_fun_ty,
        );
    }
    Ok(())
}

fn infer_default_aspect_method(
    method: &AspectMethod,
    target_name: &str,
    aspect_name: &str,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<(), MetelError> {
    let generic_map: HashMap<String, TypeVar> = method
        .generics
        .iter()
        .map(|g| (g.name.clone(), ctx.fresh_type_var_raw()))
        .collect();

    // RFC-0082 §1.2: a bare associated-type name in the aspect's own default
    // method body (e.g. `Item` in `fun get_twice(self) -> Item { ... }`) must
    // resolve to this impl's concrete binding, same as register_default_aspect_method
    // does for the pre-registered signature -- this is the SEPARATE conversion that
    // actually type-checks the default body itself against its declared return type.
    let te_to_infer = |te: &TypeExpr, ctx: &InferContext| -> InferType {
        if let TypeExpr::Named(n, args) = te {
            if args.is_empty()
                && ctx
                    .registry()
                    .aspect_assoc_type_decls(aspect_name)
                    .is_some_and(|decls| decls.iter().any(|d| d.name == *n))
            {
                if let Some(concrete) = ctx.registry().impl_assoc_type(
                    ctx.current_module_path(),
                    target_name,
                    aspect_name,
                    n,
                ) {
                    return type_to_infer(concrete);
                }
            }
        }
        if generic_map.is_empty() {
            type_expr_to_infer_with_self(te, target_name)
        } else {
            type_expr_to_infer_with_generics_and_self(te, &generic_map, target_name)
        }
    };

    // For a primitive target the `self` type must be the concrete primitive, so
    // an inherited default method unifies with call sites that produce
    // `Concrete(Type::I64)` (METEL-149 / METEL-181). User structs stay `Named`.
    let self_ty = match primitive_type_from_name(target_name) {
        Some(prim) => InferType::Concrete(prim),
        None => InferType::Named(target_name.to_string(), vec![]),
    };
    let param_types: Vec<InferType> = method
        .params
        .iter()
        .map(|p| {
            if p.name == "self" {
                self_ty.clone()
            } else if let Some(ann) = &p.type_ann {
                te_to_infer(ann, ctx)
            } else {
                ctx.fresh_var()
            }
        })
        .collect();
    let ret_ty = method
        .return_type
        .as_ref()
        .map_or_else(InferType::unit, |ann| te_to_infer(ann, ctx));
    let body = method
        .default_body
        .as_ref()
        .ok_or_else(|| MetelError::internal("missing aspect default body"))?;

    ctx.push_scope();
    for (p, pt) in method.params.iter().zip(param_types.iter()) {
        let is_mutable = p.mutable || matches!(p.receiver, Some(crate::ast::ReceiverKind::RefMut));
        ctx.bind_mono(&p.name, pt.clone(), is_mutable);
    }
    let saved_type_params = ctx.swap_type_params(generic_map);
    let saved_ret = ctx.push_return_type(ret_ty.clone());
    let body_ty = infer_block(body, ctx, fun_generalizations)?;
    constrain_with_read_copy(ctx, body_ty, ret_ty.clone(), body.span.clone());
    ctx.pop_return_type(saved_ret);
    ctx.swap_type_params(saved_type_params);
    ctx.pop_scope();

    let solved = ctx.solve()?;
    let partial_subst = ctx.default_literal_vars(&solved);
    let fun_ty = InferType::Fun(param_types, Box::new(ret_ty));
    let resolved_fun_ty = partial_subst.apply(&fun_ty);
    ctx.register_method(
        target_name.to_string(),
        method.name.clone(),
        resolved_fun_ty,
    );
    Ok(())
}

fn infer_block(
    block: &Block,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    ctx.push_scope();
    ctx.push_struct_scope();
    // Hoist struct/enum declarations defined in this block before inferring any stmt,
    // so they can be referenced anywhere within the block regardless of order.
    for decl in &block.stmts {
        match decl {
            Decl::Struct(sd) => {
                let fields = sd
                    .fields
                    .iter()
                    .map(|f| FieldEntry {
                        name: f.name.clone(),
                        ty: type_expr_to_infer(&f.type_ann),
                        span: f.span.clone(),
                        visibility: f.visibility.clone(),
                    })
                    .collect();
                ctx.register_struct_fields(sd.name.clone(), fields);
            }
            Decl::Enum(ed) => {
                let variants = ed
                    .variants
                    .iter()
                    .map(|v| VariantInfo {
                        name: v.name.clone(),
                        fields: v
                            .fields
                            .iter()
                            .map(|f| FieldEntry {
                                name: f.name.clone(),
                                ty: type_expr_to_infer(&f.type_ann),
                                span: f.span.clone(),
                                visibility: f.visibility.clone(),
                            })
                            .collect(),
                    })
                    .collect();
                ctx.register_enum(
                    ed.name.clone(),
                    EnumInfo {
                        type_params: vec![],
                        variants,
                    },
                );
            }
            _ => {}
        }
    }
    hoist_fun_decls(&block.stmts, ctx);
    let mut last_stmt_ty = InferType::unit();
    for stmt in &block.stmts {
        last_stmt_ty = infer_decl(stmt, ctx, fun_generalizations)?;
    }
    let ty = match &block.tail {
        Some(tail) => infer_expr(tail, ctx, fun_generalizations)?,
        None => last_stmt_ty,
    };
    ctx.pop_struct_scope();
    ctx.pop_scope();
    Ok(ty)
}

// Exhaustive match over every AST/type-system variant; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
fn infer_stmt(
    stmt: &Stmt,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    match stmt {
        // Issue #229: `return`/`break`/`continue` are now `Expr` variants, so a
        // bare `return 5;` used as a mid-block statement reaches here as an
        // ordinary `Stmt::Expr`. Propagate `Never` when the inner expression
        // is genuinely `Never`-typed (return/break/continue, or any other
        // diverging expression like `panic(msg)`) rather than always
        // discarding to `unit()` — needed so `infer_block`'s tail-less "last
        // statement" type correctly reflects divergence.
        Stmt::Expr(e) => {
            let ty = infer_expr(e, ctx, fun_generalizations)?;
            Ok(if ty == InferType::Never {
                InferType::never()
            } else {
                InferType::unit()
            })
        }
        Stmt::While(ws) => {
            let cond_ty = infer_expr(&ws.condition, ctx, fun_generalizations)?;
            ctx.add_constraint(cond_ty, InferType::bool(), ws.span.clone());
            infer_block(&ws.body, ctx, fun_generalizations)?;
            Ok(InferType::unit())
        }
        Stmt::For(fs) => {
            ctx.push_scope();
            if let Some(init) = &fs.init {
                match init {
                    ForInit::Let(ld) => {
                        let val_ty = infer_expr(&ld.value, ctx, fun_generalizations)?;
                        let bound_ty = if let Some(ann) = &ld.type_ann {
                            let declared = ann_to_infer(ann, ctx);
                            constrain_with_read_copy(ctx, val_ty, declared, ld.span.clone())
                        } else {
                            val_ty
                        };
                        ctx.bind_mono(&ld.name, bound_ty, false);
                    }
                    ForInit::Mut(md) => {
                        let val_ty = infer_expr(&md.value, ctx, fun_generalizations)?;
                        let bound_ty = if let Some(ann) = &md.type_ann {
                            let declared = ann_to_infer(ann, ctx);
                            constrain_with_read_copy(ctx, val_ty, declared, md.span.clone())
                        } else {
                            val_ty
                        };
                        ctx.bind_mono(&md.name, bound_ty, true);
                    }
                    ForInit::Expr(e) => {
                        infer_expr(e, ctx, fun_generalizations)?;
                    }
                }
            }
            if let Some(cond) = &fs.condition {
                let cond_ty = infer_expr(cond, ctx, fun_generalizations)?;
                ctx.add_constraint(cond_ty, InferType::bool(), fs.span.clone());
            }
            if let Some(step) = &fs.step {
                infer_expr(step, ctx, fun_generalizations)?;
            }
            infer_block(&fs.body, ctx, fun_generalizations)?;
            ctx.pop_scope();
            Ok(InferType::unit())
        }
        Stmt::ForIn(fi) => {
            let iter_ty = infer_expr(&fi.iterable, ctx, fun_generalizations)?;
            let elem_ty = ctx.fresh_var();
            let partial = ctx.solve()?;
            let resolved_iter = partial.apply(&iter_ty);
            match &resolved_iter {
                InferType::Array(elem) | InferType::SizedArray(elem, _) => {
                    ctx.add_constraint(elem_ty.clone(), *elem.clone(), fi.span.clone());
                }
                InferType::Var(_) => {
                    // Unknown type — constrain to Array as default.
                    ctx.add_constraint(
                        iter_ty,
                        InferType::Array(Box::new(elem_ty.clone())),
                        fi.span.clone(),
                    );
                }
                _ => {
                    // Look up the type name in the Iterable registry.
                    let type_name = infer_type_name(&resolved_iter).map(ToOwned::to_owned);
                    let elem_from_registry = type_name
                        .as_deref()
                        .and_then(|name| ctx.iterable_elem_type(name))
                        .cloned();
                    match elem_from_registry {
                        Some(t) => {
                            ctx.add_constraint(
                                elem_ty.clone(),
                                InferType::Concrete(t),
                                fi.span.clone(),
                            );
                        }
                        None => {
                            return Err(MetelError::type_error(
                                TypeErrorCode::T0001,
                                format!("type `{resolved_iter}` does not implement `Iterable<T>`"),
                                &fi.span,
                            ));
                        }
                    }
                }
            }
            ctx.push_scope();
            ctx.bind_mono(&fi.binding, elem_ty, fi.mutable);
            infer_block(&fi.body, ctx, fun_generalizations)?;
            ctx.pop_scope();
            Ok(InferType::unit())
        }
    }
}

// Exhaustive match over every AST/type-system variant; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
fn infer_expr(
    expr: &Expr,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    match expr {
        Expr::Literal(lit, _) => Ok(infer_literal(lit, ctx)),
        Expr::Ident(name, span) => {
            if let Some(err) = ctx.check_glob_conflict(name, span) {
                return Err(err);
            }
            ctx.lookup(name).ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("undefined name `{name}`"),
                    span,
                )
            })
        }
        Expr::ResolvedPath {
            resolved,
            symbol_id: _,
            original,
            span,
        } => {
            if let Some(err) = ctx.check_glob_conflict(resolved, span) {
                return Err(err);
            }
            ctx.lookup(resolved).ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("undefined name `{}`", original.join("::")),
                    span,
                )
            })
        }
        Expr::BinOp(lhs, op, rhs, span) => {
            infer_binop(lhs, op, rhs, span, ctx, fun_generalizations)
        }
        Expr::UnaryOp(op, operand, span) => {
            infer_unaryop(op, operand, span, ctx, fun_generalizations)
        }
        Expr::Tuple(elems, _) => {
            let elem_tys: Vec<InferType> = elems
                .iter()
                .map(|e| infer_expr(e, ctx, fun_generalizations))
                .collect::<Result<_, _>>()?;
            Ok(InferType::Tuple(elem_tys))
        }
        Expr::Array(elems, span) => {
            if elems.is_empty() {
                return Ok(InferType::Array(Box::new(ctx.fresh_var())));
            }
            let first_ty = infer_expr(&elems[0], ctx, fun_generalizations)?;
            for elem in &elems[1..] {
                let ty = infer_expr(elem, ctx, fun_generalizations)?;
                ctx.add_constraint(ty, first_ty.clone(), span.clone());
            }
            Ok(InferType::Array(Box::new(first_ty)))
        }
        Expr::RepeatArray(elem, n, _span) => {
            let elem_ty = infer_expr(elem, ctx, fun_generalizations)?;
            Ok(InferType::SizedArray(Box::new(elem_ty), *n))
        }
        Expr::Call {
            callee, args, span, ..
        } => {
            // Overloaded free-function call (METEL-180): infer argument types,
            // select the exact-match candidate, and yield its return type. The
            // selected definition's SymbolId is stamped in the construction pass.
            if let Some(name) = super::overload::callee_name(callee) {
                if ctx.is_overloaded(name) {
                    let arg_infer: Vec<InferType> = args
                        .iter()
                        .map(|a| infer_expr(a, ctx, fun_generalizations))
                        .collect::<Result<_, _>>()?;
                    // Default unresolved literal vars (a bare `42` is i64, a bare
                    // float literal is f64) so literals participate in selection
                    // the same way they type everywhere else.
                    let solved = ctx.solve()?;
                    let solved = ctx.default_literal_vars(&solved);
                    // When no candidate matches (or args can't resolve), a call
                    // can fall back to a non-overload binding of the same name
                    // from an outer source (the prelude / imports — e.g. the
                    // generic std::core `print` when a module overloads `print`
                    // for specific types). Local overload sets EXTEND such a
                    // binding rather than replace it.
                    let fallback = |ctx: &mut InferContext,
                                    arg_infer: &[InferType]|
                     -> Result<InferType, MetelError> {
                        let callee_ty = ctx
                            .lookup(name)
                            .expect("has_binding checked before fallback");
                        let ret_var = ctx.fresh_var();
                        ctx.add_constraint(
                            callee_ty,
                            InferType::Fun(arg_infer.to_vec(), Box::new(ret_var.clone())),
                            span.clone(),
                        );
                        Ok(ret_var)
                    };
                    let arg_types: Result<Vec<Type>, ()> = arg_infer
                        .iter()
                        .map(|t| infer_type_to_type(&solved.apply(t), span).map_err(|_| ()))
                        .collect();
                    let arg_types = match arg_types {
                        Ok(tys) => tys,
                        Err(()) if ctx.has_binding(name) => {
                            return fallback(ctx, &arg_infer);
                        }
                        Err(()) => {
                            return Err(MetelError::type_error(
                                TypeErrorCode::T0002,
                                format!(
                                "cannot resolve argument types for overloaded call to `{name}`; \
                                     add type annotations"
                            ),
                                span,
                            ))
                        }
                    };
                    let entries = ctx.overload_candidates(name).unwrap();
                    let entry = if let Some(entry) = super::overload::select(entries, &arg_types) {
                        entry.clone()
                    } else {
                        if ctx.has_binding(name) {
                            return fallback(ctx, &arg_infer);
                        }
                        let entries = ctx.overload_candidates(name).unwrap();
                        return Err(super::overload::no_match_error(
                            name, &arg_types, entries, span,
                        ));
                    };
                    // Commit the selection: constrain each argument to the chosen
                    // candidate's parameter type so defaulted literal vars resolve
                    // to what selection assumed.
                    for (arg_ty, param) in arg_infer.iter().zip(&entry.params) {
                        ctx.add_constraint(arg_ty.clone(), type_to_infer(param), span.clone());
                    }
                    let ret_var = ctx.fresh_var();
                    ctx.add_constraint(ret_var.clone(), type_to_infer(&entry.ret), span.clone());
                    return Ok(ret_var);
                }
            }
            let callee_ty = infer_expr(callee, ctx, fun_generalizations)?;
            // Auto-deref: &(() -> T) and &mut (() -> T) are callable directly.
            let callee_ty = match ctx.solve()?.apply(&callee_ty) {
                InferType::Reference(inner) | InferType::MutReference(inner)
                    if matches!(*inner, InferType::Fun(..)) =>
                {
                    *inner
                }
                _ => callee_ty,
            };
            let arg_tys: Vec<InferType> = args
                .iter()
                .map(|a| infer_expr(a, ctx, fun_generalizations))
                .collect::<Result<_, _>>()?;
            if let InferType::Fun(params, _) = &callee_ty {
                if params.len() != arg_tys.len() {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0004,
                        format!(
                            "expected {} argument(s), got {}",
                            params.len(),
                            arg_tys.len()
                        ),
                        span,
                    ));
                }
            }
            let ret_var = ctx.fresh_var();
            ctx.add_constraint(
                callee_ty,
                InferType::Fun(arg_tys, Box::new(ret_var.clone())),
                span.clone(),
            );
            Ok(ret_var)
        }
        Expr::Index {
            object,
            index,
            span,
        } => {
            let obj_ty = infer_expr(object, ctx, fun_generalizations)?;
            // Index expression type is checked in the construction pass (must be u64).
            // No inference constraint needed here; plain int literals are promoted to u64 by construction.
            let _idx_ty = infer_expr(index, ctx, fun_generalizations)?;
            let elem_var = ctx.fresh_var();
            ctx.add_constraint(
                obj_ty,
                InferType::Array(Box::new(elem_var.clone())),
                span.clone(),
            );
            Ok(elem_var)
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
            span,
        } => {
            let cond_ty = infer_expr(condition, ctx, fun_generalizations)?;
            ctx.add_constraint(cond_ty, InferType::bool(), span.clone());
            let then_ty = infer_block(then_branch, ctx, fun_generalizations)?;
            if let Some(else_block) = else_branch {
                let else_ty = infer_block(else_block, ctx, fun_generalizations)?;
                ctx.add_constraint(then_ty.clone(), else_ty, span.clone());
                Ok(then_ty)
            } else {
                ctx.add_constraint(then_ty, InferType::unit(), span.clone());
                Ok(InferType::unit())
            }
        }
        Expr::Assign {
            target,
            op,
            value,
            span,
        } => {
            let target_ty = match target {
                AssignTarget::Ident(name, target_span) => {
                    // RFC-0067a write-through: assigning to a binding of type `&mut T`
                    // writes through the reference to `T` — the exclusivity comes from
                    // the reference, not the binding, so this applies whether or not
                    // the binding itself is `mut` (no fixture in this corpus ever
                    // reassigns a reference binding to a *different* reference, so
                    // there is no competing "repoint" interpretation to preserve here).
                    // A binding of type `&T` (shared) is never written through — that
                    // still requires ordinary `mut` reassignment of the binding itself.
                    // Peels every `&mut` layer of a chain (`&mut &mut T`), matching
                    // read-copy's own chain handling for the same auto-deref guarantee.
                    match ctx.lookup_mono_raw(name) {
                        Some(InferType::MutReference(inner)) => {
                            ctx.mark_write_through(span.clone());
                            let mut peeled = *inner;
                            while let InferType::MutReference(next) = peeled {
                                peeled = *next;
                            }
                            peeled
                        }
                        _ => ctx.lookup_for_write(name, target_span)?,
                    }
                }
                AssignTarget::Index {
                    object,
                    index,
                    span: target_span,
                } => {
                    let obj_ty = infer_expr(object, ctx, fun_generalizations)?;
                    // Index type checked in construction pass; no inference constraint here.
                    let _idx_ty = infer_expr(index, ctx, fun_generalizations)?;
                    let elem_var = ctx.fresh_var();
                    ctx.add_constraint(
                        obj_ty,
                        InferType::Array(Box::new(elem_var.clone())),
                        target_span.clone(),
                    );
                    elem_var
                }
                AssignTarget::FieldAccess {
                    object,
                    field,
                    span: target_span,
                } => infer_field_assign_type(object, field, target_span, ctx, fun_generalizations)?,
            };
            let value_ty = infer_expr(value, ctx, fun_generalizations)?;
            match op {
                AssignOp::Assign => {
                    ctx.add_constraint(target_ty, value_ty, span.clone());
                }
                AssignOp::AddAssign
                | AssignOp::SubAssign
                | AssignOp::MulAssign
                | AssignOp::DivAssign
                | AssignOp::RemAssign => {
                    let result = ctx.fresh_var();
                    ctx.add_constraint(target_ty, result.clone(), span.clone());
                    ctx.add_constraint(value_ty, result, span.clone());
                }
            }
            Ok(InferType::unit())
        }
        Expr::FieldAccess {
            object,
            field,
            span,
        } => {
            let obj_ty = infer_expr(object, ctx, fun_generalizations)?;
            let obj_ty = ctx.solve()?.apply(&obj_ty);
            let struct_name = named_type_name(&obj_ty).ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0002,
                    "cannot infer struct type for field access; add a type annotation",
                    span,
                )
            })?;
            let type_args = match &obj_ty {
                InferType::Named(_, args) => args.clone(),
                InferType::Reference(inner) | InferType::MutReference(inner) => {
                    match inner.as_ref() {
                        InferType::Named(_, args) => args.clone(),
                        _ => vec![],
                    }
                }
                _ => vec![],
            };
            let fields = ctx
                .get_struct_fields(&struct_name)
                .ok_or_else(|| {
                    MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!("unknown type `{struct_name}`"),
                        span,
                    )
                })?
                .clone();
            let field_entry = fields
                .iter()
                .find(|entry| entry.name == *field)
                .ok_or_else(|| {
                    MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!("no field `{field}` on `{struct_name}`"),
                        span,
                    )
                })?;
            check_field_visibility(
                field_entry,
                &struct_name,
                ctx.current_module_path(),
                ctx.registry().struct_declaring_module(&struct_name),
                span,
                "access",
            )?;
            let raw_ty = field_entry.ty.clone();
            // For generic structs, substitute declared type params with the resolved args.
            if let Some(type_params) = ctx.get_struct_type_params(&struct_name).cloned() {
                let mut remap = Substitution::new();
                for (&tp, arg) in type_params.iter().zip(type_args.iter()) {
                    remap.bind(tp, arg.clone());
                }
                Ok(remap.apply(&raw_ty))
            } else {
                Ok(raw_ty)
            }
        }
        Expr::MethodCall {
            receiver,
            method,
            args,
            span,
            ..
        } => {
            let recv_ty = infer_expr(receiver, ctx, fun_generalizations)?;
            let recv_ty = ctx.solve()?.apply(&recv_ty);
            // If the receiver is a numeric literal TypeVar, default it to i64/f64
            // so method dispatch can proceed with a concrete type.
            let recv_ty = if let InferType::Var(tv) = &recv_ty {
                if ctx.is_integer_literal_var(*tv) {
                    ctx.add_constraint(recv_ty.clone(), InferType::int(), span.clone());
                    InferType::int()
                } else if ctx.is_float_literal_var(*tv) {
                    ctx.add_constraint(recv_ty.clone(), InferType::float(), span.clone());
                    InferType::float()
                } else {
                    recv_ty
                }
            } else {
                recv_ty
            };

            let arg_tys: Vec<InferType> = args
                .iter()
                .map(|a| infer_expr(a, ctx, fun_generalizations))
                .collect::<Result<_, _>>()?;

            if let Some(result) = builtin_pattern_method_type(&recv_ty, method, &arg_tys, span) {
                return result;
            }

            // Fast path: concrete named type — look up method as usual.
            if let Some(struct_name) = named_type_name(&recv_ty) {
                let recv_type_args = match &recv_ty {
                    InferType::Named(_, args) => args.clone(),
                    InferType::Reference(inner) | InferType::MutReference(inner) => {
                        match inner.as_ref() {
                            InferType::Named(_, args) => args.clone(),
                            _ => vec![],
                        }
                    }
                    _ => vec![],
                };

                // Try concrete method_env first; fall back to method_scheme_env for generic structs.
                let method_ty = if let Some(ty) = ctx.get_method_type(&struct_name, method).cloned()
                {
                    ty
                } else if let Some((scheme, struct_tvars)) = ctx
                    .method_scheme_for(&struct_name, method)
                    .map(|(s, t)| (s.clone(), t.clone()))
                {
                    // Instantiate the scheme with a fresh TypeVar for EVERY
                    // quantified var — the struct's type params and the method's
                    // own generics (e.g. `U` in `fun map<U>(...)`). Instantiating
                    // only the struct tvars would leave the method-level generics
                    // as stale shared vars, so two call sites would collide and a
                    // single call could not resolve `U` from its arguments.
                    let mut inst = Substitution::new();
                    let mut renaming: HashMap<TypeVar, TypeVar> = HashMap::new();
                    for &qv in &scheme.quantified_vars {
                        let fresh = ctx.fresh_type_var_raw();
                        inst.bind(qv, InferType::Var(fresh));
                        renaming.insert(qv, fresh);
                    }
                    let instance = inst.apply(&scheme.ty);
                    // Pin the struct's (now fresh) type params to the receiver's
                    // concrete type args so `self`/return types line up.
                    let mut pin = Substitution::new();
                    for (&tv, arg) in struct_tvars.iter().zip(recv_type_args.iter()) {
                        if let Some(&fresh) = renaming.get(&tv) {
                            pin.bind(fresh, arg.clone());
                        }
                    }
                    pin.apply(&instance)
                } else {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!("no method `{method}` on `{struct_name}`"),
                        span,
                    ));
                };

                if matches!(
                    ctx.get_method_receiver_kind(&struct_name, method),
                    Some(crate::ast::ReceiverKind::RefMut)
                ) && !chain_provides_mut_access(&recv_ty)
                {
                    if let Expr::Ident(name, recv_span) = receiver.as_ref() {
                        let _ = ctx.lookup_for_write(name, recv_span)?;
                    }
                }

                let ret_var = ctx.fresh_var();
                let receiver_ty_for_method = peel_all_references(&recv_ty);
                let expected = InferType::Fun(
                    std::iter::once(receiver_ty_for_method)
                        .chain(arg_tys)
                        .collect(),
                    Box::new(ret_var.clone()),
                );
                ctx.add_constraint(method_ty, expected, span.clone());
                return Ok(ret_var);
            }

            // Slow path: TypeVar receiver — may be a bounded generic type param.
            if let InferType::Var(tv) = &recv_ty {
                if let Some(aspect_names) = ctx.bounds_for_type_var(*tv).cloned() {
                    for aspect_name in &aspect_names {
                        if let Some(methods) = ctx.get_aspect_method_defs(aspect_name).cloned() {
                            if let Some(method_def) = methods.iter().find(|m| m.name == *method) {
                                // Resolve return type: Self → the TypeVar itself. A bare
                                // associated-type name (RFC-0082 §1.2 sugar, e.g. `Item` in
                                // `fun next(...) -> Perhaps<Item>`'s inner `Item`, or here the
                                // whole return type) means `Self::Item` -- mint the same
                                // projection placeholder as an explicit `T::Item` would.
                                let ret_ty = method_def.return_type.as_ref().map_or(
                                    InferType::unit(),
                                    |rt| match rt {
                                        TypeExpr::Named(n, _) if n == "Self" => InferType::Var(*tv),
                                        TypeExpr::Named(n, args)
                                            if args.is_empty()
                                                && ctx
                                                    .registry()
                                                    .aspect_assoc_type_decls(aspect_name)
                                                    .is_some_and(|decls| {
                                                        decls.iter().any(|d| d.name == *n)
                                                    }) =>
                                        {
                                            InferType::Var(ctx.fresh_assoc_projection_var(
                                                *tv,
                                                aspect_name.clone(),
                                                n.clone(),
                                            ))
                                        }
                                        other => type_expr_to_infer(other),
                                    },
                                );

                                // Collect declared non-self params for arity + type checking.
                                let declared_params: Vec<&Param> = method_def
                                    .params
                                    .iter()
                                    .filter(|p| p.name != "self")
                                    .collect();

                                // Arity check.
                                if args.len() != declared_params.len() {
                                    return Err(MetelError::type_error(
                                        TypeErrorCode::T0004,
                                        format!(
                                            "`{aspect_name}::{method}` expects {} argument(s), got {}",
                                            declared_params.len(), args.len()
                                        ),
                                        span,
                                    ));
                                }

                                // Infer arg types and constrain each against the declared param type.
                                let arg_tys: Vec<InferType> = args
                                    .iter()
                                    .map(|a| infer_expr(a, ctx, fun_generalizations))
                                    .collect::<Result<_, _>>()?;

                                for (arg_ty, param) in arg_tys.iter().zip(declared_params.iter()) {
                                    if let Some(ann) = &param.type_ann {
                                        // Substitute Self → TypeVar for the param's declared type.
                                        let param_ty = match ann {
                                            TypeExpr::Named(n, _) if n == "Self" => {
                                                InferType::Var(*tv)
                                            }
                                            other => type_expr_to_infer(other),
                                        };
                                        ctx.add_constraint(arg_ty.clone(), param_ty, span.clone());
                                    }
                                }

                                let ret_var = ctx.fresh_var();
                                ctx.add_constraint(ret_var.clone(), ret_ty, span.clone());
                                return Ok(ret_var);
                            }
                        }
                    }
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!(
                            "no method `{method}` on type parameter (bounds: {})",
                            aspect_names.join(" + ")
                        ),
                        span,
                    ));
                }
            }

            Err(MetelError::type_error(
                TypeErrorCode::T0002,
                "cannot infer receiver type for method call; add a type annotation",
                span,
            ))
        }
        Expr::StructLiteral {
            path, fields, span, ..
        } => {
            if path.len() == 2 {
                infer_enum_variant_literal(
                    &path[0],
                    &path[1],
                    fields,
                    span,
                    ctx,
                    fun_generalizations,
                )
            } else {
                let struct_name = path
                    .last()
                    .ok_or_else(|| MetelError::internal("empty path in struct literal"))?
                    .clone();
                infer_struct_literal(struct_name, fields, span, ctx, fun_generalizations)
            }
        }
        Expr::Ascribe { expr, ann, span } => {
            let inner_ty = infer_expr(expr, ctx, fun_generalizations)?;
            let ascribed_ty = ann_to_infer(ann, ctx);
            Ok(constrain_with_read_copy(
                ctx,
                inner_ty,
                ascribed_ty,
                span.clone(),
            ))
        }

        Expr::Cast {
            expr,
            target_type,
            span,
        } => {
            let source_ty = infer_expr(expr, ctx, fun_generalizations)?;
            let target_ty = type_expr_to_infer(target_type);
            let solved = ctx.solve()?;
            let subst = ctx.default_literal_vars(&solved);
            let source_resolved = subst.apply(&source_ty);
            let target_resolved = subst.apply(&target_ty);
            // Identity casts always allowed.
            if source_resolved == target_resolved {
                return Ok(target_ty);
            }
            // Check via From aspect registry: target must implement From<source>.
            let source_concrete = infer_to_type_for_from(&source_resolved);
            let target_name = infer_type_name(&target_resolved);
            let valid = match (source_concrete.as_ref(), target_name) {
                (Some(src_t), Some(tgt)) => ctx.has_from_impl(tgt, src_t),
                _ => false,
            };
            if !valid {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0007,
                    format!("cannot cast `{source_resolved}` to `{target_resolved}` — no `impl From<{source_resolved}> for {target_resolved}` found"),
                    span,
                ));
            }
            Ok(target_ty)
        }
        Expr::TupleAccess {
            object,
            index,
            span,
        } => {
            let obj_ty = infer_expr(object, ctx, fun_generalizations)?;
            let obj_ty = ctx.solve()?.apply(&obj_ty);
            match &obj_ty {
                InferType::Tuple(elems) => elems.get(*index).cloned().ok_or_else(|| {
                    MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!(
                            "tuple index {index} out of bounds (tuple has {} elements)",
                            elems.len()
                        ),
                        span,
                    )
                }),
                _ => Err(MetelError::type_error(
                    TypeErrorCode::T0002,
                    "cannot infer tuple type for index access; add a type annotation",
                    span,
                )),
            }
        }
        Expr::Loop { body, span } => {
            let break_var = ctx.fresh_var();
            let saved_break = ctx.push_break_type(break_var.clone());
            infer_block(body, ctx, fun_generalizations)?;
            ctx.pop_break_type(saved_break);
            let _ = span;
            Ok(break_var)
        }
        Expr::Path(segments, span) => {
            // For 2-segment paths, first try TypeName::member (static methods, enum variants).
            if let [type_name, member_name] = segments.as_slice() {
                if let Some(fun_ty) = ctx.get_method_type(type_name, member_name).cloned() {
                    return Ok(fun_ty);
                }
                // Try builtin static constructors registered as joined-path poly schemes (e.g. "List::new").
                let joined = format!("{type_name}::{member_name}");
                if let Some(ty) = ctx.lookup(&joined) {
                    return Ok(ty);
                }
                // Static method on a generic struct/enum registered as a polymorphic
                // method scheme (e.g. native `List::new`). Resolving it here — from the
                // method scheme env rather than the prelude's joined-key schemes — lets
                // std::core reference its own static methods (e.g. `List::new()` inside
                // `List::map`) regardless of how the prelude is seeded.
                if let Some((scheme, _)) = ctx
                    .method_scheme_for(type_name, member_name)
                    .map(|(s, t)| (s.clone(), t.clone()))
                {
                    let mut inst = Substitution::new();
                    for &qv in &scheme.quantified_vars {
                        inst.bind(qv, InferType::Var(ctx.fresh_type_var_raw()));
                    }
                    return Ok(inst.apply(&scheme.ty));
                }
                if let Some(info) = ctx.get_enum(type_name).cloned() {
                    if let Some(variant) = info.variants.iter().find(|v| v.name == *member_name) {
                        if variant.fields.is_empty() {
                            let type_args: Vec<InferType> =
                                info.type_params.iter().map(|_| ctx.fresh_var()).collect();
                            return Ok(InferType::Named(type_name.clone(), type_args));
                        }
                    }
                }
            }
            let path_str = segments.join("::");
            Err(MetelError::type_error(
                TypeErrorCode::T0003,
                format!("unresolved path `{path_str}`"),
                span,
            ))
        }
        Expr::Closure {
            params,
            return_type,
            body,
            ..
        } => {
            let param_types: Vec<InferType> = params
                .iter()
                .map(|p| {
                    if let Some(ann) = &p.type_ann {
                        ann_to_infer(ann, ctx)
                    } else {
                        ctx.fresh_var()
                    }
                })
                .collect();
            // Not rewritten to `map_or_else` (clippy's own suggestion): both
            // closures would capture `ctx` mutably, and `map_or_else` requires
            // constructing both simultaneously as arguments, which the borrow
            // checker rejects (unlike this sequential match).
            let ret_ty = match &return_type {
                Some(ann) => ann_to_infer(ann, ctx),
                None => ctx.fresh_var(),
            };
            ctx.push_scope();
            for (p, pt) in params.iter().zip(param_types.iter()) {
                ctx.bind_mono(&p.name, pt.clone(), p.mutable);
            }
            let saved_ret = ctx.push_return_type(ret_ty.clone());
            let body_ty = infer_block(body, ctx, fun_generalizations)?;
            constrain_with_read_copy(ctx, body_ty, ret_ty.clone(), body.span.clone());
            ctx.pop_return_type(saved_ret);
            ctx.pop_scope();
            Ok(InferType::Fun(param_types, Box::new(ret_ty)))
        }
        Expr::Match(m) => infer_match(m, ctx, fun_generalizations),
        Expr::PropagateError { expr, span } => {
            infer_propagate_error(expr, span, ctx, fun_generalizations)
        }
        // Issue #229: `return`/`break`/`continue` as expressions of type `!`.
        Expr::Return(r) => {
            let ret_ty = match &r.value {
                Some(e) => infer_expr(e, ctx, fun_generalizations)?,
                None => InferType::unit(),
            };
            if let Some(expected) = ctx.current_return_type().cloned() {
                constrain_with_read_copy(ctx, ret_ty, expected, r.span.clone());
            }
            Ok(InferType::never())
        }
        Expr::Break(b) => {
            let break_ty = match &b.value {
                Some(e) => infer_expr(e, ctx, fun_generalizations)?,
                None => InferType::unit(),
            };
            if let Some(expected) = ctx.current_break_type().cloned() {
                constrain_with_read_copy(ctx, break_ty, expected, b.span.clone());
            }
            Ok(InferType::never())
        }
        Expr::Continue(_) => Ok(InferType::never()),
    }
}

fn infer_match(
    m: &MatchExpr,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    let scrutinee_ty = infer_expr(&m.scrutinee, ctx, fun_generalizations)?;
    let result_var = ctx.fresh_var();
    for arm in &m.arms {
        ctx.push_scope();
        infer_pattern(&arm.pattern, &scrutinee_ty, ctx)?;
        if let Some(guard) = &arm.guard {
            let g = infer_expr(guard, ctx, fun_generalizations)?;
            ctx.add_constraint(g, InferType::bool(), arm.span.clone());
        }
        let arm_ty = infer_block(&arm.body, ctx, fun_generalizations)?;
        ctx.add_constraint(arm_ty, result_var.clone(), arm.span.clone());
        ctx.pop_scope();
    }
    Ok(result_var)
}

fn infer_pattern(
    pattern: &Pattern,
    scrutinee_ty: &InferType,
    ctx: &mut InferContext,
) -> Result<(), MetelError> {
    let span = pattern_span(pattern);
    match pattern {
        Pattern::Wildcard(_) => {}
        Pattern::Literal(lit, _) => {
            let lit_ty = infer_literal(lit, ctx);
            ctx.add_constraint(scrutinee_ty.clone(), lit_ty, span.clone());
        }
        Pattern::Binding(name, _) => {
            ctx.bind_mono(name, scrutinee_ty.clone(), false);
        }
        Pattern::None(_) => {
            let fresh = ctx.fresh_var();
            ctx.add_constraint(
                scrutinee_ty.clone(),
                InferType::Named("Perhaps".to_string(), vec![fresh]),
                span.clone(),
            );
        }
        Pattern::Tuple(pats, _) => {
            let elem_vars: Vec<InferType> = pats.iter().map(|_| ctx.fresh_var()).collect();
            ctx.add_constraint(
                scrutinee_ty.clone(),
                InferType::Tuple(elem_vars.clone()),
                span.clone(),
            );
            for (pat, elem_ty) in pats.iter().zip(elem_vars.iter()) {
                infer_pattern(pat, elem_ty, ctx)?;
            }
        }
        Pattern::EnumVariant {
            path,
            fields,
            span: pat_span,
        } => {
            let [enum_name, variant_name] = path.as_slice() else {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("unresolved pattern path `{}`", path.join("::")),
                    pat_span,
                ));
            };
            infer_enum_variant_pattern(
                enum_name,
                variant_name,
                fields,
                scrutinee_ty,
                pat_span,
                ctx,
            )?;
        }
        Pattern::Array {
            elems,
            rest,
            span: pat_span,
        } => {
            let elem_var = ctx.fresh_var();
            if rest.is_some() {
                // Rest pattern: scrutinee must be an Array or SizedArray, bind rest as Array
                ctx.add_constraint(
                    scrutinee_ty.clone(),
                    InferType::Array(Box::new(elem_var.clone())),
                    pat_span.clone(),
                );
                if let Some(rest_name) = rest {
                    ctx.bind_mono(
                        rest_name,
                        InferType::Array(Box::new(elem_var.clone())),
                        false,
                    );
                }
            } else {
                // Exact pattern: scrutinee must be [T; N] where N = elems.len()
                let n = elems.len() as u64;
                ctx.add_constraint(
                    scrutinee_ty.clone(),
                    InferType::SizedArray(Box::new(elem_var.clone()), n),
                    pat_span.clone(),
                );
            }
            for pat in elems {
                infer_pattern(pat, &elem_var, ctx)?;
            }
        }
    }
    Ok(())
}

fn pattern_span(pattern: &Pattern) -> &Span {
    match pattern {
        Pattern::Wildcard(s)
        | Pattern::None(s)
        | Pattern::Binding(_, s)
        | Pattern::Literal(_, s)
        | Pattern::Tuple(_, s)
        | Pattern::EnumVariant { span: s, .. }
        | Pattern::Array { span: s, .. } => s,
    }
}

fn named_type_name(ty: &InferType) -> Option<String> {
    match ty {
        InferType::Named(name, _) => Some(name.clone()),
        InferType::Reference(inner) | InferType::MutReference(inner) => named_type_name(inner),
        InferType::Concrete(c) => primitive_type_name(c),
        _ => None,
    }
}

/// Peels every reference layer of a chain (RFC-0067a §3's auto-deref chain
/// guarantee applies to method dispatch the same as field access — mirrors
/// `named_type_name`'s own recursion, which already handles arbitrary depth).
fn peel_all_references(ty: &InferType) -> InferType {
    match ty {
        InferType::Reference(inner) | InferType::MutReference(inner) => peel_all_references(inner),
        other => other.clone(),
    }
}

/// Whether a `&mut self` method call through this receiver type has write access
/// *somewhere* along the chain — true the moment a `MutReference` layer is found,
/// regardless of how many shared `Reference` layers wrap it from the outside (a
/// shared reference to a `&mut T` still carries that inner `&mut T`'s own write
/// capability; reading it out doesn't downgrade it). All-`Reference` chains with
/// no `MutReference` anywhere have no write access at all.
fn chain_provides_mut_access(ty: &InferType) -> bool {
    match ty {
        InferType::MutReference(_) => true,
        InferType::Reference(inner) => chain_provides_mut_access(inner),
        _ => false,
    }
}

/// Canonical registry name for a primitive [`Type`], or `None` for non-primitive
/// types (tuples, arrays, functions, …). This is the single source of truth for
/// mapping a concrete primitive to the string key used in method and aspect-impl
/// registries.
pub(super) fn primitive_type_name(ty: &Type) -> Option<String> {
    let name = match ty {
        Type::Str => "String",
        Type::Boolean => "boolean",
        Type::Char => "Char",
        Type::I8 => "i8",
        Type::I16 => "i16",
        Type::I32 => "i32",
        Type::I64 => "i64",
        Type::U8 => "u8",
        Type::U16 => "u16",
        Type::U32 => "u32",
        Type::U64 => "u64",
        Type::F32 => "f32",
        Type::F64 => "f64",
        _ => return None,
    };
    Some(name.to_string())
}

/// Inverse of [`primitive_type_name`]: the concrete primitive [`Type`] for a
/// registry name, or `None` if the name is not a primitive. Used so that an
/// `impl` whose target is a primitive (e.g. `impl Display for i64`) builds its
/// `self` type as `Concrete(I64)` — matching what call sites produce — rather
/// than `Named("i64", [])`, which the unifier cannot bridge.
pub(super) fn primitive_type_from_name(name: &str) -> Option<Type> {
    let ty = match name {
        "String" => Type::Str,
        "boolean" => Type::Boolean,
        "Char" => Type::Char,
        "i8" => Type::I8,
        "i16" => Type::I16,
        "i32" => Type::I32,
        "i64" => Type::I64,
        "u8" => Type::U8,
        "u16" => Type::U16,
        "u32" => Type::U32,
        "u64" => Type::U64,
        "f32" => Type::F32,
        "f64" => Type::F64,
        _ => return None,
    };
    Some(ty)
}

fn builtin_pattern_method_type(
    recv_ty: &InferType,
    method: &str,
    arg_tys: &[InferType],
    span: &Span,
) -> Option<Result<InferType, MetelError>> {
    if matches!(recv_ty, InferType::Array(_) | InferType::SizedArray(_, _)) {
        if method == "len" && arg_tys.is_empty() {
            return Some(Ok(InferType::int()));
        }
        return Some(Err(MetelError::type_error(
            TypeErrorCode::T0003,
            format!("no method `{method}` on array type; use `List<T>` for mutable collections"),
            span,
        )));
    }

    None
}

fn infer_literal(lit: &Literal, ctx: &mut InferContext) -> InferType {
    use crate::ast::{FloatKind, IntKind};
    match lit {
        Literal::Int(_) => ctx.fresh_integer_literal_var(),
        Literal::Float(_) => ctx.fresh_float_literal_var(),
        Literal::SizedInt { kind, .. } => InferType::Concrete(match kind {
            IntKind::I8 => Type::I8,
            IntKind::I16 => Type::I16,
            IntKind::I32 => Type::I32,
            IntKind::I64 => Type::I64,
            IntKind::U8 => Type::U8,
            IntKind::U16 => Type::U16,
            IntKind::U32 => Type::U32,
            IntKind::U64 => Type::U64,
        }),
        Literal::SizedFloat { kind, .. } => InferType::Concrete(match kind {
            FloatKind::F32 => Type::F32,
            FloatKind::F64 => Type::F64,
        }),
        Literal::Char(_) => InferType::Concrete(Type::Char),
        Literal::Boolean(_) => InferType::bool(),
        Literal::Str(_) => InferType::str(),
        Literal::Unit => InferType::unit(),
        Literal::None => InferType::Named("Perhaps".to_string(), vec![ctx.fresh_var()]),
    }
}

fn infer_binop(
    lhs: &Expr,
    op: &BinOp,
    rhs: &Expr,
    span: &Span,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    let lhs_ty = infer_expr(lhs, ctx, fun_generalizations)?;
    let rhs_ty = infer_expr(rhs, ctx, fun_generalizations)?;
    match op {
        BinOp::Add => {
            let subst = ctx.solve()?;
            let lhs_resolved = subst.apply(&lhs_ty);
            let rhs_resolved = subst.apply(&rhs_ty);
            if matches!(lhs_resolved, InferType::Concrete(Type::Str))
                || matches!(rhs_resolved, InferType::Concrete(Type::Str))
            {
                match (&lhs_resolved, &rhs_resolved) {
                    (InferType::Concrete(Type::Str), InferType::Concrete(Type::Str)) => {
                        return Ok(InferType::str());
                    }
                    (InferType::Concrete(Type::Str), InferType::Var(v))
                    | (InferType::Var(v), InferType::Concrete(Type::Str)) => {
                        // Numeric literal TypeVars cannot be String — reject with T0005.
                        if ctx.is_integer_literal_var(*v) || ctx.is_float_literal_var(*v) {
                            return Err(MetelError::type_error(
                                TypeErrorCode::T0005,
                                format!("`+` requires i64, f64, or String operands, got `{lhs_resolved}` and `{rhs_resolved}`"),
                                span,
                            ));
                        }
                        ctx.add_constraint(lhs_ty, InferType::str(), span.clone());
                        ctx.add_constraint(rhs_ty, InferType::str(), span.clone());
                        return Ok(InferType::str());
                    }
                    _ => {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0005,
                            format!(
                                "`+` requires i64, f64, or String operands, got `{lhs_resolved}` and `{rhs_resolved}`"
                            ),
                            span,
                        ));
                    }
                }
            }
            let result = ctx.fresh_var();
            ctx.add_constraint(lhs_ty, result.clone(), span.clone());
            ctx.add_constraint(rhs_ty, result.clone(), span.clone());
            Ok(result)
        }
        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
            let result = ctx.fresh_var();
            ctx.add_constraint(lhs_ty, result.clone(), span.clone());
            ctx.add_constraint(rhs_ty, result.clone(), span.clone());
            Ok(result)
        }
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            ctx.add_constraint(lhs_ty, rhs_ty, span.clone());
            Ok(InferType::bool())
        }
        BinOp::And | BinOp::Or => {
            ctx.add_constraint(lhs_ty, InferType::bool(), span.clone());
            ctx.add_constraint(rhs_ty, InferType::bool(), span.clone());
            Ok(InferType::bool())
        }
        BinOp::Range | BinOp::RangeInclusive => {
            ctx.add_constraint(lhs_ty, InferType::int(), span.clone());
            ctx.add_constraint(rhs_ty, InferType::int(), span.clone());
            Ok(InferType::Named(
                "Range".to_string(),
                vec![InferType::int()],
            ))
        }
    }
}

fn infer_propagate_error(
    expr: &Expr,
    span: &Span,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    let ok_ty = ctx.fresh_var();
    let source_err_ty = ctx.fresh_var();
    let inner_ty = infer_expr(expr, ctx, fun_generalizations)?;
    ctx.add_constraint(
        inner_ty,
        InferType::Named(
            "Result".to_string(),
            vec![ok_ty.clone(), source_err_ty.clone()],
        ),
        span.clone(),
    );

    let expected_return = ctx.current_return_type().cloned().ok_or_else(|| {
        MetelError::type_error(
            TypeErrorCode::T0005,
            "`?` can only be used inside a function or closure that returns Result<T, E>",
            span,
        )
    })?;

    let target_ok_ty = ctx.fresh_var();
    let target_err_ty = ctx.fresh_var();
    ctx.add_constraint(
        expected_return,
        InferType::Named(
            "Result".to_string(),
            vec![target_ok_ty, target_err_ty.clone()],
        ),
        span.clone(),
    );

    let subst = ctx.solve()?;
    let source_resolved = subst.apply(&source_err_ty);
    let target_resolved = subst.apply(&target_err_ty);
    if source_resolved != target_resolved {
        let source_concrete = infer_to_type_for_from(&source_resolved);
        let target_name = infer_type_name(&target_resolved);
        if let (Some(src_t), Some(tgt)) = (source_concrete.as_ref(), target_name) {
            if !ctx.has_from_impl(tgt, src_t) {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0007,
                    format!(
                        "cannot propagate `{source_resolved}` as `{target_resolved}` — no `impl From<{source_resolved}> for {target_resolved}` found"
                    ),
                    span,
                ));
            }
        }
    }

    Ok(ok_ty)
}

fn infer_unaryop(
    op: &UnaryOp,
    operand: &Expr,
    span: &Span,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    let ty = infer_expr(operand, ctx, fun_generalizations)?;
    match op {
        UnaryOp::Neg => Ok(ty),
        UnaryOp::Not => {
            ctx.add_constraint(ty, InferType::bool(), span.clone());
            Ok(InferType::bool())
        }
        UnaryOp::Ref => Ok(InferType::Reference(Box::new(ty))),
        UnaryOp::RefMut => {
            if let Expr::Ident(name, ident_span) = operand {
                let _ = ctx.lookup_for_write(name, ident_span)?;
            }
            Ok(InferType::MutReference(Box::new(ty)))
        }
        UnaryOp::Deref => match ctx.solve()?.apply(&ty) {
            InferType::Reference(inner) | InferType::MutReference(inner) => Ok(*inner),
            other => Err(MetelError::type_error(
                TypeErrorCode::T0002,
                format!("cannot dereference non-pointer type `{other}`"),
                span,
            )),
        },
    }
}

/// RFC-0067a §3a: constrain `actual` against `declared`, except when `actual` is
/// *syntactically* a reference and `declared` isn't — then constrain the *referent*
/// against `declared` instead (a type-directed copy out of the reference). Only
/// applies where a literal declared type is known (`let`/`mut`, ascription, `return`,
/// `break`, and a function/method/closure body against its declared return type) —
/// never inside `unify()` itself, which stays a strict match everywhere else (call
/// arguments included).
///
/// Deliberately does **not** resolve `actual` through `ctx.solve()` first: `solve()` is
/// incremental and stateful (it advances `solved_constraint_count` and commits
/// `cached_subst`), so calling it eagerly at every annotated `let`/`return`/etc. across
/// a whole program — rather than only once, at the end, as before this rule existed —
/// changed constraint-processing order for unrelated code and broke an unrelated,
/// pre-existing test (a sized-array pattern arity mismatch started reporting a raw
/// unify failure, T0001, instead of the intended non-exhaustive-match diagnostic,
/// T0008). Matching on `actual` directly instead is sufficient for every real case:
/// `&expr`/`&mut expr` (`infer_unaryop`'s `Ref`/`RefMut` arms) produce `InferType::
/// Reference`/`MutReference` directly with no `Var` indirection, and reading an
/// existing reference-typed binding (`ctx.lookup`) returns its stored `InferType`
/// as-is, not a variable standing in for it.
///
/// `declared` being an unresolved `InferType::Var` means there is no real declared
/// type at this site (e.g. a function/closure with no return-type annotation, where
/// `ret_ty` is just a fresh var to be solved from the body) — that case must fall
/// through to plain unification so the body's own reference-ness flows out normally,
/// not be mistaken for "declared as a non-reference."
///
/// Returns `declared` only when read-copy actually fires; otherwise returns `actual`
/// unchanged, matching what every call site did before this rule existed. This isn't
/// cosmetic: binding a `let` to a freshly-reconstructed `declared` (from `ann_to_infer`)
/// instead of the value's own `actual` — even though the two are constrained equal —
/// swaps in a structurally-equal but not type-variable-identical `InferType`. That
/// broke an unrelated test (a sized-array pattern arity mismatch started reporting a
/// raw unify failure, T0001, instead of the intended non-exhaustive-match diagnostic,
/// T0008), caught only by running the full suite, not by reasoning about the rule
/// in isolation.
///
/// Peels *every* reference layer, not just one: RFC-0067a §3 guarantees auto-deref
/// chains through arbitrary depth (`&&T` derefs through both levels), and read-copy
/// is specified as the same auto-deref/copy story applied at a declared-type boundary
/// rather than a separate, shallower mechanism — `let x: i64 = rr;` where
/// `rr: &&i64` must copy through both layers, not stop after one and fail to unify
/// `&i64` against `i64`.
fn constrain_with_read_copy(
    ctx: &mut InferContext,
    actual: InferType,
    declared: InferType,
    span: Span,
) -> InferType {
    if matches!(
        declared,
        InferType::Reference(_) | InferType::MutReference(_) | InferType::Var(_)
    ) {
        ctx.add_constraint(actual.clone(), declared, span);
        return actual;
    }
    let mut peeled = actual.clone();
    let mut any_peel = false;
    while let InferType::Reference(inner) | InferType::MutReference(inner) = peeled {
        peeled = *inner;
        any_peel = true;
    }
    if any_peel {
        ctx.add_constraint(peeled, declared.clone(), span);
        return declared;
    }
    // RFC-0078 §3.3: if `actual` is (plausibly) a singleton-coercible enum, bind
    // the environment to `declared` rather than the raw enum type — otherwise a
    // later use of this binding (e.g. `x + y` where `y`'s binding is still the raw
    // `Result<i64, !>`) would compare the wrong, uncoerced type. The constraint
    // recorded here still carries the raw `actual`; `InferContext::solve`'s
    // registry-aware retry (`apply_constraint_with_coercion`) is what actually
    // verifies (or rejects) the coercion once types are fully resolved — this is
    // only an optimistic environment-binding choice, not the enforcement point.
    if crate::typeinference::singleton_coerce_field_ty(ctx.registry(), &actual).is_some() {
        ctx.add_constraint(actual, declared.clone(), span);
        return declared;
    }
    ctx.add_constraint(actual.clone(), declared, span);
    actual
}

fn is_same_declaring_module(
    current_module_path: &[String],
    declaring_module: Option<&Vec<String>>,
) -> bool {
    declaring_module.is_some_and(|module| module.as_slice() == current_module_path)
}

fn check_field_visibility(
    field: &FieldEntry,
    type_name: &str,
    current_module_path: &[String],
    declaring_module: Option<&Vec<String>>,
    span: &Span,
    action: &str,
) -> Result<(), MetelError> {
    if field.visibility == Visibility::Public
        || is_same_declaring_module(current_module_path, declaring_module)
    {
        return Ok(());
    }
    Err(MetelError::type_error(
        TypeErrorCode::T0009,
        format!("visibility error: cannot {action} private field `{}` of `{type_name}` from outside its declaring module", field.name),
        span,
    ))
}

fn infer_enum_variant_literal(
    enum_name: &str,
    variant_name: &str,
    fields: &[(String, Expr)],
    span: &Span,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    let enum_decl_module = ctx.registry().enum_declaring_module(enum_name).cloned();
    let enum_info = ctx
        .get_enum(enum_name)
        .ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!("unknown enum `{enum_name}`"),
                span,
            )
        })?
        .clone();
    let variant = enum_info
        .variants
        .iter()
        .find(|v| v.name == variant_name)
        .ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!("no variant `{variant_name}` on enum `{enum_name}`"),
                span,
            )
        })?
        .clone();
    let mut remap: HashMap<TypeVar, InferType> = HashMap::new();
    for &tp in &enum_info.type_params {
        remap.insert(tp, ctx.fresh_var());
    }
    for (fname, expr) in fields {
        let field = variant
            .fields
            .iter()
            .find(|field| field.name == *fname)
            .ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("no field `{fname}` on `{enum_name}::{variant_name}`"),
                    span,
                )
            })?;
        check_field_visibility(
            field,
            &format!("{enum_name}::{variant_name}"),
            ctx.current_module_path(),
            enum_decl_module.as_ref(),
            span,
            "construct",
        )?;
        let decl_ty = match &field.ty {
            InferType::Var(v) => remap.get(v).cloned().unwrap_or_else(|| field.ty.clone()),
            other => other.clone(),
        };
        let expr_ty = infer_expr(expr, ctx, fun_generalizations)?;
        ctx.add_constraint(expr_ty, decl_ty, span.clone());
    }
    let type_args: Vec<InferType> = enum_info
        .type_params
        .iter()
        .map(|tp| remap[tp].clone())
        .collect();
    Ok(InferType::Named(enum_name.to_string(), type_args))
}

fn infer_struct_literal(
    struct_name: String,
    fields: &[(String, Expr)],
    span: &Span,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    let struct_decl_module = ctx
        .registry()
        .struct_declaring_module(&struct_name)
        .cloned();
    let expected_fields = ctx
        .get_struct_fields(&struct_name)
        .ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!("unknown struct `{struct_name}`"),
                span,
            )
        })?
        .clone();
    // For generic structs, create fresh type vars and remap declared TypeVars.
    let type_params = ctx.get_struct_type_params(&struct_name).cloned();
    let mut remap: HashMap<TypeVar, InferType> = HashMap::new();
    if let Some(ref params) = type_params {
        for &tp in params {
            remap.insert(tp, ctx.fresh_var());
        }
    }
    let apply_remap = |ty: &InferType| -> InferType {
        if remap.is_empty() {
            return ty.clone();
        }
        match ty {
            InferType::Var(v) => remap.get(v).cloned().unwrap_or_else(|| ty.clone()),
            other => other.clone(),
        }
    };
    for (name, expr) in fields {
        let field = expected_fields
            .iter()
            .find(|field| field.name == *name)
            .ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("no field `{name}` on `{struct_name}`"),
                    span,
                )
            })?;
        check_field_visibility(
            field,
            &struct_name,
            ctx.current_module_path(),
            struct_decl_module.as_ref(),
            span,
            "construct",
        )?;
        let decl_ty = apply_remap(&field.ty);
        let expr_ty = infer_expr(expr, ctx, fun_generalizations)?;
        ctx.add_constraint(expr_ty, decl_ty, span.clone());
    }
    for field in &expected_fields {
        if !fields.iter().any(|(n, _)| n == &field.name) {
            return Err(MetelError::type_error(
                TypeErrorCode::T0003,
                format!("missing field `{}` in `{struct_name}`", field.name),
                span,
            ));
        }
    }
    let type_args: Vec<InferType> = type_params
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|tp| remap[tp].clone())
        .collect();
    Ok(InferType::Named(struct_name, type_args))
}

/// Walk an lvalue chain to the root identifier for mutability checking.
/// Returns `None` when the chain passes through a pointer dereference,
/// meaning write access is conferred by the pointer rather than the binding.
fn root_binding_for_write(expr: &Expr) -> Option<(&str, &Span)> {
    match expr {
        Expr::Ident(name, span) => Some((name.as_str(), span)),
        Expr::FieldAccess { object, .. } | Expr::Index { object, .. } => {
            root_binding_for_write(object)
        }
        _ => None,
    }
}

fn infer_field_assign_type(
    object: &Expr,
    field: &str,
    target_span: &Span,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    let obj_ty = infer_expr(object, ctx, fun_generalizations)?;
    let obj_ty = ctx.solve()?.apply(&obj_ty);
    // Auto-deref through &mut T: writing via a mutable reference doesn't require
    // the reference binding itself to be mutable — only the referent is being written.
    // This must also cover a *chain* rooted in a reference (`optr.inner.x = 42` where
    // `optr: &mut Outer`): `obj_ty` here is `optr.inner`'s own type (`Point`), which no
    // longer shows any reference at all — auto-deref already stripped it one level up.
    // So the check isn't just "is `obj_ty` itself a reference," it's "is the write
    // access for this whole chain conferred by a reference anywhere along it" — which
    // reduces to asking whether the chain's *root* binding is reference-typed, since a
    // reference confers write access to everything reachable through it regardless of
    // projection depth.
    let is_through_mut_ptr = matches!(&obj_ty, InferType::MutReference(_))
        || matches!(
            root_binding_for_write(object).and_then(|(name, _)| ctx.lookup_mono_raw(name)),
            Some(InferType::MutReference(_))
        );
    if !is_through_mut_ptr {
        if let Some((name, span)) = root_binding_for_write(object) {
            let _ = ctx.lookup_for_write(name, span)?;
        }
    }
    let struct_name = named_type_name(&obj_ty).ok_or_else(|| {
        MetelError::type_error(
            TypeErrorCode::T0002,
            "cannot infer struct type for field assignment; add a type annotation",
            target_span,
        )
    })?;
    let type_args = match &obj_ty {
        InferType::Named(_, args) => args.clone(),
        InferType::Reference(inner) | InferType::MutReference(inner) => match inner.as_ref() {
            InferType::Named(_, args) => args.clone(),
            _ => vec![],
        },
        _ => vec![],
    };
    let fields = ctx
        .get_struct_fields(&struct_name)
        .ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!("unknown type `{struct_name}`"),
                target_span,
            )
        })?
        .clone();
    let field_entry = fields
        .iter()
        .find(|entry| entry.name == field)
        .ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!("no field `{field}` on `{struct_name}`"),
                target_span,
            )
        })?;
    check_field_visibility(
        field_entry,
        &struct_name,
        ctx.current_module_path(),
        ctx.registry().struct_declaring_module(&struct_name),
        target_span,
        "assign to",
    )?;
    let raw_ty = field_entry.ty.clone();
    if let Some(type_params) = ctx.get_struct_type_params(&struct_name).cloned() {
        let mut remap = Substitution::new();
        for (&tp, arg) in type_params.iter().zip(type_args.iter()) {
            remap.bind(tp, arg.clone());
        }
        Ok(remap.apply(&raw_ty))
    } else {
        Ok(raw_ty)
    }
}

fn infer_enum_variant_pattern(
    enum_name: &str,
    variant_name: &str,
    fields: &[String],
    scrutinee_ty: &InferType,
    pat_span: &Span,
    ctx: &mut InferContext,
) -> Result<(), MetelError> {
    let enum_decl_module = ctx.registry().enum_declaring_module(enum_name).cloned();
    let enum_info = ctx
        .get_enum(enum_name)
        .ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!("unknown enum `{enum_name}` in pattern"),
                pat_span,
            )
        })?
        .clone();
    let variant = enum_info
        .variants
        .iter()
        .find(|v| v.name == variant_name)
        .ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!("no variant `{variant_name}` on `{enum_name}`"),
                pat_span,
            )
        })?
        .clone();
    let mut remap: HashMap<TypeVar, InferType> = HashMap::new();
    for &tp in &enum_info.type_params {
        remap.insert(tp, ctx.fresh_var());
    }
    let type_args: Vec<InferType> = enum_info
        .type_params
        .iter()
        .map(|tp| remap[tp].clone())
        .collect();
    ctx.add_constraint(
        scrutinee_ty.clone(),
        InferType::Named(enum_name.to_string(), type_args),
        pat_span.clone(),
    );
    for field_name in fields {
        let field = variant
            .fields
            .iter()
            .find(|field| field.name == *field_name)
            .ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("no field `{field_name}` on `{enum_name}::{variant_name}`"),
                    pat_span,
                )
            })?;
        check_field_visibility(
            field,
            &format!("{enum_name}::{variant_name}"),
            ctx.current_module_path(),
            enum_decl_module.as_ref(),
            pat_span,
            "pattern-match on",
        )?;
        let field_ty = match &field.ty {
            InferType::Var(v) => remap.get(v).cloned().unwrap_or_else(|| field.ty.clone()),
            other => other.clone(),
        };
        ctx.bind_mono(field_name, field_ty, false);
    }
    Ok(())
}

// ── Helpers for From/Iterable dispatch ───────────────────────────────────────

/// Extract a concrete `Type` from an `InferType` for use in From-impl lookups.
fn infer_to_type_for_from(ty: &InferType) -> Option<Type> {
    match ty {
        InferType::Concrete(t) => Some(t.clone()),
        InferType::Named(name, _) => Some(Type::Named(name.clone(), vec![])),
        _ => None,
    }
}

/// Extract the type name string from an `InferType` for registry lookups.
fn infer_type_name(ty: &InferType) -> Option<&str> {
    match ty {
        InferType::Concrete(Type::I64) => Some("i64"),
        InferType::Concrete(Type::F64) => Some("f64"),
        InferType::Concrete(Type::Boolean) => Some("boolean"),
        InferType::Concrete(Type::Char) => Some("Char"),
        InferType::Concrete(Type::Str) => Some("String"),
        InferType::Concrete(Type::I8) => Some("i8"),
        InferType::Concrete(Type::I16) => Some("i16"),
        InferType::Concrete(Type::I32) => Some("i32"),
        InferType::Concrete(Type::U8) => Some("u8"),
        InferType::Concrete(Type::U16) => Some("u16"),
        InferType::Concrete(Type::U32) => Some("u32"),
        InferType::Concrete(Type::U64) => Some("u64"),
        InferType::Concrete(Type::F32) => Some("f32"),
        InferType::Named(name, _) => Some(name.as_str()),
        _ => None,
    }
}

// ── impl Aspect lowering pass ─────────────────────────────────────────────────

/// Lower `impl Aspect` type expressions in function parameter positions to fresh
/// anonymous generic type parameters before inference runs. This pass rewrites
/// the `FunDecl` AST in-place (via a returned owned copy).
///
/// `fun foo(x: impl Display)` becomes `fun foo<_T0: Display>(x: _T0)`.
///
/// Each `impl Aspect` occurrence generates a fresh, independent type parameter.
/// The source spelling ("impl Display") is stored in the param name as a hint
/// for error messages (the typechecker uses GenericParam.bounds for enforcement).
pub(super) fn lower_impl_aspect(fun: &FunDecl, counter: &mut usize) -> FunDecl {
    let mut extra_generics: Vec<GenericParam> = Vec::new();
    let new_params: Vec<Param> = fun
        .params
        .iter()
        .map(|p| {
            match &p.type_ann {
                Some(TypeExpr::ImplAspect {
                    bound,
                    source_spell: _,
                    ..
                }) => {
                    let anon_name = format!("_ImplT{counter}");
                    *counter += 1;
                    extra_generics.push(GenericParam {
                        name: anon_name.clone(),
                        bounds: vec![Bound {
                            polarity: Polarity::Positive,
                            aspect: *bound.clone(),
                            assoc_bindings: vec![],
                            span: p.span.clone(),
                        }],
                    });
                    Param {
                        mutable: p.mutable,
                        receiver: p.receiver.clone(),
                        name: p.name.clone(),
                        type_ann: Some(TypeExpr::Named(anon_name, vec![])),
                        // Store source spelling as a tag in the span source (best-effort).
                        // The real error message metadata lives in GenericParam.bounds.
                        span: p.span.clone(),
                    }
                }
                _ => p.clone(),
            }
        })
        .collect();

    let mut new_generics = fun.generics.clone();
    new_generics.extend(extra_generics);

    FunDecl {
        visibility: fun.visibility.clone(),
        name: fun.name.clone(),
        generics: new_generics,
        where_clause: fun.where_clause.clone(),
        params: new_params,
        return_type: fun.return_type.clone(),
        native: fun.native.clone(),
        body: fun.body.clone(),
        span: fun.span.clone(),
    }
}

/// Lower all `impl Aspect` params in all `FunDecl`s in a `Program`.
/// Returns a new program with the lowered declarations.
pub(super) fn lower_impl_aspects_in_program(program: Program) -> Program {
    let mut counter = 0usize;
    let decls = program
        .decls
        .into_iter()
        .map(|decl| match decl {
            Decl::Fun(fun) => Decl::Fun(lower_impl_aspect(&fun, &mut counter)),
            Decl::Impl(ib) => Decl::Impl(ImplBlock {
                methods: ib
                    .methods
                    .iter()
                    .map(|m| lower_impl_aspect(m, &mut counter))
                    .collect(),
                ..ib
            }),
            other => other,
        })
        .collect();
    Program { decls, ..program }
}

/// Rewrite `T::AssocType`-shaped `TypeExpr::Named` nodes into `TypeExpr::Projection`
/// wherever `T` matches one of `generics`' names (RFC-0082 SS3). Purely structural —
/// checks only whether the name is a declared generic parameter, not whether the
/// aspect it's bound to actually declares that associated type; real associated-type
/// resolution is issue #242's job. The parser can't do this itself (`type_path`
/// already accepts multi-segment names, so `T::Target` parses as a plain dotted
/// `Named` either way) since recognizing a projection needs to know which names are
/// declared generics, context the parser doesn't have.
fn lower_projections_in_decl(decl: Decl) -> Decl {
    match decl {
        Decl::Fun(fun) => Decl::Fun(lower_projections_in_fun(&fun, &[])),
        Decl::Let(let_decl) => Decl::Let(crate::ast::LetDecl {
            type_ann: let_decl
                .type_ann
                .as_ref()
                .map(|t| lower_projections_in_type(t, &std::collections::HashSet::new(), &let_decl.span)),
            value: lower_projections_in_expr(&let_decl.value, &std::collections::HashSet::new()),
            ..let_decl
        }),
        Decl::Mut(mut_decl) => Decl::Mut(crate::ast::MutDecl {
            type_ann: mut_decl
                .type_ann
                .as_ref()
                .map(|t| lower_projections_in_type(t, &std::collections::HashSet::new(), &mut_decl.span)),
            value: lower_projections_in_expr(&mut_decl.value, &std::collections::HashSet::new()),
            ..mut_decl
        }),
        Decl::Impl(ib) => Decl::Impl(crate::ast::ImplBlock {
            methods: ib
                .methods
                .iter()
                .map(|m| lower_projections_in_fun(m, &ib.generics))
                .collect(),
            ..ib
        }),
        Decl::Stmt(stmt) => Decl::Stmt(Box::new(lower_projections_in_stmt(
            &stmt,
            &std::collections::HashSet::new(),
        ))),
        other => other,
    }
}

fn lower_projections_in_block(
    block: &crate::ast::Block,
    generics: &std::collections::HashSet<String>,
) -> crate::ast::Block {
    crate::ast::Block {
        stmts: block
            .stmts
            .iter()
            .map(|d| lower_projections_in_decl_with_generics(d, generics))
            .collect(),
        tail: block
            .tail
            .as_ref()
            .map(|e| Box::new(lower_projections_in_expr(e, generics))),
        span: block.span.clone(),
    }
}

fn lower_projections_in_decl_with_generics(
    decl: &Decl,
    generics: &std::collections::HashSet<String>,
) -> Decl {
    match decl {
        Decl::Let(let_decl) => Decl::Let(crate::ast::LetDecl {
            type_ann: let_decl
                .type_ann
                .as_ref()
                .map(|t| lower_projections_in_type(t, generics, &let_decl.span)),
            value: lower_projections_in_expr(&let_decl.value, generics),
            ..let_decl.clone()
        }),
        Decl::Mut(mut_decl) => Decl::Mut(crate::ast::MutDecl {
            type_ann: mut_decl
                .type_ann
                .as_ref()
                .map(|t| lower_projections_in_type(t, generics, &mut_decl.span)),
            value: lower_projections_in_expr(&mut_decl.value, generics),
            ..mut_decl.clone()
        }),
        Decl::Stmt(stmt) => Decl::Stmt(Box::new(lower_projections_in_stmt(stmt, generics))),
        Decl::Fun(fun) => Decl::Fun(lower_projections_in_fun_with_generics(fun, generics)),
        Decl::Impl(ib) => Decl::Impl(crate::ast::ImplBlock {
            methods: ib
                .methods
                .iter()
                .map(|m| lower_projections_in_fun(m, &ib.generics))
                .collect(),
            ..ib.clone()
        }),
        other => other.clone(),
    }
}

fn lower_projections_in_fun_with_generics(
    fun: &FunDecl,
    parent_generics: &std::collections::HashSet<String>,
) -> FunDecl {
    let mut names: std::collections::HashSet<String> = parent_generics.clone();
    for g in &fun.generics {
        names.insert(g.name.clone());
    }
    if names.is_empty() {
        return fun.clone();
    }
    let params = fun
        .params
        .iter()
        .map(|p| Param {
            type_ann: p
                .type_ann
                .as_ref()
                .map(|t| lower_projections_in_type(t, &names, &p.span)),
            ..p.clone()
        })
        .collect();
    let return_type = fun
        .return_type
        .as_ref()
        .map(|t| lower_projections_in_type(t, &names, &fun.span));
    let body = lower_projections_in_block(&fun.body, &names);
    FunDecl {
        params,
        return_type,
        body,
        ..fun.clone()
    }
}

fn lower_projections_in_stmt(
    stmt: &crate::ast::Stmt,
    generics: &std::collections::HashSet<String>,
) -> crate::ast::Stmt {
    match stmt {
        crate::ast::Stmt::While(ws) => crate::ast::Stmt::While(crate::ast::WhileStmt {
            condition: lower_projections_in_expr(&ws.condition, generics),
            body: lower_projections_in_block(&ws.body, generics),
            span: ws.span.clone(),
        }),
        crate::ast::Stmt::For(fs) => {
            let init = fs.init.as_ref().map(|fi| match fi {
                crate::ast::ForInit::Let(l) => crate::ast::ForInit::Let(crate::ast::LetDecl {
                    type_ann: l
                        .type_ann
                        .as_ref()
                        .map(|t| lower_projections_in_type(t, generics, &l.span)),
                    value: lower_projections_in_expr(&l.value, generics),
                    ..l.clone()
                }),
                crate::ast::ForInit::Mut(m) => crate::ast::ForInit::Mut(crate::ast::MutDecl {
                    type_ann: m
                        .type_ann
                        .as_ref()
                        .map(|t| lower_projections_in_type(t, generics, &m.span)),
                    value: lower_projections_in_expr(&m.value, generics),
                    ..m.clone()
                }),
                crate::ast::ForInit::Expr(e) => {
                    crate::ast::ForInit::Expr(lower_projections_in_expr(e, generics))
                }
            });
            crate::ast::Stmt::For(Box::new(crate::ast::ForStmt {
                init,
                condition: fs
                    .condition
                    .as_ref()
                    .map(|c| lower_projections_in_expr(c, generics)),
                step: fs
                    .step
                    .as_ref()
                    .map(|s| lower_projections_in_expr(s, generics)),
                body: lower_projections_in_block(&fs.body, generics),
                span: fs.span.clone(),
            }))
        }
        crate::ast::Stmt::ForIn(fis) => crate::ast::Stmt::ForIn(Box::new(
            crate::ast::ForInStmt {
                binding: fis.binding.clone(),
                mutable: fis.mutable,
                iterable: lower_projections_in_expr(&fis.iterable, generics),
                body: lower_projections_in_block(&fis.body, generics),
                span: fis.span.clone(),
            },
        )),
        crate::ast::Stmt::Expr(e) => crate::ast::Stmt::Expr(lower_projections_in_expr(e, generics)),
    }
}

// Exhaustive match over every Expr variant; splitting it up would scatter
// one coherent dispatch table across many small functions with no real gain
// in clarity.
#[allow(clippy::too_many_lines)]
fn lower_projections_in_expr(
    expr: &Expr,
    generics: &std::collections::HashSet<String>,
) -> Expr {
    let go = |e: &Expr| lower_projections_in_expr(e, generics);
    match expr {
        Expr::Call {
            callee,
            type_args,
            args,
            span,
        } => Expr::Call {
            callee: Box::new(go(callee)),
            type_args: type_args
                .iter()
                .map(|t| lower_projections_in_type(t, generics, span))
                .collect(),
            args: args.iter().map(go).collect(),
            span: span.clone(),
        },
        Expr::MethodCall {
            receiver,
            method,
            type_args,
            args,
            span,
        } => Expr::MethodCall {
            receiver: Box::new(go(receiver)),
            method: method.clone(),
            type_args: type_args
                .iter()
                .map(|t| lower_projections_in_type(t, generics, span))
                .collect(),
            args: args.iter().map(go).collect(),
            span: span.clone(),
        },
        Expr::Cast {
            expr: e,
            target_type,
            span,
        } => Expr::Cast {
            expr: Box::new(go(e)),
            target_type: lower_projections_in_type(target_type, generics, span),
            span: span.clone(),
        },
        Expr::Ascribe {
            expr: e,
            ann,
            span,
        } => Expr::Ascribe {
            expr: Box::new(go(e)),
            ann: lower_projections_in_type(ann, generics, span),
            span: span.clone(),
        },
        Expr::Closure {
            params,
            return_type,
            body,
            span,
        } => Expr::Closure {
            params: params
                .iter()
                .map(|p| Param {
                    type_ann: p
                        .type_ann
                        .as_ref()
                        .map(|t| lower_projections_in_type(t, generics, &p.span)),
                    ..p.clone()
                })
                .collect(),
            return_type: return_type
                .as_ref()
                .map(|t| lower_projections_in_type(t, generics, span)),
            body: lower_projections_in_block(body, generics),
            span: span.clone(),
        },
        Expr::If {
            condition,
            then_branch,
            else_branch,
            span,
        } => Expr::If {
            condition: Box::new(go(condition)),
            then_branch: lower_projections_in_block(then_branch, generics),
            else_branch: else_branch
                .as_ref()
                .map(|b| lower_projections_in_block(b, generics)),
            span: span.clone(),
        },
        Expr::Loop { body, span } => Expr::Loop {
            body: lower_projections_in_block(body, generics),
            span: span.clone(),
        },
        Expr::Match(m) => Expr::Match(crate::ast::MatchExpr {
            scrutinee: Box::new(go(&m.scrutinee)),
            arms: m
                .arms
                .iter()
                .map(|a| crate::ast::MatchArm {
                    pattern: a.pattern.clone(),
                    guard: a.guard.as_ref().map(&go),
                    body: lower_projections_in_block(&a.body, generics),
                    span: a.span.clone(),
                })
                .collect(),
            span: m.span.clone(),
        }),
        Expr::Tuple(es, s) => Expr::Tuple(es.iter().map(go).collect(), s.clone()),
        Expr::Array(es, s) => Expr::Array(es.iter().map(go).collect(), s.clone()),
        Expr::RepeatArray(e, n, s) => Expr::RepeatArray(Box::new(go(e)), *n, s.clone()),
        Expr::BinOp(l, op, r, s) => {
            Expr::BinOp(Box::new(go(l)), op.clone(), Box::new(go(r)), s.clone())
        }
        Expr::UnaryOp(op, e, s) => Expr::UnaryOp(op.clone(), Box::new(go(e)), s.clone()),
        Expr::Assign {
            target,
            op,
            value,
            span,
        } => Expr::Assign {
            target: target.clone(),
            op: op.clone(),
            value: Box::new(go(value)),
            span: span.clone(),
        },
        Expr::FieldAccess {
            object, field, span,
        } => Expr::FieldAccess {
            object: Box::new(go(object)),
            field: field.clone(),
            span: span.clone(),
        },
        Expr::TupleAccess {
            object, index, span,
        } => Expr::TupleAccess {
            object: Box::new(go(object)),
            index: *index,
            span: span.clone(),
        },
        Expr::Index {
            object, index, span,
        } => Expr::Index {
            object: Box::new(go(object)),
            index: Box::new(go(index)),
            span: span.clone(),
        },
        Expr::PropagateError { expr: e, span } => Expr::PropagateError {
            expr: Box::new(go(e)),
            span: span.clone(),
        },
        Expr::Return(re) => Expr::Return(crate::ast::ReturnExpr {
            value: re.value.as_ref().map(|v| Box::new(go(v))),
            span: re.span.clone(),
        }),
        Expr::Break(br) => Expr::Break(crate::ast::BreakExpr {
            value: br.value.as_ref().map(|v| Box::new(go(v))),
            span: br.span.clone(),
        }),
        // Leaf expressions — no sub-Expr or TypeExpr to rewrite.
        Expr::Literal(_, _)
        | Expr::Ident(_, _)
        | Expr::Path(_, _)
        | Expr::ResolvedPath { .. }
        | Expr::Continue(_) => expr.clone(),
        Expr::StructLiteral {
            path,
            fields,
            symbol_id,
            span,
        } => Expr::StructLiteral {
            path: path.clone(),
            fields: fields.iter().map(|(n, e)| (n.clone(), go(e))).collect(),
            symbol_id: *symbol_id,
            span: span.clone(),
        },
    }
}

fn lower_projections_in_type(
    te: &TypeExpr,
    generics: &std::collections::HashSet<String>,
    fallback_span: &Span,
) -> TypeExpr {
    let go = |t: &TypeExpr| lower_projections_in_type(t, generics, fallback_span);
    match te {
        TypeExpr::Named(name, args) if args.is_empty() => {
            if let Some((base, assoc)) = name.split_once("::") {
                if generics.contains(base) {
                    return TypeExpr::Projection {
                        base: Box::new(TypeExpr::Named(base.to_string(), vec![])),
                        assoc_name: assoc.to_string(),
                        span: fallback_span.clone(),
                    };
                }
            }
            te.clone()
        }
        TypeExpr::Named(name, args) => TypeExpr::Named(name.clone(), args.iter().map(go).collect()),
        TypeExpr::Unit => TypeExpr::Unit,
        TypeExpr::Tuple(items) => TypeExpr::Tuple(items.iter().map(go).collect()),
        TypeExpr::Array(inner) => TypeExpr::Array(Box::new(go(inner))),
        TypeExpr::SizedArray(inner, n) => TypeExpr::SizedArray(Box::new(go(inner)), *n),
        TypeExpr::Reference(inner) => TypeExpr::Reference(Box::new(go(inner))),
        TypeExpr::MutReference(inner) => TypeExpr::MutReference(Box::new(go(inner))),
        TypeExpr::Fun(params, ret) => TypeExpr::Fun(
            params.iter().map(go).collect(),
            ret.as_deref().map(go).map(Box::new),
        ),
        TypeExpr::ImplAspect {
            bound,
            source_spell,
            span,
        } => TypeExpr::ImplAspect {
            bound: Box::new(go(bound)),
            source_spell: source_spell.clone(),
            span: span.clone(),
        },
        // Already a projection (e.g. re-run on already-lowered input) — nothing to do.
        TypeExpr::Projection { .. } => te.clone(),
    }
}

/// Generic-parameter names in scope for lowering projections in `fun`'s signature:
/// its own generics plus (for impl methods) the impl block's, since a method can
/// reference either (`T` from `impl<T> Aspect for Type<T>`, or its own `<U>`).
fn lower_projections_in_fun(fun: &FunDecl, extra_generics: &[GenericParam]) -> FunDecl {
    let names: std::collections::HashSet<String> = fun
        .generics
        .iter()
        .chain(extra_generics)
        .map(|g| g.name.clone())
        .collect();
    if names.is_empty() {
        return fun.clone();
    }
    lower_projections_in_fun_with_generics(fun, &names)
}

/// Lower all `T::AssocType` projections in every `FunDecl`'s params/return-type in a
/// `Program`. Also descends into function bodies to lower type annotations on
/// `let`/`mut` bindings, closure signatures, cast targets, ascribe annotations,
/// and generic type arguments in call sites — any `TypeExpr` that could reference
/// an associated type from a generic param.
pub(super) fn lower_projections_in_program(program: Program) -> Program {
    let decls = program
        .decls
        .into_iter()
        .map(lower_projections_in_decl)
        .collect();
    Program { decls, ..program }
}
