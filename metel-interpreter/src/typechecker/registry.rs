use std::collections::HashMap;

use crate::ast::{
    AspectDecl, AspectMethod, Decl, GenericParam, Program, Span, TypeExpr, WhereClause,
};
use crate::typeinference::{
    EnumInfo, FieldEntry, InferContext, InferType, TypeDefinitionRegistry, TypeScheme, TypeVar,
    TypeVarGenerator, VariantInfo,
};
use crate::types::Type;

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

fn list_new_scheme(t: TypeVar) -> TypeScheme {
    TypeScheme {
        quantified_vars: vec![t],
        param_names: vec![],
        ty: InferType::Fun(
            vec![],
            Box::new(InferType::Named("List".into(), vec![InferType::Var(t)])),
        ),
    }
}

fn list_from_scheme(t: TypeVar) -> TypeScheme {
    TypeScheme {
        quantified_vars: vec![t],
        param_names: vec![],
        ty: InferType::Fun(
            vec![InferType::Array(Box::new(InferType::Var(t)))],
            Box::new(InferType::Named("List".into(), vec![InferType::Var(t)])),
        ),
    }
}

/// Derive the free-function schemes for the prelude by parsing the embedded
/// `std::core` source and reading each `native` declaration's annotated
/// signature (METEL-181). `stdlib/core.mtl` + the `NativeKey` enum are the
/// single source of truth — there is no hand-maintained scheme list to keep in
/// sync. Used by `StdPrelude::default()` so the single-program pipeline (which
/// performs no module loading) sees the same surface as the module graph path.
fn populate_schemes_from_embedded_core(
    map: &mut HashMap<String, TypeScheme>,
    gen: &mut TypeVarGenerator,
) {
    let core_path = ["std".to_string(), "core".to_string()];
    let Some(source) = crate::stdlib::lookup(&core_path) else {
        return;
    };
    let program = crate::parser::parse(source, "<embedded std::core>")
        .expect("embedded std::core must parse; it is compiled into the binary");
    for decl in &program.decls {
        let Decl::Fun(fun) = decl else { continue };
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
                p.type_ann
                    .as_ref()
                    .map(&te)
                    .expect("native declarations are fully annotated (enforced by native_fun_ty)")
            })
            .collect();
        let ret = fun.return_type.as_ref().map(&te).unwrap_or_else(InferType::unit);
        let fun_ty = InferType::Fun(params, Box::new(ret));
        let scheme = crate::typeinference::generalize(fun_ty, &Default::default());
        map.insert(fun.name.clone(), scheme);
    }
}

/// Built-in primitive type names that implement `Display` (and therefore expose
/// `to_string`). Every numeric primitive plus `boolean`, `Char`, and `String`.
/// The runtime can format all of these — see
/// `evaluator::display::value_to_display_string`.
pub(super) const DISPLAYABLE_PRIMITIVE_NAMES: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "boolean", "Char",
    "String",
];

fn register_builtin_aspect_impls(registry: &mut TypeDefinitionRegistry) {
    use crate::types::Type;
    // Iterable impls for built-in sequence types
    registry.register_aspect_impl("Range".into(), "Iterable".into(), vec![Type::I64]);
    registry.register_aspect_impl("RangeInclusive".into(), "Iterable".into(), vec![Type::I64]);
    // Full cross-product numeric From impls: every numeric type has From<T> for every other.
    let all_numeric = [
        (Type::I8, "i8"),
        (Type::I16, "i16"),
        (Type::I32, "i32"),
        (Type::I64, "i64"),
        (Type::U8, "u8"),
        (Type::U16, "u16"),
        (Type::U32, "u32"),
        (Type::U64, "u64"),
        (Type::F32, "f32"),
        (Type::F64, "f64"),
    ];
    for (target_ty, target_name) in &all_numeric {
        for (source_ty, _) in &all_numeric {
            if target_ty != source_ty {
                registry.register_aspect_impl(
                    (*target_name).to_string(),
                    "From".into(),
                    vec![source_ty.clone()],
                );
            }
        }
    }
    // Display impls for built-in types (used by to_string method dispatch).
    // Every numeric primitive is Displayable, not just i64/f64 — the runtime
    // formats all of them (see evaluator::display::value_to_display_string).
    for name in DISPLAYABLE_PRIMITIVE_NAMES {
        registry.register_aspect_impl((*name).into(), "Display".into(), vec![]);
    }
    // Char ↔ u32 (Unicode code point) conversions
    registry.register_aspect_impl("u32".into(), "From".into(), vec![Type::Char]);
    registry.register_aspect_impl("Char".into(), "From".into(), vec![Type::U32]);
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

    // Built-in List uses a synthetic span (no source file).
    let builtin_span = Span::new(0, 0, "<builtin>");

    // Builtin types and aspects (Perhaps, Result, Display, From, Iterable) are
    // declared in the embedded std::core source and registered through the same
    // machinery as user declarations (METEL-181). When the module being checked
    // IS std::core, its own decl pass below covers them; deriving again here
    // would double-register.
    let std_core_path = ["std".to_string(), "core".to_string()];
    if current_module_path != std_core_path {
        register_program_decls(
            &crate::stdlib::core_program().decls,
            &std_core_path,
            gen,
            &mut registry,
        );
    }

    // Register built-in generic struct List<T>.
    let t = gen.fresh();
    registry.register_struct_fields(
        "List".into(),
        vec![FieldEntry {
            name: "inner".into(),
            ty: InferType::Array(Box::new(InferType::Var(t))),
            span: builtin_span.clone(),
            visibility: crate::ast::Visibility::Private,
        }],
        vec!["std".into(), "core".into()],
    );
    registry.register_struct_type_params("List".into(), vec![t]);
    registry.register_struct_generic_names("List".into(), vec!["T".into()]);
    // List method schemes (all reference struct type param t).
    let list_self = || InferType::Named("List".into(), vec![InferType::Var(t)]);
    let perhaps_t = || InferType::Named("Perhaps".into(), vec![InferType::Var(t)]);
    registry.register_method_scheme(
        "List".into(),
        "push".into(),
        TypeScheme {
            quantified_vars: vec![t],
            param_names: vec![],
            ty: InferType::Fun(
                vec![list_self(), InferType::Var(t)],
                Box::new(InferType::unit()),
            ),
        },
        vec![t],
    );
    registry.register_method_receiver(
        "List".into(),
        "push".into(),
        crate::ast::ReceiverKind::RefMut,
    );
    registry.register_method_scheme(
        "List".into(),
        "pop".into(),
        TypeScheme {
            quantified_vars: vec![t],
            param_names: vec![],
            ty: InferType::Fun(vec![list_self()], Box::new(perhaps_t())),
        },
        vec![t],
    );
    registry.register_method_receiver(
        "List".into(),
        "pop".into(),
        crate::ast::ReceiverKind::RefMut,
    );
    registry.register_method_scheme(
        "List".into(),
        "len".into(),
        TypeScheme {
            quantified_vars: vec![t],
            param_names: vec![],
            ty: InferType::Fun(vec![list_self()], Box::new(InferType::int())),
        },
        vec![t],
    );
    registry.register_method_scheme(
        "List".into(),
        "get".into(),
        TypeScheme {
            quantified_vars: vec![t],
            param_names: vec![],
            ty: InferType::Fun(vec![list_self(), InferType::int()], Box::new(perhaps_t())),
        },
        vec![t],
    );
    registry.register_method_scheme(
        "List".into(),
        "as_slice".into(),
        TypeScheme {
            quantified_vars: vec![t],
            param_names: vec![],
            ty: InferType::Fun(
                vec![list_self()],
                Box::new(InferType::Array(Box::new(InferType::Var(t)))),
            ),
        },
        vec![t],
    );

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
                // Generic struct — method bodies inferred by infer_impl_method with TypeVars.
                // Only register aspect membership; skip method type registration.
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

fn register_impl_methods<'a>(
    methods: impl Iterator<Item = &'a crate::ast::FunDecl>,
    target_name: &str,
    gen: &mut TypeVarGenerator,
    registry: &mut TypeDefinitionRegistry,
) {
    for method in methods {
        let mut param_types = vec![];
        for p in &method.params {
            let pt = if p.name == "self" {
                InferType::Named(target_name.to_string(), vec![])
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
            InferType::Named(target_name.to_string(), vec![])
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
    let bool_ty = InferType::bool();

    // Free-function builtins all come from StdPrelude — no separate list needed.
    for (name, scheme) in prelude.schemes() {
        ctx.bind_poly_if_absent(name, scheme.clone());
    }

    // Methods are not free functions; they're not in StdPrelude::schemes.
    // Every Displayable primitive exposes `to_string`. The self type is the
    // concrete primitive itself (e.g. i32 → Concrete(I32)), so dispatch on a
    // sized-integer receiver resolves correctly.
    for type_name in DISPLAYABLE_PRIMITIVE_NAMES {
        let self_ty = match *type_name {
            "boolean" => bool_ty.clone(),
            "Char" => InferType::Concrete(Type::Char),
            "String" => str_ty.clone(),
            "i8" => InferType::Concrete(Type::I8),
            "i16" => InferType::Concrete(Type::I16),
            "i32" => InferType::Concrete(Type::I32),
            "i64" => InferType::Concrete(Type::I64),
            "u8" => InferType::Concrete(Type::U8),
            "u16" => InferType::Concrete(Type::U16),
            "u32" => InferType::Concrete(Type::U32),
            "u64" => InferType::Concrete(Type::U64),
            "f32" => InferType::Concrete(Type::F32),
            "f64" => InferType::Concrete(Type::F64),
            other => unreachable!("unexpected displayable primitive `{other}`"),
        };
        ctx.register_method(
            type_name.to_string(),
            "to_string".to_string(),
            InferType::Fun(vec![self_ty], Box::new(str_ty.clone())),
        );
    }
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
    // Free-function schemes are derived from the embedded std::core source
    // (single source of truth, METEL-181). Only the List<T> static constructors
    // remain hand-written, because the List type itself still lives in the type
    // registry rather than in std::core.mtl.
    populate_schemes_from_embedded_core(map, gen);
    let t = gen.fresh();
    map.insert("List::new".into(), list_new_scheme(t));
    let t = gen.fresh();
    map.insert("List::from".into(), list_from_scheme(t));
}
