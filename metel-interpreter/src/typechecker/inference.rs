use std::collections::HashMap;

use crate::ast::{
    AspectMethod, AssignOp, AssignTarget, BinOp, Block, Bound, Decl, Expr, ForInit, FunDecl,
    GenericParam, ImplBlock, Literal, MatchExpr, Param, Pattern, Polarity, Program, Span, Stmt,
    TypeExpr, UnaryOp, Visibility,
};
use crate::error::{MetelError, TypeErrorCode};
use crate::typeinference::{
    free_vars, generalize, AspectAssumptions, EnumInfo, FieldEntry, GenericBound, InferContext,
    InferType, Substitution, TypeScheme, TypeVar, VariantInfo,
};
use crate::types::Type;

use super::conversions::{
    infer_type_to_type, type_expr_to_infer, type_expr_to_infer_with_assoc_ctx,
    type_expr_to_infer_with_generics, type_expr_to_infer_with_generics_and_self,
    type_expr_to_infer_with_self, type_to_infer, AssocResolveCtx,
};
use super::FunGeneralization;

fn type_expr_to_infer_with_ctx(
    te: &TypeExpr,
    generics: &HashMap<String, TypeVar>,
    ctx: &InferContext,
) -> InferType {
    let assoc_ctx = AssocResolveCtx {
        registry: ctx.registry(),
        current_module: ctx.current_module_path(),
        current_aspect: None,
    };
    type_expr_to_infer_with_assoc_ctx(te, generics, None, &assoc_ctx)
}

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
    if !params.is_empty() {
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
                        for aspect in bounds.iter().filter_map(GenericBound::aspect_name) {
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
    }
    type_expr_to_infer_with_ctx(te, params, ctx)
}

/// Register the names of all direct `FunDecl`s in `decls` with fresh type
/// variables so that forward references and mutual recursion work.
/// The function type of a `native` declaration, built from its annotations,
/// plus the aspect bounds of its generic params keyed by their `TypeVars` (so
/// the caller can attach them to the generalized scheme).
struct NativeFunTyResult {
    fun_ty: InferType,
    bounds: HashMap<TypeVar, Vec<GenericBound>>,
    neg_bounds: HashMap<TypeVar, Vec<GenericBound>>,
    record_kinds: HashMap<TypeVar, bool>,
    assoc_eq: HashMap<TypeVar, Vec<(String, String, InferType)>>,
}

fn native_fun_ty(fun: &FunDecl, ctx: &mut InferContext) -> Result<NativeFunTyResult, MetelError> {
    // Generic native functions (e.g. `print<T: Display>`) map each type
    // parameter to a fresh TypeVar; the caller generalizes the result into a
    // polymorphic scheme carrying the bounds.
    let generic_map = fun_generic_map(fun, ctx);
    let bounds_by_var = collect_fun_type_var_bounds(fun, &generic_map);
    let neg_bounds_by_var = collect_negative_fun_type_var_bounds(fun, &generic_map);
    let record_kinds_by_var = collect_fun_type_var_record_kinds(fun, &generic_map);
    let assoc_eq_by_var = collect_fun_assoc_eq_constraints(fun, &generic_map);
    let te_to_infer = |te: &TypeExpr| -> InferType { type_expr_to_infer_with_ctx(te, &generic_map, ctx) };
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
        record_kinds: record_kinds_by_var,
        assoc_eq: assoc_eq_by_var,
    })
}

/// A binding of one of an impl's own generic parameters: the type variable
/// that stands for it structurally, and the name it was written with.
///
/// Both are needed and they serve different jobs. The variable is what the
/// aspect query reasons about — a parameter is not a named type, so it must
/// not be *representable* as one. The name is only ever used to render a
/// diagnostic, which is a presentation concern and deliberately kept out of
/// the representation.
struct ImplParam {
    var: TypeVar,
    name: String,
}

/// Fresh variables for an impl's own generic parameters, paired with the
/// aspects each may be assumed to satisfy.
///
/// Every parameter gets an entry, including unbounded ones: an empty
/// assumption set says "abstract, and guarantees nothing", which is what makes
/// an unbounded `<T>` fail a `Copy` check rather than pass it.
fn impl_params(ib: &ImplBlock, ctx: &mut InferContext) -> (Vec<ImplParam>, AspectAssumptions) {
    let mut params = Vec::new();
    let mut assumptions = AspectAssumptions::new();
    for param in &ib.generics {
        let InferType::Var(var) = ctx.fresh_var() else {
            continue;
        };
        params.push(ImplParam {
            var,
            name: param.name.clone(),
        });
        let entry = assumptions.entry(var).or_default();
        for bound in &param.bounds {
            if bound.polarity == Polarity::Positive {
                if let Some(aspect) = bound.aspect_name() {
                    entry.insert(aspect.to_string());
                }
            }
        }
    }
    if let Some(where_clause) = &ib.where_clause {
        for constraint in &where_clause.constraints {
            // A `where` subject that is not one of the impl's own parameters
            // constrains something else entirely; it says nothing about what
            // this impl may assume.
            let Some(param) = params.iter().find(|p| p.name == constraint.name) else {
                continue;
            };
            let entry = assumptions.entry(param.var).or_default();
            for bound in &constraint.bounds {
                if bound.polarity == Polarity::Positive {
                    if let Some(aspect) = bound.aspect_name() {
                        entry.insert(aspect.to_string());
                    }
                }
            }
        }
    }
    (params, assumptions)
}


fn infer_type_to_concrete_if_closed(ty: &InferType) -> Option<Type> {
    match ty {
        InferType::Concrete(concrete) => Some(concrete.clone()),
        InferType::Tuple(items) => items
            .iter()
            .map(infer_type_to_concrete_if_closed)
            .collect::<Option<Vec<_>>>()
            .map(Type::Tuple),
        InferType::Array(item) => infer_type_to_concrete_if_closed(item)
            .map(|item| Type::Array(Box::new(item))),
        InferType::SizedArray(item, size) => infer_type_to_concrete_if_closed(item)
            .map(|item| Type::SizedArray(Box::new(item), *size)),
        InferType::Reference(item) => infer_type_to_concrete_if_closed(item)
            .map(|item| Type::Reference(Box::new(item))),
        InferType::MutReference(item) => infer_type_to_concrete_if_closed(item)
            .map(|item| Type::MutReference(Box::new(item))),
        InferType::Named(name, args) => args
            .iter()
            .map(infer_type_to_concrete_if_closed)
            .collect::<Option<Vec<_>>>()
            .map(|args| Type::Named(name.clone(), args)),
        InferType::Never | InferType::Var(_) | InferType::Fun(_, _) | InferType::Record(_) => None,
    }
}

/// Whether `ty` mentions any of `params` anywhere inside it.
fn mentions_type_param(ty: &TypeExpr, params: &std::collections::HashSet<&str>) -> bool {
    let go = |t: &TypeExpr| mentions_type_param(t, params);
    match ty {
        TypeExpr::Named(name, args) => params.contains(name.as_str()) || args.iter().any(go),
        TypeExpr::Tuple(items) => items.iter().any(go),
        TypeExpr::Record(fields) => fields.iter().any(|(_, t)| go(t)),
        TypeExpr::Array(inner)
        | TypeExpr::SizedArray(inner, _)
        | TypeExpr::Reference(inner)
        | TypeExpr::MutReference(inner) => go(inner),
        TypeExpr::Fun(ps, ret) => ps.iter().any(go) || ret.as_deref().is_some_and(go),
        TypeExpr::Projection { base, .. } => go(base),
        TypeExpr::Unit | TypeExpr::ImplAspect { .. } | TypeExpr::RecordProjection { .. } => false,
    }
}

/// An impl's target type as a concrete `Type`, but only when the target is
/// *closed*: a nominal type mentioning none of the impl's own generic
/// parameters.
///
/// Closedness has to be tested against `ib.generics` explicitly rather than
/// left to `infer_type_to_concrete_if_closed`, which cannot see it. Given
/// `extend<T: !Copy> Foo<T>: Drop`, that function reduces `Foo<T>` to a
/// perfectly concrete `Foo` applied to a nominal type *literally named* `T` —
/// closed as far as it can tell. The RFC-0071 §4 checks below would then ask
/// whether that type implements the other aspect, and `type_satisfies_aspect`
/// would match any conditional impl of it while ignoring the bounds that
/// cannot be evaluated against a placeholder — rejecting valid pairs like
/// `extend<T: Copy> Foo<T>: Copy` alongside `extend<T: !Copy> Foo<T>: Drop`,
/// where no instantiation can satisfy both.
///
/// Open targets are not unchecked: `coherence`'s cross-aspect overlap check
/// handles them precisely, comparing the two impls' bounds instead of
/// discarding them (issue #302).
fn closed_nominal_target(ib: &ImplBlock, target_name: &str) -> Option<Type> {
    if !matches!(&ib.target_type, TypeExpr::Named(_, _)) {
        return None;
    }
    let params: std::collections::HashSet<&str> =
        ib.generics.iter().map(|g| g.name.as_str()).collect();
    if mentions_type_param(&ib.target_type, &params) {
        return None;
    }
    infer_type_to_concrete_if_closed(&type_expr_to_infer_with_self(&ib.target_type, target_name))
}

/// `ty` with the struct's or enum's own type variables replaced by the
/// corresponding arguments of the impl's target type.
///
/// An argument that *is* one of the impl's own generic parameters becomes that
/// parameter's type variable — structurally a variable, never a named type.
/// That is the whole distinction: a field type written in the struct's scope
/// keeps whatever that scope meant by it, while a position filled from the
/// impl's target takes the impl's meaning, so a parameter correctly shadows a
/// same-named type in exactly the positions where it should and nowhere else.
fn substitute_impl_params(
    ty: &InferType,
    type_param_args: &HashMap<TypeVar, &TypeExpr>,
    params: &[ImplParam],
) -> InferType {
    let go = |t: &InferType| substitute_impl_params(t, type_param_args, params);
    match ty {
        InferType::Var(var) => type_param_args
            .get(var)
            .map_or_else(|| ty.clone(), |arg| type_expr_as_infer(arg, params)),
        InferType::Tuple(items) => InferType::Tuple(items.iter().map(go).collect()),
        InferType::Record(fields) => InferType::Record(
            fields
                .iter()
                .map(|(label, field_ty)| (label.clone(), go(field_ty)))
                .collect(),
        ),
        InferType::Array(item) => InferType::Array(Box::new(go(item))),
        InferType::SizedArray(item, size) => InferType::SizedArray(Box::new(go(item)), *size),
        InferType::Reference(item) => InferType::Reference(Box::new(go(item))),
        InferType::MutReference(item) => InferType::MutReference(Box::new(go(item))),
        InferType::Named(name, args) => {
            InferType::Named(name.clone(), args.iter().map(go).collect())
        }
        InferType::Fun(ps, ret) => {
            InferType::Fun(ps.iter().map(go).collect(), Box::new(go(ret)))
        }
        InferType::Concrete(_) | InferType::Never => ty.clone(),
    }
}

/// An impl target argument as an `InferType`, with any mention of the impl's
/// own generic parameters resolved to their type variables.
fn type_expr_as_infer(ty: &TypeExpr, params: &[ImplParam]) -> InferType {
    if let TypeExpr::Named(name, args) = ty {
        if args.is_empty() {
            if let Some(param) = params.iter().find(|p| &p.name == name) {
                return InferType::Var(param.var);
            }
        }
    }
    let go = |t: &TypeExpr| type_expr_as_infer(t, params);
    match ty {
        TypeExpr::Named(name, args) => {
            InferType::Named(name.clone(), args.iter().map(go).collect())
        }
        TypeExpr::Tuple(items) => InferType::Tuple(items.iter().map(go).collect()),
        TypeExpr::Record(fields) => InferType::Record(
            fields
                .iter()
                .map(|(label, field_ty)| (label.clone(), go(field_ty)))
                .collect(),
        ),
        TypeExpr::Array(inner) => InferType::Array(Box::new(go(inner))),
        TypeExpr::SizedArray(inner, size) => InferType::SizedArray(Box::new(go(inner)), *size),
        TypeExpr::Reference(inner) => InferType::Reference(Box::new(go(inner))),
        TypeExpr::MutReference(inner) => InferType::MutReference(Box::new(go(inner))),
        // Anything without a parameter to resolve keeps the ordinary lowering.
        _ => type_expr_to_infer(ty),
    }
}

/// A substituted type rendered the way it was written, with each parameter's
/// variable shown under its own name.
///
/// The names live here rather than in the type because they are a
/// presentation concern: printing `Inner<?t16>` was the diagnostic half of
/// issue #303, and the fix is to render variables properly, not to make the
/// representation carry a name it should not have.
fn display_type(ty: &InferType, params: &[ImplParam]) -> String {
    match ty {
        InferType::Var(var) => params
            .iter()
            .find(|p| p.var == *var)
            .map_or_else(|| ty.to_string(), |p| p.name.clone()),
        InferType::Named(name, args) if !args.is_empty() => {
            let rendered: Vec<String> = args.iter().map(|a| display_type(a, params)).collect();
            format!("{name}<{}>", rendered.join(", "))
        }
        InferType::Tuple(items) => {
            let rendered: Vec<String> = items.iter().map(|i| display_type(i, params)).collect();
            format!("({})", rendered.join(", "))
        }
        InferType::Array(item) => format!("{}[]", display_type(item, params)),
        InferType::SizedArray(item, size) => format!("[{}; {size}]", display_type(item, params)),
        InferType::Reference(item) => format!("&{}", display_type(item, params)),
        InferType::MutReference(item) => format!("&var {}", display_type(item, params)),
        _ => ty.to_string(),
    }
}

/// Whether a field or payload type, already substituted by
/// `substitute_impl_params`, is `Copy` under the impl's own bounds.
///
/// Defers entirely to the registry's query rather than walking the type. It
/// used to walk it, in two mutually recursive functions that re-stated the
/// `Copy` rules for tuples, fixed arrays and references — a third copy of
/// rules that already live in the query, and the copy that had drifted: it
/// answered `false` for any named type with an unresolved argument, which is
/// issue #303's wrong rejection.
fn substituted_type_is_copy(
    ty: &InferType,
    assumptions: &AspectAssumptions,
    registry: &crate::typeinference::TypeDefinitionRegistry,
    current_module: &[String],
) -> bool {
    registry.infer_type_satisfies_aspect(current_module, ty, "Copy", assumptions)
}

fn check_copy_impl_eligibility(
    ib: &ImplBlock,
    target_name: &str,
    ctx: &mut InferContext,
) -> Result<(), MetelError> {
    let (params, assumptions) = impl_params(ib, ctx);
    let mut type_param_args: HashMap<TypeVar, &TypeExpr> = HashMap::new();
    if let TypeExpr::Named(_, target_args) = &ib.target_type {
        if let Some(struct_params) = ctx.registry().raw_struct_type_params().get(target_name) {
            for (param, arg) in struct_params.iter().zip(target_args.iter()) {
                type_param_args.insert(*param, arg);
            }
        } else if let Some(enum_info) = ctx.registry().enum_info(target_name) {
            for (param, arg) in enum_info.type_params.iter().zip(target_args.iter()) {
                type_param_args.insert(*param, arg);
            }
        }
    }

    if let Some(fields) = ctx.get_struct_fields(target_name) {
        for field in fields {
            let field_ty = substitute_impl_params(&field.ty, &type_param_args, &params);
            if !substituted_type_is_copy(
                &field_ty,
                &assumptions,
                ctx.registry(),
                ctx.current_module_path(),
            ) {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0001,
                    format!(
                        "cannot implement `Copy` for `{target_name}`: field `{}` has type `{}` which is not `Copy`",
                        field.name,
                        display_type(&field_ty, &params)
                    ),
                    &field.span,
                ));
            }
        }
    } else if let Some(enum_info) = ctx.get_enum(target_name) {
        for variant in &enum_info.variants {
            for field in &variant.fields {
                let field_ty = substitute_impl_params(&field.ty, &type_param_args, &params);
                if !substituted_type_is_copy(
                    &field_ty,
                    &assumptions,
                    ctx.registry(),
                    ctx.current_module_path(),
                ) {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0001,
                        format!(
                            "cannot implement `Copy` for `{target_name}`: payload `{}` of variant `{}::{}` has type `{}` which is not `Copy`",
                            field.name,
                            target_name,
                            variant.name,
                            display_type(&field_ty, &params)
                        ),
                        &field.span,
                    ));
                }
            }
        }
    }

    if let Some(concrete_target) = closed_nominal_target(ib, target_name) {
        if ctx
            .registry()
            .type_satisfies_aspect(ctx.current_module_path(), &concrete_target, "Drop")
        {
            return Err(MetelError::type_error(
                TypeErrorCode::T0001,
                format!("`{target_name}` cannot implement both `Copy` and `Drop`"),
                &ib.span,
            ));
        }
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
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
                    ctx.bind_poly(
                        &fun.name,
                        generalize(result.fun_ty, &env_fvs)
                            .with_bounds(&result.bounds)
                            .with_neg_bounds(&result.neg_bounds)
                            .with_assoc_eq_constraints(&result.assoc_eq),
                    );
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
                                    for aspect in bounds.iter().filter_map(GenericBound::aspect_name)
                                    {
                                        if let Some(decls) =
                                            ctx.registry().aspect_assoc_type_decls(aspect)
                                        {
                                            if decls.iter().any(|d| d.name == *assoc_name) {
                                                return InferType::Var(
                                                    ctx.fresh_assoc_projection_var(
                                                        base_tv,
                                                        aspect,
                                                        assoc_name,
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
                    if type_expr_contains_impl_aspect(ann) {
                        // RFC-0037: use fresh marker vars for `impl Aspect`
                        // return positions in the provisional (hoist-time)
                        // scheme too, so forward references don't leak the
                        // aspect name as a concrete nominal type into the
                        // pre-registration constraint.
                        let mut rw_counter = 0usize;
                        let mut replacements: Vec<(String, String)> = Vec::new();
                        let rewritten =
                            rewrite_impl_aspect_returns(ann, &mut rw_counter, &mut replacements);
                        let mut extended_map = generic_map.clone();
                        for (placeholder, _) in &replacements {
                            let tv = ctx.fresh_type_var_raw();
                            extended_map.insert(placeholder.clone(), tv);
                        }
                        type_expr_to_infer_with_generics(&rewritten, &extended_map)
                    } else {
                        te_to_infer(ann, ctx)
                    }
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
) -> HashMap<TypeVar, Vec<GenericBound>> {
    let mut map: HashMap<TypeVar, Vec<GenericBound>> = HashMap::new();
    for gp in &fun.generics {
        if let Some(&tv) = generic_map.get(&gp.name) {
            let names: Vec<GenericBound> = gp
                .bounds
                .iter()
                .filter_map(|b| {
                    // Negative bounds (`T: !Drop`) are dropped from this positive
                    // aspect-name list for now — their satisfaction checking is
                    // issue #243's job, not this one's.
                    if b.polarity != crate::ast::Polarity::Positive {
                        return None;
                    }
                    GenericBound::from_ast(b)
                })
                .collect();
            if !names.is_empty() {
                map.entry(tv).or_default().extend(names);
            }
        }
    }
    if let Some(wc) = &fun.where_clause {
        for constraint in &wc.constraints {
            if let Some(&tv) = generic_map.get(constraint.name.as_str()) {
                let names: Vec<GenericBound> = constraint
                    .bounds
                    .iter()
                    .filter(|b| b.polarity == Polarity::Positive)
                    .filter_map(GenericBound::from_ast)
                    .collect();
                for name in names {
                    let entry = map.entry(tv).or_default();
                    if !entry.iter().any(|existing| matches!((existing, &name), (GenericBound::Aspect(a), GenericBound::Aspect(b)) if a == b)) {
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
) -> HashMap<TypeVar, Vec<GenericBound>> {
    let mut map: HashMap<TypeVar, Vec<GenericBound>> = HashMap::new();
    for gp in &fun.generics {
        if let Some(&tv) = generic_map.get(&gp.name) {
            let names: Vec<GenericBound> = gp
                .bounds
                .iter()
                .filter_map(|b| {
                    if b.polarity != crate::ast::Polarity::Negative {
                        return None;
                    }
                    GenericBound::from_ast(b)
                })
                .collect();
            if !names.is_empty() {
                map.entry(tv).or_default().extend(names);
            }
        }
    }
    if let Some(wc) = &fun.where_clause {
        for constraint in &wc.constraints {
            if let Some(&tv) = generic_map.get(constraint.name.as_str()) {
                let names: Vec<GenericBound> = constraint
                    .bounds
                    .iter()
                    .filter(|b| b.polarity == Polarity::Negative)
                    .filter_map(GenericBound::from_ast)
                    .collect();
                for name in names {
                    let entry = map.entry(tv).or_default();
                    if !entry.iter().any(|existing| matches!((existing, &name), (GenericBound::Aspect(a), GenericBound::Aspect(b)) if a == b)) {
                        entry.push(name);
                    }
                }
            }
        }
    }
    map
}

pub(super) fn collect_fun_type_var_record_kinds(
    fun: &FunDecl,
    generic_map: &HashMap<String, TypeVar>,
) -> HashMap<TypeVar, bool> {
    let mut map: HashMap<TypeVar, bool> = HashMap::new();
    for gp in &fun.generics {
        if gp.is_record {
            if let Some(&tv) = generic_map.get(&gp.name) {
                map.insert(tv, true);
            }
        }
    }
    if let Some(wc) = &fun.where_clause {
        for constraint in &wc.constraints {
            if constraint.is_record {
                if let Some(&tv) = generic_map.get(constraint.name.as_str()) {
                    map.insert(tv, true);
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
            let Some(aspect_name) = b.aspect_name() else {
                continue;
            };
            for (assoc_name, assoc_ty) in &b.assoc_bindings {
                let expected = type_expr_to_infer_with_generics(assoc_ty, generic_map);
                map.entry(tv).or_default().push((
                    aspect_name.to_string(),
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
        for constraint in &wc.constraints {
            if let Some(&tv) = generic_map.get(constraint.name.as_str()) {
                collect_from_bounds(tv, &constraint.bounds);
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
                        record_kinds: HashMap::new(),
                        assoc_projections: HashMap::new(),
                        assoc_eq: HashMap::new(),
                        opaque_returns: HashMap::new(),
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
            if matches!(
                &ib.target_type,
                TypeExpr::Record(_) | TypeExpr::RecordProjection { .. }
            ) {
                if ib.aspect_name.is_none() {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0001,
                        "anonymous records cannot have inherent methods; declare a `struct` if you need methods",
                        &ib.span,
                    ));
                }
                if let Some(aspect_name) = &ib.aspect_name {
                    if aspect_name == "Drop" {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0001,
                            "anonymous records cannot implement `Drop`; teardown logic requires a nominal type",
                            &ib.span,
                        ));
                    }
                    if ctx
                        .registry()
                        .aspect_declaring_module(aspect_name)
                        .is_some_and(|module| module.as_slice() != ctx.current_module_path())
                    {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0014,
                            format!(
                                "`{aspect_name}` is not local to this module and cannot be implemented for an anonymous record; declare a `struct` and implement it there"
                            ),
                            &ib.span,
                        ));
                    }
                }
            }
            // Structural blanket impl targets (`T[]`) have no nominal head. As in
            // construction, keep a nominal target name only when one exists; generic
            // structural impl bodies are inferred against their own type-parameter map.
            crate::typechecker::reject_unregisterable_impl_target(ib)?;
            // Same classification as construction, but keyed on the *last*
            // segment — inference's registries are, and unifying the spelling
            // would change what they look up.
            let target_name = crate::typechecker::impl_target_head(&ib.target_type)
                .map(|name| name.rsplit("::").next().unwrap_or(name).to_string())
                .unwrap_or_default();
            if ib.polarity == Polarity::Positive {
                if let Some(aspect_name) = &ib.aspect_name {
                    if aspect_name == "Copy" {
                        check_copy_impl_eligibility(ib, &target_name, ctx)?;
                    } else if aspect_name == "Drop" {
                        if let Some(concrete_target) = closed_nominal_target(ib, &target_name) {
                            if ctx.registry().type_satisfies_aspect(
                                ctx.current_module_path(),
                                &concrete_target,
                                "Copy",
                            ) {
                                return Err(MetelError::type_error(
                                    TypeErrorCode::T0001,
                                    format!(
                                        "`{target_name}` cannot implement both `Copy` and `Drop`"
                                    ),
                                    &ib.span,
                                ));
                            }
                        }
                    }
                }
            }
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
                    if let Some(assoc_decls) =
                        ctx.registry().aspect_assoc_type_decls(aspect_name).cloned()
                    {
                        let provided_assoc: std::collections::HashMap<&str, &TypeExpr> = ib
                            .assoc_type_defs
                            .iter()
                            .map(|d| (d.name.as_str(), &d.ty))
                            .collect();
                        for decl in &assoc_decls {
                            if let Some(concrete_ty_expr) = provided_assoc.get(decl.name.as_str()) {
                                // §1.1: if the declaration has a bound, check the
                                // concrete binding satisfies it.
                                for bound in &decl.bounds {
                                    if let Some(bound_aspect) = bound.aspect_name() {
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
                                                // Check if this name matches one of the impl's own generic parameters
                                                if let Some(gp) =
                                                    ib.generics.iter().find(|p| p.name == name)
                                                {
                                                    // This is the impl's own generic parameter - check its declared bounds
                                                    let param_bounds = gp
                                                        .bounds
                                                        .iter()
                                                        .filter(|b| {
                                                            b.polarity == Polarity::Positive
                                                        })
                                                        .filter_map(|b| {
                                                            b.aspect_name().map(ToOwned::to_owned)
                                                        })
                                                        .collect::<Vec<_>>();

                                                    if param_bounds.contains(&bound_aspect.to_string())
                                                    {
                                                        // The bound is satisfied by the impl's own parameter bounds
                                                        continue;
                                                    }
                                                    return Err(MetelError::type_error(
                                                        TypeErrorCode::T0012,
                                                        format!(
                                                            "associated type `{}` bound `{}` is not satisfied by `{}`",
                                                            decl.name, bound_aspect, name
                                                        ),
                                                        &ib.span,
                                                    ));
                                                }
                                                // Original behavior for concrete types
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

/// Check whether a `TypeExpr` tree contains any `ImplAspect` nodes (RFC-0037).
/// Used to decide whether the return-type conversion needs opaque-return handling.
fn type_expr_contains_impl_aspect(te: &TypeExpr) -> bool {
    match te {
        TypeExpr::ImplAspect { .. } => true,
        TypeExpr::Named(_, args) => args.iter().any(type_expr_contains_impl_aspect),
        TypeExpr::Tuple(elems) => elems.iter().any(type_expr_contains_impl_aspect),
        TypeExpr::Record(fields) => fields
            .iter()
            .any(|(_, ty)| type_expr_contains_impl_aspect(ty)),
        TypeExpr::Array(elem)
        | TypeExpr::SizedArray(elem, _)
        | TypeExpr::Reference(elem)
        | TypeExpr::MutReference(elem) => type_expr_contains_impl_aspect(elem),
        TypeExpr::Fun(params, ret) => {
            params.iter().any(type_expr_contains_impl_aspect)
                || ret
                    .as_ref()
                    .is_some_and(|r| type_expr_contains_impl_aspect(r))
        }
        TypeExpr::Unit | TypeExpr::Projection { .. } | TypeExpr::RecordProjection { .. } => false,
    }
}

/// Recursively rewrite a `TypeExpr`, replacing each `ImplAspect { bound, .. }`
/// with `Named(placeholder_name, [])`. Returns the rewritten tree plus a list
/// of `(placeholder_name, aspect_name)` pairs, one per replaced node (RFC-0037).
fn rewrite_impl_aspect_returns(
    te: &TypeExpr,
    counter: &mut usize,
    replacements: &mut Vec<(String, String)>,
) -> TypeExpr {
    match te {
        TypeExpr::ImplAspect { bound, .. } => {
            let aspect_name = match bound.as_ref() {
                TypeExpr::Named(name, _) => name.clone(),
                _ => String::new(),
            };
            let placeholder = format!("_OpaqueRet{counter}");
            *counter += 1;
            replacements.push((placeholder.clone(), aspect_name));
            TypeExpr::Named(placeholder, vec![])
        }
        TypeExpr::Named(name, args) => TypeExpr::Named(
            name.clone(),
            args.iter()
                .map(|a| rewrite_impl_aspect_returns(a, counter, replacements))
                .collect(),
        ),
        TypeExpr::Tuple(elems) => TypeExpr::Tuple(
            elems
                .iter()
                .map(|e| rewrite_impl_aspect_returns(e, counter, replacements))
                .collect(),
        ),
        TypeExpr::Record(fields) => TypeExpr::Record(
            fields
                .iter()
                .map(|(name, ty)| {
                    (
                        name.clone(),
                        rewrite_impl_aspect_returns(ty, counter, replacements),
                    )
                })
                .collect(),
        ),
        TypeExpr::Array(elem) => TypeExpr::Array(Box::new(rewrite_impl_aspect_returns(
            elem,
            counter,
            replacements,
        ))),
        TypeExpr::SizedArray(elem, n) => TypeExpr::SizedArray(
            Box::new(rewrite_impl_aspect_returns(elem, counter, replacements)),
            *n,
        ),
        TypeExpr::Reference(elem) => TypeExpr::Reference(Box::new(rewrite_impl_aspect_returns(
            elem,
            counter,
            replacements,
        ))),
        TypeExpr::MutReference(elem) => TypeExpr::MutReference(Box::new(
            rewrite_impl_aspect_returns(elem, counter, replacements),
        )),
        TypeExpr::Fun(params, ret) => TypeExpr::Fun(
            params
                .iter()
                .map(|p| rewrite_impl_aspect_returns(p, counter, replacements))
                .collect(),
            ret.as_ref()
                .map(|r| Box::new(rewrite_impl_aspect_returns(r, counter, replacements))),
        ),
        TypeExpr::Unit | TypeExpr::Projection { .. } | TypeExpr::RecordProjection { .. } => {
            te.clone()
        }
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
        let NativeFunTyResult {
            fun_ty,
            bounds,
            neg_bounds,
            record_kinds,
            assoc_eq,
        } = native_fun_ty(fun, ctx)?;
        // Overloaded native definitions (std::core's assert pair) are
        // dispatched by SymbolId and never enter the name-keyed scheme env.
        if ctx.is_overloaded(&fun.name) {
            return Ok(());
        }
        let env_fvs = ctx.env_free_vars();
        ctx.bind_poly(
            &fun.name,
            generalize(fun_ty.clone(), &env_fvs)
                .with_bounds(&bounds)
                .with_neg_bounds(&neg_bounds)
                .with_record_kinds(&record_kinds)
                .with_assoc_eq_constraints(&assoc_eq),
        );
        fun_generalizations.push(FunGeneralization {
            name: fun.name.clone(),
            fun_ty,
            env_fvs,
            name_map: HashMap::new(),
            bounds,
            neg_bounds,
            record_kinds,
            assoc_projections: HashMap::new(),
            assoc_eq,
            opaque_returns: HashMap::new(),
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
    let type_var_record_kinds = collect_fun_type_var_record_kinds(fun, &generic_map);
    if !type_var_record_kinds.is_empty() {
        ctx.register_fun_record_kinds(fun.name.clone(), type_var_record_kinds.clone());
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
                        for aspect in bounds.iter().filter_map(GenericBound::aspect_name) {
                            if let Some(decls) = ctx.registry().aspect_assoc_type_decls(aspect) {
                                if decls.iter().any(|d| d.name == *assoc_name) {
                                    matching_aspects.push(aspect.to_string());
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
                            &aspect,
                            assoc_name,
                        )));
                    }
                    // Fallback: named placeholder
                    return Ok(InferType::Named(format!("{n}::{assoc_name}"), vec![]));
                }
            }
        }
        Ok(type_expr_to_infer_with_ctx(te, &generic_map, ctx))
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

    // RFC-0037: return-position `impl Aspect`. When the return-type annotation
    // contains `ImplAspect` nodes (top-level or nested in Tuple/Array/etc.),
    // rewrite them into fresh anonymous type-param names, create marker TypeVars
    // for each, and record them for post-solve opaque-return processing.
    let mut pending_opaque_returns: Vec<(TypeVar, String)> = Vec::new();
    let ret_ty = if let Some(ann) = &fun.return_type {
        if type_expr_contains_impl_aspect(ann) {
            let mut rw_counter = 0usize;
            let mut replacements: Vec<(String, String)> = Vec::new();
            let rewritten = rewrite_impl_aspect_returns(ann, &mut rw_counter, &mut replacements);
            let mut extended_map = generic_map.clone();
            for (placeholder, aspect_name) in &replacements {
                let tv = ctx.fresh_type_var_raw();
                extended_map.insert(placeholder.clone(), tv);
                pending_opaque_returns.push((tv, aspect_name.clone()));
            }
            type_expr_to_infer_with_generics(&rewritten, &extended_map)
        } else {
            te_to_infer(ann, ctx)?
        }
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

    // RFC-0037: process pending opaque-return markers. For each marker, check
    // whether the body's own solve resolved it to a concrete type (unlinked case)
    // or left it as a free Var (linked case, e.g. `fun transform(x: impl A) ->
    // impl A { x }` where the return is tied to a generic param).
    //
    // Unlinked markers are "re-abstracted": the concrete type is recorded in
    // `opaque_map` (keyed by a fresh placeholder TypeVar), and a re-abstraction
    // substitution prevents `partial_subst` from collapsing the marker in
    // `resolved_ty`. This makes the function appear polymorphic (the placeholder
    // is quantified by `generalize`) while the recorded concrete type lets
    // construction (Pass 2) backfill the concrete type at each call site.
    let mut opaque_map: HashMap<TypeVar, (String, Type)> = HashMap::new();
    let mut reabstraction = Substitution::new();
    for (marker_tv, aspect_name) in &pending_opaque_returns {
        let resolved_marker = partial_subst.apply(&InferType::Var(*marker_tv));
        #[allow(clippy::match_same_arms)]
        // Var and the wildcard document distinct, deliberate no-ops
        match &resolved_marker {
            InferType::Var(_) => {
                // Linked case: marker is still a free var (tied to a generic
                // param). Ordinary generalize/bind_poly handles it — the caller
                // can name the concrete type here, which is correct (the callee
                // returns the same value the caller handed in).
            }
            InferType::Concrete(_) | InferType::Named(_, _) => {
                // Unlinked case: marker collapsed to a concrete type during the
                // body's own solve. Convert to a `Type` for recording.
                let Ok(concrete_ty) = infer_type_to_type(&resolved_marker, &fun.span) else {
                    // The concrete type still has free vars (mixed case:
                    // real generics + opaque return). Not in the RFC's
                    // examples — skip opaque handling, let it fall through
                    // to ordinary generic behavior.
                    continue;
                };
                // Verify the aspect bound at definition time (RFC-0037 §1.1):
                // the concrete type must implement the declared aspect.
                if !ctx.registry().type_satisfies_aspect(
                    ctx.current_module_path(),
                    &concrete_ty,
                    aspect_name,
                ) {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0012,
                        format!(
                            "return type `{concrete_ty}` does not implement `{aspect_name}` \
                             (required by `{}` return type `impl {aspect_name}`)",
                            fun.name
                        ),
                        &fun.span,
                    ));
                }
                let placeholder_tv = ctx.fresh_type_var_raw();
                reabstraction.bind(*marker_tv, InferType::Var(placeholder_tv));
                opaque_map.insert(placeholder_tv, (aspect_name.clone(), concrete_ty));
            }
            _ => {}
        }
    }

    let resolved_ty = if opaque_map.is_empty() {
        partial_subst.apply(&fun_ty)
    } else {
        // Compose re-abstraction (self, wins on overlap) with partial_subst
        // (other), then apply the combined substitution to fun_ty. The marker
        // vars are rebound to fresh placeholders that partial_subst cannot
        // resolve, so they survive as free vars for `generalize` to quantify.
        reabstraction.compose(&partial_subst).apply(&fun_ty)
    };

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
    // RFC-0037: attach opaque-return metadata so the locally-bound scheme
    // (used by Pass 1 call-site instantiation) carries the same identity
    // info as the cross-module-exported scheme.
    let scheme = if opaque_map.is_empty() {
        scheme
    } else {
        scheme.with_opaque_returns(&opaque_map)
    };
    let scheme = scheme.with_record_kinds(&type_var_record_kinds);
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
    let bounds: HashMap<TypeVar, Vec<GenericBound>> = type_var_bounds
        .iter()
        .filter_map(
            |(orig_tv, b)| match partial_subst.apply(&InferType::Var(*orig_tv)) {
                InferType::Var(final_tv) => Some((final_tv, b.clone())),
                _ => None,
            },
        )
        .collect();
    let neg_bounds: HashMap<TypeVar, Vec<GenericBound>> = neg_type_var_bounds
        .iter()
        .filter_map(
            |(orig_tv, b)| match partial_subst.apply(&InferType::Var(*orig_tv)) {
                InferType::Var(final_tv) => Some((final_tv, b.clone())),
                _ => None,
            },
        )
        .collect();
    let record_kinds: HashMap<TypeVar, bool> = type_var_record_kinds
        .iter()
        .filter_map(
            |(orig_tv, is_record)| match partial_subst.apply(&InferType::Var(*orig_tv)) {
                InferType::Var(final_tv) => Some((final_tv, *is_record)),
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
        record_kinds,
        assoc_projections: proj_map,
        assoc_eq,
        opaque_returns: opaque_map,
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
    let array_target_generic_name = match &ib.target_type {
        TypeExpr::Array(inner) => match inner.as_ref() {
            TypeExpr::Named(name, args) if args.is_empty() => ib
                .generics
                .iter()
                .find(|gp| gp.name == *name)
                .map(|_| name.as_str()),
            _ => None,
        },
        _ => None,
    };
    // Start with the method's own generic params.
    let mut generic_map: HashMap<String, TypeVar> = method
        .generics
        .iter()
        .map(|g| (g.name.clone(), ctx.fresh_type_var_raw()))
        .collect();

    // Seed with the target struct/enum's generic params so that type annotations
    // referencing e.g. `T` in `impl SortedList<T>` resolve to TypeVars and
    // aspect methods on bounded params are available in the body.
    let mut struct_bounds: HashMap<TypeVar, Vec<GenericBound>> = HashMap::new();
    // Ordered TypeVars for the struct's generic params (same order as struct type args).
    let mut struct_tvars_ordered: Vec<TypeVar> = Vec::new();
    if let Some(names) = ctx.struct_generic_names_for(target_name).cloned() {
        let bounds_by_pos: Option<Vec<Vec<GenericBound>>> =
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
    } else if let Some(name) = array_target_generic_name {
        if !generic_map.contains_key(name) {
            let tv = ctx.fresh_type_var_raw();
            generic_map.insert(name.to_string(), tv);
            struct_tvars_ordered.push(tv);
        }
    }

    // RFC-0036 §2.2: compute impl-level bounds (from the impl block's own
    // where clause / inline bounds) and merge them into `struct_bounds` so that
    // method dispatch and type annotations inside the body can see impl-level
    // constraints (e.g. `impl<T: Display> Greet for Box1<T>` needs `T: Display`
    // visible when resolving `self.value.to_string()`).
    let generic_names_for_impl: Vec<String> = if let Some(name) = array_target_generic_name {
        vec![name.to_string()]
    } else {
        ctx.struct_generic_names_for(target_name)
            .cloned()
            .unwrap_or_default()
    };
    let structural_self_type_expr = array_target_generic_name.map(|name| {
        TypeExpr::Array(Box::new(TypeExpr::Named(name.to_string(), vec![])))
    });
    let synth = super::registry::synth_generics_for_impl(&generic_names_for_impl, &ib.generics);
    let impl_bounds: Vec<Vec<GenericBound>> =
        super::registry::collect_type_param_bounds(&synth, ib.where_clause.as_ref());
    let impl_neg_bounds: Vec<Vec<GenericBound>> =
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
                        for aspect in bounds.iter().filter_map(GenericBound::aspect_name) {
                            if let Some(decls) = ctx.registry().aspect_assoc_type_decls(aspect) {
                                if decls.iter().any(|d| d.name == *assoc_name) {
                                    matching_aspects.push(aspect.to_string());
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
                            &aspect,
                            assoc_name,
                        )));
                    }
                    return Ok(InferType::Named(format!("{n}::{assoc_name}"), vec![]));
                }
            }
        }
        Ok(if let Some(self_replacement) = &structural_self_type_expr {
            let lowered = substitute_structural_self(te, self_replacement);
            type_expr_to_infer_with_generics(&lowered, &generic_map)
        } else if generic_map.is_empty() {
            type_expr_to_infer_with_self(te, target_name)
        } else {
            type_expr_to_infer_with_generics_and_self(te, &generic_map, target_name)
        })
    };

    // Include struct TypeVars in self type so call-site unification resolves correctly.
    // For a primitive target (`impl Display for i64`) the self type must be the
    // concrete primitive, since call sites produce `Concrete(Type::I64)` and the
    // unifier has no Named↔Concrete bridge (METEL-181).
    let self_ty = if let Some(element_tv) = struct_tvars_ordered
        .first()
        .copied()
        .filter(|_| array_target_generic_name.is_some())
    {
        InferType::Array(Box::new(InferType::Var(element_tv)))
    } else if let Some(prim) = primitive_type_from_name(target_name) {
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
        let by_var: std::collections::HashMap<TypeVar, Vec<GenericBound>> = impl_bounds
            .iter()
            .enumerate()
            .filter_map(|(i, bounds)| {
                if bounds.is_empty() {
                    return None;
                }
                let resolved_tv = struct_tvars_resolved.get(i)?;
                Some((*resolved_tv, bounds.clone()))
            })
            .collect();
        let by_neg_var: std::collections::HashMap<TypeVar, Vec<GenericBound>> = impl_neg_bounds
            .iter()
            .enumerate()
            .filter_map(|(i, bounds)| {
                if bounds.is_empty() {
                    return None;
                }
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
        if array_target_generic_name.is_some() {
            ctx.register_array_method_scheme(
                method.name.clone(),
                scheme.clone(),
                struct_tvars_resolved.clone(),
            );
            ctx.register_array_method_scheme_variant(
                method.name.clone(),
                scheme,
                struct_tvars_resolved,
                ib.aspect_name.clone(),
                method.span.clone(),
            );
        } else {
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
                ib.aspect_name.clone(),
                method.span.clone(),
            );
        }
    } else if array_target_generic_name.is_some() {
        ctx.register_array_method(method.name.clone(), resolved_fun_ty);
    } else {
        ctx.register_method(
            target_name.to_string(),
            method.name.clone(),
            resolved_fun_ty,
        );
    }
    Ok(())
}

fn substitute_structural_self(te: &TypeExpr, replacement: &TypeExpr) -> TypeExpr {
    match te {
        TypeExpr::Named(name, args) if name == "Self" && args.is_empty() => replacement.clone(),
        TypeExpr::Named(name, args) => TypeExpr::Named(
            name.clone(),
            args.iter()
                .map(|arg| substitute_structural_self(arg, replacement))
                .collect(),
        ),
        TypeExpr::Unit => TypeExpr::Unit,
        TypeExpr::Tuple(items) => TypeExpr::Tuple(
            items
                .iter()
                .map(|item| substitute_structural_self(item, replacement))
                .collect(),
        ),
        TypeExpr::Record(fields) => TypeExpr::Record(
            fields
                .iter()
                .map(|(name, ty)| {
                    (
                        name.clone(),
                        substitute_structural_self(ty, replacement),
                    )
                })
                .collect(),
        ),
        TypeExpr::Array(inner) => TypeExpr::Array(Box::new(substitute_structural_self(
            inner.as_ref(),
            replacement,
        ))),
        TypeExpr::SizedArray(inner, len) => TypeExpr::SizedArray(
            Box::new(substitute_structural_self(inner.as_ref(), replacement)),
            *len,
        ),
        TypeExpr::Reference(inner) => TypeExpr::Reference(Box::new(substitute_structural_self(
            inner.as_ref(),
            replacement,
        ))),
        TypeExpr::MutReference(inner) => TypeExpr::MutReference(Box::new(
            substitute_structural_self(inner.as_ref(), replacement),
        )),
        TypeExpr::Fun(params, ret) => TypeExpr::Fun(
            params
                .iter()
                .map(|param| substitute_structural_self(param, replacement))
                .collect(),
            ret.as_ref().map(|ret_ty| {
                Box::new(substitute_structural_self(ret_ty.as_ref(), replacement))
            }),
        ),
        TypeExpr::ImplAspect {
            bound,
            source_spell,
            span,
        } => TypeExpr::ImplAspect {
            bound: Box::new(substitute_structural_self(bound.as_ref(), replacement)),
            source_spell: source_spell.clone(),
            span: span.clone(),
        },
        TypeExpr::Projection {
            base,
            assoc_name,
            span,
        } => TypeExpr::Projection {
            base: Box::new(substitute_structural_self(base.as_ref(), replacement)),
            assoc_name: assoc_name.clone(),
            span: span.clone(),
        },
        TypeExpr::RecordProjection { path, fields, span } => TypeExpr::RecordProjection {
            path: path.clone(),
            fields: fields.clone(),
            span: span.clone(),
        },
    }
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
            let resolved_iter = peel_all_references(&partial.apply(&iter_ty));
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
                    // Prefer a per-instantiation resolution via the polymorphic
                    // method scheme over the static Iterable registry entry: for a
                    // generic struct implementing Iterable<T> generically (e.g.
                    // `extend<T> Wrapper<T>: Iterable<T> { ... }`), the registry's
                    // own recorded "type args" are the impl's still-generic
                    // parameter names, not concrete types (registered before any
                    // instantiation is known) -- reading them directly would bind
                    // elem_ty to that bogus placeholder instead of the receiver's
                    // actual instantiation.
                    let elem_from_scheme = if let InferType::Named(name, type_args) = &resolved_iter
                    {
                        ctx.method_scheme_for(name, "next")
                            .and_then(|(scheme, struct_tvars)| {
                                let mut subst = Substitution::new();
                                for (&tv, concrete) in struct_tvars.iter().zip(type_args.iter()) {
                                    subst.bind(tv, concrete.clone());
                                }
                                match subst.apply(&scheme.ty) {
                                    InferType::Fun(_, ret) => match *ret {
                                        InferType::Named(n, mut args)
                                            if n == "Perhaps" && args.len() == 1 =>
                                        {
                                            Some(args.remove(0))
                                        }
                                        _ => None,
                                    },
                                    _ => None,
                                }
                            })
                    } else {
                        None
                    };
                    // Fall back to the Iterable registry (concrete impls).
                    let elem = elem_from_scheme.or_else(|| {
                        let type_name = infer_type_name(&resolved_iter).map(ToOwned::to_owned);
                        type_name
                            .as_deref()
                            .and_then(|name| ctx.iterable_elem_type(name))
                            .cloned()
                            .map(InferType::Concrete)
                    });
                    match elem {
                        Some(t) => {
                            ctx.add_constraint(elem_ty.clone(), t, fi.span.clone());
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
            if let Some(ty) = ctx.lookup(name) {
                return Ok(ty);
            }
            if let Some(fields) = ctx.get_struct_fields(name) {
                if fields.is_empty() {
                    let type_args: Vec<InferType> = ctx
                        .get_struct_type_params(name)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|_| ctx.fresh_var())
                        .collect();
                    return Ok(InferType::Named(name.clone(), type_args));
                }
            }
            if ctx.registry().has_variant_named(name) {
                // RFC-0111 §3.1: defer to pass 2, which resolves against the expected
                // type. Recorded so a deferral that never resolves is reported after
                // solving instead of being silently accepted (metel-core#285).
                let var = ctx.fresh_var();
                if let InferType::Var(tv) = var {
                    ctx.record_variant_deferral(span.clone(), name.clone(), tv);
                }
                return Ok(var);
            }
            Err(MetelError::type_error(
                TypeErrorCode::T0003,
                format!("undefined name `{name}`"),
                span,
            ))
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
        Expr::RecordLiteral { fields, .. } => {
            let mut inferred_fields = Vec::with_capacity(fields.len());
            for (name, expr) in fields {
                inferred_fields.push((name.clone(), infer_expr(expr, ctx, fun_generalizations)?));
            }
            Ok(InferType::Record(inferred_fields))
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

            // Check for opaque-returning function and do dedicated instantiation
            if let Some(callee_name) = super::overload::callee_name(callee) {
                if let Some(scheme) = ctx.poly_scheme(callee_name) {
                    if !scheme.opaque_returns.is_empty() {
                        // This function has opaque returns - do dedicated instantiation
                        let arg_infer: Vec<InferType> = args
                            .iter()
                            .map(|a| infer_expr(a, ctx, fun_generalizations))
                            .collect::<Result<_, _>>()?;

                        // Solve constraints to get a complete substitution
                        let solved = ctx.solve()?;
                        let _solved = ctx.default_literal_vars(&solved);

                        // Instantiate the scheme with renaming to get fresh vars.
                        // Must mint from ctx's own live TypeVar generator (not a
                        // disposable one forked via fresh_var_generator, which
                        // snapshots the counter without ever advancing it) --
                        // otherwise every subsequent ordinary ctx.fresh_var() call
                        // in the rest of this function body reissues the exact
                        // same ids just handed out here, aliasing this call's
                        // opaque marker with unrelated later TypeVars. Confirmed
                        // by reproduction: three or more opaque-returning calls in
                        // one block, with .display() called on at least two of
                        // them before a third, corrupted the third's inferred type.
                        let mut renaming: HashMap<TypeVar, TypeVar> =
                            HashMap::with_capacity(scheme.quantified_vars.len());
                        let mut rename_subst = Substitution::new();
                        for &var in &scheme.quantified_vars {
                            let fresh = ctx.fresh_type_var_raw();
                            rename_subst.bind(var, InferType::Var(fresh));
                            renaming.insert(var, fresh);
                        }
                        let instantiated_ty = rename_subst.apply(&scheme.ty);

                        if let InferType::Fun(params, ret) = instantiated_ty {
                            // Constrain arguments to match the instantiated function type
                            for (arg_ty, param) in arg_infer.iter().zip(params.iter()) {
                                ctx.add_constraint(arg_ty.clone(), param.clone(), span.clone());
                            }

                            // Register aspect bounds and mark opacity guards for each opaque return
                            for (i, opaque) in scheme.opaque_returns.iter().enumerate() {
                                if let Some((aspect, _)) = opaque {
                                    if let Some(&orig_tv) = scheme.quantified_vars.get(i) {
                                        if let Some(&fresh_tv) = renaming.get(&orig_tv) {
                                            ctx.register_type_var_bound(fresh_tv, aspect.clone());
                                            ctx.mark_opaque_return_var(fresh_tv);
                                        }
                                    }
                                }
                            }

                            return Ok(*ret);
                        }
                    }
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
            let resolved_obj = ctx.solve()?.apply(&obj_ty);
            match peel_all_references(&resolved_obj) {
                InferType::Array(elem) | InferType::SizedArray(elem, _) => Ok(*elem),
                _ => {
                    let elem_var = ctx.fresh_var();
                    ctx.add_constraint(
                        obj_ty,
                        InferType::Array(Box::new(elem_var.clone())),
                        span.clone(),
                    );
                    Ok(elem_var)
                }
            }
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
                    // RFC-0110 §4.2: bare assignment to an identifier always *rebinds*,
                    // for reference-typed bindings exactly as for every other type.
                    // RFC-0067a's implicit whole-value write-through is retired — it was
                    // the one auto-deref mechanism competing with a second sensible
                    // reading of the same syntax, and it made repointing a `&var T`
                    // unrepresentable. `*p = v` (AssignTarget::Deref) is now the spelling
                    // that writes through.
                    ctx.lookup_for_write(name, target_span)?
                }
                // RFC-0110: `*p = v` writes through to the referent. The value's type is
                // the referent type, so peel exactly the layer `*` names.
                AssignTarget::Deref {
                    object,
                    span: target_span,
                } => {
                    let obj_ty = infer_expr(object, ctx, fun_generalizations)?;
                    let inner = ctx.fresh_var();
                    ctx.add_constraint(
                        obj_ty,
                        InferType::MutReference(Box::new(inner.clone())),
                        target_span.clone(),
                    );
                    inner
                }
                AssignTarget::Index {
                    object,
                    index,
                    span: target_span,
                } => {
                    let raw_obj_ty = infer_expr(object, ctx, fun_generalizations)?;
                    // RFC-0110 §4.1: an index target reaches through a reference at the
                    // root, the same way a field target already does. Peel before
                    // constraining, or `xs[0] = v` for `xs: &var i64[]` would try to
                    // unify `&var i64[]` with `?t[]` and fail.
                    let obj_ty = peel_all_references(&raw_obj_ty);
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
                AssignTarget::TupleAccess {
                    object,
                    index,
                    span: target_span,
                } => {
                    infer_tuple_assign_type(object, *index, target_span, ctx, fun_generalizations)?
                }
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
            let peeled = peel_all_references(&obj_ty);
            if let InferType::Record(fields) = &peeled {
                return fields
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, ty)| ty.clone())
                    .ok_or_else(|| {
                        MetelError::type_error(
                            TypeErrorCode::T0003,
                            format!("no field `{field}` on {peeled}"),
                            span,
                        )
                    });
            }
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
            let peeled_recv = peel_all_references(&recv_ty);
            if let InferType::Array(elem) = &peeled_recv {
                let method_ty = if let Some(ty) = ctx.get_array_method_type(method).cloned() {
                    ty
                } else if let Some((scheme, struct_tvars)) = ctx
                    .array_method_scheme_for(method)
                    .map(|(s, t)| (s.clone(), t.clone()))
                {
                    let mut inst = Substitution::new();
                    let mut renaming: HashMap<TypeVar, TypeVar> = HashMap::new();
                    for &qv in &scheme.quantified_vars {
                        let fresh = ctx.fresh_type_var_raw();
                        inst.bind(qv, InferType::Var(fresh));
                        renaming.insert(qv, fresh);
                    }
                    let instance = inst.apply(&scheme.ty);
                    let mut pin = Substitution::new();
                    for (&tv, arg) in struct_tvars.iter().zip(std::iter::once(elem.as_ref())) {
                        if let Some(&fresh) = renaming.get(&tv) {
                            pin.bind(fresh, arg.clone());
                        }
                    }
                    pin.apply(&instance)
                } else {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!("no method `{method}` on array type"),
                        span,
                    ));
                };

                if matches!(
                    ctx.get_array_method_receiver_kind(method),
                    Some(crate::ast::ReceiverKind::RefMut)
                ) && !chain_provides_mut_access(&recv_ty)
                {
                    // T0006, not T0008 — T0008 is "non-exhaustive match". This site
                    // has been miscoding the error since it was written; no fixture
                    // covered it, so nothing caught it. The spelling is `&var self`
                    // too: `&mut` is not syntax this language has (#301).
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0006,
                        format!(
                            "cannot call `&var self` method `{method}` through a shared reference"
                        ),
                        span,
                    ));
                }

                if let InferType::Fun(params, ret) = &method_ty {
                    if params.len().saturating_sub(1) != arg_tys.len() {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0004,
                            format!(
                                "expected {} argument(s), got {}",
                                params.len().saturating_sub(1),
                                arg_tys.len()
                            ),
                            span,
                        ));
                    }
                    for (arg_ty, param) in arg_tys.iter().zip(params.iter().skip(1)) {
                        ctx.add_constraint(arg_ty.clone(), param.clone(), span.clone());
                    }
                    return Ok(*ret.clone());
                }
                return Err(MetelError::internal("array method type is not a function"));
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
                    // Reached through a shared reference: reject outright, the way
                    // the array-method site above already does. The binding check
                    // below cannot speak for a receiver that is not a binding
                    // (`pair.0.bump()`), which let mutation through a `&T` past.
                    if is_shared_reference_chain(&recv_ty) {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0006,
                            format!(
                                "cannot call `&var self` method `{method}` through a shared reference"
                            ),
                            span,
                        ));
                    }
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
            //
            // Peeled first, so `x: &T` under `T: Show` reaches the same bound
            // lookup as `x: T` (#334). The concrete-receiver path above already
            // peels; without it here, a borrowing generic could not call an
            // aspect method on its own parameter, which is the shape every
            // read-only generic wants once move checking pushes it to borrow.
            let peeled_recv_for_bounds = peel_all_references(&recv_ty);
            if let InferType::Var(tv) = &peeled_recv_for_bounds {
                if let Some(aspect_names) = ctx.bounds_for_type_var(*tv) {
                    let self_generic_map: HashMap<String, TypeVar> =
                        std::iter::once(("Self".to_string(), *tv)).collect();
                    for aspect_name in aspect_names.iter().filter_map(GenericBound::aspect_name) {
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
                                                aspect_name,
                                                n,
                                            ))
                                        }
                                        other => {
                                            type_expr_to_infer_with_generics(other, &self_generic_map)
                                        }
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
                                        let param_ty =
                                            type_expr_to_infer_with_generics(ann, &self_generic_map);
                                        ctx.add_constraint(arg_ty.clone(), param_ty, span.clone());
                                    }
                                }

                                // Mutable-access guard, mirroring the concrete-receiver
                                // path above. Peeling the receiver (#334) is what makes
                                // this reachable at all: without it a `&var self` method
                                // on a bounded `T` was rejected for the wrong reason —
                                // "cannot infer receiver type" — and peeling alone would
                                // have made `x.bump()` legal through a shared `&T`.
                                let receiver_kind = method_def
                                    .params
                                    .iter()
                                    .find(|p| p.name == "self")
                                    .and_then(|p| p.receiver.clone());
                                if matches!(receiver_kind, Some(crate::ast::ReceiverKind::RefMut))
                                    && !chain_provides_mut_access(&recv_ty)
                                {
                                    if is_shared_reference_chain(&recv_ty) {
                                        return Err(MetelError::type_error(
                                            TypeErrorCode::T0006,
                                            format!(
                                                "cannot call `&var self` method `{method}` through a shared reference"
                                            ),
                                            span,
                                        ));
                                    }
                                    if let Expr::Ident(name, recv_span) = receiver.as_ref() {
                                        let _ = ctx.lookup_for_write(name, recv_span)?;
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
                            aspect_names
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(" + ")
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
            } else if path.len() == 1
                && ctx.registry().has_variant_named(&path[0])
                && ctx.get_struct_fields(&path[0]).is_none()
            {
                let var = ctx.fresh_var();
                if let InferType::Var(tv) = var {
                    ctx.record_variant_deferral(span.clone(), path[0].clone(), tv);
                }
                Ok(var)
            } else {
                let struct_name = path
                    .last()
                    .ok_or_else(|| MetelError::internal("empty path in struct literal"))?
                    .clone();
                infer_struct_literal(struct_name, fields, span, ctx, fun_generalizations)
            }
        }
        Expr::RecordProjection { path, fields, span } => {
            let base_expr = record_projection_base_expr(path, span);
            let base_ty = infer_expr(&base_expr, ctx, fun_generalizations)?;
            let base_ty = ctx.solve()?.apply(&base_ty);
            let struct_name = named_type_name(&base_ty).ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0002,
                    "record projection requires a nominal struct value",
                    span,
                )
            })?;
            let type_args = match &base_ty {
                InferType::Named(_, args) => args.clone(),
                InferType::Reference(inner) | InferType::MutReference(inner) => match inner.as_ref() {
                    InferType::Named(_, args) => args.clone(),
                    _ => vec![],
                },
                _ => vec![],
            };
            let declared_fields = ctx
                .get_struct_fields(&struct_name)
                .ok_or_else(|| {
                    MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!("unknown type `{struct_name}`"),
                        span,
                    )
                })?
                .clone();
            let mut projected = Vec::with_capacity(fields.len());
            for field in fields {
                let field_entry = declared_fields
                    .iter()
                    .find(|entry| entry.name == *field)
                    .ok_or_else(|| {
                        MetelError::type_error(
                            TypeErrorCode::T0003,
                            format!("no field `{field}` on `{struct_name}`"),
                            span,
                        )
                    })?;
                let raw_ty = field_entry.ty.clone();
                let ty = if let Some(type_params) = ctx.get_struct_type_params(&struct_name).cloned()
                {
                    let mut remap = Substitution::new();
                    for (&tp, arg) in type_params.iter().zip(type_args.iter()) {
                        remap.bind(tp, arg.clone());
                    }
                    remap.apply(&raw_ty)
                } else {
                    raw_ty
                };
                projected.push((field.clone(), ty));
            }
            Ok(InferType::Record(projected))
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
            let target_ty = ann_to_infer(target_type, ctx);
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
            let peeled = peel_all_references(&obj_ty);
            match &peeled {
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
            span,
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
            ctx.record_closure_return_type(span.clone(), ret_ty.clone());
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
    let raw_scrutinee_ty = infer_expr(&m.scrutinee, ctx, fun_generalizations)?;
    // RFC-0108: a `&T`/`&mut T` scrutinee matches against `T`'s own patterns —
    // peel reference layers before pattern inference, the same way method-call
    // receiver resolution already does. Applying the current substitution first
    // resolves a scrutinee var to its reference shape where solve order allows
    // (matching `Expr::Index`'s own peel), then peels every layer.
    let scrutinee_ty = peel_all_references(&ctx.solve()?.apply(&raw_scrutinee_ty));
    // RFC-0107: resolve bare variant patterns against the scrutinee's enum in Pass 1
    // too, so a one-segment fieldful pattern (`Some { value }`) doesn't hit
    // `infer_pattern`'s two-segment path assertion, and a bare no-field variant
    // (`Red`) is typed as the variant rather than a spurious binding.
    let scrutinee_enum_name = match &scrutinee_ty {
        InferType::Named(name, _) | InferType::Concrete(Type::Named(name, _)) => {
            Some(name.clone())
        }
        _ => None,
    };
    let scrutinee_variants: Option<(String, Vec<(String, bool)>)> = scrutinee_enum_name
        .and_then(|name| {
            ctx.get_enum(&name).map(|info| {
                (
                    name.clone(),
                    info.variants
                        .iter()
                        .map(|v| (v.name.clone(), v.fields.is_empty()))
                        .collect(),
                )
            })
        });
    let result_var = ctx.fresh_var();
    for arm in &m.arms {
        let pattern = match &scrutinee_variants {
            Some((enum_name, variants)) => {
                super::construction::resolve_bare_variant(&arm.pattern, enum_name, variants)
            }
            None => arm.pattern.clone(),
        };
        ctx.push_scope();
        infer_pattern(&pattern, &scrutinee_ty, ctx)?;
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
        Pattern::Record {
            fields,
            span: pat_span,
        } => {
            let field_vars: Vec<(String, InferType)> = fields
                .iter()
                .map(|name| (name.clone(), ctx.fresh_var()))
                .collect();
            ctx.add_constraint(
                scrutinee_ty.clone(),
                InferType::Record(field_vars.clone()),
                pat_span.clone(),
            );
            for (name, ty) in field_vars {
                ctx.bind_mono(&name, ty, false);
            }
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
        | Pattern::Binding(_, s)
        | Pattern::Literal(_, s)
        | Pattern::Tuple(_, s)
        | Pattern::EnumVariant { span: s, .. }
        | Pattern::Record { span: s, .. }
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

fn record_projection_base_expr(path: &[String], span: &Span) -> Expr {
    if path.len() == 1 {
        Expr::Ident(path[0].clone(), span.clone())
    } else {
        Expr::Path(path.to_vec(), span.clone())
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
/// Whether the receiver is reached *through a shared reference* — a reference
/// chain, none of whose layers grants mutable access.
///
/// Distinct from `!chain_provides_mut_access`, which is also true of an owned
/// receiver. An owned receiver may still be mutated when its binding is `var`,
/// so it must fall through to the binding check rather than be rejected here.
fn is_shared_reference_chain(ty: &InferType) -> bool {
    matches!(ty, InferType::Reference(_)) && !chain_provides_mut_access(ty)
}

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
    let recv_ty = peel_all_references(recv_ty);
    let _ = span;
    if matches!(recv_ty, InferType::Array(_) | InferType::SizedArray(_, _))
        && method == "len"
        && arg_tys.is_empty()
    {
        return Some(Ok(InferType::int()));
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
            ctx.add_operand_constraint(lhs_ty, result.clone(), span.clone(), op.symbol());
            ctx.add_operand_constraint(rhs_ty, result.clone(), span.clone(), op.symbol());
            Ok(result)
        }
        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
            let result = ctx.fresh_var();
            ctx.add_operand_constraint(lhs_ty, result.clone(), span.clone(), op.symbol());
            ctx.add_operand_constraint(rhs_ty, result.clone(), span.clone(), op.symbol());
            Ok(result)
        }
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            // Tagged with the operator so a mismatch names it (`operator `==` cannot be
            // applied to `i64` and `String``) instead of the bare `cannot unify`, which
            // never mentioned that a comparison was what required the two to agree.
            ctx.add_operand_constraint(lhs_ty, rhs_ty, span.clone(), op.symbol());
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
    // Note the `Var(_)` arm here is deliberately left inspecting the *raw* `declared`.
    // Substituting it was tried and fixed nothing: where `declared` is still a variable
    // — the closure's own return type while its body's tail is being constrained — the
    // constraint that would resolve it has not been generated yet, so applying the
    // current substitution is a no-op. That is an ordering limitation, not a missing
    // `apply`, and it is out of scope here. See RFC-0112 §1.0.
    if matches!(
        declared,
        InferType::Reference(_) | InferType::MutReference(_) | InferType::Var(_)
    ) {
        ctx.add_constraint(actual.clone(), declared, span);
        return actual;
    }
    // Decide whether to peel against the *substituted* type, not the raw one. Without
    // this, the decision is made before the information needed to make it exists: a call
    // returning `&T` yields a fresh `InferType::Var` here, which matches no reference
    // pattern below, so the peel is silently skipped and the later unification of that
    // var against the declared referent type fails with T0001. `let n: i64 = g();` for
    // `fun g() -> &i64` failed where the equivalent `let n: i64 = r;` succeeded — a
    // distinction no user could predict.
    //
    // Same shape as `infer_match`'s scrutinee peel (RFC-0108) and `Expr::Call`'s
    // auto-deref, both of which already solve-and-apply before inspecting. A solve
    // failure here is not fatal: constraints can be transiently inconsistent mid-pass,
    // and the real error surfaces from the final solve, so fall back to the raw type
    // and let this call behave exactly as it did before.
    let resolved_actual = ctx
        .solve()
        .map_or_else(|_| actual.clone(), |subst| subst.apply(&actual));
    let mut peeled = resolved_actual;
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
        Expr::FieldAccess { object, .. }
        | Expr::TupleAccess { object, .. }
        | Expr::Index { object, .. } => {
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
    let peeled = peel_all_references(&obj_ty);
    if let InferType::Record(fields) = &peeled {
        return fields
            .iter()
            .find(|(name, _)| name == field)
            .map(|(_, ty)| ty.clone())
            .ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("no field `{field}` on {peeled}"),
                    target_span,
                )
            });
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

fn infer_tuple_assign_type(
    object: &Expr,
    index: usize,
    target_span: &Span,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    let obj_ty = infer_expr(object, ctx, fun_generalizations)?;
    let obj_ty = ctx.solve()?.apply(&obj_ty);
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
    // Reach through a reference at the root, the way field- and index-path assignment
    // already do (`s.x = v` and `xs[0] = v` both work for a `&var` receiver). Peeled
    // *after* the `is_through_mut_ptr` check above, which needs the unpeeled shape.
    match peel_all_references(&obj_ty) {
        InferType::Tuple(elems) => elems.get(index).cloned().ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!(
                    "tuple index {index} out of bounds (tuple has {} elements)",
                    elems.len()
                ),
                target_span,
            )
        }),
        _ => Err(MetelError::type_error(
            TypeErrorCode::T0002,
            "cannot infer tuple type for assignment; add a type annotation",
            target_span,
        )),
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
            if let Some(type_ann) = &p.type_ann {
                Param {
                    mutable: p.mutable,
                    receiver: p.receiver.clone(),
                    name: p.name.clone(),
                    type_ann: Some(lower_impl_aspect_param_type(
                        type_ann,
                        counter,
                        &mut extra_generics,
                    )),
                    span: p.span.clone(),
                }
            } else {
                p.clone()
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

fn lower_impl_aspect_param_type(
    type_expr: &TypeExpr,
    counter: &mut usize,
    extra_generics: &mut Vec<GenericParam>,
) -> TypeExpr {
    match type_expr {
        TypeExpr::ImplAspect { bound, span, .. } => {
            let anon_name = format!("_ImplT{counter}");
            *counter += 1;
            extra_generics.push(GenericParam {
                name: anon_name.clone(),
                is_record: false,
                bounds: vec![Bound {
                    polarity: Polarity::Positive,
                    head: crate::ast::BoundHead::Aspect(bound.as_ref().clone()),
                    assoc_bindings: vec![],
                    span: span.clone(),
                }],
            });
            TypeExpr::Named(anon_name, vec![])
        }
        TypeExpr::Named(name, args) => TypeExpr::Named(
            name.clone(),
            args.iter()
                .map(|arg| lower_impl_aspect_param_type(arg, counter, extra_generics))
                .collect(),
        ),
        TypeExpr::Tuple(items) => TypeExpr::Tuple(
            items
                .iter()
                .map(|item| lower_impl_aspect_param_type(item, counter, extra_generics))
                .collect(),
        ),
        TypeExpr::Record(fields) => TypeExpr::Record(
            fields
                .iter()
                .map(|(name, field_ty)| {
                    (
                        name.clone(),
                        lower_impl_aspect_param_type(field_ty, counter, extra_generics),
                    )
                })
                .collect(),
        ),
        TypeExpr::Array(inner) => TypeExpr::Array(Box::new(lower_impl_aspect_param_type(
            inner,
            counter,
            extra_generics,
        ))),
        TypeExpr::SizedArray(inner, len) => TypeExpr::SizedArray(
            Box::new(lower_impl_aspect_param_type(inner, counter, extra_generics)),
            *len,
        ),
        TypeExpr::Reference(inner) => TypeExpr::Reference(Box::new(lower_impl_aspect_param_type(
            inner,
            counter,
            extra_generics,
        ))),
        TypeExpr::MutReference(inner) => TypeExpr::MutReference(Box::new(
            lower_impl_aspect_param_type(inner, counter, extra_generics),
        )),
        TypeExpr::Fun(params, ret) => TypeExpr::Fun(
            params
                .iter()
                .map(|param| lower_impl_aspect_param_type(param, counter, extra_generics))
                .collect(),
            ret.as_ref().map(|ret_ty| {
                Box::new(lower_impl_aspect_param_type(
                    ret_ty,
                    counter,
                    extra_generics,
                ))
            }),
        ),
        TypeExpr::Projection {
            base,
            assoc_name,
            span,
        } => TypeExpr::Projection {
            base: Box::new(lower_impl_aspect_param_type(base, counter, extra_generics)),
            assoc_name: assoc_name.clone(),
            span: span.clone(),
        },
        TypeExpr::RecordProjection { .. } | TypeExpr::Unit => type_expr.clone(),
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
            type_ann: let_decl.type_ann.as_ref().map(|t| {
                lower_projections_in_type(t, &std::collections::HashSet::new(), &let_decl.span)
            }),
            value: lower_projections_in_expr(&let_decl.value, &std::collections::HashSet::new()),
            ..let_decl
        }),
        Decl::Mut(mut_decl) => Decl::Mut(crate::ast::MutDecl {
            type_ann: mut_decl.type_ann.as_ref().map(|t| {
                lower_projections_in_type(t, &std::collections::HashSet::new(), &mut_decl.span)
            }),
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
        crate::ast::Stmt::ForIn(fis) => crate::ast::Stmt::ForIn(Box::new(crate::ast::ForInStmt {
            binding: fis.binding.clone(),
            mutable: fis.mutable,
            iterable: lower_projections_in_expr(&fis.iterable, generics),
            body: lower_projections_in_block(&fis.body, generics),
            span: fis.span.clone(),
        })),
        crate::ast::Stmt::Expr(e) => crate::ast::Stmt::Expr(lower_projections_in_expr(e, generics)),
    }
}

// Exhaustive match over every Expr variant; splitting it up would scatter
// one coherent dispatch table across many small functions with no real gain
// in clarity.
#[allow(clippy::too_many_lines)]
fn lower_projections_in_expr(expr: &Expr, generics: &std::collections::HashSet<String>) -> Expr {
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
        Expr::Ascribe { expr: e, ann, span } => Expr::Ascribe {
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
        Expr::RecordLiteral { fields, span } => Expr::RecordLiteral {
            fields: fields.iter().map(|(name, expr)| (name.clone(), go(expr))).collect(),
            span: span.clone(),
        },
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
            object,
            field,
            span,
        } => Expr::FieldAccess {
            object: Box::new(go(object)),
            field: field.clone(),
            span: span.clone(),
        },
        Expr::TupleAccess {
            object,
            index,
            span,
        } => Expr::TupleAccess {
            object: Box::new(go(object)),
            index: *index,
            span: span.clone(),
        },
        Expr::Index {
            object,
            index,
            span,
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
        Expr::RecordProjection { path, fields, span } => Expr::RecordProjection {
            path: path.clone(),
            fields: fields.clone(),
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
        TypeExpr::Record(fields) => TypeExpr::Record(
            fields
                .iter()
                .map(|(name, ty)| (name.clone(), go(ty)))
                .collect(),
        ),
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
        TypeExpr::Projection { .. } | TypeExpr::RecordProjection { .. } => te.clone(),
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
