use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::conversions::{
    type_expr_to_infer, type_expr_to_infer_with_assoc_ctx, type_expr_to_infer_with_generics,
    type_expr_to_infer_with_self, AssocResolveCtx,
};
use crate::ast::{
    AspectDecl, AspectMethod, Decl, GenericParam, Polarity, Program, Span, TypeExpr, WhereClause,
};
use crate::name_resolver::ModuleScope;
use crate::symbols::SymbolId;
use crate::typeinference::{
    EnumInfo, FieldEntry, InferContext, InferType, TypeDefinitionRegistry, TypeScheme, TypeVar,
    TypeVarGenerator, VariantInfo,
};

/// Collect merged aspect-name bounds per type param from inline bounds + where clause.
/// Returns one Vec<String> per param (same order as `generics`), containing all
/// required aspect names for that param (deduped).
pub(super) fn collect_type_param_bounds(
    generics: &[GenericParam],
    where_clause: Option<&WhereClause>,
) -> Vec<Vec<String>> {
    generics
        .iter()
        .map(|gp| {
            // Negative bounds (`T: !Drop`) are dropped from this positive aspect-name
            // list for now — their satisfaction checking is issue #243's job.
            let mut names: Vec<String> = gp
                .bounds
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
            if let Some(wc) = where_clause {
                for (param_name, bounds) in &wc.constraints {
                    if param_name != &gp.name {
                        continue;
                    }
                    for b in bounds.iter().filter(|b| b.polarity == Polarity::Positive) {
                        if let TypeExpr::Named(n, _) = &b.aspect {
                            if !names.contains(n) {
                                names.push(n.clone());
                            }
                        }
                    }
                }
            }
            names
        })
        .collect()
}

/// Collect **negative** aspect-name bounds per type param (RFC-0072, issue #243).
/// Mirrors `collect_type_param_bounds` but filters for `Polarity::Negative`.
pub(super) fn collect_negative_type_param_bounds(
    generics: &[GenericParam],
    where_clause: Option<&WhereClause>,
) -> Vec<Vec<String>> {
    generics
        .iter()
        .map(|gp| {
            let mut names: Vec<String> = gp
                .bounds
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
            if let Some(wc) = where_clause {
                for (param_name, bounds) in &wc.constraints {
                    if param_name != &gp.name {
                        continue;
                    }
                    for b in bounds.iter().filter(|b| b.polarity == Polarity::Negative) {
                        if let TypeExpr::Named(n, _) = &b.aspect {
                            if !names.contains(n) {
                                names.push(n.clone());
                            }
                        }
                    }
                }
            }
            names
        })
        .collect()
}

/// Synthesize a `Vec<GenericParam>` from the struct's own canonical generic names
/// merged with the impl block's own `ib.generics`. This lets the bound-collection
/// helpers work uniformly for both inline (`impl<T: Bound>`) and where-clause
/// (`impl for Type<T> where T: Bound`) forms.
pub(super) fn synth_generics_for_impl(
    struct_generic_names: &[String],
    ib_generics: &[GenericParam],
) -> Vec<GenericParam> {
    struct_generic_names
        .iter()
        .map(|n| GenericParam {
            name: n.clone(),
            bounds: ib_generics
                .iter()
                .find(|g| &g.name == n)
                .map(|g| g.bounds.clone())
                .unwrap_or_default(),
        })
        .collect()
}

/// Derive the prelude schemes by parsing the embedded `std::core` source:
/// free `native` functions by name, plus static native methods on generic
/// structs as joined-key schemes (`List::new`) quantified over the struct's
/// type params (METEL-181). `stdlib/core.mtl` + the `NativeKey` enum are the
/// single source of truth — there is no hand-maintained scheme list to keep in
/// sync. Used by `CorePrelude::default()` so the single-program pipeline (which
/// performs no module loading) sees the same surface as the module graph path.
fn populate_schemes_from_embedded_core(
    map: &mut HashMap<String, TypeScheme>,
    gen: &mut TypeVarGenerator,
) {
    let program = crate::stdlib::core_program();
    for decl in &program.decls {
        match decl {
            Decl::Fun(fun) => {
                if fun.native.is_none() {
                    continue;
                }
                // Overloaded std::core natives (the assert pair) are dispatched
                // by SymbolId via the seeded overload table, never by name.
                if super::overload::core_overload_table().contains_key(&fun.name) {
                    continue;
                }
                let generic_map: HashMap<String, TypeVar> = fun
                    .generics
                    .iter()
                    .map(|g| (g.name.clone(), gen.fresh()))
                    .collect();
                let te = |t: &TypeExpr| -> InferType {
                    if generic_map.is_empty() {
                        type_expr_to_infer(t)
                    } else {
                        type_expr_to_infer_with_generics(t, &generic_map)
                    }
                };
                let params: Vec<InferType> = fun
                    .params
                    .iter()
                    .map(|p| {
                        p.type_ann.as_ref().map(&te).expect(
                            "native declarations are fully annotated (enforced by native_fun_ty)",
                        )
                    })
                    .collect();
                let ret = fun.return_type.as_ref().map_or_else(InferType::unit, &te);
                let fun_ty = InferType::Fun(params, Box::new(ret));
                let bounds = super::inference::collect_fun_type_var_bounds(fun, &generic_map);
                let neg_bounds = super::inference::collect_negative_fun_type_var_bounds(fun, &generic_map);
                let scheme = crate::typeinference::generalize(fun_ty, &HashSet::default())
                    .with_bounds(&bounds)
                    .with_neg_bounds(&neg_bounds);
                map.insert(fun.name.clone(), scheme);
            }
            Decl::Impl(ib) => {
                // Static native methods on generic structs become joined-key
                // schemes ("List::new") quantified over the struct's params.
                let TypeExpr::Named(target_name, target_args) = &ib.target_type else {
                    continue;
                };
                let generic_map: HashMap<String, TypeVar> = target_args
                    .iter()
                    .filter_map(|te| match te {
                        TypeExpr::Named(n, args) if args.is_empty() => {
                            Some((n.clone(), gen.fresh()))
                        }
                        _ => None,
                    })
                    .collect();
                if generic_map.is_empty() {
                    continue;
                }
                for method in &ib.methods {
                    if method.native.is_none()
                        || method.params.first().is_some_and(|p| p.receiver.is_some())
                    {
                        continue;
                    }
                    let params: Vec<InferType> = method
                        .params
                        .iter()
                        .map(|p| {
                            p.type_ann
                                .as_ref()
                                .map(|ann| type_expr_to_infer_with_generics(ann, &generic_map))
                                .expect(
                                "native declarations are fully annotated (enforced by native_fun_ty)",
                            )
                        })
                        .collect();
                    let ret = method
                        .return_type
                        .as_ref()
                        .map_or_else(InferType::unit, |ann| {
                            type_expr_to_infer_with_generics(ann, &generic_map)
                        });
                    let fun_ty = InferType::Fun(params, Box::new(ret));
                    let scheme = crate::typeinference::generalize(fun_ty, &HashSet::default());
                    map.insert(format!("{target_name}::{}", method.name), scheme);
                }
            }
            _ => {}
        }
    }
}

fn register_builtin_aspect_impls(registry: &mut TypeDefinitionRegistry) {
    use crate::symbols::{SYM_TYPE_RANGE, SYM_TYPE_RANGE_INCLUSIVE};
    use crate::types::Type;
    // Iterable impls for built-in sequence types. Runtime ranges are intrinsic,
    // so these stay hand-registered; the primitive Display impls and the
    // numeric From cross-product are declared in the embedded std::core source
    // and registered through the normal impl-decl pass (METEL-181). Target
    // registered directly by id — Range/RangeInclusive are fixed builtin type ids,
    // not names needing scope resolution (ADR-0042); the aspect half stays the
    // literal name "Iterable" (see `impl_aspect_env`'s doc for why).
    registry.register_aspect_impl_by_id(SYM_TYPE_RANGE, "Iterable", vec![Type::I64]);
    registry.register_aspect_impl_by_id(SYM_TYPE_RANGE_INCLUSIVE, "Iterable", vec![Type::I64]);
}

/// Build the `TypeDefinitionRegistry` from the program's declarations and built-in types.
/// Allocates `TypeVars` from `gen`; the caller must pass the same `gen` to
/// `InferContext::new` so that all `TypeVar` IDs are globally unique.
pub(super) fn build_registry(
    program: &Program,
    gen: &mut TypeVarGenerator,
    current_module_path: &[String],
    symbols: Option<&HashMap<(Vec<String>, String), SymbolId>>,
    scopes: Option<&HashMap<Vec<String>, ModuleScope>>,
) -> TypeDefinitionRegistry {
    let mut registry = TypeDefinitionRegistry::new();
    if let (Some(symbols), Some(scopes)) = (symbols, scopes) {
        // Cloned once per module here, not per lookup — `impl_aspect_env`'s
        // resolution needs its own `Rc` handle to share cheaply as this registry
        // gets merged across modules (`merge_from`), but `ResolvedNames` itself
        // doesn't carry these as `Rc`, so the one clone happens at the boundary.
        registry.set_symbol_resolution(Rc::new(symbols.clone()), Rc::new(scopes.clone()));
    }
    register_builtin_aspect_impls(&mut registry);

    // Builtin types and aspects (Perhaps, Result, List, Display, From,
    // Iterable) are declared in the embedded std::core source and registered
    // through the same machinery as user declarations (METEL-181). When the
    // module being checked IS std::core, its own decl pass below covers them;
    // deriving again here would double-register.
    let std_core_path = ["std".to_string(), "core".to_string()];
    if current_module_path != std_core_path {
        register_program_decls(
            &crate::stdlib::core_program().decls,
            &std_core_path,
            gen,
            &mut registry,
        );
    }

    register_program_decls(&program.decls, current_module_path, gen, &mut registry);

    registry
}

/// Register a program's type-level declarations (structs, enums, aspects, impl
/// signatures) into the registry. Used both for the module being checked and
/// for the embedded `std::core` decls, which seed every module's registry.
// Exhaustive match over every AST/type-system variant; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
fn register_program_decls(
    decls: &[Decl],
    current_module_path: &[String],
    gen: &mut TypeVarGenerator,
    registry: &mut TypeDefinitionRegistry,
) {
    // Pass 1: register structs, enums, and aspects.
    for decl in decls {
        match decl {
            Decl::Struct(sd) if sd.generics.is_empty() => {
                let fields: Vec<FieldEntry> = sd
                    .fields
                    .iter()
                    .map(|f| FieldEntry {
                        name: f.name.clone(),
                        ty: type_expr_to_infer(&f.type_ann),
                        span: f.span.clone(),
                        visibility: f.visibility.clone(),
                    })
                    .collect();
                registry.register_struct_fields(
                    sd.name.clone(),
                    fields,
                    current_module_path.to_vec(),
                );
            }
            Decl::Struct(sd) => {
                let mut gen_map: HashMap<String, TypeVar> = HashMap::new();
                let mut type_params = vec![];
                for gp in &sd.generics {
                    let tv = gen.fresh();
                    gen_map.insert(gp.name.clone(), tv);
                    type_params.push(tv);
                }
                let fields: Vec<FieldEntry> = sd
                    .fields
                    .iter()
                    .map(|f| FieldEntry {
                        name: f.name.clone(),
                        ty: type_expr_to_infer_with_generics(&f.type_ann, &gen_map),
                        span: f.span.clone(),
                        visibility: f.visibility.clone(),
                    })
                    .collect();
                registry.register_struct_fields(
                    sd.name.clone(),
                    fields,
                    current_module_path.to_vec(),
                );
                registry.register_struct_type_params(sd.name.clone(), type_params);
                registry.register_struct_generic_names(
                    sd.name.clone(),
                    sd.generics.iter().map(|g| g.name.clone()).collect(),
                );
                let bounds = collect_type_param_bounds(&sd.generics, sd.where_clause.as_ref());
                if bounds.iter().any(|b| !b.is_empty()) {
                    registry.register_type_param_bounds(sd.name.clone(), bounds);
                }
                let neg_bounds = collect_negative_type_param_bounds(&sd.generics, sd.where_clause.as_ref());
                if neg_bounds.iter().any(|b| !b.is_empty()) {
                    registry.register_neg_type_param_bounds(sd.name.clone(), neg_bounds);
                }
            }
            Decl::Enum(ed) => {
                let mut gen_map: HashMap<String, TypeVar> = HashMap::new();
                let mut type_params = vec![];
                for gp in &ed.generics {
                    let tv = gen.fresh();
                    gen_map.insert(gp.name.clone(), tv);
                    type_params.push(tv);
                }
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
                                ty: type_expr_to_infer_with_generics(&f.type_ann, &gen_map),
                                span: f.span.clone(),
                                visibility: f.visibility.clone(),
                            })
                            .collect(),
                    })
                    .collect();
                registry.register_struct_generic_names(
                    ed.name.clone(),
                    ed.generics.iter().map(|g| g.name.clone()).collect(),
                );
                let bounds = collect_type_param_bounds(&ed.generics, ed.where_clause.as_ref());
                registry.register_enum(
                    ed.name.clone(),
                    EnumInfo {
                        type_params,
                        variants,
                    },
                    current_module_path.to_vec(),
                );
                if bounds.iter().any(|b| !b.is_empty()) {
                    registry.register_type_param_bounds(ed.name.clone(), bounds);
                }
                let neg_bounds = collect_negative_type_param_bounds(&ed.generics, ed.where_clause.as_ref());
                if neg_bounds.iter().any(|b| !b.is_empty()) {
                    registry.register_neg_type_param_bounds(ed.name.clone(), neg_bounds);
                }
            }
            Decl::Aspect(ad) => {
                register_aspect_decl(ad, current_module_path, registry);
            }
            _ => {}
        }
    }

    // Pass 2: register impl method signatures once all aspect definitions are known.
    // Methods on generic structs (where the target type has registered type params) are
    // skipped here — they contain T-typed params that need TypeVars, not Named("T",[]).
    // infer_impl_method in inference.rs registers them correctly as polymorphic schemes.
    for decl in decls {
        if let Decl::Impl(ib) = decl {
            let target_name = match &ib.target_type {
                TypeExpr::Named(name, _) => name.clone(),
                _ => continue,
            };
            // A generic struct OR generic enum (both register non-empty generic
            // names). Their Metel method bodies are inferred by infer_impl_method
            // with TypeVars as polymorphic schemes, so skip the concrete
            // registration here — registering a concrete `(Enum, T) -> T` entry
            // would shadow the scheme and make the method-level `T` a dangling
            // Named("T"). NATIVE methods have no body and are never inferred, so
            // their annotated signatures are registered as polymorphic schemes
            // over the type's params (List<T> in std::core).
            // Also deferred whenever the impl block itself declares generics
            // (RFC-0036/RFC-0061, issue #233) — `target_name` may not even name a
            // real struct/enum in that case (a bare type-parameter blanket target,
            // or — skipped above via `continue` — a structural target), so
            // registering concrete method schemes against it here would be wrong.
            let is_generic_target = !ib.generics.is_empty()
                || registry
                    .struct_generic_names_for(target_name.as_str())
                    .is_some_and(|names| !names.is_empty());
            if is_generic_target {
                register_generic_impl_method_schemes(ib, &target_name, gen, registry);
            } else {
                register_impl_methods(ib.methods.iter(), &target_name, gen, registry);
                // A negative impl (RFC-0081, `impl !Aspect for Type {}`, issue #264)
                // carries no methods of its own (enforced empty by the parser) and
                // must not inherit the aspect's default-bodied methods either — it's
                // a declaration of non-implementation, not a real impl missing some
                // overrides. Without this guard the registry would believe the type
                // has the aspect's default methods callable, exactly backwards from
                // what `impl !Aspect` means.
                if ib.polarity == Polarity::Positive {
                    register_default_aspect_methods(
                        ib,
                        &target_name,
                        gen,
                        registry,
                        current_module_path,
                    );
                }
            }
            // Track which aspects this type implements (with concrete type args).
            // TODO(generic-impl): `impl<T>` syntax now exists (issue #233), but this
            // conversion still isn't generic-param-aware: type args that are generic
            // params arrive as Named("T",[]) here and are stored verbatim, causing
            // has_from_impl / iterable_elem_type lookups to fail for a conditional
            // impl's own type params. `is_generic_target` above already keeps this
            // whole block from running for those impls, so the immediate crash risk
            // is gone, but a correct conversion (wildcard sentinel or a separate
            // generic-impl registry) is still issue #241/#245's job, not this one's.
            //
            // Negative impls (RFC-0081, `impl !Aspect for Type {}`) must not reach
            // this registration at all — `ib.polarity == Negative` means the type
            // definitively does NOT implement the aspect; registering it here would
            // make positive-bound checks silently and wrongly succeed. Orphan rule,
            // finality (conflict with a concrete positive impl), and not inheriting
            // the aspect's default-bodied methods are all handled now (issue #264).
            // Still deferred: actually taking priority over a *blanket* positive
            // impl, and being consulted by `T: !Aspect` bound satisfaction — both
            // need RFC-0036/RFC-0072 (issues #241/#243) to have real semantics
            // first; there's nothing to override or consult yet.
            if ib.polarity == Polarity::Positive {
                if let Some(aspect_name) = &ib.aspect_name {
                    let type_args: Vec<crate::types::Type> = ib
                        .aspect_type_args
                        .iter()
                        .filter_map(|te| {
                            use super::conversions::type_expr_to_infer;
                            match type_expr_to_infer(te) {
                                InferType::Concrete(t) => Some(t),
                                InferType::Named(n, _) => {
                                    Some(crate::types::Type::Named(n, vec![]))
                                }
                                _ => None,
                            }
                        })
                        .collect();
                    // RFC-0036 §2.2/§3.1: when `is_generic_target` and the impl
                    // carries conditional bounds, register into
                    // `conditional_impl_bounds` INSTEAD OF the unconditional
                    // `impl_aspect_env` — this fixes the confirmed bug where a
                    // conditional impl was silently marking the aspect as
                    // unconditionally implemented.
                    //
                    if is_generic_target {
                        let generic_names = registry
                            .struct_generic_names_for(target_name.as_str())
                            .cloned()
                            .unwrap_or_default();
                        let synth = synth_generics_for_impl(&generic_names, &ib.generics);
                        let pos_bounds = collect_type_param_bounds(&synth, ib.where_clause.as_ref());
                        let neg_bounds = collect_negative_type_param_bounds(&synth, ib.where_clause.as_ref());
                        if pos_bounds.iter().any(|b| !b.is_empty())
                            || neg_bounds.iter().any(|b| !b.is_empty())
                        {
                            registry.register_conditional_impl_bounds(
                                current_module_path,
                                &target_name,
                                aspect_name,
                                pos_bounds,
                                neg_bounds,
                            );
                        } else {
                            // Unconditional generic impl (no conditional bounds)
                            // — register normally so `aspect_satisfied_by` fallback
                            // works.
                            registry.register_aspect_impl(
                                current_module_path,
                                &target_name,
                                aspect_name,
                                type_args,
                            );
                        }
                    } else {
                        // Non-generic target: unconditional impl as before.
                        registry.register_aspect_impl(
                            current_module_path,
                            &target_name,
                            aspect_name,
                            type_args,
                        );
                    }
                    // RFC-0082 §2: register concrete associated-type bindings for
                    // non-generic impls. Generic impls are deferred to #241.
                    if !is_generic_target && !ib.assoc_type_defs.is_empty() {
                        let mut bindings = HashMap::new();
                        for def in &ib.assoc_type_defs {
                            let infer_ty = super::conversions::type_expr_to_infer_with_self(
                                &def.ty,
                                &target_name,
                            );
                            let dummy = Span::new(0, 0, "");
                            if let Ok(concrete_ty) =
                                super::conversions::infer_type_to_type(&infer_ty, &dummy)
                            {
                                bindings.insert(def.name.clone(), concrete_ty);
                            }
                        }
                        if !bindings.is_empty() {
                            registry.register_impl_assoc_types(
                                current_module_path,
                                &target_name,
                                aspect_name,
                                bindings,
                            );
                        }
                    }
                }
            } else if ib.polarity == Polarity::Negative {
                if let Some(aspect_name) = &ib.aspect_name {
                    if !ib.generics.is_empty() {
                        // RFC-0081's primary use case is a blanket generic negative
                        // impl such as `impl<T> !Send for Rc<T> {}`. Reuse the same
                        // per-parameter bound bookkeeping as positive conditional
                        // impls, but route it into the negative table so matching
                        // instantiations are treated as explicitly *not*
                        // implementing the aspect.
                        let generic_names = registry
                            .struct_generic_names_for(target_name.as_str())
                            .cloned()
                            .unwrap_or_default();
                        let synth = synth_generics_for_impl(&generic_names, &ib.generics);
                        let pos_bounds = collect_type_param_bounds(&synth, ib.where_clause.as_ref());
                        let neg_bounds =
                            collect_negative_type_param_bounds(&synth, ib.where_clause.as_ref());
                        registry.register_neg_conditional_impl_bounds(
                            current_module_path,
                            &target_name,
                            aspect_name,
                            pos_bounds,
                            neg_bounds,
                        );
                    } else if let TypeExpr::Named(_, target_type_args) = &ib.target_type {
                        let concrete_target_args: Vec<crate::types::Type> = target_type_args
                            .iter()
                            .filter_map(|te| match type_expr_to_infer(te) {
                                InferType::Concrete(t) => Some(t),
                                InferType::Named(n, _) => {
                                    Some(crate::types::Type::Named(n, vec![]))
                                }
                                _ => None,
                            })
                            .collect();
                        registry.register_neg_impl(
                            current_module_path,
                            &target_name,
                            aspect_name,
                            concrete_target_args,
                        );
                    }
                }
            }
        }
    }
}

fn register_aspect_decl(
    ad: &AspectDecl,
    declaring_module: &[String],
    registry: &mut TypeDefinitionRegistry,
) {
    let method_names = ad.methods.iter().map(|m| m.name.clone()).collect();
    registry.register_aspect(ad.name.clone(), method_names);
    registry.register_aspect_method_defs(ad.name.clone(), ad.methods.clone());
    registry.register_aspect_declaring_module(ad.name.clone(), declaring_module.to_vec());
    registry.register_aspect_assoc_types(ad.name.clone(), ad.assoc_types.clone());
}

/// Register the annotated signatures of NATIVE methods in an impl block on a
/// generic struct (e.g. `impl List<T>` in `std::core`) as polymorphic schemes
/// over the struct's registered type params. Metel-bodied methods are handled
/// by `infer_impl_method` instead; static native methods (no receiver) are
/// exposed as joined-key prelude schemes (`List::new`), not method schemes.
/// Register polymorphic method schemes for instance methods on a generic struct
/// or enum, derived from their (fully required) parameter/return annotations.
///
/// This covers both native methods (which have no body to infer) and Metel-bodied
/// methods. Deriving the scheme from annotations is what lets the single-program
/// path (`check_with_ctx`, no module loading) resolve `std::core`'s bodied generic
/// methods like `Perhaps::map` / `List::filter` — there is no separate `std::core`
/// module check there to run `infer_impl_method`. In the graph path the inferred
/// scheme later overwrites this one for `std::core`'s own module; downstream modules
/// use this annotation-derived scheme directly. Static methods (no receiver) are
/// handled as joined-key schemes elsewhere and skipped here.
fn register_generic_impl_method_schemes(
    ib: &crate::ast::ImplBlock,
    target_name: &str,
    gen: &mut TypeVarGenerator,
    registry: &mut TypeDefinitionRegistry,
) {
    // Type params for the generic target — a struct or an enum.
    let type_params: Vec<TypeVar> =
        if let Some(tps) = registry.raw_struct_type_params().get(target_name).cloned() {
            tps
        } else if let Some(info) = registry.enum_info(target_name) {
            info.type_params.clone()
        } else {
            return;
        };
    if type_params.is_empty() {
        return;
    }
    let Some(generic_names) = registry.struct_generic_names_for(target_name).cloned() else {
        return;
    };
    let type_gen_map: HashMap<String, TypeVar> = generic_names
        .iter()
        .cloned()
        .zip(type_params.iter().copied())
        .collect();
    // RFC-0036: compute impl-level bounds from the impl block's generics + where clause.
    let synth = synth_generics_for_impl(&generic_names, &ib.generics);
    let impl_bounds = collect_type_param_bounds(&synth, ib.where_clause.as_ref());
    let impl_neg_bounds = collect_negative_type_param_bounds(&synth, ib.where_clause.as_ref());
    let by_var: HashMap<TypeVar, Vec<String>> = type_params
        .iter()
        .zip(impl_bounds.iter())
        .filter(|(_, b)| !b.is_empty())
        .map(|(&tv, b)| (tv, b.clone()))
        .collect();
    let by_neg_var: HashMap<TypeVar, Vec<String>> = type_params
        .iter()
        .zip(impl_neg_bounds.iter())
        .filter(|(_, b)| !b.is_empty())
        .map(|(&tv, b)| (tv, b.clone()))
        .collect();
    let self_ty = InferType::Named(
        target_name.to_string(),
        type_params.iter().map(|tv| InferType::Var(*tv)).collect(),
    );
    for method in &ib.methods {
        // Only instance methods (those with a receiver) dispatch through the
        // method scheme env; static methods become joined-key schemes elsewhere.
        let Some(receiver) = method.params.first().and_then(|p| p.receiver.clone()) else {
            continue;
        };
        // Method-level generics (e.g. `U` in `fun map<U>`) get their own fresh
        // quantified vars in addition to the type's params.
        let mut gen_map = type_gen_map.clone();
        let mut quantified = type_params.clone();
        for g in &method.generics {
            let tv = gen.fresh();
            gen_map.insert(g.name.clone(), tv);
            quantified.push(tv);
        }
        let mut param_types = vec![self_ty.clone()];
        for p in method.params.iter().filter(|p| p.receiver.is_none()) {
            let ann = p
                .type_ann
                .as_ref()
                .expect("declarations on generic types are fully annotated");
            param_types.push(type_expr_to_infer_with_generics(ann, &gen_map));
        }
        let ret_ty = method
            .return_type
            .as_ref()
            .map_or_else(InferType::unit, |ann| {
                type_expr_to_infer_with_generics(ann, &gen_map)
            });
        let scheme = TypeScheme {
                quantified_vars: quantified,
                param_names: vec![],
                bounds: vec![],
                neg_bounds: vec![],
                assoc_projections: vec![],
                assoc_eq_constraints: vec![],
                opaque_returns: vec![],
                ty: InferType::Fun(param_types, Box::new(ret_ty)),
            }
            .with_bounds(&by_var)
            .with_neg_bounds(&by_neg_var);
        // struct_tvars: only the type's params are pinned from the receiver;
        // method-level generics are recovered from the arguments at the call site.
        let struct_tvars = type_params.clone();
        registry.register_method_scheme(
            target_name.to_string(),
            method.name.clone(),
            scheme.clone(),
            struct_tvars.clone(),
        );
        registry.register_method_scheme_variant(
            target_name.to_string(),
            method.name.clone(),
            scheme,
            struct_tvars,
        );
        registry.register_method_receiver(target_name.to_string(), method.name.clone(), receiver);
    }
}

fn register_impl_methods<'a>(
    methods: impl Iterator<Item = &'a crate::ast::FunDecl>,
    target_name: &str,
    gen: &mut TypeVarGenerator,
    registry: &mut TypeDefinitionRegistry,
) {
    // `self` on a primitive target must be the concrete primitive type
    // (e.g. Concrete(I32), not Named("i32")) so call sites unify (METEL-181).
    let self_ty = || {
        super::inference::primitive_type_from_name(target_name).map_or_else(
            || InferType::Named(target_name.to_string(), vec![]),
            InferType::Concrete,
        )
    };
    for method in methods {
        let mut param_types = vec![];
        for p in &method.params {
            let pt = if p.name == "self" {
                self_ty()
            } else if let Some(ann) = &p.type_ann {
                type_expr_to_infer_with_self(ann, target_name)
            } else {
                InferType::Var(gen.fresh())
            };
            param_types.push(pt);
        }
        let ret_ty = method
            .return_type
            .as_ref()
            .map_or_else(InferType::unit, |ann| {
                type_expr_to_infer_with_self(ann, target_name)
            });
        registry.register_method(
            target_name.to_string(),
            method.name.clone(),
            InferType::Fun(param_types, Box::new(ret_ty)),
        );
        if let Some(receiver) = method.params.first().and_then(|p| p.receiver.clone()) {
            registry.register_method_receiver(
                target_name.to_string(),
                method.name.clone(),
                receiver,
            );
        }
    }
}

fn register_default_aspect_methods(
    ib: &crate::ast::ImplBlock,
    target_name: &str,
    gen: &mut TypeVarGenerator,
    registry: &mut TypeDefinitionRegistry,
    current_module_path: &[String],
) {
    let Some(aspect_name) = &ib.aspect_name else {
        return;
    };
    let Some(methods) = registry.aspect_method_defs(aspect_name).cloned() else {
        return;
    };
    let provided: std::collections::HashSet<&str> =
        ib.methods.iter().map(|m| m.name.as_str()).collect();

    for method in methods {
        if method.default_body.is_none() || provided.contains(method.name.as_str()) {
            continue;
        }
        register_default_aspect_method(
            &method,
            target_name,
            aspect_name,
            gen,
            registry,
            current_module_path,
        );
    }
}

fn register_default_aspect_method(
    method: &AspectMethod,
    target_name: &str,
    aspect_name: &str,
    gen: &mut TypeVarGenerator,
    registry: &mut TypeDefinitionRegistry,
    current_module_path: &[String],
) {
    // RFC-0082 §1.2: bare associated-type names inside the aspect's own method
    // signatures (e.g. `Item` in `fun get_twice(self) -> Item { ... }`, sugar for
    // `Self::Item`) must resolve to the concrete binding this specific impl gave
    // for `Item`, not fall through to a dangling `Named("Item", [])`.
    let assoc_ctx = AssocResolveCtx {
        registry,
        current_module: current_module_path,
        current_aspect: Some(aspect_name),
    };
    let empty_generics = std::collections::HashMap::new();
    let mut param_types = vec![];
    for p in &method.params {
        let pt = if p.name == "self" {
            super::inference::primitive_type_from_name(target_name).map_or_else(
                || InferType::Named(target_name.to_string(), vec![]),
                InferType::Concrete,
            )
        } else if let Some(ann) = &p.type_ann {
            type_expr_to_infer_with_assoc_ctx(
                ann,
                &empty_generics,
                Some(target_name),
                &assoc_ctx,
            )
        } else {
            InferType::Var(gen.fresh())
        };
        param_types.push(pt);
    }
    let ret_ty = method
        .return_type
        .as_ref()
        .map_or_else(InferType::unit, |ann| {
            type_expr_to_infer_with_assoc_ctx(ann, &empty_generics, Some(target_name), &assoc_ctx)
        });
    registry.register_method(
        target_name.to_string(),
        method.name.clone(),
        InferType::Fun(param_types, Box::new(ret_ty)),
    );
    if let Some(receiver) = method.params.first().and_then(|p| p.receiver.clone()) {
        registry.register_method_receiver(target_name.to_string(), method.name.clone(), receiver);
    }
}

/// Seed `ctx` with all built-in free-function bindings from `CorePrelude`,
/// plus built-in method registrations and aspect declarations.
pub(super) fn register_primitive_type_bindings(
    ctx: &mut InferContext,
    prelude: &super::CorePrelude,
) {
    // Free-function builtins all come from CorePrelude — no separate list needed.
    for (name, scheme) in prelude.schemes() {
        ctx.bind_poly_if_absent(name, scheme.clone());
    }

    // The primitive Display/From impls (to_string, the numeric From
    // cross-product, Char ↔ u32) are declared in the embedded std::core source
    // and registered by build_registry's impl-decl pass (METEL-181).
    // String::len is declared in the embedded std::core source (`impl String`)
    // and registered by build_registry's impl-decl pass.
    // T[]::len — handled as a special case in the typechecker; no TypeVar needed here.

    // The core aspects (Display/Iterable/From) are declared in the embedded
    // std::core source and registered by build_registry's decl pass (METEL-181).
}

/// Add all built-in function schemes from `CorePrelude` to `scheme_env`.
/// Used by the construction pass so builtin names are known during typed-AST building.
pub(super) fn register_builtin_schemes(
    scheme_env: &mut HashMap<String, TypeScheme>,
    prelude: &super::CorePrelude,
) {
    for (name, scheme) in prelude.schemes() {
        scheme_env
            .entry(name.clone())
            .or_insert_with(|| scheme.clone());
    }
}

/// Populate `map` with all built-in function schemes.
/// Called by `CorePrelude::default()` — this is the single canonical list.
pub(super) fn populate_std_schemes(
    map: &mut HashMap<String, TypeScheme>,
    gen: &mut TypeVarGenerator,
) {
    // All schemes — free functions and the List<T> static constructors — are
    // derived from the embedded std::core source (single source of truth,
    // METEL-181).
    populate_schemes_from_embedded_core(map, gen);
}
