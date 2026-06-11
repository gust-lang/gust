use std::collections::HashMap;

use crate::ast::*;
use crate::error::{MetelError, TypeErrorCode};
use crate::typeinference::*;
use crate::types::Type;

use super::conversions::{
    infer_type_to_type, type_expr_to_infer, type_expr_to_infer_with_generics,
    type_expr_to_infer_with_generics_and_self, type_expr_to_infer_with_self, type_to_infer,
};
use super::FunGeneralization;

/// Resolve a type annotation, substituting any name that matches the current
/// function's generic type params with the corresponding TypeVar rather than
/// producing a Named type.  Must be used for all annotations inside function
/// bodies; bare `type_expr_to_infer` ignores the param map.
fn ann_to_infer(te: &TypeExpr, ctx: &InferContext) -> InferType {
    let params = ctx.type_params();
    if params.is_empty() {
        type_expr_to_infer(te)
    } else {
        type_expr_to_infer_with_generics(te, params)
    }
}

/// Register the names of all direct `FunDecl`s in `decls` with fresh type
/// variables so that forward references and mutual recursion work.
/// The function type of a `native` declaration, built from its annotations.
/// Native functions must be non-generic and fully annotated.
fn native_fun_ty(fun: &FunDecl, ctx: &mut InferContext) -> Result<InferType, MetelError> {
    // Generic native functions (e.g. `List::new<T>`) map each type parameter to a
    // fresh TypeVar; the caller generalizes the result into a polymorphic scheme.
    let generic_map = fun_generic_map(fun, ctx);
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
    Ok(InferType::Fun(param_types, Box::new(ret_ty)))
}

pub(super) fn hoist_fun_decls(decls: &[Decl], ctx: &mut InferContext) {
    for decl in decls {
        if let Decl::Fun(fun) = decl {
            // Native functions have a concrete annotated signature and no body;
            // bind it eagerly so forward references resolve. Errors here surface
            // again (deterministically) in infer_fun_decl.
            if fun.native.is_some() {
                if let Ok(fun_ty) = native_fun_ty(fun, ctx) {
                    let env_fvs = ctx.env_free_vars();
                    ctx.bind_poly(&fun.name, generalize(fun_ty, &env_fvs));
                }
                continue;
            }
            // Overloaded names are not bound to a single shared type — each
            // definition is checked independently and dispatched by SymbolId
            // (METEL-180). Binding here would unify their distinct signatures.
            if ctx.is_overloaded(&fun.name) {
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
                if !type_var_bounds.is_empty() {
                    ctx.register_fun_bounds(fun.name.clone(), type_var_bounds.clone());
                }

                let te_to_infer = |te: &TypeExpr| -> InferType {
                    type_expr_to_infer_with_generics(te, &generic_map)
                };

                let param_types: Vec<InferType> = fun
                    .params
                    .iter()
                    .map(|p| {
                        if let Some(ann) = &p.type_ann {
                            te_to_infer(ann)
                        } else {
                            ctx.fresh_var()
                        }
                    })
                    .collect();

                let ret_ty = if let Some(ann) = &fun.return_type {
                    te_to_infer(ann)
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

fn collect_fun_type_var_bounds(
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
                    if let TypeExpr::Named(n, _) = b {
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
                    .filter_map(|b| {
                        if let TypeExpr::Named(n, _) = b {
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

fn infer_decl(
    decl: &Decl,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    match decl {
        Decl::Let(ld) => {
            let env_fvs = ctx.env_free_vars();
            let val_ty = infer_expr(&ld.value, ctx, fun_generalizations)?;
            if let Some(ann) = &ld.type_ann {
                ctx.add_constraint(val_ty.clone(), ann_to_infer(ann, ctx), ld.span.clone());
            }
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
                    });
                    return Ok(InferType::unit());
                }
            }
            ctx.bind_mono(&ld.name, val_ty, false);
            Ok(InferType::unit())
        }
        Decl::Mut(md) => {
            let val_ty = infer_expr(&md.value, ctx, fun_generalizations)?;
            if let Some(ann) = &md.type_ann {
                ctx.add_constraint(val_ty.clone(), ann_to_infer(ann, ctx), md.span.clone());
            }
            ctx.bind_mono(&md.name, val_ty, true);
            Ok(InferType::unit())
        }
        Decl::Fun(fd) => {
            infer_fun_decl(fd, ctx, fun_generalizations)?;
            Ok(InferType::unit())
        }
        Decl::Struct(_) | Decl::Enum(_) | Decl::Aspect(_) => Ok(InferType::unit()),
        Decl::Impl(ib) => {
            let target_name = match &ib.target_type {
                TypeExpr::Named(name, _) => name.rsplit("::").next().unwrap_or(name).to_string(),
                _ => {
                    return Err(MetelError::internal(
                        "generic impl blocks not yet supported",
                    ))
                }
            };
            let mut inherited_defaults = vec![];
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
            }
            for method in &ib.methods {
                infer_impl_method(method, &target_name, ctx, fun_generalizations)?;
            }
            for method in &inherited_defaults {
                infer_default_aspect_method(method, &target_name, ctx, fun_generalizations)?;
            }
            Ok(InferType::unit())
        }
        Decl::Stmt(stmt) => infer_stmt(stmt, ctx, fun_generalizations),
    }
}

fn infer_fun_decl(
    fun: &FunDecl,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<(), MetelError> {
    // Native functions have no Metel body to infer. Validate and record their
    // annotated signature for the construction pass; dispatch is by NativeKey.
    if fun.native.is_some() {
        let fun_ty = native_fun_ty(fun, ctx)?;
        let env_fvs = ctx.env_free_vars();
        ctx.bind_poly(&fun.name, generalize(fun_ty.clone(), &env_fvs));
        fun_generalizations.push(FunGeneralization {
            name: fun.name.clone(),
            fun_ty,
            env_fvs,
            name_map: HashMap::new(),
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

    let te_to_infer = |te: &TypeExpr| -> InferType {
        if generic_map.is_empty() {
            type_expr_to_infer(te)
        } else {
            type_expr_to_infer_with_generics(te, &generic_map)
        }
    };

    let param_types: Vec<InferType> = fun
        .params
        .iter()
        .map(|p| {
            if let Some(ann) = &p.type_ann {
                te_to_infer(ann)
            } else {
                ctx.fresh_var()
            }
        })
        .collect();

    let ret_ty = if let Some(ann) = &fun.return_type {
        te_to_infer(ann)
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
    let saved_tp_bounds = ctx.swap_type_param_bounds(type_var_bounds);
    let saved_ret = ctx.push_return_type(ret_ty.clone());
    let body_ty = infer_block(&fun.body, ctx, fun_generalizations)?;

    ctx.add_constraint(body_ty, ret_ty.clone(), fun.body.span.clone());

    ctx.pop_return_type(saved_ret);
    ctx.swap_type_param_bounds(saved_tp_bounds);
    ctx.swap_type_params(saved_type_params);
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
    // Store resolved_ty (post-solve) so the re-generalization in check_impl uses the
    // already-solved type and is not perturbed by a now-empty final substitution.
    fun_generalizations.push(FunGeneralization {
        name: fun.name.clone(),
        fun_ty: resolved_ty,
        env_fvs,
        name_map,
    });
    Ok(())
}

fn infer_impl_method(
    method: &FunDecl,
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

    let te_to_infer = |te: &TypeExpr| -> InferType {
        if generic_map.is_empty() {
            type_expr_to_infer_with_self(te, target_name)
        } else {
            type_expr_to_infer_with_generics_and_self(te, &generic_map, target_name)
        }
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
                self_ty.clone()
            } else if let Some(ann) = &p.type_ann {
                te_to_infer(ann)
            } else {
                ctx.fresh_var()
            }
        })
        .collect();
    let ret_ty = method
        .return_type
        .as_ref()
        .map(te_to_infer)
        .unwrap_or_else(InferType::unit);

    // Native methods have no Metel body; their signature comes entirely from
    // annotations (METEL-181). Skip body inference but still register the
    // method scheme below so call sites resolve.
    if method.native.is_none() {
        ctx.push_scope();
        for (p, pt) in method.params.iter().zip(param_types.iter()) {
            let is_mutable =
                p.mutable || matches!(p.receiver, Some(crate::ast::ReceiverKind::RefMut));
            ctx.bind_mono(&p.name, pt.clone(), is_mutable);
        }
        let saved_type_params = ctx.swap_type_params(generic_map);
        let saved_tp_bounds = ctx.swap_type_param_bounds(struct_bounds);
        let saved_ret = ctx.push_return_type(ret_ty.clone());
        let body_ty = infer_block(&method.body, ctx, fun_generalizations)?;
        ctx.add_constraint(body_ty, ret_ty.clone(), method.body.span.clone());
        ctx.pop_return_type(saved_ret);
        ctx.swap_type_param_bounds(saved_tp_bounds);
        ctx.swap_type_params(saved_type_params);
        ctx.pop_scope();
    }

    let solved = ctx.solve()?;
    let partial_subst = ctx.default_literal_vars(&solved);
    let fun_ty = InferType::Fun(param_types, Box::new(ret_ty));
    let resolved_fun_ty = partial_subst.apply(&fun_ty);

    // If the resolved method type still has free TypeVars from the struct's generic params,
    // store it as a polymorphic scheme so Pass 2 can instantiate it per call site.
    let struct_tvars_free: std::collections::HashSet<TypeVar> =
        struct_tvars_ordered.iter().copied().collect();
    if !struct_tvars_free.is_empty()
        && free_vars(&resolved_fun_ty)
            .iter()
            .any(|v| struct_tvars_free.contains(v))
    {
        let scheme = generalize(resolved_fun_ty, &std::collections::HashSet::new());
        ctx.register_method_scheme(
            target_name.to_string(),
            method.name.clone(),
            scheme,
            struct_tvars_ordered,
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
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<(), MetelError> {
    let generic_map: HashMap<String, TypeVar> = method
        .generics
        .iter()
        .map(|g| (g.name.clone(), ctx.fresh_type_var_raw()))
        .collect();

    let te_to_infer = |te: &TypeExpr| -> InferType {
        if generic_map.is_empty() {
            type_expr_to_infer_with_self(te, target_name)
        } else {
            type_expr_to_infer_with_generics_and_self(te, &generic_map, target_name)
        }
    };

    // TODO(named-concrete): same issue as infer_impl_method — see comment there.
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
                te_to_infer(ann)
            } else {
                ctx.fresh_var()
            }
        })
        .collect();
    let ret_ty = method
        .return_type
        .as_ref()
        .map(te_to_infer)
        .unwrap_or_else(InferType::unit);
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
    ctx.add_constraint(body_ty, ret_ty.clone(), body.span.clone());
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

fn infer_stmt(
    stmt: &Stmt,
    ctx: &mut InferContext,
    fun_generalizations: &mut Vec<FunGeneralization>,
) -> Result<InferType, MetelError> {
    match stmt {
        Stmt::Expr(e) => {
            infer_expr(e, ctx, fun_generalizations)?;
            Ok(InferType::unit())
        }
        Stmt::Return(r) => {
            let ret_ty = match &r.value {
                Some(e) => infer_expr(e, ctx, fun_generalizations)?,
                None => InferType::unit(),
            };
            if let Some(expected) = ctx.current_return_type().cloned() {
                ctx.add_constraint(ret_ty, expected, r.span.clone());
            }
            Ok(InferType::never())
        }
        Stmt::Break(bs) => {
            let break_ty = match &bs.value {
                Some(e) => infer_expr(e, ctx, fun_generalizations)?,
                None => InferType::unit(),
            };
            if let Some(expected) = ctx.current_break_type().cloned() {
                ctx.add_constraint(break_ty, expected, bs.span.clone());
            }
            Ok(InferType::never())
        }
        Stmt::Continue(_) => Ok(InferType::never()),
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
                        if let Some(ann) = &ld.type_ann {
                            ctx.add_constraint(
                                val_ty.clone(),
                                ann_to_infer(ann, ctx),
                                ld.span.clone(),
                            );
                        }
                        ctx.bind_mono(&ld.name, val_ty, false);
                    }
                    ForInit::Mut(md) => {
                        let val_ty = infer_expr(&md.value, ctx, fun_generalizations)?;
                        if let Some(ann) = &md.type_ann {
                            ctx.add_constraint(
                                val_ty.clone(),
                                ann_to_infer(ann, ctx),
                                md.span.clone(),
                            );
                        }
                        ctx.bind_mono(&md.name, val_ty, true);
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
                InferType::Array(elem) => {
                    ctx.add_constraint(elem_ty.clone(), *elem.clone(), fi.span.clone());
                }
                InferType::SizedArray(elem, _) => {
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
                    let solved = ctx.solve()?;
                    let arg_types: Vec<Type> = arg_infer
                        .iter()
                        .map(|t| infer_type_to_type(&solved.apply(t), span))
                        .collect::<Result<_, _>>()
                        .map_err(|_| {
                            MetelError::type_error(
                                TypeErrorCode::T0002,
                                format!(
                                    "cannot resolve argument types for overloaded call to `{name}`; \
                                     add type annotations"
                                ),
                                span,
                            )
                        })?;
                    let entries = ctx.overload_candidates(name).unwrap();
                    let ret = match super::overload::select(entries, &arg_types) {
                        Some(entry) => entry.ret.clone(),
                        None => {
                            return Err(super::overload::no_match_error(
                                name, &arg_types, entries, span,
                            ))
                        }
                    };
                    let ret_var = ctx.fresh_var();
                    ctx.add_constraint(ret_var.clone(), type_to_infer(&ret), span.clone());
                    return Ok(ret_var);
                }
            }
            let callee_ty = infer_expr(callee, ctx, fun_generalizations)?;
            // Auto-deref: *(() -> T) and *mut (() -> T) are callable directly.
            let callee_ty = match ctx.solve()?.apply(&callee_ty) {
                InferType::Pointer(inner) | InferType::MutPointer(inner)
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
            match else_branch {
                Some(else_block) => {
                    let else_ty = infer_block(else_block, ctx, fun_generalizations)?;
                    ctx.add_constraint(then_ty.clone(), else_ty, span.clone());
                    Ok(then_ty)
                }
                None => {
                    ctx.add_constraint(then_ty, InferType::unit(), span.clone());
                    Ok(InferType::unit())
                }
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
                    ctx.lookup_for_write(name, target_span)?
                }
                AssignTarget::Deref {
                    object,
                    span: target_span,
                } => {
                    let obj_ty = infer_expr(object, ctx, fun_generalizations)?;
                    match ctx.solve()?.apply(&obj_ty) {
                        InferType::Pointer(inner) | InferType::MutPointer(inner) => *inner,
                        other => {
                            return Err(MetelError::type_error(
                                TypeErrorCode::T0002,
                                format!("cannot assign through non-pointer type `{other}`"),
                                target_span,
                            ));
                        }
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
                InferType::Pointer(inner) | InferType::MutPointer(inner) => match inner.as_ref() {
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
                    InferType::Pointer(inner) | InferType::MutPointer(inner) => {
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
                } else if let Some((scheme, struct_tvars)) =
                    ctx.method_scheme_for(&struct_name, method)
                {
                    // Instantiate the scheme using the receiver's concrete type args.
                    let mut subst = Substitution::new();
                    for (&tv, arg) in struct_tvars.iter().zip(recv_type_args.iter()) {
                        subst.bind(tv, arg.clone());
                    }
                    subst.apply(&scheme.ty)
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
                ) && !matches!(recv_ty, InferType::MutPointer(_))
                {
                    if let Expr::Ident(name, recv_span) = receiver.as_ref() {
                        let _ = ctx.lookup_for_write(name, recv_span)?;
                    }
                }

                let ret_var = ctx.fresh_var();
                let receiver_ty_for_method = match &recv_ty {
                    InferType::Pointer(inner) | InferType::MutPointer(inner) => *inner.clone(),
                    _ => recv_ty.clone(),
                };
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
                                // Resolve return type: Self → the TypeVar itself.
                                let ret_ty = method_def
                                    .return_type
                                    .as_ref()
                                    .map(|rt| match rt {
                                        TypeExpr::Named(n, _) if n == "Self" => InferType::Var(*tv),
                                        other => type_expr_to_infer(other),
                                    })
                                    .unwrap_or(InferType::unit());

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
        Expr::StructLiteral { path, fields, span } => {
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
            ctx.add_constraint(inner_ty.clone(), ascribed_ty, span.clone());
            Ok(inner_ty)
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
            let ret_ty = return_type
                .as_ref()
                .map(|ann| ann_to_infer(ann, ctx))
                .unwrap_or_else(|| ctx.fresh_var());
            ctx.push_scope();
            for (p, pt) in params.iter().zip(param_types.iter()) {
                ctx.bind_mono(&p.name, pt.clone(), p.mutable);
            }
            let saved_ret = ctx.push_return_type(ret_ty.clone());
            let body_ty = infer_block(body, ctx, fun_generalizations)?;
            ctx.add_constraint(body_ty, ret_ty.clone(), body.span.clone());
            ctx.pop_return_type(saved_ret);
            ctx.pop_scope();
            Ok(InferType::Fun(param_types, Box::new(ret_ty)))
        }
        Expr::Match(m) => infer_match(m, ctx, fun_generalizations),
        Expr::PropagateError { expr, span } => {
            infer_propagate_error(expr, span, ctx, fun_generalizations)
        }
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
        InferType::Pointer(inner) | InferType::MutPointer(inner) => named_type_name(inner),
        InferType::Concrete(c) => primitive_type_name(c),
        _ => None,
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
        UnaryOp::Ref => Ok(InferType::Pointer(Box::new(ty))),
        UnaryOp::RefMut => {
            if let Expr::Ident(name, ident_span) = operand {
                let _ = ctx.lookup_for_write(name, ident_span)?;
            }
            Ok(InferType::MutPointer(Box::new(ty)))
        }
        UnaryOp::Deref => match ctx.solve()?.apply(&ty) {
            InferType::Pointer(inner) | InferType::MutPointer(inner) => Ok(*inner),
            other => Err(MetelError::type_error(
                TypeErrorCode::T0002,
                format!("cannot dereference non-pointer type `{other}`"),
                span,
            )),
        },
    }
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
        Expr::FieldAccess { object, .. } => root_binding_for_write(object),
        Expr::Index { object, .. } => root_binding_for_write(object),
        Expr::UnaryOp(UnaryOp::Deref, _, _) => None,
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
    // Auto-deref through *mut T: writing via a mutable pointer doesn't require
    // the pointer binding itself to be mutable — only the pointee is being written.
    let is_through_mut_ptr = matches!(&obj_ty, InferType::MutPointer(_));
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
        InferType::Pointer(inner) | InferType::MutPointer(inner) => match inner.as_ref() {
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
                    let anon_name = format!("_ImplT{}", counter);
                    *counter += 1;
                    extra_generics.push(GenericParam {
                        name: anon_name.clone(),
                        bounds: vec![*bound.clone()],
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
