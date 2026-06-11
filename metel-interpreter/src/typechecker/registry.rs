use std::collections::HashMap;

use crate::ast::{
    AspectDecl, AspectMethod, Decl, GenericParam, Program, TypeExpr, WhereClause,
};
use crate::typeinference::{
    EnumInfo, FieldEntry, InferContext, InferType, TypeDefinitionRegistry, TypeScheme, TypeVar,
    TypeVarGenerator, VariantInfo,
};
use super::conversions::{
    type_expr_to_infer, type_expr_to_infer_with_generics, type_expr_to_infer_with_self,
};

/// Collect merged aspect-name bounds per type param from inline bounds + where clause.
/// Returns one Vec<String> per param (same order as `generics`), containing all
/// required aspect names for that param (deduped).
fn collect_type_param_bounds(
    generics: &[GenericParam],
    where_clause: Option<&WhereClause>,
) -> Vec<Vec<String>> {
    generics
        .iter()
        .map(|gp| {
            let mut names: Vec<String> = gp
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
            if let Some(wc) = where_clause {
                for (param_name, bounds) in &wc.constraints {
                    if param_name != &gp.name {
                        continue;
                    }
                    for b in bounds {
                        if let TypeExpr::Named(n, _) = b {
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

/// Derive the prelude schemes by parsing the embedded `std::core` source:
/// free `native` functions by name, plus static native methods on generic
/// structs as joined-key schemes (`List::new`) quantified over the struct's
/// type params (METEL-181). `stdlib/core.mtl` + the `NativeKey` enum are the
/// single source of truth — there is no hand-maintained scheme list to keep in
/// sync. Used by `StdPrelude::default()` so the single-program pipeline (which
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
                let ret = fun
                    .return_type
                    .as_ref()
                    .map(&te)
                    .unwrap_or_else(InferType::unit);
                let fun_ty = InferType::Fun(params, Box::new(ret));
                let scheme = crate::typeinference::generalize(fun_ty, &Default::default());
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
                        .map(|ann| type_expr_to_infer_with_generics(ann, &generic_map))
                        .unwrap_or_else(InferType::unit);
                    let fun_ty = InferType::Fun(params, Box::new(ret));
                    let scheme = crate::typeinference::generalize(fun_ty, &Default::default());
                    map.insert(format!("{target_name}::{}", method.name), scheme);
                }
            }
            _ => {}
        }
    }
}

fn register_builtin_aspect_impls(registry: &mut TypeDefinitionRegistry) {
    use crate::types::Type;
    // Iterable impls for built-in sequence types. Runtime ranges are intrinsic,
    // so these stay hand-registered; the primitive Display impls and the
    // numeric From cross-product are declared in the embedded std::core source
    // and registered through the normal impl-decl pass (METEL-181).
    registry.register_aspect_impl("Range".into(), "Iterable".into(), vec![Type::I64]);
    registry.register_aspect_impl("RangeInclusive".into(), "Iterable".into(), vec![Type::I64]);
}

/// Build the `TypeDefinitionRegistry` from the program's declarations and built-in types.
/// Allocates TypeVars from `gen`; the caller must pass the same `gen` to
/// `InferContext::new` so that all TypeVar IDs are globally unique.
pub(super) fn build_registry(
    program: &Program,
    gen: &mut TypeVarGenerator,
    current_module_path: &[String],
) -> TypeDefinitionRegistry {
    let mut registry = TypeDefinitionRegistry::new();
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
/// for the embedded std::core decls, which seed every module's registry.
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
            if registry
                .raw_struct_type_params()
                .contains_key(target_name.as_str())
            {
                // Generic struct — Metel method bodies are inferred by
                // infer_impl_method with TypeVars, so skip them here. NATIVE
                // methods have no body and are never inferred, so their
                // annotated signatures are registered as polymorphic schemes
                // over the struct's type params (List<T> in std::core).
                register_generic_native_impl_methods(ib, &target_name, registry);
            } else {
                register_impl_methods(ib.methods.iter(), &target_name, gen, registry);
                register_default_aspect_methods(ib, &target_name, gen, registry);
            }
            // Track which aspects this type implements (with concrete type args).
            // TODO(generic-impl): Once impl<T> syntax is added, type args that are generic
            // params will arrive as Named("T",[]) here and be stored verbatim, causing
            // has_from_impl / iterable_elem_type lookups to fail. At that point this
            // conversion must be made generic-param-aware (e.g. wildcard sentinel or
            // a separate generic-impl registry).
            if let Some(aspect_name) = &ib.aspect_name {
                let type_args: Vec<crate::types::Type> = ib
                    .aspect_type_args
                    .iter()
                    .filter_map(|te| {
                        use super::conversions::type_expr_to_infer;
                        match type_expr_to_infer(te) {
                            InferType::Concrete(t) => Some(t),
                            InferType::Named(n, _) => Some(crate::types::Type::Named(n, vec![])),
                            _ => None,
                        }
                    })
                    .collect();
                registry.register_aspect_impl(target_name.clone(), aspect_name.clone(), type_args);
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
}

/// Register the annotated signatures of NATIVE methods in an impl block on a
/// generic struct (e.g. `impl List<T>` in std::core) as polymorphic schemes
/// over the struct's registered type params. Metel-bodied methods are handled
/// by infer_impl_method instead; static native methods (no receiver) are
/// exposed as joined-key prelude schemes (`List::new`), not method schemes.
fn register_generic_native_impl_methods(
    ib: &crate::ast::ImplBlock,
    target_name: &str,
    registry: &mut TypeDefinitionRegistry,
) {
    if !ib.methods.iter().any(|m| m.native.is_some()) {
        return;
    }
    let Some(type_params) = registry.raw_struct_type_params().get(target_name).cloned() else {
        return;
    };
    let Some(generic_names) = registry.struct_generic_names_for(target_name).cloned() else {
        return;
    };
    let gen_map: HashMap<String, TypeVar> = generic_names
        .iter()
        .cloned()
        .zip(type_params.iter().copied())
        .collect();
    let self_ty = InferType::Named(
        target_name.to_string(),
        type_params.iter().map(|tv| InferType::Var(*tv)).collect(),
    );
    for method in &ib.methods {
        if method.native.is_none() {
            continue;
        }
        let Some(receiver) = method.params.first().and_then(|p| p.receiver.clone()) else {
            continue;
        };
        let mut param_types = vec![self_ty.clone()];
        for p in method.params.iter().filter(|p| p.receiver.is_none()) {
            let ann = p
                .type_ann
                .as_ref()
                .expect("native declarations are fully annotated (enforced by native_fun_ty)");
            param_types.push(type_expr_to_infer_with_generics(ann, &gen_map));
        }
        let ret_ty = method
            .return_type
            .as_ref()
            .map(|ann| type_expr_to_infer_with_generics(ann, &gen_map))
            .unwrap_or_else(InferType::unit);
        registry.register_method_scheme(
            target_name.to_string(),
            method.name.clone(),
            TypeScheme {
                quantified_vars: type_params.clone(),
                param_names: vec![],
                ty: InferType::Fun(param_types, Box::new(ret_ty)),
            },
            type_params.clone(),
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
        super::inference::primitive_type_from_name(target_name)
            .map(InferType::Concrete)
            .unwrap_or_else(|| InferType::Named(target_name.to_string(), vec![]))
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
            .map(|ann| type_expr_to_infer_with_self(ann, target_name))
            .unwrap_or_else(InferType::unit);
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
        register_default_aspect_method(&method, target_name, gen, registry);
    }
}

fn register_default_aspect_method(
    method: &AspectMethod,
    target_name: &str,
    gen: &mut TypeVarGenerator,
    registry: &mut TypeDefinitionRegistry,
) {
    let mut param_types = vec![];
    for p in &method.params {
        let pt = if p.name == "self" {
            super::inference::primitive_type_from_name(target_name)
                .map(InferType::Concrete)
                .unwrap_or_else(|| InferType::Named(target_name.to_string(), vec![]))
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
        .map(|ann| type_expr_to_infer_with_self(ann, target_name))
        .unwrap_or_else(InferType::unit);
    registry.register_method(
        target_name.to_string(),
        method.name.clone(),
        InferType::Fun(param_types, Box::new(ret_ty)),
    );
    if let Some(receiver) = method.params.first().and_then(|p| p.receiver.clone()) {
        registry.register_method_receiver(target_name.to_string(), method.name.clone(), receiver);
    }
}

/// Seed `ctx` with all built-in free-function bindings from `StdPrelude`,
/// plus built-in method registrations and aspect declarations.
pub(super) fn register_primitive_type_bindings(
    ctx: &mut InferContext,
    prelude: &super::StdPrelude,
) {
    let str_ty = InferType::str();
    let int_ty = InferType::int();

    // Free-function builtins all come from StdPrelude — no separate list needed.
    for (name, scheme) in prelude.schemes() {
        ctx.bind_poly_if_absent(name, scheme.clone());
    }

    // The primitive Display/From impls (to_string, the numeric From
    // cross-product, Char ↔ u32) are declared in the embedded std::core source
    // and registered by build_registry's impl-decl pass (METEL-181).
    ctx.register_method(
        "String".to_string(),
        "len".to_string(),
        InferType::Fun(vec![str_ty.clone()], Box::new(int_ty.clone())),
    );
    // T[]::len — handled as a special case in the typechecker; no TypeVar needed here.

    // The core aspects (Display/Iterable/From) are declared in the embedded
    // std::core source and registered by build_registry's decl pass (METEL-181).
}

/// Add all built-in function schemes from `StdPrelude` to `scheme_env`.
/// Used by the construction pass so builtin names are known during typed-AST building.
pub(super) fn register_builtin_schemes(
    scheme_env: &mut HashMap<String, TypeScheme>,
    prelude: &super::StdPrelude,
) {
    for (name, scheme) in prelude.schemes() {
        scheme_env
            .entry(name.clone())
            .or_insert_with(|| scheme.clone());
    }
}

/// Populate `map` with all built-in function schemes.
/// Called by `StdPrelude::default()` — this is the single canonical list.
pub(super) fn populate_std_schemes(
    map: &mut HashMap<String, TypeScheme>,
    gen: &mut TypeVarGenerator,
) {
    // All schemes — free functions and the List<T> static constructors — are
    // derived from the embedded std::core source (single source of truth,
    // METEL-181).
    populate_schemes_from_embedded_core(map, gen);
}
