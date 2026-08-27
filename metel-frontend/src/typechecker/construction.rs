use std::collections::HashMap;

use crate::ast::{
    AspectMethod, AssignTarget, BinOp, Block, Decl, Expr, ForInit, FunDecl, ImplBlock, Literal,
    MatchExpr, Param, Pattern, Program, Span, Stmt, TypeExpr, UnaryOp,
};
use crate::error::{MetelError, TypeErrorCode};
use crate::symbols::SymbolId;
use crate::typed_ast::{
    FunBody, MethodDispatch, TypedAspectDecl, TypedBlock, TypedBreakExpr, TypedDecl, TypedEnumDecl,
    TypedExpr, TypedForInStmt, TypedForInit, TypedForStmt, TypedFunDecl, TypedImplBlock,
    TypedLetDecl, TypedMatchArm, TypedMatchExpr, TypedMutDecl, TypedPlace, TypedProgram,
    TypedReturnExpr, TypedStmt, TypedStructDecl, TypedWhileStmt,
};
use crate::typeinference::{
    self, unify, EnumInfo, GenericBound, InferType, RowConstraint, Substitution,
    TypeDefinitionRegistry, TypeScheme, TypeVar, TypeVarGenerator, VariantInfo,
};
use crate::types::Type;

use super::conversions::{
    infer_type_to_type, resolved_to_type, type_expr_to_infer_with_assoc_ctx, type_to_infer,
    AssocResolveCtx,
};
use super::SchemeEnv;

type ConcreteFields = Vec<(String, Type, Span)>;
type ConcreteStructEnv = HashMap<String, ConcreteFields>;
/// metel-core#736 / RFC-0138: a generic `FunDecl`'s own shape, keyed by name in
/// `ConstructCtx::fn_table`. See that field's doc comment.
type FnDeclShape = (Vec<Param>, Option<TypeExpr>, Block);

/// Build the concrete (fully-resolved `Type`) struct field map from inference results.
/// Generic structs are excluded — they are resolved per-use-site during construction.
pub(super) fn build_concrete_struct_env(
    registry: &TypeDefinitionRegistry,
    subst: &Substitution,
) -> Result<ConcreteStructEnv, MetelError> {
    registry
        .raw_struct_env()
        .iter()
        .filter(|(name, _)| {
            !registry
                .raw_struct_type_params()
                .contains_key(name.as_str())
        })
        .map(|(name, fields)| {
            let concrete = fields
                .iter()
                .map(|field| {
                    Ok((
                        field.name.clone(),
                        infer_type_to_type(&subst.apply(&field.ty), &field.span)?,
                        field.span.clone(),
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((name.clone(), concrete))
        })
        .collect()
}

/// Build the concrete method type map from inference results.
/// Methods that still have free `TypeVars` (generic struct params) are skipped here;
/// they are resolved at the call site via `method_scheme_env`.
pub(super) fn build_concrete_method_env(
    registry: &TypeDefinitionRegistry,
    subst: &Substitution,
) -> Result<HashMap<String, HashMap<String, Type>>, MetelError> {
    let dummy = Span::new(0, 0, "");
    registry
        .raw_method_env()
        .iter()
        .map(|(type_name, methods)| {
            let concrete: HashMap<_, _> = methods
                .iter()
                .filter_map(|(mname, mty)| {
                    let resolved = subst.apply(mty);
                    // Skip methods that still have unresolved TypeVars — they belong in scheme env.
                    if !typeinference::free_vars(&resolved).is_empty() {
                        return None;
                    }
                    infer_type_to_type(&resolved, &dummy)
                        .ok()
                        .map(|t| (mname.clone(), t))
                })
                .collect();
            Ok((type_name.clone(), concrete))
        })
        .collect()
}

/// Scope-aware context for Pass 2. Mirrors `InferContext`'s scope management but
/// holds concrete `Type` values; no constraint emission.
struct ConstructCtx<'a> {
    subst: &'a Substitution,
    scheme_env: &'a SchemeEnv,
    env: Vec<HashMap<String, Type>>,
    /// Stack of concrete struct field maps (name → fields with spans), innermost last.
    struct_scopes: Vec<ConcreteStructEnv>,
    /// Unified registry — source of truth for type definitions across all passes. See ADR-0025.
    registry: &'a TypeDefinitionRegistry,
    method_env: HashMap<String, HashMap<String, Type>>,
    /// Shared generator continued from Pass 1; keeps `TypeVar` identities globally unique.
    gen: TypeVarGenerator,
    /// Return type of the innermost enclosing function (None = unit / unknown).
    current_return_ty: Option<Type>,
    /// Break value type of the innermost enclosing `loop` (None = no loop or bare break).
    current_break_ty: Option<Type>,
    loop_depth: usize,
    /// Generic type param name → fresh `TypeVar`; populated during construction-at-call-time
    /// so type annotations like `T[]` in a generic body resolve to concrete types.
    generic_params: HashMap<String, TypeVar>,
    /// Symbol intern table from the name resolver; used to populate `TypedImplBlock::aspect_id`.
    /// `None` for single-module pipelines that don't go through `check_graph`.
    symbols: Option<&'a HashMap<(Vec<String>, String), SymbolId>>,
    /// Free-function overload table for the current module (METEL-180). Used to
    /// identify overloaded declarations and resolve overloaded call sites to
    /// the selected definition's `SymbolId`.
    overloads: &'a crate::typeinference::OverloadTable,
    /// Module path being constructed; used with `symbols` to assign `def_id` to
    /// top-level functions and to resolve constructed struct/enum types to their
    /// type `SymbolId` (METEL-185 / ADR-0041).
    current_module: &'a [String],
    /// Resolved bare-`Ident` reference table (METEL-187 / ADR-0041): reference-site
    /// span → referent `SymbolId`. Used to stamp `Call::callee_id` so direct calls to
    /// top-level functions dispatch by id. `None` for the single-program path.
    references: Option<&'a HashMap<Span, SymbolId>>,
    /// Inferred closure return types recorded during Pass 1, keyed by closure span.
    closure_return_types: Option<&'a HashMap<Span, InferType>>,
    /// Concrete target-type name `Self` denotes in the innermost enclosing impl-block
    /// method body (None outside one). #774 (revised): a body-internal `let x:
    /// Self.{ field }`/`Self::AssocType` annotation resolves through this the same
    /// way `construct_impl_method`'s own param/return-type resolution already does,
    /// instead of each call site needing `self_ty_name` threaded in by hand.
    current_self_type_name: Option<String>,
    /// metel-core#736 / RFC-0138: scope-stacked table of generic `FunDecl`s visible
    /// at the current construction point (top-level, hoisted in `construct_program`;
    /// nested, hoisted per-block in `construct_block`, mirroring the local-struct
    /// hoist just above it). Lets a bare `Expr::Ident` reference to a generic
    /// function (`let alias = identity;`) recover `identity`'s own `params`/
    /// `return_type`/`body` to build a `GenericClosure` node from, the same shape
    /// `construct_decl`'s closure-literal special case already builds one from.
    fn_table: Vec<HashMap<String, FnDeclShape>>,
}

impl<'a> ConstructCtx<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        subst: &'a Substitution,
        scheme_env: &'a SchemeEnv,
        registry: &'a TypeDefinitionRegistry,
        gen: TypeVarGenerator,
        symbols: Option<&'a HashMap<(Vec<String>, String), SymbolId>>,
        overloads: &'a crate::typeinference::OverloadTable,
        current_module: &'a [String],
        references: Option<&'a HashMap<Span, SymbolId>>,
        closure_return_types: Option<&'a HashMap<Span, InferType>>,
    ) -> Result<Self, MetelError> {
        let concrete_struct_env = build_concrete_struct_env(registry, subst)?;
        let method_env = build_concrete_method_env(registry, subst)?;
        let mut ctx = Self {
            subst,
            scheme_env,
            env: vec![HashMap::new()],
            struct_scopes: vec![concrete_struct_env], // global scope pre-pushed
            registry,
            method_env,
            gen,
            current_return_ty: None,
            current_break_ty: None,
            loop_depth: 0,
            generic_params: HashMap::new(),
            symbols,
            overloads,
            current_module,
            references,
            closure_return_types,
            current_self_type_name: None,
            fn_table: vec![HashMap::new()],
        };
        // Derive concrete types for all monomorphic entries in scheme_env.
        // Both builtins and user functions are populated here — no second registration site.
        let dummy = Span::new(0, 0, "");
        for (name, scheme) in scheme_env {
            if scheme.quantified_vars.is_empty() {
                let resolved = subst.apply(&scheme.ty);
                if let Ok(ty) = infer_type_to_type(&resolved, &dummy) {
                    ctx.env.last_mut().unwrap().insert(name.clone(), ty);
                }
            }
        }
        Ok(ctx)
    }

    fn push_scope(&mut self) {
        self.env.push(HashMap::new());
        self.fn_table.push(HashMap::new());
    }
    fn pop_scope(&mut self) {
        self.env.pop();
        self.fn_table.pop();
    }

    fn push_struct_scope(&mut self) {
        self.struct_scopes.push(HashMap::new());
    }
    fn pop_struct_scope(&mut self) {
        self.struct_scopes.pop();
    }

    fn register_local_struct(&mut self, name: String, fields: Vec<(String, Type, Span)>) {
        self.struct_scopes.last_mut().unwrap().insert(name, fields);
    }

    fn get_type_param_record_kinds(&self, name: &str) -> Option<&Vec<bool>> {
        self.registry.type_param_record_kinds_for(name)
    }

    fn get_struct_fields(&self, name: &str) -> Option<&Vec<(String, Type, Span)>> {
        self.struct_scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn has_struct_named(&self, name: &str) -> bool {
        self.get_struct_fields(name).is_some() || self.registry.raw_struct_env().contains_key(name)
    }

    fn bind(&mut self, name: impl Into<String>, ty: Type) {
        self.env.last_mut().unwrap().insert(name.into(), ty);
    }

    fn lookup(&self, name: &str) -> Option<&Type> {
        self.env.iter().rev().find_map(|s| s.get(name))
    }

    /// metel-core#736 / RFC-0138: register a generic `FunDecl`'s own shape (its
    /// `params`/`return_type`/`body`) in the innermost scope, so a later bare
    /// reference to it (`let alias = name;`) can build a `GenericClosure` from it.
    fn register_fn_decl(
        &mut self,
        name: impl Into<String>,
        params: Vec<Param>,
        return_type: Option<TypeExpr>,
        body: Block,
    ) {
        self.fn_table
            .last_mut()
            .unwrap()
            .insert(name.into(), (params, return_type, body));
    }

    fn lookup_fn_decl(&self, name: &str) -> Option<&FnDeclShape> {
        self.fn_table.iter().rev().find_map(|s| s.get(name))
    }

    /// Resolve the stable `SymbolId` of a direct-call callee, if it refers to a
    /// statically-resolved top-level declaration (METEL-187). A bare `Ident` is
    /// looked up in the resolver's reference table by span; a normalized
    /// `ResolvedPath` carries its id directly. Locals and dynamic callees return
    /// `None` (dispatched by value at runtime). The evaluator tolerates an id that
    /// has no symbol registration (e.g. a top-level `let`-bound value) by falling
    /// back to name lookup, so this may be stamped liberally.
    fn resolved_callee_id(&self, callee: &Expr) -> Option<SymbolId> {
        match callee {
            Expr::Ident(_, span) => self.references.and_then(|r| r.get(span).copied()),
            Expr::ResolvedPath { symbol_id, .. } => *symbol_id,
            _ => None,
        }
    }

    /// Resolve a struct/enum type name to its declaration `SymbolId` (METEL-185).
    /// Uses the registry's declaring-module index, falling back to the current
    /// module for locally-declared types. `None` without resolver context.
    fn type_symbol_id(&self, type_name: &str) -> Option<SymbolId> {
        let symbols = self.symbols?;
        if let Some(module) = self
            .registry
            .struct_declaring_module(type_name)
            .or_else(|| self.registry.enum_declaring_module(type_name))
        {
            return symbols
                .get(&(module.clone(), type_name.to_string()))
                .copied();
        }
        // Builtin types (impl on i64/String/List/…) live in std::core, pre-seeded
        // with their SYM_TYPE_* ids; fall back there, then the current module.
        let std_core = vec!["std".to_string(), "core".to_string()];
        symbols
            .get(&(std_core, type_name.to_string()))
            .or_else(|| symbols.get(&(self.current_module.to_vec(), type_name.to_string())))
            .copied()
    }

    fn can_be_unqualified_variant(&self, name: &str) -> bool {
        self.registry.has_variant_named(name)
    }

    fn push_return_type(&mut self, ty: Option<Type>) -> Option<Type> {
        std::mem::replace(&mut self.current_return_ty, ty)
    }
    fn pop_return_type(&mut self, prev: Option<Type>) {
        self.current_return_ty = prev;
    }

    fn push_self_type_name(&mut self, name: Option<String>) -> Option<String> {
        std::mem::replace(&mut self.current_self_type_name, name)
    }
    fn pop_self_type_name(&mut self, prev: Option<String>) {
        self.current_self_type_name = prev;
    }
    fn push_break_type(&mut self, ty: Option<Type>) -> Option<Type> {
        std::mem::replace(&mut self.current_break_ty, ty)
    }
    fn pop_break_type(&mut self, prev: Option<Type>) {
        self.current_break_ty = prev;
    }

    fn enter_loop(&mut self) {
        self.loop_depth += 1;
    }
    fn exit_loop(&mut self) {
        debug_assert!(self.loop_depth > 0, "loop depth underflow");
        self.loop_depth -= 1;
    }
    fn push_loop_depth_reset(&mut self) -> usize {
        std::mem::replace(&mut self.loop_depth, 0)
    }
    fn pop_loop_depth(&mut self, prev: usize) {
        self.loop_depth = prev;
    }
    fn is_in_loop(&self) -> bool {
        self.loop_depth > 0
    }

    /// Convert a type expression to an `InferType`, substituting generic param names
    /// to their `TypeVars` when `self.generic_params` is populated (construction-at-call-time).
    /// `Self` resolves through `current_self_type_name` when set (#774, revised) --
    /// e.g. a body-internal `let x: Self.{ field }` inside an impl-block method.
    fn type_expr_to_infer_ctx(&self, te: &TypeExpr) -> InferType {
        let assoc_ctx = AssocResolveCtx {
            registry: self.registry,
            current_module: self.current_module,
            current_aspect: None,
        };
        type_expr_to_infer_with_assoc_ctx(
            te,
            &self.generic_params,
            self.current_self_type_name.as_deref(),
            &assoc_ctx,
        )
    }
}

fn unqualified_variant_needs_annotation_error(name: &str, span: &Span) -> MetelError {
    MetelError::type_error(
        TypeErrorCode::T0002,
        format!("cannot infer type of `{name}`; add a type annotation"),
        span,
    )
}

fn resolve_expected_enum<'a>(
    expected_ty: Option<&'a Type>,
    span: &Span,
    ctx: &'a ConstructCtx<'_>,
) -> Result<(&'a String, &'a EnumInfo), MetelError> {
    let expected_ty = expected_ty.ok_or_else(|| {
        MetelError::type_error(
            TypeErrorCode::T0002,
            "cannot infer type; add a type annotation",
            span,
        )
    })?;
    match expected_ty {
        Type::Named(enum_name, _) => {
            let enum_info = ctx.registry.enum_info(enum_name).ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0001,
                    format!("expected enum type, found `{expected_ty}`"),
                    span,
                )
            })?;
            Ok((enum_name, enum_info))
        }
        _ => Err(MetelError::type_error(
            TypeErrorCode::T0001,
            format!("expected enum type, found `{expected_ty}`"),
            span,
        )),
    }
}

fn resolve_unqualified_variant_expr(
    variant_name: &str,
    expected_ty: Option<&Type>,
    span: &Span,
    ctx: &ConstructCtx<'_>,
) -> Result<TypedExpr, MetelError> {
    let expected_ty = expected_ty
        .cloned()
        .ok_or_else(|| unqualified_variant_needs_annotation_error(variant_name, span))?;
    let (enum_name, enum_info) = resolve_expected_enum(Some(&expected_ty), span, ctx)?;
    let enum_name = enum_name.clone();
    let variant = enum_info
        .variants
        .iter()
        .find(|variant| variant.name == variant_name)
        .ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0001,
                format!("cannot unify `{variant_name}` with `{expected_ty}`"),
                span,
            )
        })?;
    if !variant.fields.is_empty() {
        return Err(MetelError::type_error(
            TypeErrorCode::T0003,
            format!("missing fields for `{enum_name}::{variant_name}`"),
            span,
        ));
    }
    Ok(TypedExpr::StructLiteral {
        path: vec![enum_name.clone(), variant_name.to_string()],
        fields: vec![],
        ty: expected_ty,
        type_id: ctx.type_symbol_id(&enum_name),
        span: span.clone(),
    })
}

/// Construct a `TypedBlock` for a generic (polymorphic) function body at call time.
///
/// RFC-0053 §4 (metel-core#757): `[T; N]` coerces to `T[]`, never the reverse
/// -- a `T[]` value has no statically-known length, so accepting it where a
/// specific `[T; N]` is expected defeats the type's entire point.
///
/// `construct_expr`'s `expected_ty`/`hint` parameter is advisory, not
/// enforced, for an argument that already has a fully-determined type of its
/// own (a bare identifier, unlike an array literal, which `Expr::Array`'s own
/// branch does shape against the hint) -- nothing else here rejects a `T[]`
/// argument passed where the hint specifically requires a `[T; N]`.
///
/// This is deliberately its own narrow, direction-aware check rather than a
/// change to `unify()`'s general Array/SizedArray matching: `unify()` is
/// shared by many symmetric/structural unification call sites throughout the
/// typechecker that have nothing to do with actual-vs-expected coercion
/// checking, and making it asymmetric there breaks real, legitimate uses of
/// the *valid* `[T; N]` -> `T[]` direction (confirmed directly -- an earlier
/// attempt that changed `unify()` itself passed the two repros in the linked
/// issue but failed 5 existing fixtures elsewhere in the corpus, plus a
/// generic-method case and a match-pattern-exhaustiveness case found by hand
/// that weren't in the corpus at all). Call this explicitly at every place an
/// argument's constructed type needs checking against its expected/declared
/// type instead.
fn reject_dynamic_array_where_sized_expected(
    expected: Option<&Type>,
    actual: &TypedExpr,
) -> Result<(), MetelError> {
    if let (Some(Type::SizedArray(_, n)), Type::Array(_)) =
        (expected, peel_type_references(actual.ty()))
    {
        return Err(MetelError::type_error(
            TypeErrorCode::T0001,
            format!("expected a fixed-size array of {n} element(s), got a dynamically-sized array"),
            actual.span(),
        ));
    }
    Ok(())
}

/// Instantiates `scheme` with fresh type vars, unifies each instantiated parameter
/// type with the corresponding runtime argument type (via `arg_types`), then runs the
/// construction pass on `body` with the resulting substitution.
/// Construct method-call arguments, hinting each with the corresponding non-self
/// parameter type from a concrete method function type so integer/float literals
/// adopt the expected element type (e.g. `List<i32>.push(5)` → `I32`).
fn construct_method_args(
    method_fun_ty: &Type,
    args: &[crate::ast::Expr],
    ctx: &mut ConstructCtx,
) -> Result<Vec<TypedExpr>, crate::error::MetelError> {
    if let Type::Fun(params, _) = method_fun_ty {
        // params[0] is self/receiver; the rest correspond to args.
        let arg_params: Vec<Option<&Type>> = params
            .iter()
            .skip(1)
            .map(Some)
            .chain(std::iter::repeat(None))
            .take(args.len())
            .collect();
        args.iter()
            .zip(arg_params.iter())
            .map(|(a, hint)| {
                let typed = construct_expr(a, *hint, ctx)?;
                reject_dynamic_array_where_sized_expected(*hint, &typed)?;
                Ok(typed)
            })
            .collect()
    } else {
        args.iter().map(|a| construct_expr(a, None, ctx)).collect()
    }
}

pub(super) fn symbolic_aspect_method_type(
    registry: &TypeDefinitionRegistry,
    aspect: &str,
    method: &crate::ast::AspectMethod,
    placeholder: &str,
) -> Option<InferType> {
    let mut gen = TypeVarGenerator::with_counter(3_000_000);
    let scheme = symbolic_aspect_method_scheme(registry, aspect, method, placeholder, &mut gen)?;
    let mut subst = Substitution::new();
    for (var, generic) in scheme.quantified_vars.iter().zip(&method.generics) {
        subst.bind(
            *var,
            InferType::Named(
                format!("__metel_move_check_method_generic_{}", generic.name),
                Vec::new(),
            ),
        );
    }
    Some(subst.apply(&scheme.ty))
}

pub(super) fn symbolic_aspect_method_scheme(
    registry: &TypeDefinitionRegistry,
    aspect: &str,
    method: &crate::ast::AspectMethod,
    placeholder: &str,
    gen: &mut TypeVarGenerator,
) -> Option<TypeScheme> {
    let assoc_ctx = super::conversions::AssocResolveCtx {
        registry,
        current_module: &[],
        current_aspect: Some(aspect),
    };
    let generic_map: HashMap<String, TypeVar> = method
        .generics
        .iter()
        .map(|generic| (generic.name.clone(), gen.fresh()))
        .collect();
    let params = method
        .params
        .iter()
        .map(|param| {
            if param.receiver.is_some() || param.name == "self" {
                Some(InferType::Named(placeholder.to_string(), Vec::new()))
            } else {
                param.type_ann.as_ref().map(|ann| {
                    super::conversions::type_expr_to_infer_with_assoc_ctx(
                        ann,
                        &generic_map,
                        Some(placeholder),
                        &assoc_ctx,
                    )
                })
            }
        })
        .collect::<Option<Vec<_>>>()?;
    let ret = method
        .return_type
        .as_ref()
        .map_or_else(InferType::unit, |ret| {
            super::conversions::type_expr_to_infer_with_assoc_ctx(
                ret,
                &generic_map,
                Some(placeholder),
                &assoc_ctx,
            )
        });
    let quantified_vars = method
        .generics
        .iter()
        .filter_map(|generic| generic_map.get(&generic.name).copied())
        .collect();
    Some(TypeScheme {
        quantified_vars,
        param_names: method
            .generics
            .iter()
            .map(|generic| generic.name.clone())
            .collect(),
        bounds: super::registry::collect_type_param_bounds(&method.generics, None),
        neg_bounds: super::registry::collect_negative_type_param_bounds(&method.generics, None),
        record_kinds: super::registry::collect_type_param_record_kinds(&method.generics, None),
        assoc_projections: vec![],
        assoc_eq_constraints: vec![],
        opaque_returns: vec![],
        ty: InferType::Fun(params, Box::new(ret)),
    })
}

pub(super) fn symbolic_impl_method_scheme(
    registry: &TypeDefinitionRegistry,
    impl_generics: &[crate::ast::GenericParam],
    method_generics: &[crate::ast::GenericParam],
    target_type: &TypeExpr,
    aspect_name: Option<&str>,
    params: &[crate::ast::Param],
    return_type: Option<&TypeExpr>,
) -> Option<TypeScheme> {
    let mut gen = TypeVarGenerator::with_counter(5_000_000);
    let generics: Vec<_> = impl_generics
        .iter()
        .chain(method_generics)
        .cloned()
        .collect();
    let generic_map: HashMap<String, TypeVar> = generics
        .iter()
        .map(|generic| (generic.name.clone(), gen.fresh()))
        .collect();
    let assoc_ctx = super::conversions::AssocResolveCtx {
        registry,
        current_module: &[],
        current_aspect: aspect_name,
    };
    let self_ty = super::conversions::type_expr_to_infer_with_assoc_ctx(
        target_type,
        &generic_map,
        None,
        &assoc_ctx,
    );
    let param_types = params
        .iter()
        .map(|param| {
            if param.receiver.is_some() || param.name == "self" {
                Some(self_ty.clone())
            } else {
                param.type_ann.as_ref().map(|annotation| {
                    super::conversions::type_expr_to_infer_with_assoc_ctx(
                        annotation,
                        &generic_map,
                        None,
                        &assoc_ctx,
                    )
                })
            }
        })
        .collect::<Option<Vec<_>>>()?;
    let ret = return_type.map_or_else(InferType::unit, |annotation| {
        super::conversions::type_expr_to_infer_with_assoc_ctx(
            annotation,
            &generic_map,
            None,
            &assoc_ctx,
        )
    });
    let quantified_vars = generics
        .iter()
        .filter_map(|generic| generic_map.get(&generic.name).copied())
        .collect();
    Some(TypeScheme {
        quantified_vars,
        param_names: generics
            .iter()
            .map(|generic| generic.name.clone())
            .collect(),
        bounds: super::registry::collect_type_param_bounds(&generics, None),
        neg_bounds: super::registry::collect_negative_type_param_bounds(&generics, None),
        record_kinds: super::registry::collect_type_param_record_kinds(&generics, None),
        assoc_projections: vec![],
        assoc_eq_constraints: vec![],
        opaque_returns: vec![],
        ty: InferType::Fun(param_types, Box::new(ret)),
    })
}

pub(super) fn construct_generic_body(
    scheme: &TypeScheme,
    params: &[crate::ast::Param],
    arg_types: &[crate::types::Type],
    body: &crate::ast::Block,
    span: &crate::ast::Span,
    type_ctx: &crate::typeinference::TypeCtx,
    expected_ret: Option<&crate::types::Type>,
) -> Result<crate::typed_ast::TypedBlock, crate::error::MetelError> {
    use super::conversions::{infer_type_to_type, type_to_infer};
    use crate::typeinference::{instantiate_with_renaming, TypeVarGenerator};

    // Use a high starting counter to avoid collisions with registry TypeVars (allocated
    // starting from 0 during build_registry). The substitution built here would otherwise
    // incorrectly resolve registry TypeVars when ConstructCtx::new applies it.
    let mut gen = TypeVarGenerator::with_counter(1_000_000);

    let (instance, renaming) = instantiate_with_renaming(scheme, &mut gen);
    let InferType::Fun(param_infertypes, ret_infertype) = instance else {
        return Err(crate::error::MetelError::internal(
            "construct_generic_body: scheme is not a function type",
        ));
    };

    // Unify instantiated param types with concrete arg types from runtime values.
    // Unification failures are skipped (not errors) — the typechecker already validated
    // the program; here we only need a "good enough" substitution for construction.
    // This handles cases where generic type parameters can't be recovered from runtime
    // values (e.g. `Named("MyResult", [])` vs `Named("MyResult", [T, E])`).
    let mut subst = Substitution::new();
    for (param_it, arg_ty) in param_infertypes.iter().zip(arg_types.iter()) {
        let arg_it = type_to_infer(arg_ty);
        if let Ok(s) = typeinference::unify(&subst.apply(param_it), &arg_it) {
            subst = subst.compose(&s);
        }
    }

    // metel-core#716: a type parameter that appears only in the return position (no
    // argument mentions it — `fun make<V>() -> V[]`, `List::new<T>() -> List<T>`) gets
    // nothing from the loop above. Before falling back to Never (which follows), also try
    // the caller's own expected return type — already correctly resolved at the call site
    // (the same computation `instantiate_scheme_with_expected_ret` does for the outer
    // call's own signature; this is that information's only path into the body's own
    // construction, which arg_types alone can never carry for a no-argument call).
    // Unification failures here are skipped for the same "good enough substitution"
    // reason as the argument loop above.
    if let Some(expected) = expected_ret {
        if let Ok(s) = typeinference::unify(&subst.apply(&ret_infertype), &type_to_infer(expected))
        {
            subst = subst.compose(&s);
        }
    }

    // Fill any still-unresolved type vars with Never (not Unit) so
    // `infer_type_to_type` does not error during construction. `unify` treats
    // `Never` as the bottom type and no-ops instead of binding (see `unify`'s
    // `(Never, _) | (_, Never) => Ok(Substitution::new())` arm), which is
    // exactly why a var can still be unresolved here even after the loop above
    // -- e.g. `value_to_type`'s own placeholder for an empty collection's
    // element type (#271) unifies as a no-op, leaving the element TypeVar
    // unbound. Defaulting to `Unit` here (as opposed to `Never`) would make
    // that placeholder look like a real concrete type to everything
    // downstream, which previously produced spurious construction errors for
    // any dead branch that called a non-Unit method on it; `Never` coerces to
    // any type through the rest of construction (binops, method-call
    // receivers) and defers to the evaluator's runtime dynamic dispatch
    // instead, matching this function's own "evaluation correctness is
    // unaffected -- runtime dispatch goes by value kind" contract.
    let all_free: std::collections::HashSet<_> = param_infertypes
        .iter()
        .chain(std::iter::once(&*ret_infertype))
        .flat_map(typeinference::free_vars)
        .collect();
    for v in all_free {
        if subst.lookup(v).is_none() {
            subst.bind(v, InferType::Never);
        }
    }

    let ret_ty = infer_type_to_type(&subst.apply(&ret_infertype), span).ok();

    // Generic bodies are constructed at call time; overloaded functions are never
    // generic, so there is no overload table to consult here.
    let empty_overloads = crate::typeinference::OverloadTable::new();
    // Generic bodies are reconstructed at runtime; their inner direct calls are
    // re-resolved here without a reference table (callee_id stamping is skipped),
    // and without pass 1's write-through analysis (empty set — same limitation).
    let mut ctx = ConstructCtx::new(
        &subst,
        &type_ctx.scheme_env,
        &type_ctx.registry,
        gen,
        None,
        &empty_overloads,
        &[],
        None,
        None,
    )?;

    // Build name → fresh TypeVar mapping so type annotations like `T[]` in the body
    // resolve to concrete types. scheme.param_names[i] corresponds to quantified_vars[i],
    // and renaming maps original TypeVar → fresh TypeVar.
    if !scheme.param_names.is_empty() {
        let mut gp: HashMap<String, TypeVar> = HashMap::new();
        for (orig_var, name) in scheme.quantified_vars.iter().zip(scheme.param_names.iter()) {
            if let Some(&fresh_var) = renaming.get(orig_var) {
                gp.insert(name.clone(), fresh_var);
            }
        }
        ctx.generic_params = gp;
    }

    ctx.push_scope();
    for (param, param_it) in params.iter().zip(param_infertypes.iter()) {
        let concrete_ty =
            infer_type_to_type(&subst.apply(param_it), span).unwrap_or(crate::types::Type::Unit);
        ctx.bind(&param.name, concrete_ty);
    }
    let saved_return = ctx.push_return_type(ret_ty.clone());
    let typed_block = construct_block(body, ret_ty.as_ref(), &mut ctx)?;
    ctx.pop_return_type(saved_return);
    ctx.pop_scope();

    Ok(typed_block)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn construct_program(
    program: &Program,
    subst: &Substitution,
    scheme_env: &SchemeEnv,
    registry: &TypeDefinitionRegistry,
    gen: TypeVarGenerator,
    symbols: Option<&HashMap<(Vec<String>, String), SymbolId>>,
    overloads: &crate::typeinference::OverloadTable,
    current_module: &[String],
    references: Option<&HashMap<Span, SymbolId>>,
    closure_return_types: Option<&HashMap<Span, InferType>>,
) -> Result<TypedProgram, MetelError> {
    let mut ctx = ConstructCtx::new(
        subst,
        scheme_env,
        registry,
        gen,
        symbols,
        overloads,
        current_module,
        references,
        closure_return_types,
    )?;

    // metel-core#736 / RFC-0138: hoist every top-level `FunDecl`'s own shape into
    // `ctx.fn_table` before constructing any declaration, so a bare reference to a
    // generic function (`let alias = identity;`) can find it regardless of
    // declaration order -- mirroring the local-struct hoist in `construct_block`.
    for decl in &program.decls {
        if let Decl::Fun(fd) = decl {
            ctx.register_fn_decl(
                fd.name.clone(),
                fd.params.clone(),
                fd.return_type.clone(),
                fd.body.clone(),
            );
        }
    }

    let mut out = vec![];
    for decl in &program.decls {
        let mut typed = construct_decl(decl, &mut ctx)?;
        // Assign each non-overloaded top-level function its stable identity so the
        // evaluator can register and dispatch it by `SymbolId` (METEL-187). Only
        // genuine top-level declarations are post-processed here; methods and
        // nested/local functions keep `def_id: None`.
        if let TypedDecl::Fun(f) = &mut typed {
            if f.symbol_id.is_none() {
                if let Some(syms) = symbols {
                    f.def_id = syms
                        .get(&(current_module.to_vec(), f.name.clone()))
                        .copied();
                }
            }
        }
        // Same identity assignment for top-level `let`/`mut` (ADR-0042): only
        // module-level bindings reach this loop, so this never touches a block-local
        // or `for`-init binding (those keep `def_id: None` from construction).
        if let TypedDecl::Let(ld) = &mut typed {
            if let Some(syms) = symbols {
                ld.def_id = syms
                    .get(&(current_module.to_vec(), ld.name.clone()))
                    .copied();
            }
        }
        if let TypedDecl::Mut(md) = &mut typed {
            if let Some(syms) = symbols {
                md.def_id = syms
                    .get(&(current_module.to_vec(), md.name.clone()))
                    .copied();
            }
        }
        out.push(typed);
    }
    Ok(out)
}

#[allow(clippy::too_many_lines)]
fn construct_decl(decl: &Decl, ctx: &mut ConstructCtx) -> Result<TypedDecl, MetelError> {
    match decl {
        Decl::Let(ld) => {
            // Let-polymorphism: if a closure is in scheme_env with quantified vars,
            // store it as GenericClosure. The name stays absent from ctx.env so call
            // sites use scheme_env instantiation in construct_call.
            if let Expr::Closure {
                params,
                return_type,
                body,
                span: cls_span,
            } = &ld.value
            {
                if let Some(scheme) = ctx.scheme_env.get(ld.name.as_str()) {
                    if !scheme.quantified_vars.is_empty() {
                        return Ok(TypedDecl::Let(TypedLetDecl {
                            name: ld.name.clone(),
                            type_ann: ld.type_ann.clone(),
                            value: TypedExpr::GenericClosure {
                                name: Some(ld.name.clone()),
                                params: params.clone(),
                                return_type: return_type.clone(),
                                body: body.clone(),
                                ty: Type::Unit,
                                span: cls_span.clone(),
                            },
                            def_id: None,
                            span: ld.span.clone(),
                        }));
                    }
                }
            }
            // metel-core#736 / RFC-0138: a bare reference to an already-declared
            // generic function (`let alias = identity;`) needs the same treatment
            // as the closure-literal case above -- the widened inference-side gate
            // (`infer_decl`'s `Decl::Let` arm) re-generalizes `alias` into
            // `scheme_env` under its own name whenever this shape matches, so the
            // check below is exactly the closure case's own check, just sourcing
            // `params`/`return_type`/`body` from the referenced function's own
            // declaration (via `ctx.fn_table`, hoisted in `construct_program`/
            // `construct_block`) instead of an inline literal.
            if let Expr::Ident(name, ident_span) = &ld.value {
                if let Some(scheme) = ctx.scheme_env.get(ld.name.as_str()) {
                    if !scheme.quantified_vars.is_empty() {
                        if let Some((params, return_type, body)) = ctx.lookup_fn_decl(name) {
                            let (params, return_type, body) =
                                (params.clone(), return_type.clone(), body.clone());
                            return Ok(TypedDecl::Let(TypedLetDecl {
                                name: ld.name.clone(),
                                type_ann: ld.type_ann.clone(),
                                value: TypedExpr::GenericClosure {
                                    name: Some(ld.name.clone()),
                                    params,
                                    return_type,
                                    body,
                                    ty: Type::Unit,
                                    span: ident_span.clone(),
                                },
                                def_id: None,
                                span: ld.span.clone(),
                            }));
                        }
                    }
                }
            }
            let expected_ty = ld
                .type_ann
                .as_ref()
                .map(|ann| resolved_to_type(&ctx.type_expr_to_infer_ctx(ann), ctx.subst, &ld.span))
                .transpose()?;
            let value = construct_expr(&ld.value, expected_ty.as_ref(), ctx)?;
            let value = match &expected_ty {
                Some(t) => {
                    let value =
                        maybe_read_copy(t, value, &ld.span, ctx.registry, ctx.current_module)?;
                    maybe_singleton_coerce(t, value, &ld.span, ctx.registry)?
                }
                None => value,
            };
            let ty = expected_ty.unwrap_or_else(|| value.ty().clone());
            ctx.bind(&ld.name, ty);
            Ok(TypedDecl::Let(TypedLetDecl {
                name: ld.name.clone(),
                type_ann: ld.type_ann.clone(),
                value,
                def_id: None,
                span: ld.span.clone(),
            }))
        }
        Decl::Mut(md) => {
            let expected_ty = md
                .type_ann
                .as_ref()
                .map(|ann| resolved_to_type(&ctx.type_expr_to_infer_ctx(ann), ctx.subst, &md.span))
                .transpose()?;
            let value = construct_expr(&md.value, expected_ty.as_ref(), ctx)?;
            let value = match &expected_ty {
                Some(t) => {
                    let value =
                        maybe_read_copy(t, value, &md.span, ctx.registry, ctx.current_module)?;
                    maybe_singleton_coerce(t, value, &md.span, ctx.registry)?
                }
                None => value,
            };
            let ty = expected_ty.unwrap_or_else(|| value.ty().clone());
            ctx.bind(&md.name, ty);
            Ok(TypedDecl::Mut(TypedMutDecl {
                name: md.name.clone(),
                type_ann: md.type_ann.clone(),
                value,
                def_id: None,
                span: md.span.clone(),
            }))
        }
        Decl::Fun(fd) => construct_fun_decl(fd, ctx),
        Decl::Struct(sd) => Ok(TypedDecl::Struct(TypedStructDecl {
            name: sd.name.clone(),
            generics: sd.generics.clone(),
            fields: sd.fields.clone(),
            span: sd.span.clone(),
        })),
        Decl::Enum(ed) => Ok(TypedDecl::Enum(TypedEnumDecl {
            name: ed.name.clone(),
            generics: ed.generics.clone(),
            variants: ed.variants.clone(),
            span: ed.span.clone(),
        })),
        Decl::Impl(ib) => construct_impl_decl(ib, ctx),
        Decl::Aspect(td) => Ok(TypedDecl::Aspect(TypedAspectDecl {
            name: td.name.clone(),
            generics: td.generics.clone(),
            methods: td.methods.clone(),
            span: td.span.clone(),
        })),
        Decl::Stmt(stmt) => Ok(TypedDecl::Stmt(Box::new(construct_stmt(stmt, ctx)?))),
    }
}

#[allow(clippy::too_many_lines)]
fn construct_fun_decl(fun: &FunDecl, ctx: &mut ConstructCtx) -> Result<TypedDecl, MetelError> {
    // Native functions carry no Metel body; lower the host binding to a NativeKey
    // and emit a Native body for the evaluator to dispatch (METEL-182).
    if let Some(binding) = &fun.native {
        let key = crate::native_keys::NativeKey::from_path(&binding.key_path).ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!(
                    "unknown native binding `@{}`; no host implementation is registered for it",
                    binding.key_path.join(".")
                ),
                &binding.span,
            )
        })?;
        // Overloaded native definitions (std::core's assert pair) carry their
        // overload SymbolId like any overloaded decl.
        let symbol_id = super::overload::entry_for_decl(ctx.overloads, fun).map(|e| e.symbol_id);
        return Ok(TypedDecl::Fun(TypedFunDecl {
            name: fun.name.clone(),
            generics: fun.generics.clone(),
            params: fun.params.clone(),
            return_type: fun.return_type.clone(),
            body: FunBody::Native(key),
            symbol_id,
            def_id: None,
            span: fun.span.clone(),
        }));
    }

    // Overloaded definitions (METEL-180) never enter the name-keyed scheme env;
    // their concrete signature comes straight from the overload entry, and the
    // typed decl carries the entry's SymbolId for the evaluator's registry.
    let overload_entry = super::overload::entry_for_decl(ctx.overloads, fun).cloned();
    let scheme = match &overload_entry {
        Some(entry) => TypeScheme {
            quantified_vars: vec![],
            param_names: vec![],
            bounds: vec![],
            neg_bounds: vec![],
            record_kinds: vec![],
            assoc_projections: vec![],
            assoc_eq_constraints: vec![],
            opaque_returns: vec![],
            ty: InferType::Fun(
                entry
                    .params
                    .iter()
                    .map(|t| InferType::Concrete(t.clone()))
                    .collect(),
                Box::new(InferType::Concrete(entry.ret.clone())),
            ),
        },
        None => ctx
            .scheme_env
            .get(fun.name.as_str())
            .ok_or_else(|| MetelError::internal(format!("missing type for fn `{}`", fun.name)))?
            .clone(),
    };

    let body = if scheme.quantified_vars.is_empty() {
        let (param_types, ret_ty) = match ctx.subst.apply(&scheme.ty) {
            InferType::Fun(params, ret) => {
                let pts = params
                    .iter()
                    .map(|p| infer_type_to_type(p, &fun.span))
                    .collect::<Result<Vec<_>, _>>()?;
                let rt = infer_type_to_type(&ret, &fun.span).ok();
                (pts, rt)
            }
            _ => {
                return Err(MetelError::internal(format!(
                    "expected Fun type for `{}`",
                    fun.name
                )))
            }
        };
        ctx.push_scope();
        for (param, ty) in fun.params.iter().zip(param_types.iter()) {
            ctx.bind(&param.name, ty.clone());
        }
        let saved_return = ctx.push_return_type(ret_ty.clone());
        let typed_block = construct_block(&fun.body, ret_ty.as_ref(), ctx)?;
        ctx.pop_return_type(saved_return);
        ctx.pop_scope();
        // RFC-0078 §6: a function declared `-> !` must diverge on every path.
        if matches!(ret_ty, Some(Type::Never)) && !fun_body_diverges(&typed_block) {
            return Err(MetelError::type_error(
                TypeErrorCode::T0016,
                format!(
                    "function `{}` is declared `-> !` but does not diverge on all paths",
                    fun.name
                ),
                &fun.span,
            ));
        }
        FunBody::Typed(typed_block)
    } else if scheme.quantified_vars.iter().all(|qvar| {
        scheme.opaque_returns.iter().any(|opaque| {
            if let Some((_, _concrete_ty)) = opaque {
                // Check if this quantified var is bound to a concrete type in opaque_returns
                // We need to find if there's an opaque entry that covers this quantified var position
                if let Some(idx) = scheme.quantified_vars.iter().position(|v| v == qvar) {
                    scheme.opaque_returns.get(idx).is_some()
                } else {
                    false
                }
            } else {
                false
            }
        })
    }) {
        // All quantified vars are accounted for by opaque_returns - eager build
        let mut subst = Substitution::new();
        for (i, qvar) in scheme.quantified_vars.iter().enumerate() {
            if let Some(Some((_, concrete_ty))) = scheme.opaque_returns.get(i) {
                subst.bind(*qvar, InferType::Concrete(concrete_ty.clone()));
            }
        }
        let substituted_ty = subst.apply(&scheme.ty);
        let (param_types, ret_ty) = match substituted_ty {
            InferType::Fun(params, ret) => {
                let pts = params
                    .iter()
                    .map(|p| infer_type_to_type(p, &fun.span))
                    .collect::<Result<Vec<_>, _>>()?;
                let rt = infer_type_to_type(&ret, &fun.span).ok();
                (pts, rt)
            }
            _ => {
                return Err(MetelError::internal(format!(
                    "expected Fun type for `{}`",
                    fun.name
                )))
            }
        };
        ctx.push_scope();
        for (param, ty) in fun.params.iter().zip(param_types.iter()) {
            ctx.bind(&param.name, ty.clone());
        }
        let saved_return = ctx.push_return_type(ret_ty.clone());
        let typed_block = construct_block(&fun.body, ret_ty.as_ref(), ctx)?;
        ctx.pop_return_type(saved_return);
        ctx.pop_scope();
        // RFC-0078 §6: a function declared `-> !` must diverge on every path.
        if matches!(ret_ty, Some(Type::Never)) && !fun_body_diverges(&typed_block) {
            return Err(MetelError::type_error(
                TypeErrorCode::T0016,
                format!(
                    "function `{}` is declared `-> !` but does not diverge on all paths",
                    fun.name
                ),
                &fun.span,
            ));
        }
        FunBody::Typed(typed_block)
    } else {
        FunBody::Generic(fun.body.clone())
    };

    Ok(TypedDecl::Fun(TypedFunDecl {
        name: fun.name.clone(),
        generics: fun.generics.clone(),
        params: fun.params.clone(),
        return_type: fun.return_type.clone(),
        body,
        symbol_id: overload_entry.map(|e| e.symbol_id),
        def_id: None,
        span: fun.span.clone(),
    }))
}

/// Reject a `Drop` impl that supplies a destructor body, while still allowing a
/// type to *declare* itself `Drop`.
///
/// RFC-0071 §9c gates #290 (the `Drop` aspect) on #292 (destructor invocation):
/// between them, a `drop` body compiles and never runs — "a feature that looks
/// functional and silently does nothing". #292 moved to v0.13.0, so the gate
/// fired and that state must not ship (#345).
///
/// The rejection is narrower than "reject `Drop` impls", deliberately. Declaring
/// `Drop` has type-level effects that are implemented and correct *today*, and
/// none of them involve the destructor running: `Copy`/`Drop` exclusion,
/// `T: !Drop` bounds, the ban on `Drop` for anonymous records, and the move
/// checker's refusal to partially move a `Drop` value. An empty `fun drop(self)
/// {}` claims nothing that is not delivered. A body with statements in it does.
fn reject_inert_destructor(ib: &ImplBlock, ctx: &ConstructCtx) -> Result<(), MetelError> {
    if ib.polarity != crate::ast::Polarity::Positive {
        return Ok(());
    }
    // Match the stdlib aspect by its declaring module, not by name, so a user
    // module's own unrelated `Drop` aspect is unaffected — the same discipline
    // `coherence.rs` uses for the `Copy`/`Drop` exclusion.
    let Some(aspect_name) = ib.aspect_name.as_deref() else {
        return Ok(());
    };
    if aspect_name != "Drop" {
        return Ok(());
    }
    let is_std_core_drop = ctx
        .registry
        .aspect_declaring_module(aspect_name)
        .is_some_and(|module| module.as_slice() == ["std".to_string(), "core".to_string()]);
    if !is_std_core_drop {
        return Ok(());
    }

    for method in &ib.methods {
        if method.name != "drop" {
            continue;
        }
        let body_is_empty = method.body.stmts.is_empty() && method.body.tail.is_none();
        if body_is_empty {
            continue;
        }
        return Err(MetelError::type_error(
            TypeErrorCode::T0001,
            "a `drop` body cannot run yet: destructor invocation is not implemented \
             (metel-core#292), so this cleanup would silently never happen. Leave the \
             body empty to declare the type `Drop` for its type-level effects — \
             `Copy` exclusion, `!Drop` bounds, and the partial-move ban — or move the \
             cleanup into an ordinary method the caller invokes"
                .to_string(),
            &method.span,
        ));
    }
    Ok(())
}

fn construct_impl_decl(ib: &ImplBlock, ctx: &mut ConstructCtx) -> Result<TypedDecl, MetelError> {
    reject_inert_destructor(ib, ctx)?;
    // An impl block that declares its own generics (RFC-0036 conditional impls,
    // RFC-0061 structural blanket impls: `impl<T: Bound> Aspect for Type<T>` /
    // `impl<T: Display> Display for T[]`) can't have its methods eagerly constructed
    // against a concrete `self` type here — same reason generic-struct methods
    // already defer to `FunBody::Generic` below. Real bound-satisfaction checking at
    // each instantiation is issue #241/#245's job, not this one's; this only needs to
    // not crash on construction.
    // Structural targets (`T[]`, tuples, `fun` types, anonymous records) have no
    // nominal name to key registry lookups on, so they key on the empty string
    // and their methods take the deferred path below. This used to be reachable
    // only when the impl also declared generics; a structural target *without*
    // them fell through to an internal error (metel-core#581).
    super::reject_unregisterable_impl_target(ib)?;
    let defers_bodies = super::impl_defers_method_bodies(ib);
    let target_name = super::impl_target_head(&ib.target_type)
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut methods = ib
        .methods
        .iter()
        .map(|m| construct_impl_method(m, &target_name, defers_bodies, ctx))
        .collect::<Result<Vec<_>, _>>()?;
    // Default aspect-method bodies are constructed eagerly against a concrete `self`
    // type today (see `construct_default_aspect_method`) — not sound to do against a
    // conditional/structural target without knowing the concrete instantiation.
    // Skipped for now when the impl has its own generics; issue #241/#245's job to
    // do this properly once bound-satisfaction checking exists.
    //
    // Also skipped for a negative impl (RFC-0081, issue #264): `impl !Aspect for
    // Type {}` declares non-implementation, so it must not inherit the aspect's
    // default method bodies — that would make the type appear to implement the
    // aspect via inherited defaults, the opposite of what a negative impl means.
    if !defers_bodies && ib.polarity == crate::ast::Polarity::Positive {
        methods.extend(construct_default_aspect_methods(ib, &target_name, ctx)?);
    }

    // Resolve aspect_id from the symbol table when available.
    let aspect_id = ib.aspect_name.as_deref().and_then(|aspect_name| {
        let declaring_module = ctx.registry.aspect_declaring_module(aspect_name)?;
        ctx.symbols?
            .get(&(declaring_module.clone(), aspect_name.to_string()))
            .copied()
    });

    Ok(TypedDecl::Impl(TypedImplBlock {
        polarity: ib.polarity,
        generics: ib.generics.clone(),
        aspect_name: ib.aspect_name.clone(),
        aspect_id,
        target_type_id: ctx.type_symbol_id(&target_name),
        aspect_type_args: ib.aspect_type_args.clone(),
        target_type: ib.target_type.clone(),
        methods,
        span: ib.span.clone(),
    }))
}

fn construct_impl_method(
    method: &FunDecl,
    target_name: &str,
    impl_has_generics: bool,
    ctx: &mut ConstructCtx,
) -> Result<TypedFunDecl, MetelError> {
    // Native method: no Metel body; lower the host binding to a NativeKey
    // (METEL-181). Dispatched at runtime by the evaluator's impl-method path.
    if let Some(binding) = &method.native {
        let key = crate::native_keys::NativeKey::from_path(&binding.key_path).ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!(
                    "unknown native binding `@{}`; no host implementation is registered for it",
                    binding.key_path.join(".")
                ),
                &binding.span,
            )
        })?;
        return Ok(TypedFunDecl {
            name: method.name.clone(),
            generics: method.generics.clone(),
            params: method.params.clone(),
            return_type: method.return_type.clone(),
            body: FunBody::Native(key),
            symbol_id: None,
            def_id: None,
            span: method.span.clone(),
        });
    }

    // Methods on a generic struct OR generic enum have T-typed params that can't be
    // resolved to concrete types in Pass 2 without call-site type args. Store the body
    // as Generic (untyped) so the evaluator constructs it at runtime — same pattern as
    // top-level generic fns. (Using raw_struct_type_params would miss enums, whose
    // methods would then be eagerly constructed here and fail on e.g. `match self`.)
    // Also deferred whenever the *impl block itself* declares generics (RFC-0036/
    // RFC-0061) — `target_name` may not even name a real struct/enum in that case
    // (RFC-0061's structural targets), so `struct_generic_names_for` can't be relied
    // on to catch it. And deferred whenever the *method itself* declares its own
    // generics (RFC-0040 §7, issue #746) -- a method on an otherwise-concrete target
    // (`extend Foo { fun describe<U: Aspect>(...) }`) is just as unresolvable here
    // without call-site type args as an impl-level generic is; missing this case used
    // to eagerly resolve `U` as a literal, nonexistent named type instead of deferring.
    let is_generic_target = impl_has_generics
        || !method.generics.is_empty()
        || ctx
            .registry
            .struct_generic_names_for(target_name)
            .is_some_and(|names| !names.is_empty());
    if is_generic_target {
        return Ok(TypedFunDecl {
            name: method.name.clone(),
            generics: method.generics.clone(),
            params: method.params.clone(),
            return_type: method.return_type.clone(),
            body: FunBody::Generic(method.body.clone()),
            symbol_id: None,
            def_id: None,
            span: method.span.clone(),
        });
    }

    let self_ty = super::inference::primitive_type_from_name(target_name)
        .unwrap_or_else(|| Type::Named(target_name.to_string(), vec![]));
    // #774: `type_expr_to_infer_with_self` resolves `Self` but carries no
    // `AssocResolveCtx` (no registry access), which `Self.{ field }` needs to look
    // up the target struct's actual fields -- mirrors the same fix in inference.rs's
    // own `infer_impl_method`, this pass's Pass-1 counterpart.
    let empty_generics: HashMap<String, TypeVar> = HashMap::new();
    let assoc_ctx = AssocResolveCtx {
        registry: ctx.registry,
        current_module: ctx.current_module,
        current_aspect: None,
    };
    let te_to_infer = |te: &TypeExpr| {
        type_expr_to_infer_with_assoc_ctx(te, &empty_generics, Some(target_name), &assoc_ctx)
    };
    let param_types: Vec<Type> = method
        .params
        .iter()
        .map(|p| {
            if p.name == "self" {
                Ok(self_ty.clone())
            } else {
                p.type_ann.as_ref().map_or_else(
                    || {
                        Err(MetelError::type_error(
                            TypeErrorCode::T0002,
                            format!("parameter `{}` needs a type annotation", p.name),
                            &p.span,
                        ))
                    },
                    |ann| resolved_to_type(&te_to_infer(ann), ctx.subst, &p.span),
                )
            }
        })
        .collect::<Result<_, _>>()?;
    let ret_ty = method
        .return_type
        .as_ref()
        .map(|ann| resolved_to_type(&te_to_infer(ann), ctx.subst, &method.span))
        .transpose()?;
    ctx.push_scope();
    for (p, ty) in method.params.iter().zip(param_types.iter()) {
        ctx.bind(&p.name, ty.clone());
    }
    let saved_return = ctx.push_return_type(ret_ty.clone());
    let saved_self = ctx.push_self_type_name(Some(target_name.to_string()));
    let typed_block = construct_block(&method.body, ret_ty.as_ref(), ctx)?;
    ctx.pop_self_type_name(saved_self);
    ctx.pop_return_type(saved_return);
    ctx.pop_scope();
    Ok(TypedFunDecl {
        name: method.name.clone(),
        generics: method.generics.clone(),
        params: method.params.clone(),
        return_type: method.return_type.clone(),
        body: FunBody::Typed(typed_block),
        symbol_id: None,
        def_id: None,
        span: method.span.clone(),
    })
}

// Synthesize typed method bodies for aspect methods not provided by this impl block.
// Bodies come from the aspect's default_body; Self is substituted with the concrete target type.
// The evaluator never needs to know about defaults — see ADR-0034.
fn construct_default_aspect_methods(
    ib: &ImplBlock,
    target_name: &str,
    ctx: &mut ConstructCtx,
) -> Result<Vec<TypedFunDecl>, MetelError> {
    let Some(aspect_name) = &ib.aspect_name else {
        return Ok(vec![]);
    };
    let Some(methods) = ctx.registry.aspect_method_defs(aspect_name).cloned() else {
        return Ok(vec![]);
    };
    let provided: std::collections::HashSet<&str> =
        ib.methods.iter().map(|m| m.name.as_str()).collect();

    methods
        .iter()
        .filter(|method| method.default_body.is_some() && !provided.contains(method.name.as_str()))
        .map(|method| construct_default_aspect_method(method, target_name, ctx))
        .collect()
}

fn construct_default_aspect_method(
    method: &AspectMethod,
    target_name: &str,
    ctx: &mut ConstructCtx,
) -> Result<TypedFunDecl, MetelError> {
    let self_ty = super::inference::primitive_type_from_name(target_name)
        .unwrap_or_else(|| Type::Named(target_name.to_string(), vec![]));
    // #774: `type_expr_to_infer_with_self` resolves `Self` but carries no
    // `AssocResolveCtx` (no registry access), which `Self.{ field }` needs to look
    // up the target struct's actual fields -- mirrors the same fix in inference.rs's
    // own `infer_impl_method`, this pass's Pass-1 counterpart.
    let empty_generics: HashMap<String, TypeVar> = HashMap::new();
    let assoc_ctx = AssocResolveCtx {
        registry: ctx.registry,
        current_module: ctx.current_module,
        current_aspect: None,
    };
    let te_to_infer = |te: &TypeExpr| {
        type_expr_to_infer_with_assoc_ctx(te, &empty_generics, Some(target_name), &assoc_ctx)
    };
    let param_types: Vec<Type> = method
        .params
        .iter()
        .map(|p| {
            if p.name == "self" {
                Ok(self_ty.clone())
            } else {
                p.type_ann.as_ref().map_or_else(
                    || {
                        Err(MetelError::type_error(
                            TypeErrorCode::T0002,
                            format!("parameter `{}` needs a type annotation", p.name),
                            &p.span,
                        ))
                    },
                    |ann| resolved_to_type(&te_to_infer(ann), ctx.subst, &p.span),
                )
            }
        })
        .collect::<Result<_, _>>()?;
    let ret_ty = method
        .return_type
        .as_ref()
        .map(|ann| resolved_to_type(&te_to_infer(ann), ctx.subst, &method.span))
        .transpose()?;
    let body = method
        .default_body
        .as_ref()
        .ok_or_else(|| MetelError::internal("missing aspect default body"))?;
    ctx.push_scope();
    for (p, ty) in method.params.iter().zip(param_types.iter()) {
        ctx.bind(&p.name, ty.clone());
    }
    let saved_return = ctx.push_return_type(ret_ty.clone());
    let saved_self = ctx.push_self_type_name(Some(target_name.to_string()));
    let typed_block = construct_block(body, ret_ty.as_ref(), ctx)?;
    ctx.pop_self_type_name(saved_self);
    ctx.pop_return_type(saved_return);
    ctx.pop_scope();
    Ok(TypedFunDecl {
        name: method.name.clone(),
        generics: method.generics.clone(),
        params: method.params.clone(),
        return_type: method.return_type.clone(),
        body: FunBody::Typed(typed_block),
        symbol_id: None,
        def_id: None,
        span: method.span.clone(),
    })
}

fn construct_block(
    block: &Block,
    expected_tail_ty: Option<&Type>,
    ctx: &mut ConstructCtx,
) -> Result<TypedBlock, MetelError> {
    ctx.push_scope();
    ctx.push_struct_scope();
    // Hoist struct/enum declarations defined in this block so they are available
    // for any expression in the block regardless of declaration order.
    for decl in &block.stmts {
        if let Decl::Struct(sd) = decl {
            let fields = sd
                .fields
                .iter()
                .map(|f| {
                    let ty = resolved_to_type(
                        &ctx.type_expr_to_infer_ctx(&f.type_ann),
                        ctx.subst,
                        &f.span,
                    )?;
                    Ok((f.name.clone(), ty, f.span.clone()))
                })
                .collect::<Result<_, MetelError>>()?;
            ctx.register_local_struct(sd.name.clone(), fields);
        }
    }
    // metel-core#736 / RFC-0138: hoist this block's own nested `FunDecl`s the same
    // way, so a bare reference to a nested generic function works regardless of
    // whether the reference textually precedes or follows the declaration --
    // matching how `hoist_fun_decls` already makes nested mutual recursion work
    // on the inference side.
    for decl in &block.stmts {
        if let Decl::Fun(fd) = decl {
            ctx.register_fn_decl(
                fd.name.clone(),
                fd.params.clone(),
                fd.return_type.clone(),
                fd.body.clone(),
            );
        }
    }
    let mut stmts = vec![];
    for stmt in &block.stmts {
        stmts.push(construct_decl(stmt, ctx)?);
    }
    let tail = match &block.tail {
        Some(e) => {
            let constructed = construct_expr(e, expected_tail_ty, ctx)?;
            let constructed = match expected_tail_ty {
                Some(t) => {
                    let constructed = maybe_read_copy(
                        t,
                        constructed,
                        e.span(),
                        ctx.registry,
                        ctx.current_module,
                    )?;
                    maybe_singleton_coerce(t, constructed, e.span(), ctx.registry)?
                }
                None => constructed,
            };
            Some(Box::new(constructed))
        }
        None => None,
    };
    ctx.pop_struct_scope();
    ctx.pop_scope();
    Ok(TypedBlock {
        stmts,
        tail,
        span: block.span.clone(),
    })
}

// Exhaustive match over every AST/type-system variant; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
fn construct_stmt(stmt: &Stmt, ctx: &mut ConstructCtx) -> Result<TypedStmt, MetelError> {
    match stmt {
        Stmt::Expr(e) => Ok(TypedStmt::Expr(construct_expr(e, None, ctx)?)),
        Stmt::While(ws) => {
            let condition = construct_expr(&ws.condition, None, ctx)?;
            ctx.enter_loop();
            let body = construct_block(&ws.body, None, ctx)?;
            ctx.exit_loop();
            Ok(TypedStmt::While(TypedWhileStmt {
                condition,
                body,
                span: ws.span.clone(),
            }))
        }
        Stmt::For(fs) => {
            ctx.push_scope();
            let init = match &fs.init {
                Some(ForInit::Let(ld)) => {
                    let expected_ty = ld
                        .type_ann
                        .as_ref()
                        .map(|ann| {
                            resolved_to_type(&ctx.type_expr_to_infer_ctx(ann), ctx.subst, &ld.span)
                        })
                        .transpose()?;
                    let value = construct_expr(&ld.value, expected_ty.as_ref(), ctx)?;
                    let value = match &expected_ty {
                        Some(t) => {
                            let value = maybe_read_copy(
                                t,
                                value,
                                &ld.span,
                                ctx.registry,
                                ctx.current_module,
                            )?;
                            maybe_singleton_coerce(t, value, &ld.span, ctx.registry)?
                        }
                        None => value,
                    };
                    let ty = expected_ty.unwrap_or_else(|| value.ty().clone());
                    ctx.bind(&ld.name, ty);
                    let typed_ld = TypedLetDecl {
                        name: ld.name.clone(),
                        type_ann: ld.type_ann.clone(),
                        value,
                        def_id: None,
                        span: ld.span.clone(),
                    };
                    Some(TypedForInit::Let(typed_ld))
                }
                Some(ForInit::Mut(md)) => {
                    let expected_ty = md
                        .type_ann
                        .as_ref()
                        .map(|ann| {
                            resolved_to_type(&ctx.type_expr_to_infer_ctx(ann), ctx.subst, &md.span)
                        })
                        .transpose()?;
                    let value = construct_expr(&md.value, expected_ty.as_ref(), ctx)?;
                    let value = match &expected_ty {
                        Some(t) => {
                            let value = maybe_read_copy(
                                t,
                                value,
                                &md.span,
                                ctx.registry,
                                ctx.current_module,
                            )?;
                            maybe_singleton_coerce(t, value, &md.span, ctx.registry)?
                        }
                        None => value,
                    };
                    let ty = expected_ty.unwrap_or_else(|| value.ty().clone());
                    ctx.bind(&md.name, ty);
                    let typed_md = TypedMutDecl {
                        name: md.name.clone(),
                        type_ann: md.type_ann.clone(),
                        value,
                        def_id: None,
                        span: md.span.clone(),
                    };
                    Some(TypedForInit::Mut(typed_md))
                }
                Some(ForInit::Expr(e)) => Some(TypedForInit::Expr(construct_expr(e, None, ctx)?)),
                None => None,
            };
            let condition = match &fs.condition {
                Some(c) => Some(construct_expr(c, None, ctx)?),
                None => None,
            };
            let step = match &fs.step {
                Some(s) => Some(construct_expr(s, None, ctx)?),
                None => None,
            };
            ctx.enter_loop();
            let body = construct_block(&fs.body, None, ctx)?;
            ctx.exit_loop();
            ctx.pop_scope();
            Ok(TypedStmt::For(Box::new(TypedForStmt {
                init,
                condition,
                step,
                body,
                span: fs.span.clone(),
            })))
        }
        Stmt::ForIn(fi) => {
            let iterable = construct_expr(&fi.iterable, None, ctx)?;
            let elem_ty = match peel_type_references(iterable.ty()) {
                Type::Array(elem) | Type::SizedArray(elem, _) => *elem.clone(),
                Type::Named(name, _) if name == "Range" => Type::I64,
                Type::Named(type_name, type_args) => {
                    // User-defined Iterable: derive elem type from next() -> Perhaps<T>.
                    // Concrete-impl method_env first; fall back to the polymorphic
                    // method_scheme_env (mirrors the method-call construction path
                    // above) for a generic struct implementing Iterable<T> generically
                    // -- e.g. `extend<T> Wrapper<T>: Iterable<T> { ... }` -- whose
                    // `next` is only registered there, not in method_env.
                    let next_ret = ctx
                        .method_env
                        .get(type_name.as_str())
                        .and_then(|m| m.get("next"))
                        .and_then(|ty| {
                            if let Type::Fun(_, ret) = ty {
                                Some(ret.as_ref().clone())
                            } else {
                                None
                            }
                        })
                        .or_else(|| {
                            let (scheme, struct_tvars) =
                                ctx.registry.method_scheme_for(type_name.as_str(), "next")?;
                            let mut subst = Substitution::new();
                            for (&tv, concrete) in struct_tvars.iter().zip(type_args.iter()) {
                                subst.bind(tv, type_to_infer(concrete));
                            }
                            match subst.apply(&scheme.ty) {
                                InferType::Fun(_, ret) => {
                                    let dummy = Span::new(0, 0, "");
                                    infer_type_to_type(&ret, &dummy).ok()
                                }
                                _ => None,
                            }
                        });
                    match next_ret {
                        Some(Type::Named(n, mut args)) if n == "Perhaps" && args.len() == 1 => {
                            args.remove(0)
                        }
                        _ => {
                            return Err(MetelError::internal(format!(
                                "for-in: `{type_name}` has no `next() -> Perhaps<T>` method"
                            )))
                        }
                    }
                }
                _ => return Err(MetelError::internal("for-in over non-iterable type")),
            };
            ctx.push_scope();
            ctx.bind(&fi.binding, elem_ty);
            ctx.enter_loop();
            let body = construct_block(&fi.body, None, ctx)?;
            ctx.exit_loop();
            ctx.pop_scope();
            Ok(TypedStmt::ForIn(Box::new(TypedForInStmt {
                binding: fi.binding.clone(),
                mutable: fi.mutable,
                iterable,
                body,
                span: fi.span.clone(),
            })))
        }
    }
}

// Exhaustive match over every AST/type-system variant; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
fn construct_expr(
    expr: &Expr,
    expected_ty: Option<&Type>,
    ctx: &mut ConstructCtx,
) -> Result<TypedExpr, MetelError> {
    match expr {
        Expr::Literal(lit, span) => {
            let ty = construct_literal_type(lit, expected_ty, span)?;
            Ok(TypedExpr::Literal(lit.clone(), ty, span.clone()))
        }
        Expr::Ident(name, span) => {
            if let Some(ty) = ctx.lookup(name).cloned() {
                return Ok(TypedExpr::Ident(name.clone(), ty, span.clone()));
            }
            if let Some(fields) = ctx.get_struct_fields(name) {
                if fields.is_empty() {
                    let ty = if let Some(Type::Named(expected_name, _)) = expected_ty {
                        if expected_name == name {
                            expected_ty
                                .cloned()
                                .unwrap_or_else(|| Type::Named(name.clone(), vec![]))
                        } else {
                            Type::Named(name.clone(), vec![])
                        }
                    } else {
                        Type::Named(name.clone(), vec![])
                    };
                    return Ok(TypedExpr::StructLiteral {
                        path: vec![name.clone()],
                        fields: vec![],
                        ty,
                        type_id: ctx.type_symbol_id(name),
                        span: span.clone(),
                    });
                }
            }
            if ctx.can_be_unqualified_variant(name) {
                return resolve_unqualified_variant_expr(name, expected_ty, span, ctx);
            }
            // #736: `name` may be a real, checked declaration -- a generic
            // function's own scheme is deliberately kept out of `ctx.env`
            // (see the `GenericClosure` construction above for why: call
            // sites resolve it through `scheme_env` instead), so a bare,
            // non-call reference to it reaches here. It genuinely exists;
            // `undefined name` would be a lie. Generic functions are
            // call-only today (`functions.md`'s first-class-functions
            // carve-out; RFC-0138 proposes lifting this) -- say so instead.
            if let Some(scheme) = ctx.scheme_env.get(name.as_str()) {
                if !scheme.quantified_vars.is_empty() {
                    // metel-core#736 / RFC-0138 §4: a concrete expected type here
                    // (a higher-order call argument whose own parameter position is
                    // monomorphic, or an explicitly-annotated `let`) instantiates
                    // this one reference once, at this one use site -- rank-1, not
                    // let-polymorphism, so no `GenericClosure`/`fn_table` lookup is
                    // needed: `instantiate_scheme_for_call` (the same helper a
                    // direct call already uses) unifies `expected_ty`'s own param
                    // types against the scheme exactly as if they were argument
                    // types.
                    if let Some(Type::Fun(expected_params, _)) = expected_ty {
                        let arity_matches = matches!(
                            &scheme.ty,
                            InferType::Fun(p, _) if p.len() == expected_params.len()
                        );
                        if arity_matches {
                            let arg_types: Vec<&Type> = expected_params.iter().collect();
                            if let Ok((concrete, var_map)) = instantiate_scheme_for_call(
                                scheme,
                                &arg_types,
                                span,
                                &mut ctx.gen,
                                ctx.registry,
                                ctx.current_module,
                            ) {
                                check_fun_call_bounds(
                                    name,
                                    &var_map,
                                    span,
                                    ctx.registry,
                                    ctx.current_module,
                                )?;
                                check_scheme_bounds(
                                    name,
                                    scheme,
                                    &var_map,
                                    span,
                                    ctx.registry,
                                    ctx.current_module,
                                )?;
                                check_fun_call_assoc_eq(
                                    name,
                                    &var_map,
                                    span,
                                    ctx.registry,
                                    ctx.current_module,
                                )?;
                                check_scheme_assoc_eq(
                                    name,
                                    scheme,
                                    &var_map,
                                    span,
                                    ctx.registry,
                                    ctx.current_module,
                                )?;
                                check_fun_call_neg_bounds(
                                    name,
                                    &var_map,
                                    span,
                                    ctx.registry,
                                    ctx.current_module,
                                )?;
                                check_scheme_neg_bounds(
                                    name,
                                    scheme,
                                    &var_map,
                                    span,
                                    ctx.registry,
                                    ctx.current_module,
                                )?;
                                return Ok(TypedExpr::Ident(name.clone(), concrete, span.clone()));
                            }
                        }
                    }
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!(
                            "generic function `{name}` cannot be referenced except by \
                             direct call; a generic function is not yet a first-class \
                             value (RFC-0138)"
                        ),
                        span,
                    ));
                }
            }
            Err(MetelError::type_error(
                TypeErrorCode::T0003,
                format!("undefined name `{name}`"),
                span,
            ))
        }
        Expr::ResolvedPath {
            resolved,
            original,
            symbol_id: _,
            span,
        } => {
            let ty = ctx.lookup(resolved).cloned().ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("undefined name `{}`", original.join("::")),
                    span,
                )
            })?;
            Ok(TypedExpr::Ident(resolved.clone(), ty, span.clone()))
        }
        Expr::BinOp(lhs, op, rhs, span) => construct_binop(lhs, op, rhs, span, ctx),
        Expr::UnaryOp(op, operand, span) => {
            // For negation, propagate expected_ty to the operand so `-100` in
            // `let x: i8 = -100` resolves to i8. Unsigned targets are excluded:
            // negation of an unsigned value is a type error that must stay detectable.
            let inner_hint = if matches!(op, UnaryOp::Neg) {
                match expected_ty {
                    Some(Type::U8 | Type::U16 | Type::U32 | Type::U64) => None,
                    other => other,
                }
            } else {
                None
            };
            construct_unaryop(op, operand, span, inner_hint, ctx)
        }
        Expr::Tuple(elems, span) => {
            let typed: Vec<TypedExpr> = elems
                .iter()
                .map(|e| construct_expr(e, None, ctx))
                .collect::<Result<_, _>>()?;
            let ty = Type::Tuple(typed.iter().map(|e| e.ty().clone()).collect());
            Ok(TypedExpr::Tuple(typed, ty, span.clone()))
        }
        Expr::RecordLiteral { fields, span } => {
            let expected_record = match expected_ty {
                Some(Type::Record(fields)) => Some(fields),
                _ => None,
            };
            let mut typed_fields = Vec::with_capacity(fields.len());
            for (name, expr) in fields {
                let hint = expected_record.and_then(|expected_fields| {
                    expected_fields
                        .iter()
                        .find(|(expected_name, _)| expected_name == name)
                        .map(|(_, ty)| ty)
                });
                typed_fields.push((name.clone(), construct_expr(expr, hint, ctx)?));
            }
            let ty = expected_record.map_or_else(
                || {
                    Type::Record(
                        typed_fields
                            .iter()
                            .map(|(name, expr)| (name.clone(), expr.ty().clone()))
                            .collect(),
                    )
                },
                |expected_fields| Type::Record(expected_fields.clone()),
            );
            Ok(TypedExpr::RecordLiteral {
                fields: typed_fields,
                ty,
                span: span.clone(),
            })
        }
        Expr::Array(elems, span) => {
            if elems.is_empty() {
                let ty = expected_ty.cloned().ok_or_else(|| {
                    MetelError::type_error(
                        TypeErrorCode::T0002,
                        "cannot infer element type of empty array; add a type annotation",
                        span,
                    )
                })?;
                return Ok(TypedExpr::Array(vec![], ty, span.clone()));
            }
            // When the expected type is SizedArray, validate element count and use that type.
            if let Some(Type::SizedArray(expected_elem, n)) = expected_ty {
                if elems.len() as u64 != *n {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0001,
                        format!("expected array of {} element(s), got {}", n, elems.len()),
                        span,
                    ));
                }
                let typed: Vec<TypedExpr> = elems
                    .iter()
                    .map(|e| construct_expr(e, Some(expected_elem.as_ref()), ctx))
                    .collect::<Result<_, _>>()?;
                let ty = Type::SizedArray(expected_elem.clone(), *n);
                return Ok(TypedExpr::Array(typed, ty, span.clone()));
            }
            // When expected type is Array(T), propagate element type hint.
            if let Some(Type::Array(expected_elem)) = expected_ty {
                let typed: Vec<TypedExpr> = elems
                    .iter()
                    .map(|e| construct_expr(e, Some(expected_elem.as_ref()), ctx))
                    .collect::<Result<_, _>>()?;
                let ty = Type::Array(expected_elem.clone());
                return Ok(TypedExpr::Array(typed, ty, span.clone()));
            }
            let typed: Vec<TypedExpr> = elems
                .iter()
                .map(|e| construct_expr(e, None, ctx))
                .collect::<Result<_, _>>()?;
            let elem_ty = typed[0].ty().clone();
            let ty = Type::Array(Box::new(elem_ty));
            Ok(TypedExpr::Array(typed, ty, span.clone()))
        }
        Expr::RepeatArray(elem, n, span) => {
            let elem_hint: Option<&Type> = match expected_ty {
                Some(Type::SizedArray(elem_ty, _) | Type::Array(elem_ty)) => Some(elem_ty.as_ref()),
                _ => None,
            };
            let typed_elem = construct_expr(elem, elem_hint, ctx)?;
            let elem_ty = typed_elem.ty().clone();
            let ty = Type::SizedArray(Box::new(elem_ty), *n);
            Ok(TypedExpr::RepeatArray(
                Box::new(typed_elem),
                *n,
                ty,
                span.clone(),
            ))
        }
        Expr::Call {
            callee,
            type_args,
            args,
            span,
        } => construct_call(callee, type_args, args, span, expected_ty, ctx),
        Expr::Index {
            object,
            index,
            span,
        } => {
            let typed_obj = construct_expr(object, None, ctx)?;
            let typed_idx = construct_expr(index, Some(&Type::U64), ctx)?;
            if typed_idx.ty() != &Type::U64 {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0001,
                    format!(
                        "array index must be u64, got {}; use `expr as u64`",
                        typed_idx.ty()
                    ),
                    span,
                ));
            }
            if matches!(peel_type_references(typed_obj.ty()), Type::SizedArray(_, 0))
                && matches!(
                    index.as_ref(),
                    Expr::Literal(Literal::Int(_) | Literal::SizedInt { .. }, _)
                )
            {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0001,
                    "cannot index an empty fixed-size array with a literal index",
                    span,
                ));
            }
            let elem_ty = match peel_type_references(typed_obj.ty()) {
                Type::Array(elem) | Type::SizedArray(elem, _) => *elem.clone(),
                _ => {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0001,
                        "indexed value is not an array",
                        span,
                    ))
                }
            };
            Ok(TypedExpr::Index {
                object: Box::new(typed_obj),
                index: Box::new(typed_idx),
                ty: elem_ty,
                span: span.clone(),
            })
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
            span,
        } => {
            let condition = construct_expr(condition, None, ctx)?;
            let then_branch = construct_block(then_branch, expected_ty, ctx)?;
            let (else_branch, ty) = match else_branch {
                Some(eb) => {
                    let typed_else = construct_block(eb, expected_ty, ctx)?;
                    // RFC-0078: prefer whichever branch's type isn't `!` — a
                    // diverging branch (e.g. a `return`-only `then`) must not mask
                    // the other branch's real type; only `!` if both diverge.
                    let ty = merge_branch_types(&[
                        block_result_type(&then_branch),
                        block_result_type(&typed_else),
                    ]);
                    (Some(typed_else), ty)
                }
                None => (None, Type::Unit),
            };
            Ok(TypedExpr::If {
                condition: Box::new(condition),
                then_branch,
                else_branch,
                ty,
                span: span.clone(),
            })
        }
        Expr::Assign {
            target,
            op,
            value,
            span,
        } => {
            // RFC-0110 §4.2: bare assignment to an identifier rebinds. The value's
            // expected type is simply the binding's own type — no peeling. Writing
            // through is spelled `*p = v` (AssignTarget::Deref), one `*` per layer.
            let value_hint: Option<Type> = match target {
                AssignTarget::Ident(name, _) => ctx.lookup(name).cloned(),
                AssignTarget::Deref { object, .. } => match construct_expr(object, None, ctx) {
                    Ok(o) => match o.ty() {
                        Type::MutReference(inner) => Some((**inner).clone()),
                        _ => None,
                    },
                    Err(_) => None,
                },
                _ => None,
            };
            let typed_value = construct_expr(value, value_hint.as_ref(), ctx)?;
            let typed_place = assign_target_to_typed_place(target, ctx)?;
            let _ = typed_place_ty(&typed_place, ctx, span)?;
            Ok(TypedExpr::Assign {
                target: typed_place,
                op: op.clone(),
                value: Box::new(typed_value),
                ty: Type::Unit,
                span: span.clone(),
            })
        }
        Expr::FieldAccess {
            object,
            field,
            span,
        } => {
            let typed_obj = construct_expr(object, None, ctx)?;
            let peeled = peel_type_references(typed_obj.ty());
            if let Type::Record(fields) = peeled {
                let field_ty = fields
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, ty)| ty.clone())
                    .ok_or_else(|| {
                        MetelError::internal(format!("no field `{field}` on {peeled}"))
                    })?;
                return Ok(TypedExpr::FieldAccess {
                    object: Box::new(typed_obj),
                    field: field.clone(),
                    ty: field_ty,
                    span: span.clone(),
                });
            }
            let (struct_name, type_args) = match peeled {
                Type::Named(name, args) => (name.clone(), args.clone()),
                t => {
                    return Err(MetelError::internal(format!(
                        "field access on non-struct type {t}"
                    )))
                }
            };
            let field_ty = if let Some(type_params) =
                ctx.registry.raw_struct_type_params().get(&struct_name)
            {
                // Generic struct: look up raw InferType field, build remap, apply, convert.
                let raw_fields =
                    ctx.registry
                        .raw_struct_env()
                        .get(&struct_name)
                        .ok_or_else(|| {
                            MetelError::internal(format!("missing raw fields for `{struct_name}`"))
                        })?;
                let raw_ty = raw_fields
                    .iter()
                    .find(|entry| entry.name == *field)
                    .map(|entry| entry.ty.clone())
                    .ok_or_else(|| {
                        MetelError::internal(format!("no field `{field}` on `{struct_name}`"))
                    })?;
                let mut remap = Substitution::new();
                for (&tp, arg) in type_params.iter().zip(type_args.iter()) {
                    remap.bind(tp, type_to_infer(arg));
                }
                infer_type_to_type(&remap.apply(&raw_ty), span)?
            } else {
                ctx.get_struct_fields(&struct_name)
                    .and_then(|fs| fs.iter().find(|(name, _, _)| name == field))
                    .map(|(_, ty, _)| ty.clone())
                    .ok_or_else(|| {
                        MetelError::internal(format!("no field `{field}` on `{struct_name}`"))
                    })?
            };
            Ok(TypedExpr::FieldAccess {
                object: Box::new(typed_obj),
                field: field.clone(),
                ty: field_ty,
                span: span.clone(),
            })
        }
        Expr::MethodCall {
            receiver,
            method,
            type_args,
            args,
            span,
        } => {
            let typed_receiver = construct_expr(receiver, None, ctx)?;
            // Peel references before the builtin-pattern gate, matching what the
            // scheme-based path below already does and what
            // `builtin_pattern_method_expr` itself checks. Without this, `.len()`
            // on a `&T[]` fell past the builtin and into the scheme lookup, which
            // has no entry for it, so the diagnostic claimed the method did not
            // exist on arrays at all (#314).
            //
            // `args.is_empty()` guards the *construction* below, not the builtin
            // match: every builtin pattern is nullary, so a call with arguments can
            // never match one, and constructing its arguments here only to discard
            // them advances `ctx.gen` — the shared TypeVar generator — a second
            // time before the real path constructs them again. That shifts `?tN`
            // numbering for everything downstream (#307).
            if args.is_empty()
                && matches!(
                    peel_type_references(typed_receiver.ty()),
                    Type::Array(_) | Type::SizedArray(_, _)
                )
            {
                let typed_args: Vec<TypedExpr> = args
                    .iter()
                    .map(|arg| construct_expr(arg, None, ctx))
                    .collect::<Result<_, _>>()?;
                if let Some(result) =
                    builtin_pattern_method_expr(typed_receiver.clone(), method, typed_args, span)
                {
                    return result;
                }
            }
            // Resolve explicit method type args once.
            let explicit_method_tys: Option<Vec<Type>> = if type_args.is_empty() {
                None
            } else {
                Some(
                    type_args
                        .iter()
                        .map(|te| infer_type_to_type(&ctx.type_expr_to_infer_ctx(te), span))
                        .collect::<Result<_, _>>()?,
                )
            };

            // Resolve the method's function type and construct the arguments.
            if let Type::Array(elem) | Type::SizedArray(elem, _) =
                peel_type_references(typed_receiver.ty())
            {
                let candidates = ctx
                    .registry
                    .array_method_scheme_variants_for(method)
                    .to_vec();
                if candidates.is_empty() {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!("no method `{method}` on array type"),
                        span,
                    ));
                }
                let receiver_type_args = [elem.as_ref().clone()];
                let (method_fun_ty, typed_args, winning_aspect) = resolve_generic_method_call(
                    &candidates,
                    &receiver_type_args,
                    explicit_method_tys.as_deref(),
                    args,
                    method,
                    span,
                    ctx,
                )?;
                if matches!(
                    ctx.registry.array_method_receiver_kind(method),
                    Some(crate::ast::ReceiverKind::RefMut)
                ) && !type_chain_provides_mut_access(typed_receiver.ty())
                {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0008,
                        format!(
                            "cannot call `&var self` method `{method}` through shared receiver"
                        ),
                        span,
                    ));
                }
                let ret_ty = match method_fun_ty {
                    Type::Fun(_, ret) => *ret,
                    _ => return Err(MetelError::internal("array method type is not a function")),
                };
                let dispatch = dispatch_for_resolved_method(ctx, winning_aspect.as_deref());
                return Ok(TypedExpr::MethodCall {
                    receiver: Box::new(typed_receiver),
                    method: method.clone(),
                    args: typed_args,
                    ty: ret_ty,
                    dispatch,
                    span: span.clone(),
                });
            }

            if matches!(peel_type_references(typed_receiver.ty()), Type::Never) {
                // Receiver's type is unknowable -- e.g. `construct_generic_body`'s
                // call-time reconstruction sampling an empty collection's element
                // type (issue #271), or a genuinely diverging receiver expression.
                // Either way nothing here actually runs, but construction must
                // still produce a valid typed node: skip static method resolution
                // and defer to runtime dynamic dispatch, which resolves by the
                // receiver's real value/kind (see `eval_method_call_expr`), not by
                // this placeholder type.
                let typed_args = args
                    .iter()
                    .map(|a| construct_expr(a, None, ctx))
                    .collect::<Result<_, _>>()?;
                return Ok(TypedExpr::MethodCall {
                    receiver: Box::new(typed_receiver),
                    method: method.clone(),
                    args: typed_args,
                    ty: Type::Never,
                    dispatch: crate::typed_ast::MethodDispatch::Dynamic,
                    span: span.clone(),
                });
            }
            let (struct_name, receiver_type_args) = match peel_type_references(typed_receiver.ty())
            {
                Type::Named(name, targs) => (name.clone(), targs.clone()),
                t => match super::inference::primitive_type_name(t) {
                    Some(name) => (name, vec![]),
                    None => {
                        return Err(MetelError::internal(format!(
                            "method call on non-struct type {t}"
                        )))
                    }
                },
            };

            // Resolve the method's function type and construct the arguments.
            // Two cases: a concrete method already in method_env (fast path), or a
            // polymorphic scheme on a generic struct/enum (slow path).
            let (method_fun_ty, typed_args, dispatch): (Type, Vec<TypedExpr>, MethodDispatch) =
                if let Some(ty) = ctx
                    .method_env
                    .get(&struct_name)
                    .and_then(|m| m.get(method.as_str()))
                    .cloned()
                {
                    if explicit_method_tys.is_some() {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0004,
                            format!("method `{method}` on `{struct_name}` has no type parameters"),
                            span,
                        ));
                    }
                    let typed_args = construct_method_args(&ty, args, ctx)?;
                    (ty, typed_args, MethodDispatch::Dynamic)
                } else {
                    // Slow path: method on a generic struct/enum — look up the polymorphic
                    // scheme(s) and instantiate against the receiver's concrete type
                    // arguments. More than one candidate can be registered here (issue
                    // #272: different aspects providing the same method name for the
                    // same generic target) -- try each and use the one whose bounds the
                    // receiver's concrete type args actually satisfy.
                    let candidates = ctx
                        .registry
                        .method_scheme_variants_for(&struct_name, method)
                        .to_vec();
                    if candidates.is_empty() {
                        return Err(MetelError::internal(format!(
                            "no method `{method}` on `{struct_name}`"
                        )));
                    }
                    let (ty, typed_args, winning_aspect) = resolve_generic_method_call(
                        &candidates,
                        &receiver_type_args,
                        explicit_method_tys.as_deref(),
                        args,
                        method,
                        span,
                        ctx,
                    )?;
                    let dispatch = dispatch_for_resolved_method(ctx, winning_aspect.as_deref());
                    (ty, typed_args, dispatch)
                };
            let ret_ty = match method_fun_ty {
                Type::Fun(_, ret) => *ret,
                _ => return Err(MetelError::internal("method type is not a function")),
            };
            Ok(TypedExpr::MethodCall {
                receiver: Box::new(typed_receiver),
                method: method.clone(),
                args: typed_args,
                ty: ret_ty,
                dispatch,
                span: span.clone(),
            })
        }
        Expr::StructLiteral {
            path,
            fields,
            symbol_id,
            span,
        } => {
            let resolved_path = if path.len() == 1
                && ctx.can_be_unqualified_variant(&path[0])
                && !ctx.has_struct_named(&path[0])
            {
                let expected_ty = expected_ty
                    .ok_or_else(|| unqualified_variant_needs_annotation_error(&path[0], span))?;
                let (enum_name, enum_info) = resolve_expected_enum(Some(expected_ty), span, ctx)?;
                if enum_info
                    .variants
                    .iter()
                    .any(|variant| variant.name == path[0])
                {
                    vec![enum_name.clone(), path[0].clone()]
                } else {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0001,
                        format!("cannot unify `{}` with `{expected_ty}`", path[0]),
                        span,
                    ));
                }
            } else {
                path.clone()
            };
            // Look up field type hints from the struct definition for non-generic structs.
            // Clone to release the borrow on ctx before calling construct_expr below.
            let type_name = resolved_path.last().map_or("", std::string::String::as_str);
            let field_hints: HashMap<String, Type> = ctx
                .get_struct_fields(type_name)
                .map(|fs| fs.iter().map(|(n, t, _)| (n.clone(), t.clone())).collect())
                .unwrap_or_default();
            let typed_fields: Vec<(String, TypedExpr)> = fields
                .iter()
                .map(|(name, expr)| {
                    let hint = field_hints.get(name.as_str());
                    Ok((name.clone(), construct_expr(expr, hint, ctx)?))
                })
                .collect::<Result<_, _>>()?;

            let ty = if resolved_path.len() == 2 {
                construct_enum_literal_ty(
                    &resolved_path[0],
                    &resolved_path[1],
                    &typed_fields,
                    expected_ty,
                    span,
                    ctx,
                )?
            } else {
                let type_name = resolved_path.last().unwrap();
                if let Some(type_params) = ctx.registry.raw_struct_type_params().get(type_name) {
                    // Generic struct: infer type args from the typed field values.
                    let raw_fields = ctx
                        .registry
                        .raw_struct_env()
                        .get(type_name.as_str())
                        .ok_or_else(|| {
                            MetelError::internal(format!("missing raw fields for `{type_name}`"))
                        })?;
                    let mut remap: HashMap<TypeVar, InferType> = HashMap::new();
                    for &tp in type_params {
                        remap.entry(tp).or_insert_with(|| InferType::Var(tp));
                    }
                    // Match each field value type to its raw InferType param; resolve via subst.
                    for (fname, fexpr) in &typed_fields {
                        if let Some(field) = raw_fields.iter().find(|entry| entry.name == *fname) {
                            if let InferType::Var(v) = &field.ty {
                                if type_params.contains(v) {
                                    remap.insert(*v, type_to_infer(fexpr.ty()));
                                }
                            }
                        }
                    }
                    let type_args: Vec<Type> = type_params
                        .iter()
                        .map(|tp| {
                            let it = remap.get(tp).cloned().unwrap_or(InferType::Var(*tp));
                            infer_type_to_type(&ctx.subst.apply(&it), span)
                        })
                        .collect::<Result<_, _>>()?;
                    // T0012: check each resolved type arg satisfies the declared bounds.
                    let generic_types_by_name: HashMap<String, Type> = ctx
                        .registry
                        .struct_generic_names_for(type_name)
                        .into_iter()
                        .flatten()
                        .cloned()
                        .zip(type_args.iter().cloned())
                        .collect();
                    let record_kinds = ctx
                        .get_type_param_record_kinds(type_name)
                        .cloned()
                        .unwrap_or_else(|| vec![false; type_args.len()]);
                    if let Some(param_bounds) = ctx.registry.type_param_bounds_for(type_name) {
                        for (i, bounds) in param_bounds.iter().enumerate() {
                            let record_kind = record_kinds.get(i).copied().unwrap_or(false);
                            if bounds.is_empty() && !record_kind {
                                continue;
                            }
                            let Some(arg) = type_args.get(i) else {
                                continue;
                            };
                            check_type_satisfies_bounds(
                                arg,
                                bounds,
                                record_kind,
                                type_name,
                                span,
                                ctx.registry,
                                ctx.current_module,
                                &generic_types_by_name,
                            )?;
                        }
                    }
                    // T0012 negative bounds: check each resolved type arg does NOT
                    // implement the declared negative bounds (RFC-0072, issue #243).
                    if let Some(neg_param_bounds) =
                        ctx.registry.neg_type_param_bounds_for(type_name)
                    {
                        for (i, neg_bounds) in neg_param_bounds.iter().enumerate() {
                            let record_kind = record_kinds.get(i).copied().unwrap_or(false);
                            if neg_bounds.is_empty() && !record_kind {
                                continue;
                            }
                            let Some(arg) = type_args.get(i) else {
                                continue;
                            };
                            check_type_does_not_satisfy_bound(
                                arg,
                                neg_bounds,
                                record_kind,
                                type_name,
                                span,
                                ctx.registry,
                                ctx.current_module,
                                &generic_types_by_name,
                            )?;
                        }
                    }
                    Type::Named(type_name.clone(), type_args)
                } else {
                    Type::Named(type_name.clone(), vec![])
                }
            };

            // Resolve the constructed type's stable identity. A module-qualified
            // literal carries its resolver-stamped id (correct across modules with
            // same-named types); otherwise derive it from the declaring-module index
            // (struct name, or the enum name for a 2-segment `Enum::Variant` literal).
            let type_id = symbol_id.or_else(|| {
                if resolved_path.len() == 2 {
                    ctx.type_symbol_id(&resolved_path[0])
                } else {
                    ctx.type_symbol_id(resolved_path.last().unwrap())
                }
            });

            Ok(TypedExpr::StructLiteral {
                path: resolved_path,
                fields: typed_fields,
                ty,
                type_id,
                span: span.clone(),
            })
        }
        Expr::RecordProjection { path, fields, span } => {
            let base_expr = if path.len() == 1 {
                Expr::Ident(path[0].clone(), span.clone())
            } else {
                Expr::Path(path.clone(), span.clone())
            };
            let typed_base = construct_expr(&base_expr, None, ctx)?;
            let (struct_name, type_args) = match peel_type_references(typed_base.ty()) {
                Type::Named(name, args) => (name.clone(), args.clone()),
                other => {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0002,
                        format!("record projection requires a nominal struct value, got {other}"),
                        span,
                    ))
                }
            };
            let mut projected_fields = Vec::with_capacity(fields.len());
            let mut record_ty = Vec::with_capacity(fields.len());
            for field in fields {
                let field_ty = if let Some(type_params) =
                    ctx.registry.raw_struct_type_params().get(&struct_name)
                {
                    let raw_fields =
                        ctx.registry
                            .raw_struct_env()
                            .get(&struct_name)
                            .ok_or_else(|| {
                                MetelError::internal(format!(
                                    "missing raw fields for `{struct_name}`"
                                ))
                            })?;
                    let raw_ty = raw_fields
                        .iter()
                        .find(|entry| entry.name == *field)
                        .map(|entry| entry.ty.clone())
                        .ok_or_else(|| {
                            MetelError::type_error(
                                TypeErrorCode::T0003,
                                format!("no field `{field}` on `{struct_name}`"),
                                span,
                            )
                        })?;
                    let mut remap = Substitution::new();
                    for (&tp, arg) in type_params.iter().zip(type_args.iter()) {
                        remap.bind(tp, type_to_infer(arg));
                    }
                    infer_type_to_type(&remap.apply(&raw_ty), span)?
                } else {
                    ctx.get_struct_fields(&struct_name)
                        .and_then(|entries| entries.iter().find(|(name, _, _)| name == field))
                        .map(|(_, ty, _)| ty.clone())
                        .ok_or_else(|| {
                            MetelError::type_error(
                                TypeErrorCode::T0003,
                                format!("no field `{field}` on `{struct_name}`"),
                                span,
                            )
                        })?
                };
                record_ty.push((field.clone(), field_ty.clone()));
                projected_fields.push((
                    field.clone(),
                    TypedExpr::FieldAccess {
                        object: Box::new(typed_base.clone()),
                        field: field.clone(),
                        ty: field_ty,
                        span: span.clone(),
                    },
                ));
            }
            Ok(TypedExpr::RecordLiteral {
                fields: projected_fields,
                ty: Type::Record(record_ty),
                span: span.clone(),
            })
        }
        Expr::Path(segments, span) => {
            // For 2-segment paths, try method_env first (static methods, enum variant constructors).
            if let [type_name, member_name] = segments.as_slice() {
                if let Some(ty) = ctx
                    .method_env
                    .get(type_name.as_str())
                    .and_then(|m| m.get(member_name.as_str()))
                    .cloned()
                {
                    return Ok(TypedExpr::Path(segments.clone(), ty, span.clone()));
                }
                // Also check enum variants via enum_env.
                if let Some(info) = ctx.registry.enum_info(type_name.as_str()) {
                    if let Some(variant) = info.variants.iter().find(|v| &v.name == member_name) {
                        if variant.fields.is_empty() {
                            // A unit enum variant is a value, not a constructor: emit it as
                            // a (field-less) struct literal so it carries the enum's type
                            // SymbolId onto the runtime value, like any other constructor
                            // (METEL-185). The evaluator builds `Value::Enum` from a
                            // 2-segment struct-literal path.
                            return Ok(TypedExpr::StructLiteral {
                                path: segments.clone(),
                                fields: vec![],
                                ty: Type::Named(type_name.clone(), vec![]),
                                type_id: ctx.type_symbol_id(type_name),
                                span: span.clone(),
                            });
                        }
                        let field_types: Vec<Type> = variant
                            .fields
                            .iter()
                            .map(|field| infer_type_to_type(&field.ty, span))
                            .collect::<Result<_, _>>()?;
                        let ty = Type::Fun(
                            field_types,
                            Box::new(Type::Named(type_name.clone(), vec![])),
                        );
                        return Ok(TypedExpr::Path(segments.clone(), ty, span.clone()));
                    }
                }
            }
            Err(MetelError::internal(format!(
                "unresolved path `{}`",
                segments.join("::")
            )))
        }
        Expr::Closure {
            params,
            return_type,
            body,
            span,
        } => {
            let param_types: Vec<Type> = params
                .iter()
                .map(|p| {
                    p.type_ann.as_ref().map_or_else(
                        || {
                            Err(MetelError::type_error(
                                TypeErrorCode::T0002,
                                format!("closure parameter `{}` needs a type annotation", p.name),
                                &p.span,
                            ))
                        },
                        |ann| {
                            resolved_to_type(&ctx.type_expr_to_infer_ctx(ann), ctx.subst, &p.span)
                        },
                    )
                })
                .collect::<Result<_, _>>()?;
            let ret_ty = if let Some(inferred) = ctx
                .closure_return_types
                .and_then(|types| types.get(span))
                .map(|ty| ctx.subst.apply(ty))
            {
                infer_type_to_type(&inferred, span)?
            } else {
                return_type
                    .as_ref()
                    .map(|ann| resolved_to_type(&ctx.type_expr_to_infer_ctx(ann), ctx.subst, span))
                    .transpose()?
                    .unwrap_or(Type::Unit)
            };
            ctx.push_scope();
            for (p, ty) in params.iter().zip(param_types.iter()) {
                ctx.bind(&p.name, ty.clone());
            }
            // Without this, unmentioned type params in variant literals (e.g. the
            // E in Result::Ok inside a ()->Result<T,E>) have no hint and fail T0002.
            let body_expected = Some(&ret_ty);
            // Push the closure's own return type so an explicit `return` inside its
            // body (constructed via `construct_stmt`'s `Stmt::Return` arm) compares
            // against the closure's declared type, not whatever enclosing function's
            // return type happened to be in scope (RFC-0067a's read-copy relies on
            // this being correct — without it, `return`ing a reference out of a
            // closure declared to return the referent type silently skipped the copy).
            let saved_return = ctx.push_return_type(Some(ret_ty.clone()));
            let saved_loop_depth = ctx.push_loop_depth_reset();
            let typed_body = construct_block(body, body_expected, ctx)?;
            ctx.pop_loop_depth(saved_loop_depth);
            ctx.pop_return_type(saved_return);
            ctx.pop_scope();
            let ty = Type::Fun(param_types, Box::new(ret_ty));
            Ok(TypedExpr::Closure {
                params: params.clone(),
                return_type: return_type.clone(),
                body: typed_body,
                ty,
                span: span.clone(),
            })
        }
        Expr::Match(m) => construct_match(m, expected_ty, ctx),
        Expr::PropagateError { expr, span } => construct_propagate_error(expr, span, ctx),
        Expr::Ascribe { expr, ann, span } => {
            let ty = resolved_to_type(&ctx.type_expr_to_infer_ctx(ann), ctx.subst, span)?;
            let constructed = construct_expr(expr, Some(&ty), ctx)?;
            let constructed =
                maybe_read_copy(&ty, constructed, span, ctx.registry, ctx.current_module)?;
            maybe_singleton_coerce(&ty, constructed, span, ctx.registry)
        }

        Expr::Cast {
            expr,
            target_type,
            span,
        } => {
            let typed_expr = construct_expr(expr, None, ctx)?;
            let ty = resolved_to_type(&ctx.type_expr_to_infer_ctx(target_type), ctx.subst, span)?;
            Ok(TypedExpr::Cast {
                expr: Box::new(typed_expr),
                target_type: target_type.clone(),
                ty,
                span: span.clone(),
            })
        }

        Expr::TupleAccess {
            object,
            index,
            span,
        } => {
            let typed_obj = construct_expr(object, None, ctx)?;
            let ty = match peel_type_references(typed_obj.ty()) {
                Type::Tuple(elems) => elems.get(*index).cloned().ok_or_else(|| {
                    MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!(
                            "tuple index {index} out of bounds (tuple has {} elements)",
                            elems.len()
                        ),
                        span,
                    )
                })?,
                _ => {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0002,
                        "cannot infer tuple type for index access; add a type annotation",
                        span,
                    ))
                }
            };
            Ok(TypedExpr::TupleAccess {
                object: Box::new(typed_obj),
                index: *index,
                ty,
                span: span.clone(),
            })
        }
        Expr::Loop { body, span } => {
            let saved_break = ctx.push_break_type(expected_ty.cloned());
            ctx.enter_loop();
            let typed_body = construct_block(body, None, ctx)?;
            ctx.exit_loop();
            ctx.pop_break_type(saved_break);
            let ty = find_loop_break_type(&typed_body).unwrap_or(Type::Never);
            Ok(TypedExpr::Loop {
                body: typed_body,
                ty,
                span: span.clone(),
            })
        }
        // Issue #229: `return`/`break`/`continue` as expressions of type `!`,
        // reachable anywhere (not just as a braced statement). Direct port of
        // the former `Stmt::Return`/`Break`/`Continue` construction.
        Expr::Return(r) => {
            let return_ty = ctx.current_return_ty.clone();
            let value = match &r.value {
                Some(e) => {
                    let constructed = construct_expr(e, return_ty.as_ref(), ctx)?;
                    Some(Box::new(match &return_ty {
                        Some(t) => {
                            let constructed = maybe_read_copy(
                                t,
                                constructed,
                                e.span(),
                                ctx.registry,
                                ctx.current_module,
                            )?;
                            maybe_singleton_coerce(t, constructed, e.span(), ctx.registry)?
                        }
                        None => constructed,
                    }))
                }
                None => None,
            };
            Ok(TypedExpr::Return(TypedReturnExpr {
                value,
                span: r.span.clone(),
            }))
        }
        Expr::Break(b) => {
            if !ctx.is_in_loop() {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0021,
                    "`break` used with no enclosing loop",
                    &b.span,
                ));
            }
            let break_ty = ctx.current_break_ty.clone();
            let value = match &b.value {
                Some(e) => {
                    let constructed = construct_expr(e, break_ty.as_ref(), ctx)?;
                    Some(Box::new(match &break_ty {
                        Some(t) => {
                            let constructed = maybe_read_copy(
                                t,
                                constructed,
                                e.span(),
                                ctx.registry,
                                ctx.current_module,
                            )?;
                            maybe_singleton_coerce(t, constructed, e.span(), ctx.registry)?
                        }
                        None => constructed,
                    }))
                }
                None => None,
            };
            Ok(TypedExpr::Break(TypedBreakExpr {
                value,
                span: b.span.clone(),
            }))
        }
        Expr::Continue(span) => {
            if !ctx.is_in_loop() {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0021,
                    "`continue` used with no enclosing loop",
                    span,
                ));
            }
            Ok(TypedExpr::Continue(span.clone()))
        }
    }
}

fn builtin_pattern_method_expr(
    receiver: TypedExpr,
    method: &str,
    args: Vec<TypedExpr>,
    span: &Span,
) -> Option<Result<TypedExpr, MetelError>> {
    if matches!(
        peel_type_references(receiver.ty()),
        Type::Array(_) | Type::SizedArray(_, _)
    ) && method == "len"
        && args.is_empty()
    {
        return Some(Ok(TypedExpr::MethodCall {
            receiver: Box::new(receiver),
            method: method.to_string(),
            args,
            ty: Type::I64,
            dispatch: crate::typed_ast::MethodDispatch::Dynamic,
            span: span.clone(),
        }));
    }

    None
}

/// Issue #229: `break` can now be a block's own tail expression (e.g.
/// `loop { if (c) { break 5 } }`, no longer requiring `break 5;` as a
/// statement), so the tail must be checked too, not just `block.stmts`.
fn find_loop_break_type(block: &TypedBlock) -> Option<Type> {
    if let Some(tail) = &block.tail {
        if let Some(ty) = find_break_in_expr(tail) {
            return Some(ty);
        }
    }
    block.stmts.iter().find_map(find_break_in_decl)
}

fn find_break_in_decl(decl: &TypedDecl) -> Option<Type> {
    match decl {
        TypedDecl::Stmt(stmt) => find_break_in_stmt(stmt),
        _ => None,
    }
}

fn find_break_in_stmt(stmt: &TypedStmt) -> Option<Type> {
    match stmt {
        TypedStmt::Expr(expr) => find_break_in_expr(expr),
        // break inside a nested while/for/for-in exits that loop, not the outer loop
        TypedStmt::While(_) | TypedStmt::For(_) | TypedStmt::ForIn(_) => None,
    }
}

fn find_break_in_expr(expr: &TypedExpr) -> Option<Type> {
    match expr {
        TypedExpr::Break(b) => Some(b.value.as_ref().map_or(Type::Unit, |v| v.ty().clone())),
        TypedExpr::If {
            then_branch,
            else_branch,
            ..
        } => find_loop_break_type(then_branch)
            .or_else(|| else_branch.as_ref().and_then(find_loop_break_type)),
        // A `break` written as a match-arm body -- same shape as an `if`
        // branch, previously never checked (a pre-existing gap, fixed here
        // since #229 unifies match-arm bodies through the same mechanism).
        TypedExpr::Match(m) => m.arms.iter().find_map(|a| find_loop_break_type(&a.body)),
        // Everything else: a nested loop's own `break` exits that inner loop,
        // not the outer one; a closure's `break` doesn't escape to the
        // enclosing loop either. Both fall out of the same `None` as any
        // other non-propagating expression kind.
        _ => None,
    }
}

fn construct_match(
    m: &MatchExpr,
    expected_ty: Option<&Type>,
    ctx: &mut ConstructCtx,
) -> Result<TypedExpr, MetelError> {
    let scrutinee = construct_expr(&m.scrutinee, None, ctx)?;
    // RFC-0108: peel `&T`/`&mut T` layers so a reference-typed scrutinee matches
    // against `T`'s own patterns, using construction's own existing
    // `peel_type_references` (already used for method/field receivers). Only the
    // local copy used for pattern resolution and exhaustiveness is peeled — the
    // typed scrutinee expression's own recorded type (`scrutinee.ty()`) is untouched.
    let scrutinee_ty = peel_type_references(scrutinee.ty()).clone();
    // RFC-0107: bare variant patterns (`Red`, `Some { value }`) resolve against the
    // scrutinee's own enum. Compute the enum's variant list once from the (already
    // reference-peeled, RFC-0108) scrutinee type; `None` here means the scrutinee
    // isn't a known enum, so patterns are left exactly as written.
    let scrutinee_variants: Option<(String, Vec<(String, bool)>)> = match &scrutinee_ty {
        Type::Named(enum_name, _) => ctx.registry.enum_info(enum_name).map(|info| {
            (
                enum_name.clone(),
                info.variants
                    .iter()
                    .map(|v| (v.name.clone(), v.fields.is_empty()))
                    .collect(),
            )
        }),
        _ => None,
    };
    // RFC-0032 §4/§5, RFC-0034 §5: same idea, for a struct rather than an enum --
    // Pass 2's own counterpart of `infer_match`'s parallel resolution (inference.rs),
    // since construction re-derives everything from the AST rather than reusing
    // Pass 1's rewritten patterns.
    let scrutinee_struct_name: Option<String> = match &scrutinee_ty {
        Type::Named(name, _) if ctx.registry.struct_fields(name).is_some() => Some(name.clone()),
        _ => None,
    };
    let mut typed_arms = vec![];
    for arm in &m.arms {
        let pattern = if let Some((enum_name, variants)) = &scrutinee_variants {
            resolve_bare_variant(&arm.pattern, enum_name, variants)
        } else if let Some(struct_name) = &scrutinee_struct_name {
            resolve_struct_pattern(&arm.pattern, struct_name)
        } else {
            arm.pattern.clone()
        };
        ctx.push_scope();
        construct_pattern_bindings(&pattern, &scrutinee_ty, ctx)?;
        let guard = match &arm.guard {
            Some(g) => Some(construct_expr(g, None, ctx)?),
            None => None,
        };
        let body = construct_block(&arm.body, expected_ty, ctx)?;
        typed_arms.push(TypedMatchArm {
            pattern,
            guard,
            body,
            span: arm.span.clone(),
        });
        ctx.pop_scope();
    }
    check_match_exhaustiveness(
        &typed_arms,
        &scrutinee_ty,
        ctx.registry.raw_enum_env(),
        &m.span,
    )?;
    // RFC-0078 §3.4: if all arms diverge, the match's type is `!`. An empty match
    // (only legal on a `!` scrutinee, per the exhaustiveness check above) is
    // vacuously `!` too — it can never actually be entered.
    let expr_type = if typed_arms.is_empty() {
        Type::Never
    } else {
        merge_branch_types(
            &typed_arms
                .iter()
                .map(|a| block_result_type(&a.body))
                .collect::<Vec<_>>(),
        )
    };
    Ok(TypedExpr::Match(TypedMatchExpr {
        scrutinee: Box::new(scrutinee),
        arms: typed_arms,
        expr_type,
        span: m.span.clone(),
    }))
}

/// RFC-0078: a block's own type when used as an expression (`if`/`match` branch
/// body). The tail expression's type if there is one; else `!` if the block's
/// last statement is a `Never`-typed expression statement (`return`/`break`/
/// `continue`, or any other diverging expression like `panic(msg)`) — mirroring
/// pass 1's tail-less handling (`infer_block`, `src/typechecker/inference.rs`);
/// else `Unit` for an ordinary non-diverging statement-only block. Since issue
/// #229, `return`/`break`/`continue` are ordinary `Expr`s reached only through
/// `TypedStmt::Expr`/a tail expression — the type check is generic rather than
/// naming those variants specifically, which also means a bare `panic(msg);`
/// (semicolon, not tail position) is correctly recognized as diverging too.
fn block_result_type(block: &TypedBlock) -> Type {
    if let Some(tail) = &block.tail {
        return tail.ty().clone();
    }
    match block.stmts.last() {
        Some(TypedDecl::Stmt(stmt)) => match &**stmt {
            TypedStmt::Expr(e) if *e.ty() == Type::Never => Type::Never,
            _ => Type::Unit,
        },
        _ => Type::Unit,
    }
}

/// RFC-0078 §6: does a function body genuinely diverge (never returns from the
/// function at all), as opposed to merely having "type `!`" as a block
/// expression? These differ precisely for `return`: `block_result_type` above
/// correctly treats a `return`-terminated block as `!`-typed for match/if
/// arm-merging purposes (code after it is unreachable, sound at any type) — but
/// a *function* ending in a reachable, ordinary `return 5` does not diverge; it
/// returns, which is exactly what `-> !` forbids. `return <expr>` only counts
/// as divergence here if `<expr>` itself never produces a value (e.g.
/// `return panic(msg)`) — checked wherever `Return` appears, since issue #229
/// lets it be either the block's tail expression or (wrapped in
/// `TypedStmt::Expr`) an ordinary statement.
fn fun_body_diverges(block: &TypedBlock) -> bool {
    fn is_divergent_return(e: &TypedExpr) -> bool {
        match e {
            TypedExpr::Return(r) => r.value.as_ref().is_some_and(|v| *v.ty() == Type::Never),
            other => *other.ty() == Type::Never,
        }
    }
    if let Some(tail) = &block.tail {
        return is_divergent_return(tail);
    }
    match block.stmts.last() {
        Some(TypedDecl::Stmt(stmt)) => match &**stmt {
            TypedStmt::Expr(e) => is_divergent_return(e),
            _ => false,
        },
        _ => false,
    }
}

/// RFC-0078 §3.4: merge sibling branch/arm types — the first non-`!` type, or `!`
/// if every branch diverges. A diverging branch imposes no constraint of its own
/// (`! <: T` for all `T`), so it must never be picked over a concretely-typed
/// sibling regardless of source order.
fn merge_branch_types(types: &[Type]) -> Type {
    types
        .iter()
        .find(|t| **t != Type::Never)
        .cloned()
        .unwrap_or(Type::Never)
}

/// Map `enum_info`'s type params to the scrutinee's concrete type args, the same
/// substitution `bind_enum_variant_fields` builds for pattern binding — needed here
/// to resolve a variant's field types for the current instantiation (e.g. whether
/// `Result<T, !>`'s `Err { error: E }` is uninhabited depends on what `E` actually is).
fn enum_variant_type_param_remap(enum_info: &EnumInfo, type_args: &[Type]) -> Substitution {
    let mut remap = Substitution::new();
    for (&tp, arg_ty) in enum_info.type_params.iter().zip(type_args.iter()) {
        remap.bind(tp, InferType::Concrete(arg_ty.clone()));
    }
    remap
}

/// RFC-0078 §3.2: a variant is uninhabited if any of its fields' (substituted)
/// type is `!` — no value of that variant can ever be constructed, since a struct
/// literal needs a value for every field and none exists of type `!`. A zero-field
/// variant is always inhabited (e.g. `Perhaps::None`).
fn is_variant_uninhabited(variant: &VariantInfo, remap: &Substitution, span: &Span) -> bool {
    variant
        .fields
        .iter()
        .any(|f| infer_type_to_type(&remap.apply(&f.ty), span).is_ok_and(|t| t == Type::Never))
}

fn check_match_exhaustiveness(
    arms: &[TypedMatchArm],
    scrutinee_ty: &Type,
    enum_env: &HashMap<String, EnumInfo>,
    span: &Span,
) -> Result<(), MetelError> {
    if arms
        .iter()
        .any(|a| a.guard.is_none() && is_catch_all_pattern(&a.pattern))
    {
        return Ok(());
    }
    let exhaustive = match scrutinee_ty {
        Type::Boolean => {
            let has_true = arms
                .iter()
                .any(|a| a.guard.is_none() && is_bool_literal_pattern(&a.pattern, true));
            let has_false = arms
                .iter()
                .any(|a| a.guard.is_none() && is_bool_literal_pattern(&a.pattern, false));
            has_true && has_false
        }
        // RFC-0078 §3.2: a variant whose payload is uninhabited (some field's type
        // is `!`) can never be constructed, so it doesn't need a covering arm to be
        // exhaustive. This subsumes `Result<T, !>` (§4.1) as the general rule's
        // special case, rather than hardcoding `Result`/`Perhaps` separately —
        // both are ordinary entries in `enum_env` like any user enum.
        Type::Named(name, type_args) => {
            if let Some(enum_info) = enum_env.get(name.as_str()) {
                let remap = enum_variant_type_param_remap(enum_info, type_args);
                enum_info.variants.iter().all(|v| {
                    is_variant_uninhabited(v, &remap, span)
                        || arms.iter().any(|a| {
                            a.guard.is_none() && pattern_covers_variant(&a.pattern, name, &v.name)
                        })
                })
            } else {
                false
            }
        }
        // Never is uninhabited — a match on it is vacuously exhaustive.
        Type::Never => true,
        // SizedArray [T; N]: exhaustive if there is an arm with an exact N-element array
        // pattern (each element itself exhaustive) or a rest pattern.
        Type::SizedArray(_, n) => arms.iter().any(|a| {
            a.guard.is_none()
                && match &a.pattern {
                    Pattern::Array {
                        elems,
                        rest: Some(_),
                        ..
                    } => elems.iter().all(is_catch_all_pattern),
                    Pattern::Array {
                        elems, rest: None, ..
                    } => elems.len() as u64 == *n && elems.iter().all(is_catch_all_pattern),
                    _ => false,
                }
        }),
        // Int, Float, Str, Tuple, Array, Fun — value-infinite; only a catch-all suffices.
        _ => false,
    };
    if !exhaustive {
        return Err(MetelError::type_error(
            TypeErrorCode::T0008,
            "non-exhaustive match: not all cases are covered".to_string(),
            span,
        ));
    }
    Ok(())
}

fn is_catch_all_pattern(pattern: &Pattern) -> bool {
    match pattern {
        // A struct pattern, like `Record`, is irrefutable by construction: field
        // sub-patterns here are always plain bindings (no `field: subpattern` form),
        // and `infer_struct_pattern` already requires either every field named or a
        // trailing `..` (RFC-0032 §5) before this point is ever reached -- so an
        // unguarded arm with one always covers the entire struct type, regardless of
        // which fields it names.
        Pattern::Wildcard(_)
        | Pattern::Binding(_, _)
        | Pattern::Record { .. }
        | Pattern::Struct { .. } => true,
        // A tuple pattern is irrefutable when every element is also irrefutable.
        Pattern::Tuple(pats, _) => pats.iter().all(is_catch_all_pattern),
        // An array pattern with a rest binding is irrefutable if all explicit elems are.
        Pattern::Array {
            elems,
            rest: Some(_),
            ..
        } => elems.iter().all(is_catch_all_pattern),
        _ => false,
    }
}

fn is_bool_literal_pattern(pattern: &Pattern, expected: bool) -> bool {
    matches!(pattern, Pattern::Literal(Literal::Boolean(b), _) if *b == expected)
}

/// Returns true if `pattern` (unguarded) covers variant `variant_name` of enum `enum_name`.
fn pattern_covers_variant(pattern: &Pattern, enum_name: &str, variant_name: &str) -> bool {
    match pattern {
        Pattern::EnumVariant { path, .. } => {
            path.first().map(String::as_str) == Some(enum_name)
                && path.get(1).map(String::as_str) == Some(variant_name)
        }
        _ => false,
    }
}

/// RFC-0107: rewrite a bare variant match-arm pattern into its fully-qualified form
/// when it resolves against the scrutinee's enum. `variants` is `(name, is_fieldless)`
/// for each variant of the scrutinee enum `enum_name`. A bare no-field variant is
/// parsed as a `Binding` (`Red`); a bare fieldful variant is parsed as a one-segment
/// `EnumVariant` (`Some { value }`). Anything else — including a `Binding` that names
/// no variant, which stays an ordinary binding — is returned unchanged. Resolution is
/// top-level only: nested bare variants inside a tuple/array pattern are out of scope
/// (they would resolve against a nested field type, not the scrutinee enum). Once
/// rewritten, every downstream consumer sees an ordinary two-segment `EnumVariant`,
/// so exhaustiveness, binding, and runtime matching need no changes.
pub(super) fn resolve_bare_variant(
    pattern: &Pattern,
    enum_name: &str,
    variants: &[(String, bool)],
) -> Pattern {
    match pattern {
        Pattern::Binding(name, span)
            if variants
                .iter()
                .any(|(vn, fieldless)| vn == name && *fieldless) =>
        {
            Pattern::EnumVariant {
                path: vec![enum_name.to_string(), name.clone()],
                fields: vec![],
                rest: false,
                span: span.clone(),
            }
        }
        Pattern::EnumVariant {
            path,
            fields,
            rest,
            span,
        } if path.len() == 1 && variants.iter().any(|(vn, _)| vn == &path[0]) => {
            Pattern::EnumVariant {
                path: vec![enum_name.to_string(), path[0].clone()],
                fields: fields.clone(),
                rest: *rest,
                span: span.clone(),
            }
        }
        _ => pattern.clone(),
    }
}

/// RFC-0032 §4/§5, RFC-0034 §5: rewrite a one-segment `EnumVariant` pattern into a
/// `Struct` pattern when it names `struct_name`, the scrutinee's own struct -- the
/// struct-pattern counterpart of `resolve_bare_variant` above. Shares that function's
/// grammar ambiguity (`Point { x, y }` parses as a one-segment `EnumVariant` regardless
/// of whether `Point` is an enum or a struct) and its resolution strategy (let the
/// scrutinee's type decide, once it's known). Once rewritten, every downstream
/// consumer sees a `Pattern::Struct`, so field binding, visibility checking, and
/// runtime matching need no further disambiguation.
pub(super) fn resolve_struct_pattern(pattern: &Pattern, struct_name: &str) -> Pattern {
    match pattern {
        Pattern::EnumVariant {
            path,
            fields,
            rest,
            span,
        } if path.len() == 1 && path[0] == struct_name => Pattern::Struct {
            name: struct_name.to_string(),
            fields: fields.clone(),
            rest: *rest,
            span: span.clone(),
        },
        _ => pattern.clone(),
    }
}

fn construct_pattern_bindings(
    pattern: &Pattern,
    scrutinee_ty: &Type,
    ctx: &mut ConstructCtx,
) -> Result<(), MetelError> {
    match pattern {
        Pattern::Wildcard(_) | Pattern::Literal(_, _) => {}
        Pattern::Binding(name, _) => {
            ctx.bind(name, scrutinee_ty.clone());
        }
        Pattern::Tuple(pats, _) => {
            let elems = match scrutinee_ty {
                Type::Tuple(ts) => ts.clone(),
                _ => return Err(MetelError::internal("tuple pattern on non-tuple")),
            };
            for (pat, elem_ty) in pats.iter().zip(elems.iter()) {
                construct_pattern_bindings(pat, elem_ty, ctx)?;
            }
        }
        Pattern::EnumVariant {
            path,
            fields,
            rest: _,
            span,
        } => {
            let [enum_name, variant_name] = path.as_slice() else {
                return Err(MetelError::internal("invalid pattern path"));
            };
            let _ = span;
            bind_enum_variant_fields(enum_name, variant_name, fields, scrutinee_ty, ctx)?;
        }
        Pattern::Struct { name, fields, .. } => {
            bind_struct_pattern_fields(name, fields, scrutinee_ty, ctx)?;
        }
        Pattern::Record { fields, .. } => {
            let Type::Record(record_fields) = scrutinee_ty else {
                return Err(MetelError::internal("record pattern on non-record type"));
            };
            for field in fields {
                let field_ty = record_fields
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, ty)| ty.clone())
                    .ok_or_else(|| {
                        MetelError::internal(format!("missing record field `{field}`"))
                    })?;
                ctx.bind(field, field_ty);
            }
        }
        Pattern::Array {
            elems,
            rest,
            span: _,
        } => {
            let elem_ty = match scrutinee_ty {
                Type::Array(t) | Type::SizedArray(t, _) => *t.clone(),
                _ => return Err(MetelError::internal("array pattern on non-array type")),
            };
            if let Some(rest_name) = rest {
                ctx.bind(rest_name, Type::Array(Box::new(elem_ty.clone())));
            }
            for pat in elems {
                construct_pattern_bindings(pat, &elem_ty, ctx)?;
            }
        }
    }
    Ok(())
}

fn extract_type_args_from_type(ty: &Type) -> Vec<Type> {
    match ty {
        Type::Named(_, args) => args.clone(),
        _ => vec![],
    }
}

// Exhaustive handling of every enum-literal construction case (generic args,
// variant shapes, inference fallbacks); splitting it up would scatter one
// coherent dispatch table across many small functions with no real gain in
// clarity.
#[allow(clippy::too_many_lines)]
fn construct_enum_literal_ty(
    enum_name: &str,
    variant_name: &str,
    typed_fields: &[(String, TypedExpr)],
    expected_ty: Option<&Type>,
    span: &Span,
    ctx: &mut ConstructCtx,
) -> Result<Type, MetelError> {
    // Resolve concrete type arguments using the same instantiate-then-unify
    // pattern as instantiate_scheme_for_call.
    let enum_info = ctx.registry.enum_info(enum_name).ok_or_else(|| {
        MetelError::type_error(
            TypeErrorCode::T0003,
            format!("unknown enum `{enum_name}`"),
            span,
        )
    })?;
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
        })?;

    // Assign a fresh type variable to each formal type parameter and
    // build an instantiation substitution for this particular usage site.
    let mut init_subst = Substitution::new();
    let fresh_vars: Vec<InferType> = enum_info
        .type_params
        .iter()
        .map(|&tp| {
            let fresh = InferType::Var(ctx.gen.fresh());
            init_subst.bind(tp, fresh.clone());
            fresh
        })
        .collect();

    // Unify each instantiated field type against the actual expression type
    // to solve for the fresh variables.
    let mut local_subst = Substitution::new();
    for (field_name, typed_expr) in typed_fields {
        if let Some(field_entry) = variant
            .fields
            .iter()
            .find(|entry| &entry.name == field_name)
        {
            let instantiated = init_subst.apply(&field_entry.ty);
            let actual = type_to_infer(typed_expr.ty());
            if let Ok(s) = unify(
                &local_subst.apply(&instantiated),
                &local_subst.apply(&actual),
            ) {
                local_subst = local_subst.compose(&s);
            }
        }
    }

    // Apply the local substitution to recover concrete type arguments.
    // If a type param remains unresolved (fieldless variants like `Perhaps::None`),
    // fall back to the annotation's args.
    // type_to_infer normalises Perhaps/Result into Named for uniform handling.
    let hint_args: Vec<Type> = expected_ty
        .map(|ty| {
            if let InferType::Named(n, args) = type_to_infer(ty) {
                if n == enum_name {
                    args.iter()
                        .map(|a| infer_type_to_type(a, span))
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap_or_default()
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        })
        .unwrap_or_default();
    let concrete_args: Vec<Type> = fresh_vars
        .iter()
        .enumerate()
        .map(|(i, fv)| {
            let resolved = local_subst.apply(fv);
            if matches!(resolved, InferType::Var(_)) {
                hint_args.get(i).cloned().ok_or_else(|| {
                    MetelError::type_error(
                        TypeErrorCode::T0002,
                        "cannot infer type; add a type annotation",
                        span,
                    )
                })
            } else {
                infer_type_to_type(&resolved, span)
            }
        })
        .collect::<Result<_, _>>()?;

    // T0012: check each resolved type arg satisfies the enum's declared bounds.
    let generic_types_by_name: HashMap<String, Type> = ctx
        .registry
        .struct_generic_names_for(enum_name)
        .into_iter()
        .flatten()
        .cloned()
        .zip(concrete_args.iter().cloned())
        .collect();
    let record_kinds = ctx
        .get_type_param_record_kinds(enum_name)
        .cloned()
        .unwrap_or_else(|| vec![false; concrete_args.len()]);
    if let Some(param_bounds) = ctx.registry.type_param_bounds_for(enum_name) {
        for (i, bounds) in param_bounds.iter().enumerate() {
            let record_kind = record_kinds.get(i).copied().unwrap_or(false);
            if bounds.is_empty() && !record_kind {
                continue;
            }
            let Some(concrete_arg) = concrete_args.get(i) else {
                continue;
            };
            check_type_satisfies_bounds(
                concrete_arg,
                bounds,
                record_kind,
                enum_name,
                span,
                ctx.registry,
                ctx.current_module,
                &generic_types_by_name,
            )?;
        }
    }
    // T0012 negative bounds: check each resolved type arg does NOT implement
    // the declared negative bounds (RFC-0072, issue #243).
    if let Some(neg_param_bounds) = ctx.registry.neg_type_param_bounds_for(enum_name) {
        for (i, neg_bounds) in neg_param_bounds.iter().enumerate() {
            let record_kind = record_kinds.get(i).copied().unwrap_or(false);
            if neg_bounds.is_empty() && !record_kind {
                continue;
            }
            let Some(concrete_arg) = concrete_args.get(i) else {
                continue;
            };
            check_type_does_not_satisfy_bound(
                concrete_arg,
                neg_bounds,
                record_kind,
                enum_name,
                span,
                ctx.registry,
                ctx.current_module,
                &generic_types_by_name,
            )?;
        }
    }

    let infer_args: Vec<InferType> = concrete_args.iter().map(type_to_infer).collect();
    infer_type_to_type(&InferType::Named(enum_name.to_string(), infer_args), span)
}

fn bind_enum_variant_fields(
    enum_name: &str,
    variant_name: &str,
    fields: &[String],
    scrutinee_ty: &Type,
    ctx: &mut ConstructCtx,
) -> Result<(), MetelError> {
    let enum_info = ctx
        .registry
        .enum_info(enum_name)
        .ok_or_else(|| MetelError::internal(format!("unknown enum `{enum_name}`")))?
        .clone();
    let variant = enum_info
        .variants
        .iter()
        .find(|v| v.name == variant_name)
        .ok_or_else(|| MetelError::internal(format!("unknown variant `{variant_name}`")))?
        .clone();
    let type_args = extract_type_args_from_type(scrutinee_ty);
    let mut remap = Substitution::new();
    for (&tp, arg_ty) in enum_info.type_params.iter().zip(type_args.iter()) {
        remap.bind(tp, InferType::Concrete(arg_ty.clone()));
    }
    for field_name in fields {
        let (template_ty, field_span) = variant
            .fields
            .iter()
            .find(|entry| entry.name == *field_name)
            .map(|entry| (entry.ty.clone(), entry.span.clone()))
            .ok_or_else(|| {
                MetelError::internal(format!(
                    "no field `{field_name}` on variant `{variant_name}`"
                ))
            })?;
        let concrete = infer_type_to_type(&remap.apply(&template_ty), &field_span)?;
        ctx.bind(field_name, concrete);
    }
    Ok(())
}

/// RFC-0032 §4/§5, RFC-0034 §5: Pass 2 counterpart of `infer_struct_pattern` --
/// field-visibility and exhaustiveness were already checked in Pass 1 against the
/// scrutinee's unsubstituted type; this binds each named field to its concrete
/// (generic-substituted) type for the arm body, the same shape as
/// `bind_enum_variant_fields` above.
fn bind_struct_pattern_fields(
    struct_name: &str,
    fields: &[String],
    scrutinee_ty: &Type,
    ctx: &mut ConstructCtx,
) -> Result<(), MetelError> {
    let struct_fields = ctx
        .registry
        .struct_fields(struct_name)
        .ok_or_else(|| MetelError::internal(format!("unknown struct `{struct_name}`")))?
        .clone();
    let type_params = ctx
        .registry
        .struct_type_params_for(struct_name)
        .cloned()
        .unwrap_or_default();
    let type_args = extract_type_args_from_type(scrutinee_ty);
    let mut remap = Substitution::new();
    for (&tp, arg_ty) in type_params.iter().zip(type_args.iter()) {
        remap.bind(tp, InferType::Concrete(arg_ty.clone()));
    }
    for field_name in fields {
        let (template_ty, field_span) = struct_fields
            .iter()
            .find(|entry| entry.name == *field_name)
            .map(|entry| (entry.ty.clone(), entry.span.clone()))
            .ok_or_else(|| {
                MetelError::internal(format!("no field `{field_name}` on `{struct_name}`"))
            })?;
        let concrete = infer_type_to_type(&remap.apply(&template_ty), &field_span)?;
        ctx.bind(field_name, concrete);
    }
    Ok(())
}

/// Build a typed Call expression.
///
/// For polymorphic callees (Idents in `scheme_env` whose type still contains free
/// vars), re-instantiate the scheme against the concrete argument types using
/// local unification. This is the Pass 2 counterpart of the inline
/// solve-and-generalize done in `infer_fun_decl`.
// Exhaustive match over every AST/type-system variant; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
fn construct_call(
    callee: &Expr,
    type_args: &[TypeExpr],
    args: &[Expr],
    span: &Span,
    expected_ty: Option<&Type>,
    ctx: &mut ConstructCtx,
) -> Result<TypedExpr, MetelError> {
    // Overloaded free-function call (METEL-180): select the candidate whose
    // parameter types exactly match the argument types and stamp its SymbolId
    // into the call; the evaluator dispatches through its symbol registry.
    // No implicit coercion participates in selection.
    if let Some(name) = super::overload::callee_name(callee) {
        if ctx.overloads.contains_key(name) {
            let typed_args: Vec<TypedExpr> = args
                .iter()
                .map(|a| construct_expr(a, None, ctx))
                .collect::<Result<_, _>>()?;
            let arg_types: Vec<Type> = typed_args.iter().map(|a| a.ty().clone()).collect();
            let entries = &ctx.overloads[name];
            match super::overload::select(entries, &arg_types) {
                Some(entry) => {
                    let fun_ty = Type::Fun(entry.params.clone(), Box::new(entry.ret.clone()));
                    let typed_callee =
                        TypedExpr::Ident(name.to_string(), fun_ty, callee.span().clone());
                    return Ok(TypedExpr::Call {
                        callee: Box::new(typed_callee),
                        args: typed_args,
                        ty: entry.ret.clone(),
                        callee_id: Some(entry.symbol_id),
                        span: span.clone(),
                    });
                }
                // No exact match: fall back to a non-overload binding of the
                // same name (prelude/imports), mirroring the inference pass.
                // The normal path below re-constructs the arguments.
                None if ctx.lookup(name).is_some() || ctx.scheme_env.contains_key(name) => {}
                None => {
                    return Err(super::overload::no_match_error(
                        name, &arg_types, entries, span,
                    ))
                }
            }
        }
    }
    // For monomorphic callee identifiers already in scope, extract param types as hints so
    // inherently ambiguous args (bare `[]`, `None`) can resolve without requiring ascription.
    // Generic (scheme-based) callees need arg types first for instantiation — no hints there.
    let param_hints: Vec<Option<Type>> = match callee {
        Expr::Ident(name, _) => match ctx.lookup(name) {
            Some(Type::Fun(params, _)) if params.len() == args.len() => {
                params.iter().map(|p| Some(p.clone())).collect()
            }
            _ => vec![None; args.len()],
        },
        Expr::Path(segments, _) => {
            let last = segments.last().map_or("", std::string::String::as_str);
            match ctx.lookup(last) {
                Some(Type::Fun(params, _)) if params.len() == args.len() => {
                    params.iter().map(|p| Some(p.clone())).collect()
                }
                _ => vec![None; args.len()],
            }
        }
        Expr::ResolvedPath { resolved, .. } => match ctx.lookup(resolved) {
            Some(Type::Fun(params, _)) if params.len() == args.len() => {
                params.iter().map(|p| Some(p.clone())).collect()
            }
            _ => vec![None; args.len()],
        },
        _ => vec![None; args.len()],
    };

    let typed_args: Vec<TypedExpr> = args
        .iter()
        .zip(param_hints.iter())
        .map(|(a, hint)| {
            let typed = construct_expr(a, hint.as_ref(), ctx)?;
            reject_dynamic_array_where_sized_expected(hint.as_ref(), &typed)?;
            Ok(typed)
        })
        .collect::<Result<_, _>>()?;
    let arg_types: Vec<&Type> = typed_args
        .iter()
        .map(super::super::typed_ast::TypedExpr::ty)
        .collect();

    // Resolve explicit type args once, outside the match.
    let explicit_tys: Option<Vec<Type>> = if type_args.is_empty() {
        None
    } else {
        Some(
            type_args
                .iter()
                .map(|te| infer_type_to_type(&ctx.type_expr_to_infer_ctx(te), span))
                .collect::<Result<_, _>>()?,
        )
    };

    let (typed_callee, fun_ty) = match callee {
        Expr::Ident(name, ident_span) if ctx.lookup(name).is_none() => {
            let scheme = ctx.scheme_env.get(name.as_str()).ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("undefined name `{name}`"),
                    ident_span,
                )
            })?;
            let (concrete, var_map) = match &explicit_tys {
                Some(tys) => instantiate_scheme_with_turbofish(
                    scheme,
                    tys,
                    span,
                    ctx.registry,
                    ctx.current_module,
                )?,
                None => {
                    match instantiate_scheme_for_call(
                        scheme,
                        &arg_types,
                        span,
                        &mut ctx.gen,
                        ctx.registry,
                        ctx.current_module,
                    ) {
                        Ok(result) => result,
                        Err(e) => {
                            // Arg-based instantiation failed (e.g. zero-arg generic call
                            // whose only free type variable appears in the return type).
                            // Try resolving it from the expected type via unification,
                            // same fallback as the qualified-path call branch below.
                            match expected_ty {
                                Some(expected) => instantiate_scheme_with_expected_ret(
                                    scheme,
                                    &arg_types,
                                    expected,
                                    span,
                                    &mut ctx.gen,
                                    ctx.registry,
                                    ctx.current_module,
                                )
                                .map_err(|_| e)?,
                                None => return Err(e),
                            }
                        }
                    }
                }
            };
            check_fun_call_bounds(name, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_bounds(
                name,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            check_fun_call_assoc_eq(name, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_assoc_eq(
                name,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            check_fun_call_neg_bounds(name, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_neg_bounds(
                name,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            let typed = TypedExpr::Ident(name.clone(), concrete.clone(), ident_span.clone());
            (typed, concrete)
        }
        // Qualified static constructors like "List::new" / "List::from" registered as joined-key schemes.
        Expr::Path(segments, path_span)
            if {
                let joined = segments.join("::");
                ctx.lookup(&joined).is_none() && ctx.scheme_env.contains_key(joined.as_str())
            } =>
        {
            let joined = segments.join("::");
            let scheme = ctx.scheme_env.get(joined.as_str()).unwrap();
            let (concrete, var_map) = match &explicit_tys {
                Some(tys) => instantiate_scheme_with_turbofish(
                    scheme,
                    tys,
                    span,
                    ctx.registry,
                    ctx.current_module,
                )?,
                None => {
                    match instantiate_scheme_for_call(
                        scheme,
                        &arg_types,
                        span,
                        &mut ctx.gen,
                        ctx.registry,
                        ctx.current_module,
                    ) {
                        Ok(result) => result,
                        Err(e) => {
                            // Arg-based instantiation failed (e.g. zero-arg generic constructor).
                            // Try resolving the return type from the expected type via unification.
                            match expected_ty {
                                Some(expected) => instantiate_scheme_with_expected_ret(
                                    scheme,
                                    &arg_types,
                                    expected,
                                    span,
                                    &mut ctx.gen,
                                    ctx.registry,
                                    ctx.current_module,
                                )
                                .map_err(|_| e)?,
                                None => return Err(e),
                            }
                        }
                    }
                }
            };
            check_fun_call_bounds(&joined, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_bounds(
                &joined,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            check_fun_call_assoc_eq(&joined, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_assoc_eq(
                &joined,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            check_fun_call_neg_bounds(&joined, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_neg_bounds(
                &joined,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            let typed = TypedExpr::Path(segments.clone(), concrete.clone(), path_span.clone());
            (typed, concrete)
        }
        Expr::Path(segments, path_span)
            if {
                let last = segments.last().map_or("", std::string::String::as_str);
                ctx.lookup(last).is_none()
                && ctx.scheme_env.contains_key(last)
                // Only use scheme instantiation if method_env doesn't have it
                && !(segments.len() == 2 && ctx.method_env
                    .get(segments[0].as_str())
                    .and_then(|m| m.get(segments[1].as_str()))
                    .is_some())
            } =>
        {
            let last = segments.last().unwrap().clone();
            let scheme = ctx.scheme_env.get(last.as_str()).unwrap();
            let (concrete, var_map) = match &explicit_tys {
                Some(tys) => instantiate_scheme_with_turbofish(
                    scheme,
                    tys,
                    span,
                    ctx.registry,
                    ctx.current_module,
                )?,
                None => instantiate_scheme_for_call(
                    scheme,
                    &arg_types,
                    span,
                    &mut ctx.gen,
                    ctx.registry,
                    ctx.current_module,
                )?,
            };
            check_fun_call_bounds(&last, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_bounds(
                &last,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            check_fun_call_assoc_eq(&last, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_assoc_eq(
                &last,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            check_fun_call_neg_bounds(&last, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_neg_bounds(
                &last,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            let typed = TypedExpr::Path(segments.clone(), concrete.clone(), path_span.clone());
            (typed, concrete)
        }
        Expr::ResolvedPath {
            resolved,
            symbol_id: _,
            original: _,
            span: rspan,
        } if ctx.lookup(resolved).is_none() && ctx.scheme_env.contains_key(resolved.as_str()) => {
            let scheme = ctx.scheme_env.get(resolved.as_str()).unwrap();
            let (concrete, var_map) = match &explicit_tys {
                Some(tys) => instantiate_scheme_with_turbofish(
                    scheme,
                    tys,
                    span,
                    ctx.registry,
                    ctx.current_module,
                )?,
                None => instantiate_scheme_for_call(
                    scheme,
                    &arg_types,
                    span,
                    &mut ctx.gen,
                    ctx.registry,
                    ctx.current_module,
                )?,
            };
            check_fun_call_bounds(resolved, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_bounds(
                resolved,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            check_fun_call_assoc_eq(resolved, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_assoc_eq(
                resolved,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            check_fun_call_neg_bounds(resolved, &var_map, span, ctx.registry, ctx.current_module)?;
            check_scheme_neg_bounds(
                resolved,
                scheme,
                &var_map,
                span,
                ctx.registry,
                ctx.current_module,
            )?;
            let typed = TypedExpr::Ident(resolved.clone(), concrete.clone(), rspan.clone());
            (typed, concrete)
        }
        _ => {
            let typed = construct_expr(callee, None, ctx)?;
            let ty = typed.ty().clone();
            (typed, ty)
        }
    };

    // Re-construct args with the now-known concrete param types as hints if
    // pre-construction defaulting diverged (e.g. unsuffixed integer literals in
    // turbofish calls: clamp::<i32>(5, 0, 10) would have built I64 args).
    let fun_ty_for_hints = match &fun_ty {
        Type::Reference(inner) | Type::MutReference(inner)
            if matches!(inner.as_ref(), Type::Fun(..)) =>
        {
            inner.as_ref()
        }
        other => other,
    };
    let typed_args = if let Type::Fun(params, _) = fun_ty_for_hints {
        if params.len() == typed_args.len()
            && typed_args
                .iter()
                .zip(params.iter())
                .any(|(a, p)| a.ty() != p)
        {
            args.iter()
                .zip(params.iter())
                .map(|(a, p)| construct_expr(a, Some(p), ctx))
                .collect::<Result<_, _>>()?
        } else {
            typed_args
        }
    } else {
        typed_args
    };

    // #775: turbofish pins each quantified var directly from the explicit
    // `::<T>` types, bypassing the unification against actual argument types
    // that an inferred call gets for free inside instantiate_scheme_for_call.
    // Nothing downstream checked the arguments actually agree with those
    // pinned types, so a real mismatch (`identity::<i64>("hello")`) was
    // silently accepted. The reconstruction-with-hints step above already
    // resolves the one legitimate case that can *look* like a mismatch here
    // — an unsuffixed integer literal defaulted before the turbofish type was
    // known (`clamp::<i32>(5, 0, 10)`) — so check for a real mismatch after
    // it runs, not before: `construct_expr` doesn't coerce a value to a hint
    // it structurally can't satisfy, so anything still disagreeing here is
    // genuine, not a literal that just needed the hint.
    if explicit_tys.is_some() {
        if let Type::Fun(params, _) = fun_ty_for_hints {
            if params.len() == typed_args.len()
                && typed_args
                    .iter()
                    .zip(params.iter())
                    .any(|(a, p)| a.ty() != p)
            {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0001,
                    "argument type mismatch",
                    span,
                ));
            }
        }
    }

    // Auto-deref: calling through a &Fun or &mut Fun is allowed.
    let fun_ty_inner = match &fun_ty {
        Type::Reference(inner) | Type::MutReference(inner)
            if matches!(inner.as_ref(), Type::Fun(..)) =>
        {
            inner.as_ref()
        }
        other => other,
    };
    match fun_ty_inner {
        Type::Fun(params, ret) => {
            if params.len() != typed_args.len() {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0004,
                    format!(
                        "expected {} argument(s), got {}",
                        params.len(),
                        typed_args.len()
                    ),
                    span,
                ));
            }
            Ok(TypedExpr::Call {
                callee: Box::new(typed_callee),
                args: typed_args,
                ty: *ret.clone(),
                callee_id: ctx.resolved_callee_id(callee),
                span: span.clone(),
            })
        }
        _ => Err(MetelError::type_error(
            TypeErrorCode::T0001,
            "called a non-function value",
            span,
        )),
    }
}

/// Check that the concrete types instantiated for a function's generic type params
/// satisfy the aspect bounds declared on that function. Emits T0012 on the call span.
fn check_fun_call_bounds(
    fun_name: &str,
    var_to_type: &HashMap<TypeVar, Type>,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(), MetelError> {
    let bounds_map = registry.fun_bounds_for(fun_name);
    let record_kinds = registry.fun_record_kinds_for(fun_name);
    let generic_types_by_name: HashMap<String, Type> = HashMap::new();
    for (tv, concrete) in var_to_type {
        let bounds = bounds_map
            .and_then(|map| map.get(tv))
            .map_or(&[][..], Vec::as_slice);
        let record_kind = record_kinds
            .and_then(|map| map.get(tv))
            .copied()
            .unwrap_or(false);
        if bounds.is_empty() && !record_kind {
            continue;
        }
        check_type_satisfies_bounds(
            concrete,
            bounds,
            record_kind,
            fun_name,
            span,
            registry,
            current_module,
            &generic_types_by_name,
        )?;
    }
    Ok(())
}

/// Try instantiating one candidate method scheme against the receiver's
/// concrete type args and constructing the call's arguments against it,
/// running the same bound/neg-bound/assoc-eq checks an ordinary single-scheme
/// resolution would. Returns `Err` if this particular candidate doesn't apply
/// (wrong arg count, bounds not satisfied, ...) -- the caller tries the next
/// candidate rather than surfacing this as the final error.
#[allow(clippy::too_many_arguments)]
fn try_generic_method_scheme(
    scheme: &TypeScheme,
    struct_tvars: &[TypeVar],
    receiver_type_args: &[Type],
    explicit_method_tys: Option<&[Type]>,
    args: &[Expr],
    method: &str,
    span: &Span,
    ctx: &mut ConstructCtx,
) -> Result<(Type, Vec<TypedExpr>), MetelError> {
    let mut subst = Substitution::new();
    for (&tv, concrete) in struct_tvars.iter().zip(receiver_type_args.iter()) {
        subst.bind(tv, type_to_infer(concrete));
    }
    if let Some(explicit) = explicit_method_tys {
        let free: Vec<TypeVar> = {
            let mut fv: Vec<TypeVar> = typeinference::free_vars(&scheme.ty)
                .into_iter()
                .filter(|v| !struct_tvars.contains(v))
                .collect();
            fv.sort();
            fv
        };
        if explicit.len() != free.len() {
            return Err(MetelError::type_error(
                TypeErrorCode::T0004,
                format!(
                    "expected {} type argument(s), got {}",
                    free.len(),
                    explicit.len()
                ),
                span,
            ));
        }
        for (tv, concrete_ty) in free.iter().zip(explicit.iter()) {
            subst.bind(*tv, type_to_infer(concrete_ty));
        }
    }
    let partial_params: Vec<InferType> = match subst.apply(&scheme.ty) {
        InferType::Fun(p, _) => p,
        _ => return Err(MetelError::internal("method scheme is not a function type")),
    };
    let typed_args: Vec<TypedExpr> = args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            // params[0] is self; arguments line up with params[1..].
            let hint = partial_params
                .get(i + 1)
                .and_then(|it| infer_type_to_type(&subst.apply(it), span).ok());
            let typed = construct_expr(a, hint.as_ref(), ctx)?;
            reject_dynamic_array_where_sized_expected(hint.as_ref(), &typed)?;
            Ok(typed)
        })
        .collect::<Result<_, _>>()?;
    for (param_it, arg) in partial_params.iter().skip(1).zip(typed_args.iter()) {
        let arg_it = type_to_infer(arg.ty());
        if let Ok(s) = typeinference::unify(&subst.apply(param_it), &arg_it) {
            subst = subst.compose(&s);
        }
    }
    let mut var_to_type: HashMap<TypeVar, Type> = HashMap::new();
    for &tv in &scheme.quantified_vars {
        if let Ok(t) = infer_type_to_type(&subst.apply(&InferType::Var(tv)), span) {
            var_to_type.insert(tv, t);
        }
    }
    check_scheme_bounds(
        method,
        scheme,
        &var_to_type,
        span,
        ctx.registry,
        ctx.current_module,
    )?;
    check_scheme_neg_bounds(
        method,
        scheme,
        &var_to_type,
        span,
        ctx.registry,
        ctx.current_module,
    )?;
    check_scheme_assoc_eq(
        method,
        scheme,
        &var_to_type,
        span,
        ctx.registry,
        ctx.current_module,
    )?;
    let method_fun_ty = infer_type_to_type(&subst.apply(&scheme.ty), span)?;
    Ok((method_fun_ty, typed_args))
}

/// Resolve a method call against every candidate scheme registered for
/// `method` (issue #272: more than one exists when different aspects register
/// the same method name for the same/overlapping generic or structural
/// target -- coherence rejects that pair upfront unless their bounds are
/// provably disjoint, so at most one candidate's bounds can ever actually be
/// satisfied by a given concrete instantiation). Returns the first candidate
/// whose bounds the receiver's concrete type args satisfy, along with the
/// winning candidate's owning aspect name (if any); if none do, propagates the
/// last candidate's error (matching plain single-candidate behavior when
/// there's exactly one, the overwhelmingly common case).
///
/// The caller uses the returned aspect name to stamp the call site's
/// `MethodDispatch` with the specific aspect that was actually selected here,
/// rather than leaving it `Dynamic` -- a later, per-program (not per-call-
/// site) pass would otherwise have no way to reproduce this same bound-based
/// choice and could dispatch to the wrong candidate at runtime.
#[allow(clippy::too_many_arguments)]
fn resolve_generic_method_call(
    candidates: &[(TypeScheme, Vec<TypeVar>, Option<String>)],
    receiver_type_args: &[Type],
    explicit_method_tys: Option<&[Type]>,
    args: &[Expr],
    method: &str,
    span: &Span,
    ctx: &mut ConstructCtx,
) -> Result<(Type, Vec<TypedExpr>, Option<String>), MetelError> {
    let mut last_err = None;
    for (scheme, struct_tvars, aspect_name) in candidates {
        match try_generic_method_scheme(
            scheme,
            struct_tvars,
            receiver_type_args,
            explicit_method_tys,
            args,
            method,
            span,
            ctx,
        ) {
            Ok((ty, typed_args)) => return Ok((ty, typed_args, aspect_name.clone())),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("resolve_generic_method_call requires a non-empty candidate list"))
}

/// Resolve `aspect_name` to its stable `SymbolId`, the same lookup
/// `construct_impl_decl` uses for `TypedImplBlock::aspect_id`. `None` if no
/// resolver context is available (single-program path) or the name doesn't
/// resolve -- callers fall back to `MethodDispatch::Dynamic` in that case,
/// same as before this existed.
fn resolve_aspect_id(ctx: &ConstructCtx, aspect_name: &str) -> Option<SymbolId> {
    let declaring_module = ctx.registry.aspect_declaring_module(aspect_name)?;
    ctx.symbols?
        .get(&(declaring_module.clone(), aspect_name.to_string()))
        .copied()
}

/// The `MethodDispatch` to stamp for a generic/structural method call once
/// `resolve_generic_method_call` has already picked the correct candidate
/// (issue #272): `Aspect { aspect_id }` when the winner belongs to a resolvable
/// aspect, so the evaluator dispatches to that exact aspect impl rather than
/// re-deriving (and potentially mis-deriving) the choice itself; `Dynamic`
/// otherwise, unchanged from prior behavior.
fn dispatch_for_resolved_method(ctx: &ConstructCtx, aspect_name: Option<&str>) -> MethodDispatch {
    aspect_name
        .and_then(|name| resolve_aspect_id(ctx, name))
        .map_or(MethodDispatch::Dynamic, |aspect_id| {
            MethodDispatch::Aspect { aspect_id }
        })
}

/// Enforce the aspect bounds carried ON a scheme (`TypeScheme::bounds`,
/// positional per quantified var). This is how bounds on prelude/imported
/// schemes are checked — the TypeVar-keyed `fun_bounds` registry above only
/// matches schemes from the defining module.
fn check_scheme_bounds(
    fun_name: &str,
    scheme: &TypeScheme,
    var_to_type: &HashMap<TypeVar, Type>,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(), MetelError> {
    let generic_types_by_name: HashMap<String, Type> = scheme
        .quantified_vars
        .iter()
        .zip(&scheme.param_names)
        .filter_map(|(tv, name)| var_to_type.get(tv).cloned().map(|ty| (name.clone(), ty)))
        .collect();
    for (index, tv) in scheme.quantified_vars.iter().enumerate() {
        let bounds = scheme.bounds.get(index).map_or(&[][..], Vec::as_slice);
        let record_kind = scheme.record_kinds.get(index).copied().unwrap_or(false);
        if bounds.is_empty() && !record_kind {
            continue;
        }
        let Some(concrete) = var_to_type.get(tv) else {
            continue;
        };
        check_type_satisfies_bounds(
            concrete,
            bounds,
            record_kind,
            fun_name,
            span,
            registry,
            current_module,
            &generic_types_by_name,
        )?;
    }
    Ok(())
}

/// RFC-0082 §4: enforce associated-type equality constraints from `fun_bounds`.
fn check_fun_call_assoc_eq(
    fun_name: &str,
    var_to_type: &HashMap<TypeVar, Type>,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(), MetelError> {
    let Some(eq_map) = registry.fun_assoc_eq_constraints_for(fun_name) else {
        return Ok(());
    };
    for (tv, constraints) in eq_map {
        let Some(concrete) = var_to_type.get(tv) else {
            continue;
        };
        for (aspect, assoc, expected_infer) in constraints {
            let Some(actual_ty) =
                registry.impl_assoc_type(current_module, &concrete.to_string(), aspect, assoc)
            else {
                continue;
            };
            // Substitute the expected type through var_to_type.
            let expected_subst = match expected_infer {
                InferType::Var(v) => {
                    if let Some(t) = var_to_type.get(v) {
                        type_to_infer(t)
                    } else {
                        continue; // still free — skip comparison
                    }
                }
                other => other.clone(),
            };
            let expected_ty = match expected_subst {
                InferType::Concrete(t) => t,
                InferType::Named(n, _) => Type::Named(n, vec![]),
                _ => continue,
            };
            if *actual_ty != expected_ty {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0012,
                    format!(
                        "associated type equality constraint violated: `{aspect}::{assoc}` \
                         is `{actual_ty}` but expected `{expected_ty}`"
                    ),
                    span,
                ));
            }
        }
    }
    Ok(())
}

/// RFC-0082 §4: enforce associated-type equality constraints from a scheme's
/// `assoc_eq_constraints` field.
fn check_scheme_assoc_eq(
    _fun_name: &str,
    scheme: &TypeScheme,
    var_to_type: &HashMap<TypeVar, Type>,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(), MetelError> {
    if scheme.assoc_eq_constraints.is_empty() {
        return Ok(());
    }
    for (tv, constraints) in scheme
        .quantified_vars
        .iter()
        .zip(&scheme.assoc_eq_constraints)
    {
        if constraints.is_empty() {
            continue;
        }
        let Some(concrete) = var_to_type.get(tv) else {
            continue;
        };
        for (aspect, assoc, expected_infer) in constraints {
            let Some(actual_ty) =
                registry.impl_assoc_type(current_module, &concrete.to_string(), aspect, assoc)
            else {
                continue;
            };
            let expected_subst = match expected_infer {
                InferType::Var(v) => {
                    if let Some(t) = var_to_type.get(v) {
                        type_to_infer(t)
                    } else {
                        continue;
                    }
                }
                other => other.clone(),
            };
            let expected_ty = match expected_subst {
                InferType::Concrete(t) => t,
                InferType::Named(n, _) => Type::Named(n, vec![]),
                _ => continue,
            };
            if *actual_ty != expected_ty {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0012,
                    format!(
                        "associated type equality constraint violated: `{aspect}::{assoc}` \
                         is `{actual_ty}` but expected `{expected_ty}`"
                    ),
                    span,
                ));
            }
        }
    }
    Ok(())
}

fn resolve_row_field_type(
    ty: &TypeExpr,
    generic_types_by_name: &HashMap<String, Type>,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<Type, MetelError> {
    let mut generic_map = HashMap::new();
    let mut subst = Substitution::new();
    for (index, (name, concrete)) in generic_types_by_name.iter().enumerate() {
        let tv = TypeVar(index.try_into().expect("generic map index fits in u32"));
        generic_map.insert(name.clone(), tv);
        subst.bind(tv, type_to_infer(concrete));
    }
    let assoc_ctx = AssocResolveCtx {
        registry,
        current_module,
        current_aspect: None,
    };
    let inferred = type_expr_to_infer_with_assoc_ctx(ty, &generic_map, None, &assoc_ctx);
    infer_type_to_type(&subst.apply(&inferred), span)
}

fn row_bound_error_prefix(bound: &GenericBound) -> String {
    match bound {
        GenericBound::Row(_) => format!("row bound `{bound}`"),
        GenericBound::Aspect(name) => format!("bound `{name}`"),
    }
}

fn check_record_kind_requirement(
    concrete: &Type,
    bounds: &[GenericBound],
    record_kind: bool,
    fun_name: &str,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(), MetelError> {
    let has_row_bound = bounds
        .iter()
        .any(|bound| matches!(bound, GenericBound::Row(_)));
    if has_row_bound && !record_kind {
        return Err(MetelError::type_error(
            TypeErrorCode::T0012,
            format!(
                "row bound requires a record-kinded type parameter in `{fun_name}`; add `record` before the type parameter"
            ),
            span,
        ));
    }
    if !record_kind {
        return Ok(());
    }
    match concrete {
        Type::Record(_) => Ok(()),
        Type::Named(name, _) => {
            let message = match registry.visible_type_kind(current_module, name) {
                Some(crate::typeinference::VisibleTypeKind::Struct) => format!(
                    "`{name}` is a struct, but a struct never satisfies a row bound; conversion to a record is not available in this release"
                ),
                _ => format!("`{name}` is not a record, and only records satisfy a `record` type parameter"),
            };
            Err(MetelError::type_error(TypeErrorCode::T0012, message, span))
        }
        other => Err(MetelError::type_error(
            TypeErrorCode::T0012,
            format!(
                "`{other}` is not a record, and only records satisfy a `record` type parameter"
            ),
            span,
        )),
    }
}

fn check_positive_row_bound(
    record_fields: &[(String, Type)],
    row: &RowConstraint,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
    generic_types_by_name: &HashMap<String, Type>,
) -> Result<(), MetelError> {
    let bound = GenericBound::Row(row.clone());
    let actual_labels: Vec<&str> = record_fields
        .iter()
        .map(|(label, _)| label.as_str())
        .collect();
    if !row.open && record_fields.len() != row.fields.len() {
        return Err(MetelError::type_error(
            TypeErrorCode::T0012,
            format!(
                "{} requires exactly these labels, but the record has: {}",
                row_bound_error_prefix(&bound),
                actual_labels.join(", ")
            ),
            span,
        ));
    }
    for required in &row.fields {
        let Ok(index) = record_fields
            .binary_search_by(|(label, _)| label.as_str().cmp(required.label.as_str()))
        else {
            return Err(MetelError::type_error(
                TypeErrorCode::T0012,
                format!(
                    "{} requires label `{}`, but the record has: {}",
                    row_bound_error_prefix(&bound),
                    required.label,
                    actual_labels.join(", ")
                ),
                span,
            ));
        };
        if let Some(expected_ty_expr) = &required.ty {
            let expected_ty = resolve_row_field_type(
                expected_ty_expr,
                generic_types_by_name,
                span,
                registry,
                current_module,
            )?;
            let actual_ty = &record_fields[index].1;
            if actual_ty != &expected_ty {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0012,
                    format!(
                        "{} requires label `{}` to have type `{expected_ty}`, but it is `{actual_ty}`",
                        row_bound_error_prefix(&bound),
                        required.label
                    ),
                    span,
                ));
            }
        }
    }
    Ok(())
}

fn check_negative_row_bound(
    record_fields: &[(String, Type)],
    row: &RowConstraint,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
    generic_types_by_name: &HashMap<String, Type>,
) -> Result<(), MetelError> {
    let bound = GenericBound::Row(row.clone());
    for forbidden in &row.fields {
        let Ok(index) = record_fields
            .binary_search_by(|(label, _)| label.as_str().cmp(forbidden.label.as_str()))
        else {
            continue;
        };
        let actual_ty = &record_fields[index].1;
        let matches = if let Some(expected_ty_expr) = &forbidden.ty {
            let expected_ty = resolve_row_field_type(
                expected_ty_expr,
                generic_types_by_name,
                span,
                registry,
                current_module,
            )?;
            actual_ty == &expected_ty
        } else {
            true
        };
        if matches {
            return Err(MetelError::type_error(
                TypeErrorCode::T0012,
                format!(
                    "negative row bound `!{}` is not satisfied because the record has label `{}`",
                    bound, forbidden.label
                ),
                span,
            ));
        }
    }
    Ok(())
}

/// Check one concrete type against a set of required bounds. Aspect bounds are
/// checked against the impl registry; row bounds are handled structurally.
#[allow(clippy::too_many_arguments)] // threads registry + module + generic map through bound checking
#[allow(clippy::too_many_lines)] // structural/reference Copy diagnostics extend the central bound checker
fn check_type_satisfies_bounds(
    concrete: &Type,
    bounds: &[GenericBound],
    record_kind: bool,
    fun_name: &str,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
    generic_types_by_name: &HashMap<String, Type>,
) -> Result<(), MetelError> {
    check_record_kind_requirement(
        concrete,
        bounds,
        record_kind,
        fun_name,
        span,
        registry,
        current_module,
    )?;
    if let Type::Record(record_fields) = concrete {
        for bound in bounds {
            if let GenericBound::Row(row) = bound {
                check_positive_row_bound(
                    record_fields,
                    row,
                    span,
                    registry,
                    current_module,
                    generic_types_by_name,
                )?;
            }
        }
    }

    let type_name = match concrete {
        Type::Named(n, _) => n.clone(),
        Type::Array(elem) => {
            for aspect in bounds.iter().filter_map(GenericBound::aspect_name) {
                if !registry.type_satisfies_aspect(current_module, concrete, aspect) {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0012,
                        format!(
                            "`{concrete}` does not implement `{aspect}` (required by `{fun_name}`)\n       hint: arrays implement `{aspect}` only when their element type `{elem}` does"
                        ),
                        span,
                    ));
                }
            }
            return Ok(());
        }
        Type::SizedArray(elem, _) => {
            for aspect in bounds.iter().filter_map(GenericBound::aspect_name) {
                if !registry.type_satisfies_aspect(current_module, concrete, aspect) {
                    let message = if aspect == "Copy" {
                        format!(
                            "`{concrete}` does not implement `{aspect}` (required by `{fun_name}`)\n       hint: fixed-size arrays implement `{aspect}` only when their element type `{elem}` does"
                        )
                    } else {
                        format!(
                            "`{concrete}` does not implement `{aspect}` (required by `{fun_name}`)"
                        )
                    };
                    return Err(MetelError::type_error(TypeErrorCode::T0012, message, span));
                }
            }
            return Ok(());
        }
        Type::Tuple(_) => {
            for aspect in bounds.iter().filter_map(GenericBound::aspect_name) {
                if !registry.type_satisfies_aspect(current_module, concrete, aspect) {
                    let message = if aspect == "Copy" {
                        format!(
                            "`{concrete}` does not implement `{aspect}` (required by `{fun_name}`)\n       hint: tuples implement `{aspect}` only when every element does"
                        )
                    } else {
                        format!(
                            "`{concrete}` does not implement `{aspect}` (required by `{fun_name}`)\n       hint: tuple impls are not yet provided; use a named struct instead"
                        )
                    };
                    return Err(MetelError::type_error(TypeErrorCode::T0012, message, span));
                }
            }
            return Ok(());
        }
        Type::Reference(_) | Type::MutReference(_) | Type::Fun(_, _) => {
            for aspect in bounds.iter().filter_map(GenericBound::aspect_name) {
                if !registry.type_satisfies_aspect(current_module, concrete, aspect) {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0012,
                        format!(
                            "`{concrete}` does not implement `{aspect}` (required by `{fun_name}`)"
                        ),
                        span,
                    ));
                }
            }
            return Ok(());
        }
        Type::Record(_) => {
            for aspect in bounds.iter().filter_map(GenericBound::aspect_name) {
                if !registry.type_satisfies_aspect(current_module, concrete, aspect) {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0012,
                        format!(
                            "`{concrete}` does not implement `{aspect}` (required by `{fun_name}`)\n       hint: an anonymous record satisfies only the auto-derived aspects, field-wise; anything impl-based needs a nominal type — declare a `struct` and implement `{aspect}` there"
                        ),
                        span,
                    ));
                }
            }
            return Ok(());
        }
        other => match super::inference::primitive_type_name(other) {
            Some(n) => n,
            None => return Ok(()),
        },
    };
    for aspect in bounds.iter().filter_map(GenericBound::aspect_name) {
        if !registry.type_satisfies_aspect(current_module, concrete, aspect) {
            return Err(MetelError::type_error(
                TypeErrorCode::T0012,
                format!("`{type_name}` does not implement `{aspect}` (required by `{fun_name}`)"),
                span,
            ));
        }
    }
    Ok(())
}

/// Check that a concrete type does NOT satisfy a set of negative bounds.
#[allow(clippy::too_many_arguments)] // mirrors check_type_satisfies_bounds' parameter list
fn check_type_does_not_satisfy_bound(
    concrete: &Type,
    neg_bounds: &[GenericBound],
    record_kind: bool,
    fun_name: &str,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
    generic_types_by_name: &HashMap<String, Type>,
) -> Result<(), MetelError> {
    check_record_kind_requirement(
        concrete,
        neg_bounds,
        record_kind,
        fun_name,
        span,
        registry,
        current_module,
    )?;
    if let Type::Record(record_fields) = concrete {
        for bound in neg_bounds {
            if let GenericBound::Row(row) = bound {
                check_negative_row_bound(
                    record_fields,
                    row,
                    span,
                    registry,
                    current_module,
                    generic_types_by_name,
                )?;
            }
        }
    }

    // Unlike `check_type_satisfies_bounds` above, this doesn't need a per-shape
    // hint message (the negative-bound loop below never had one, even for
    // `Type::Named`) — only a display string, so every structural shape can
    // share one arm via `Type`'s own `Display` impl. Without these arms, any
    // `concrete` that isn't `Type::Named` or a recognized primitive — every
    // tuple, array, record, reference, and function type — fell through to
    // `None => return Ok(())` below and skipped the aspect check entirely
    // (#632). Observable today for tuples, fixed-size arrays, and shared
    // references, which really are `Copy` — a `T: !Copy` bound silently
    // accepted them. Records and function types aren't classified `Copy` by
    // `type_satisfies_aspect` either way (confirmed: the positive `T: Copy`
    // path already rejects both), so this closes the same hole for whichever
    // aspect they do end up satisfying, without changing today's behavior for
    // either shape.
    let type_name = match concrete {
        Type::Named(n, _) => n.clone(),
        Type::Array(_)
        | Type::SizedArray(_, _)
        | Type::Tuple(_)
        | Type::Reference(_)
        | Type::MutReference(_)
        | Type::Fun(_, _)
        | Type::Record(_) => concrete.to_string(),
        other => match super::inference::primitive_type_name(other) {
            Some(n) => n,
            None => return Ok(()),
        },
    };
    for aspect in neg_bounds.iter().filter_map(GenericBound::aspect_name) {
        if registry.type_satisfies_aspect(current_module, concrete, aspect) {
            if aspect == "Drop" && registry.type_satisfies_aspect(current_module, concrete, "Copy")
            {
                continue;
            }
            return Err(MetelError::type_error(
                TypeErrorCode::T0012,
                format!(
                    "`{type_name}` implements `{aspect}`; `!{aspect}` bound not satisfied (required by `{fun_name}`)"
                ),
                span,
            ));
        }
    }
    Ok(())
}

/// Check negative bounds via the TypeVar-keyed registry (module-local, same
/// lifetime as `fun_bounds`).
fn check_fun_call_neg_bounds(
    fun_name: &str,
    var_to_type: &HashMap<TypeVar, Type>,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(), MetelError> {
    let bounds_map = registry.neg_fun_bounds_for(fun_name);
    let record_kinds = registry.fun_record_kinds_for(fun_name);
    let generic_types_by_name: HashMap<String, Type> = HashMap::new();
    for (tv, concrete) in var_to_type {
        let bounds = bounds_map
            .and_then(|map| map.get(tv))
            .map_or(&[][..], Vec::as_slice);
        let record_kind = record_kinds
            .and_then(|map| map.get(tv))
            .copied()
            .unwrap_or(false);
        if bounds.is_empty() && !record_kind {
            continue;
        }
        check_type_does_not_satisfy_bound(
            concrete,
            bounds,
            record_kind,
            fun_name,
            span,
            registry,
            current_module,
            &generic_types_by_name,
        )?;
    }
    Ok(())
}

/// Check negative bounds carried ON a scheme (`TypeScheme::neg_bounds`,
/// positional per quantified var). Handles imported/prelude schemes whose
/// TypeVar-keyed `neg_fun_bounds` registry entry may not exist locally.
fn check_scheme_neg_bounds(
    fun_name: &str,
    scheme: &TypeScheme,
    var_to_type: &HashMap<TypeVar, Type>,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(), MetelError> {
    let generic_types_by_name: HashMap<String, Type> = scheme
        .quantified_vars
        .iter()
        .zip(&scheme.param_names)
        .filter_map(|(tv, name)| var_to_type.get(tv).cloned().map(|ty| (name.clone(), ty)))
        .collect();
    for (index, tv) in scheme.quantified_vars.iter().enumerate() {
        let bounds = scheme.neg_bounds.get(index).map_or(&[][..], Vec::as_slice);
        let record_kind = scheme.record_kinds.get(index).copied().unwrap_or(false);
        if bounds.is_empty() && !record_kind {
            continue;
        }
        let Some(concrete) = var_to_type.get(tv) else {
            continue;
        };
        check_type_does_not_satisfy_bound(
            concrete,
            bounds,
            record_kind,
            fun_name,
            span,
            registry,
            current_module,
            &generic_types_by_name,
        )?;
    }
    Ok(())
}

fn instantiate_scheme_for_call(
    scheme: &TypeScheme,
    arg_types: &[&Type],
    span: &Span,
    gen: &mut TypeVarGenerator,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(Type, HashMap<TypeVar, Type>), MetelError> {
    let (instance, renaming) = typeinference::instantiate_with_renaming(scheme, gen);

    let InferType::Fun(params, ret) = instance else {
        return Err(MetelError::internal("scheme type is not a function"));
    };

    let mut subst = Substitution::new();
    for (param, arg_ty) in params.iter().zip(arg_types.iter()) {
        let arg_infer = type_to_infer(arg_ty);
        let applied = subst.apply(param);
        let s = unify(&applied, &arg_infer).map_err(|_| {
            MetelError::type_error(TypeErrorCode::T0001, "argument type mismatch", span)
        })?;
        subst = subst.compose(&s);
    }

    // RFC-0082 backfill: for each projection in the scheme, resolve the base
    // type param to a concrete type and bind the projection's placeholder var
    // to the concrete associated type from the impl.
    for proj in scheme.assoc_projections.iter().flatten() {
        let (base_pos, aspect, assoc, placeholder_tv) = proj;
        let base_orig = scheme.quantified_vars[*base_pos];
        let fresh_base = renaming.get(&base_orig).copied().unwrap_or(base_orig);
        if let InferType::Named(base_name, _) = subst.apply(&InferType::Var(fresh_base)) {
            if let Some(concrete_ty) =
                registry.impl_assoc_type(current_module, &base_name, aspect, assoc)
            {
                if let Some(fresh_placeholder) = renaming.get(placeholder_tv) {
                    subst.bind(*fresh_placeholder, InferType::Concrete(concrete_ty.clone()));
                }
            }
        }
    }

    // RFC-0037 backfill: for each opaque-return quantified var, bind its fresh
    // copy to the concrete type recorded at definition time. This lets the
    // `infer_type_to_type` calls below succeed using the known concrete type
    // rather than requiring ordinary substitution to have resolved it.
    for (i, opaque) in scheme.opaque_returns.iter().enumerate() {
        if let Some((_aspect, concrete_ty)) = opaque {
            if let Some(&orig_tv) = scheme.quantified_vars.get(i) {
                if let Some(&fresh_tv) = renaming.get(&orig_tv) {
                    subst.bind(fresh_tv, InferType::Concrete(concrete_ty.clone()));
                }
            }
        }
    }

    let concrete_params: Vec<Type> = params
        .iter()
        .map(|p| infer_type_to_type(&subst.apply(p), span))
        .collect::<Result<_, _>>()?;
    let concrete_ret = infer_type_to_type(&subst.apply(&ret), span)?;

    // Build original-quantified-var → concrete-type mapping for bound checking.
    let mut var_to_concrete: HashMap<TypeVar, Type> = HashMap::new();
    for (orig_var, fresh_var) in &renaming {
        if let Ok(t) = infer_type_to_type(&subst.apply(&InferType::Var(*fresh_var)), span) {
            var_to_concrete.insert(*orig_var, t);
        }
    }

    Ok((
        Type::Fun(concrete_params, Box::new(concrete_ret)),
        var_to_concrete,
    ))
}

fn instantiate_scheme_with_turbofish(
    scheme: &TypeScheme,
    explicit_types: &[Type],
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(Type, HashMap<TypeVar, Type>), MetelError> {
    if explicit_types.len() != scheme.quantified_vars.len() {
        return Err(MetelError::type_error(
            TypeErrorCode::T0004,
            format!(
                "expected {} type argument(s), got {}",
                scheme.quantified_vars.len(),
                explicit_types.len()
            ),
            span,
        ));
    }
    let mut subst = Substitution::new();
    let mut var_to_concrete: HashMap<TypeVar, Type> = HashMap::new();
    for (&qvar, concrete_ty) in scheme.quantified_vars.iter().zip(explicit_types.iter()) {
        subst.bind(qvar, type_to_infer(concrete_ty));
        var_to_concrete.insert(qvar, concrete_ty.clone());
    }
    // RFC-0082 backfill: bind projection placeholder vars to their concrete associated types.
    for proj in scheme.assoc_projections.iter().flatten() {
        let (base_pos, aspect, assoc, placeholder_tv) = proj;
        if let Some(Type::Named(base_name, _)) =
            var_to_concrete.get(&scheme.quantified_vars[*base_pos])
        {
            if let Some(concrete_ty) =
                registry.impl_assoc_type(current_module, base_name, aspect, assoc)
            {
                subst.bind(*placeholder_tv, InferType::Concrete(concrete_ty.clone()));
            }
        }
    }
    // RFC-0037 backfill: bind opaque-return vars to their concrete types.
    for (i, opaque) in scheme.opaque_returns.iter().enumerate() {
        if let Some((_aspect, concrete_ty)) = opaque {
            if let Some(&orig_tv) = scheme.quantified_vars.get(i) {
                subst.bind(orig_tv, InferType::Concrete(concrete_ty.clone()));
            }
        }
    }
    let instantiated = subst.apply(&scheme.ty);
    let concrete_ty = infer_type_to_type(&instantiated, span)?;
    Ok((concrete_ty, var_to_concrete))
}

/// Instantiate a scheme by unifying its return type with `expected_ret`.
/// Used for zero-arg generic constructors (e.g. `List::new()`) where T cannot
/// be inferred from arguments but is known from the enclosing let annotation.
fn instantiate_scheme_with_expected_ret(
    scheme: &TypeScheme,
    arg_types: &[&Type],
    expected_ret: &Type,
    span: &Span,
    gen: &mut TypeVarGenerator,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(Type, HashMap<TypeVar, Type>), MetelError> {
    let (instance, renaming) = typeinference::instantiate_with_renaming(scheme, gen);
    let InferType::Fun(params, ret) = instance else {
        return Err(MetelError::internal("scheme type is not a function"));
    };
    let mut subst = Substitution::new();
    for (param, arg_ty) in params.iter().zip(arg_types.iter()) {
        let applied = subst.apply(param);
        let s = typeinference::unify(&applied, &type_to_infer(arg_ty)).map_err(|_| {
            MetelError::type_error(TypeErrorCode::T0001, "argument type mismatch", span)
        })?;
        subst = subst.compose(&s);
    }
    let applied_ret = subst.apply(&ret);
    let s = typeinference::unify(&applied_ret, &type_to_infer(expected_ret)).map_err(|_| {
        MetelError::type_error(
            TypeErrorCode::T0001,
            "return type does not match annotation",
            span,
        )
    })?;
    subst = subst.compose(&s);
    // RFC-0082 backfill: bind projection placeholder vars to their concrete associated types.
    for proj in scheme.assoc_projections.iter().flatten() {
        let (base_pos, aspect, assoc, placeholder_tv) = proj;
        let base_orig = scheme.quantified_vars[*base_pos];
        let fresh_base = renaming.get(&base_orig).copied().unwrap_or(base_orig);
        if let InferType::Named(base_name, _) = subst.apply(&InferType::Var(fresh_base)) {
            if let Some(concrete_ty) =
                registry.impl_assoc_type(current_module, &base_name, aspect, assoc)
            {
                if let Some(fresh_placeholder) = renaming.get(placeholder_tv) {
                    subst.bind(*fresh_placeholder, InferType::Concrete(concrete_ty.clone()));
                }
            }
        }
    }
    // RFC-0037 backfill: bind opaque-return vars to their concrete types.
    for (i, opaque) in scheme.opaque_returns.iter().enumerate() {
        if let Some((_aspect, concrete_ty)) = opaque {
            if let Some(&orig_tv) = scheme.quantified_vars.get(i) {
                if let Some(&fresh_tv) = renaming.get(&orig_tv) {
                    subst.bind(fresh_tv, InferType::Concrete(concrete_ty.clone()));
                }
            }
        }
    }
    let concrete_params: Vec<Type> = params
        .iter()
        .map(|p| infer_type_to_type(&subst.apply(p), span))
        .collect::<Result<_, _>>()?;
    let concrete_ret = infer_type_to_type(&subst.apply(&ret), span)?;
    let mut var_to_concrete: HashMap<TypeVar, Type> = HashMap::new();
    for (orig_var, fresh_var) in &renaming {
        if let Ok(t) = infer_type_to_type(&subst.apply(&InferType::Var(*fresh_var)), span) {
            var_to_concrete.insert(*orig_var, t);
        }
    }
    Ok((
        Type::Fun(concrete_params, Box::new(concrete_ret)),
        var_to_concrete,
    ))
}

fn construct_literal_type(
    lit: &Literal,
    expected_ty: Option<&Type>,
    span: &Span,
) -> Result<Type, MetelError> {
    use crate::ast::{FloatKind, IntKind};
    match lit {
        Literal::Int(n) => {
            match expected_ty {
                Some(Type::I8) => Ok(Type::I8),
                Some(Type::I16) => Ok(Type::I16),
                Some(Type::I32) => Ok(Type::I32),
                Some(Type::U8) => Ok(Type::U8),
                Some(Type::U16) => Ok(Type::U16),
                Some(Type::U32) => Ok(Type::U32),
                Some(Type::U64) => {
                    if *n < 0 {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0005,
                            format!("integer literal `{n}` is negative and cannot be used as a u64 index"),
                            span,
                        ));
                    }
                    Ok(Type::U64)
                }
                Some(Type::F32) => Ok(Type::F32),
                Some(Type::F64) => Ok(Type::F64),
                _ => Ok(Type::I64),
            }
        }
        Literal::Float(_) => match expected_ty {
            Some(Type::F32) => Ok(Type::F32),
            _ => Ok(Type::F64),
        },
        Literal::SizedInt { kind, .. } => Ok(match kind {
            IntKind::I8 => Type::I8,
            IntKind::I16 => Type::I16,
            IntKind::I32 => Type::I32,
            IntKind::I64 => Type::I64,
            IntKind::U8 => Type::U8,
            IntKind::U16 => Type::U16,
            IntKind::U32 => Type::U32,
            IntKind::U64 => Type::U64,
        }),
        Literal::SizedFloat { kind, .. } => Ok(match kind {
            FloatKind::F32 => Type::F32,
            FloatKind::F64 => Type::F64,
        }),
        Literal::Char(_) => Ok(Type::Char),
        Literal::Boolean(_) => Ok(Type::Boolean),
        Literal::Str(_) => Ok(Type::Str),
        Literal::Unit => Ok(Type::Unit),
    }
}

fn construct_binop(
    lhs: &Expr,
    op: &BinOp,
    rhs: &Expr,
    span: &Span,
    ctx: &mut ConstructCtx,
) -> Result<TypedExpr, MetelError> {
    let lhs_built = construct_expr(lhs, None, ctx)?;
    let rhs_built = construct_expr(rhs, None, ctx)?;
    // If one operand is a polymorphic literal that defaulted to i64/f64, and the other
    // has a more specific numeric type, re-build the literal with the concrete type.
    let lt = lhs_built.ty().clone();
    let rt = rhs_built.ty().clone();
    let (lhs, rhs) = if (lt == Type::I64 || lt == Type::F64) && rt.is_numeric() && rt != lt {
        (construct_expr(lhs, Some(&rt), ctx)?, rhs_built)
    } else if (rt == Type::I64 || rt == Type::F64) && lt.is_numeric() && lt != rt {
        (lhs_built, construct_expr(rhs, Some(&lt), ctx)?)
    } else {
        (lhs_built, rhs_built)
    };
    let ty = match op {
        BinOp::Add => {
            let t = lhs.ty();
            if !matches!(t, Type::Str | Type::Never) && !t.is_numeric() {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0005,
                    format!("`+` requires a numeric type or String operands, got `{t}`"),
                    span,
                ));
            }
            t.clone()
        }
        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
            let t = lhs.ty();
            if !matches!(t, Type::Never) && !t.is_numeric() {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0005,
                    format!("arithmetic operator requires a numeric type operand, got `{t}`"),
                    span,
                ));
            }
            t.clone()
        }
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let t = lhs.ty();
            if !matches!(t, Type::Str | Type::Char | Type::Never) && !t.is_numeric() {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0005,
                    format!(
                        "ordering comparison requires a numeric type or String operands, got `{t}`"
                    ),
                    span,
                ));
            }
            Type::Boolean
        }
        BinOp::Eq | BinOp::Ne => {
            // metel-core#279: this arm previously returned `Type::Boolean` with no
            // operand check at all, sitting right beside the ordering arm above that
            // does check. Pass 1 only constrains the two operands to unify *with each
            // other*, so mixed types were caught by unification while same-type-on-both
            // -sides reached an evaluator that only has `==` arms for the primitive
            // scalars — producing an I0001 internal error at run time for references,
            // structs, enums (including `Perhaps`), arrays, tuples and unit.
            //
            // Deliberately *rejects* rather than peeling references: peeling would
            // silently commit the language to referent-equality semantics, and whether
            // two references should compare referents (Rust) or identity (Go) is an open
            // design question (metel-core#263). Rejecting is direction-neutral, and
            // status-quo-preserving since these already failed, just badly.
            //
            // The real fix is routing `==` through the `Eq` aspect (metel-core#263 /
            // RFC-0062); this guard relaxes as that lands.
            let t = lhs.ty();
            if !matches!(t, Type::Str | Type::Char | Type::Boolean | Type::Never) && !t.is_numeric()
            {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0005,
                    format!(
                        "equality comparison requires a primitive operand (numeric, \
                         boolean, String, or char), got `{t}`; `==` does not yet dispatch \
                         through the `Eq` aspect — use `.eq(..)` on a type that implements it"
                    ),
                    span,
                ));
            }
            Type::Boolean
        }
        BinOp::And | BinOp::Or => Type::Boolean,
        BinOp::Range | BinOp::RangeInclusive => Type::Named("Range".to_string(), vec![Type::I64]),
    };
    Ok(TypedExpr::BinOp(
        Box::new(lhs),
        op.clone(),
        Box::new(rhs),
        ty,
        span.clone(),
    ))
}

fn type_to_type_expr(ty: &Type) -> TypeExpr {
    let named = |s: &str| TypeExpr::Named(s.to_string(), vec![]);
    match ty {
        Type::I64 => named("i64"),
        Type::F64 => named("f64"),
        Type::Boolean => named("boolean"),
        Type::Char => named("Char"),
        Type::Str => named("String"),
        Type::Unit => TypeExpr::Unit,
        Type::Never => named("!"),
        Type::I8 => named("i8"),
        Type::I16 => named("i16"),
        Type::I32 => named("i32"),
        Type::U8 => named("u8"),
        Type::U16 => named("u16"),
        Type::U32 => named("u32"),
        Type::U64 => named("u64"),
        Type::F32 => named("f32"),
        Type::Tuple(items) => TypeExpr::Tuple(items.iter().map(type_to_type_expr).collect()),
        Type::Record(fields) => TypeExpr::Record(
            fields
                .iter()
                .map(|(name, ty)| (name.clone(), type_to_type_expr(ty)))
                .collect(),
        ),
        Type::Array(item) => TypeExpr::Array(Box::new(type_to_type_expr(item))),
        Type::SizedArray(item, n) => TypeExpr::SizedArray(Box::new(type_to_type_expr(item)), *n),
        Type::Reference(item) => TypeExpr::Reference(Box::new(type_to_type_expr(item))),
        Type::MutReference(item) => TypeExpr::MutReference(Box::new(type_to_type_expr(item))),
        Type::Fun(params, ret) => TypeExpr::Fun(
            params.iter().map(type_to_type_expr).collect(),
            Some(Box::new(type_to_type_expr(ret))),
        ),
        Type::Named(name, args) => {
            TypeExpr::Named(name.clone(), args.iter().map(type_to_type_expr).collect())
        }
    }
}

fn construct_propagate_error(
    expr: &Expr,
    span: &Span,
    ctx: &mut ConstructCtx,
) -> Result<TypedExpr, MetelError> {
    let scrutinee = construct_expr(expr, None, ctx)?;
    let (ok_ty, source_err_ty) = match scrutinee.ty() {
        Type::Named(name, args) if name == "Result" && args.len() == 2 => {
            (args[0].clone(), args[1].clone())
        }
        other => {
            return Err(MetelError::type_error(
                TypeErrorCode::T0005,
                format!("`?` requires a Result<T, E> expression, got `{other}`"),
                span,
            ));
        }
    };

    let return_ty = ctx.current_return_ty.clone().ok_or_else(|| {
        MetelError::type_error(
            TypeErrorCode::T0005,
            "`?` can only be used inside a function or closure that returns Result<T, E>",
            span,
        )
    })?;
    let target_err_ty = match &return_ty {
        Type::Named(name, args) if name == "Result" && args.len() == 2 => args[1].clone(),
        other => {
            return Err(MetelError::type_error(
                TypeErrorCode::T0005,
                format!(
                    "`?` requires the enclosing function to return Result<T, E>, got `{other}`"
                ),
                span,
            ));
        }
    };

    let ok_arm = TypedMatchArm {
        pattern: Pattern::EnumVariant {
            path: vec!["Result".to_string(), "Ok".to_string()],
            fields: vec!["value".to_string()],
            rest: false,
            span: span.clone(),
        },
        guard: None,
        body: TypedBlock {
            stmts: vec![],
            tail: Some(Box::new(TypedExpr::Ident(
                "value".to_string(),
                ok_ty.clone(),
                span.clone(),
            ))),
            span: span.clone(),
        },
        span: span.clone(),
    };

    let err_value = if source_err_ty == target_err_ty {
        TypedExpr::Ident("error".to_string(), source_err_ty, span.clone())
    } else {
        TypedExpr::Cast {
            expr: Box::new(TypedExpr::Ident(
                "error".to_string(),
                source_err_ty,
                span.clone(),
            )),
            target_type: type_to_type_expr(&target_err_ty),
            ty: target_err_ty,
            span: span.clone(),
        }
    };
    let err_arm = TypedMatchArm {
        pattern: Pattern::EnumVariant {
            path: vec!["Result".to_string(), "Err".to_string()],
            fields: vec!["error".to_string()],
            rest: false,
            span: span.clone(),
        },
        guard: None,
        body: TypedBlock {
            stmts: vec![TypedDecl::Stmt(Box::new(TypedStmt::Expr(
                TypedExpr::Return(TypedReturnExpr {
                    value: Some(Box::new(TypedExpr::StructLiteral {
                        path: vec!["Result".to_string(), "Err".to_string()],
                        fields: vec![("error".to_string(), err_value)],
                        ty: return_ty,
                        type_id: Some(crate::symbols::SYM_TYPE_RESULT),
                        span: span.clone(),
                    })),
                    span: span.clone(),
                }),
            )))],
            tail: None,
            span: span.clone(),
        },
        span: span.clone(),
    };

    Ok(TypedExpr::Match(TypedMatchExpr {
        scrutinee: Box::new(scrutinee),
        arms: vec![ok_arm, err_arm],
        expr_type: ok_ty,
        span: span.clone(),
    }))
}

/// The first shared-reference type encountered walking an lvalue path towards its root,
/// if any. Mirrors `typed_ast::is_lvalue_path`'s shape deliberately: the two must agree on
/// what a path *is*, or one admits a form the other rejects (see #313).
fn shared_reference_root_in_lvalue_path(expr: &TypedExpr) -> Option<&Type> {
    match expr {
        TypedExpr::FieldAccess { object, .. }
        | TypedExpr::TupleAccess { object, .. }
        | TypedExpr::Index { object, .. }
        | TypedExpr::UnaryOp(UnaryOp::Deref, object, _, _) => {
            if let Type::Reference(inner) = object.ty() {
                Some(inner.as_ref())
            } else {
                shared_reference_root_in_lvalue_path(object)
            }
        }
        _ => None,
    }
}

fn construct_unaryop(
    op: &UnaryOp,
    operand: &Expr,
    span: &Span,
    expected_ty: Option<&Type>,
    ctx: &mut ConstructCtx,
) -> Result<TypedExpr, MetelError> {
    let operand = construct_expr(operand, expected_ty, ctx)?;
    let ty = match op {
        UnaryOp::Neg => {
            let t = operand.ty();
            if !matches!(t, Type::Never) && !t.is_numeric() {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0005,
                    format!("unary negation requires a numeric type operand, got `{t}`"),
                    span,
                ));
            }
            t.clone()
        }
        UnaryOp::Not => Type::Boolean,
        // metel-core#280: addressability is checked here, not at run time. The rule is
        // purely syntactic on the typed AST, so it was always static-determinable; the
        // evaluator used to decide it with the same predicate and raise
        // `MetelError::internal` on failure — an internal error for a documented,
        // user-reachable rejection (RFC-0044 §9), raised only after any side effects in
        // the surrounding statement had already run.
        UnaryOp::Ref | UnaryOp::RefMut => {
            if !crate::typed_ast::is_lvalue_path(&operand) {
                // `&<rvalue>` / `&var <rvalue>` is temporary lifetime extension
                // (matching Rust/C++: `foo(&Vec::new())`, `foo(&mut Vec::new())`):
                // materialize the value into a fresh, independent storage cell
                // instead of requiring the caller to bind it to a name first. Sound
                // for both forms — nothing outside this expression can ever alias the
                // cell, so a mutable reference to it can never conflict with anything.
                let mutable = matches!(op, UnaryOp::RefMut);
                let inner_ty = operand.ty().clone();
                let ty = if mutable {
                    Type::MutReference(Box::new(inner_ty))
                } else {
                    Type::Reference(Box::new(inner_ty))
                };
                return Ok(TypedExpr::RefTemp {
                    init: Box::new(operand),
                    mutable,
                    ty,
                    span: span.clone(),
                });
            }
            if matches!(op, UnaryOp::RefMut) {
                // `&var` cannot manufacture exclusive access out of any shared
                // reference encountered along the lvalue path, whether spelled
                // explicitly (`&var *r`) or through selectors (`&var r.field`).
                if let Some(shared_inner) = shared_reference_root_in_lvalue_path(&operand) {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0006,
                        format!(
                            "cannot take `&var` through a shared reference `&{shared_inner}`; \
                             a shared reference never grants write access"
                        ),
                        span,
                    ));
                }
            }
            if matches!(op, UnaryOp::RefMut) {
                Type::MutReference(Box::new(operand.ty().clone()))
            } else {
                Type::Reference(Box::new(operand.ty().clone()))
            }
        }
        UnaryOp::Deref => match operand.ty() {
            Type::Reference(inner) | Type::MutReference(inner) => *inner.clone(),
            t => {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0002,
                    format!("cannot dereference non-pointer type `{t}`"),
                    span,
                ));
            }
        },
    };
    Ok(TypedExpr::UnaryOp(
        op.clone(),
        Box::new(operand),
        ty,
        span.clone(),
    ))
}

/// Peels every reference layer of a chain down to the first non-reference type
/// (RFC-0067a §3's auto-deref chain guarantee — `&&T` derefs through both levels —
/// applies uniformly to field access, method dispatch, and read-copy; a single-level
/// peel leaves a receiver like `&&mut Counter` still wrapped after one step).
fn peel_type_references(ty: &Type) -> &Type {
    match ty {
        Type::Reference(inner) | Type::MutReference(inner) => peel_type_references(inner),
        other => other,
    }
}

fn type_chain_provides_mut_access(ty: &Type) -> bool {
    match ty {
        Type::MutReference(_) => true,
        Type::Reference(inner) => type_chain_provides_mut_access(inner),
        _ => false,
    }
}

/// RFC-0067a §3a: if `actual`'s type is a reference to exactly `expected`, synthesize
/// the internal deref-copy node so the value is copied out of the reference. The parser
/// never produces `UnaryOp::Deref` any more (§3 removed explicit `*p`); this is the only
/// place it's constructed post-RFC-0067a. Only called from sites that have a genuine
/// declared/expected type in hand (`construct_block`'s tail, `Stmt::Return`/`Break`,
/// `Decl::Let`/`Mut`, `Expr::Ascribe`) — never from `construct_expr`'s or
/// `construct_unaryop`'s generic dispatch, which call-argument construction also goes
/// through and must not gain this coercion.
/// Peels *every* reference layer needed to reach `expected`, not just one — RFC-0067a
/// §3's auto-deref chain guarantee (`&&T` derefs through both levels) applies to
/// read-copy the same as ordinary auto-deref: `let x: i64 = rr;` where `rr: &&i64`
/// synthesizes two nested internal `Deref` nodes, one per layer, matching how the
/// evaluator already unwraps one `Value::Reference`/`MutReference` per node.
///
/// #649: this is a *copy* — the referent is duplicated, not moved, and the reference
/// keeps pointing at a still-valid original — so it's only sound when the fully
/// peeled referent type is `Copy` (RFC-0067a §3a's own words: "a non-`Copy` `T`
/// cannot be produced this way"). Checked once against the final, fully-dereferenced
/// type, not each intermediate reference layer: a `&T`/`&var T` layer being read
/// *through* isn't itself what's being duplicated, only the payload at the end of
/// the chain is.
fn maybe_read_copy(
    expected: &Type,
    actual: TypedExpr,
    span: &Span,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<TypedExpr, MetelError> {
    // If `expected` is itself a reference type, this isn't read-copy at all — it's the
    // ordinary `&mut T` -> `&T` widening coercion (unify() already accepts it; nothing
    // to synthesize here). Peeling anyway would over-run past the intended coercion
    // down to the fully-dereferenced value, which is wrong.
    if matches!(expected, Type::Reference(_) | Type::MutReference(_)) {
        return Ok(actual);
    }
    let mut current = actual;
    let mut peeled_any = false;
    while current.ty() != expected {
        let inner = match current.ty() {
            Type::Reference(inner) | Type::MutReference(inner) => (**inner).clone(),
            _ => break,
        };
        peeled_any = true;
        current = TypedExpr::UnaryOp(UnaryOp::Deref, Box::new(current), inner, span.clone());
    }
    if peeled_any && !registry.type_satisfies_aspect(current_module, current.ty(), "Copy") {
        let ty = current.ty();
        return Err(MetelError::type_error(
            TypeErrorCode::T0024,
            format!(
                "cannot copy `{ty}` out of a reference: `{ty}` does not implement `Copy`\n\
                 \x20      hint: use `.clone()` if `{ty}` implements `Clone`, or restructure to take ownership instead"
            ),
            span,
        ));
    }
    Ok(current)
}

/// RFC-0078 §3.3: the inhabited-singleton coercion. If `actual`'s type is a named
/// enum with more than one variant, exactly one of which is inhabited (by the same
/// rule `check_match_exhaustiveness` uses) with exactly one field, and that field's
/// (substituted) type equals `expected`, wrap `actual` in `SingletonCoerce`.
/// Otherwise return `actual` unchanged. Mirrors `maybe_read_copy`'s shape and
/// call-site pattern — called from the same handful of sites that have a genuine
/// expected type in hand.
fn maybe_singleton_coerce(
    expected: &Type,
    actual: TypedExpr,
    span: &Span,
    registry: &TypeDefinitionRegistry,
) -> Result<TypedExpr, MetelError> {
    let actual_ty = actual.ty().clone();
    if &actual_ty == expected {
        return Ok(actual);
    }
    let Type::Named(name, type_args) = &actual_ty else {
        return Ok(actual);
    };
    let Some(enum_info) = registry.enum_info(name) else {
        return Ok(actual);
    };
    if enum_info.variants.len() <= 1 {
        return Ok(actual);
    }
    let remap = enum_variant_type_param_remap(enum_info, type_args);
    let mut inhabited: Option<&VariantInfo> = None;
    for v in &enum_info.variants {
        if is_variant_uninhabited(v, &remap, span) {
            continue;
        }
        if v.fields.len() != 1 || inhabited.is_some() {
            // Zero/multi-field sole-inhabited variant, or more than one inhabited
            // variant: the conditions for the rule don't hold (RFC-0078 §3.3).
            return Ok(actual);
        }
        inhabited = Some(v);
    }
    let Some(variant) = inhabited else {
        return Ok(actual);
    };
    let field = &variant.fields[0];
    let field_ty = infer_type_to_type(&remap.apply(&field.ty), &field.span)?;
    if &field_ty != expected {
        return Ok(actual);
    }
    Ok(TypedExpr::SingletonCoerce {
        inner: Box::new(actual),
        variant: variant.name.clone(),
        field: field.name.clone(),
        ty: field_ty,
        span: span.clone(),
    })
}

// ── Typed place construction ──────────────────────────────────────────────────

fn assign_target_to_typed_place(
    target: &AssignTarget,
    ctx: &mut ConstructCtx<'_>,
) -> Result<TypedPlace, MetelError> {
    match target {
        AssignTarget::Ident(name, span) => Ok(TypedPlace::Ident(name.clone(), span.clone())),
        AssignTarget::FieldAccess {
            object,
            field,
            span,
        } => Ok(TypedPlace::Field {
            object: Box::new(expr_to_typed_place(object, ctx)?),
            field: field.clone(),
            span: span.clone(),
        }),
        AssignTarget::TupleAccess {
            object,
            index,
            span,
        } => {
            let typed_object = expr_to_typed_place(object, ctx)?;
            let raw_object_ty = typed_place_ty(&typed_object, ctx, span)?;
            // Reach through a reference at the root, matching field/index paths.
            let object_ty = peel_type_references(&raw_object_ty).clone();
            match object_ty {
                Type::Tuple(elems) => {
                    if *index >= elems.len() {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0003,
                            format!(
                                "tuple index {index} out of bounds (tuple has {} elements)",
                                elems.len()
                            ),
                            span,
                        ));
                    }
                    Ok(TypedPlace::Tuple {
                        object: Box::new(typed_object),
                        index: *index,
                        span: span.clone(),
                    })
                }
                _ => Err(MetelError::type_error(
                    TypeErrorCode::T0002,
                    "cannot infer tuple type for assignment; add a type annotation",
                    span,
                )),
            }
        }
        AssignTarget::Index {
            object,
            index,
            span,
        } => {
            let typed_idx = construct_expr(index, Some(&Type::U64), ctx)?;
            if typed_idx.ty() != &Type::U64 {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0001,
                    format!(
                        "array index must be u64, got {}; use `expr as u64`",
                        typed_idx.ty()
                    ),
                    span,
                ));
            }
            Ok(TypedPlace::Index {
                object: Box::new(expr_to_typed_place(object, ctx)?),
                index: Box::new(typed_idx),
                span: span.clone(),
            })
        }
        // RFC-0110: `*p = v`. `*` must name a `&var T` — writing through a shared `&T`
        // is not permitted, matching `&`'s read-only contract.
        AssignTarget::Deref { object, span } => {
            let typed_object = construct_expr(object, None, ctx)?;
            match typed_object.ty() {
                Type::MutReference(_) => Ok(TypedPlace::Deref {
                    object: Box::new(typed_object),
                    span: span.clone(),
                }),
                t => Err(MetelError::type_error(
                    TypeErrorCode::T0002,
                    format!("cannot write through `{t}`; `&var T` required"),
                    span,
                )),
            }
        }
    }
}

fn expr_to_typed_place(expr: &Expr, ctx: &mut ConstructCtx<'_>) -> Result<TypedPlace, MetelError> {
    match expr {
        Expr::Ident(name, span) => Ok(TypedPlace::Ident(name.clone(), span.clone())),
        Expr::FieldAccess {
            object,
            field,
            span,
        } => Ok(TypedPlace::Field {
            object: Box::new(expr_to_typed_place(object, ctx)?),
            field: field.clone(),
            span: span.clone(),
        }),
        Expr::TupleAccess {
            object,
            index,
            span,
        } => {
            let typed_object = expr_to_typed_place(object, ctx)?;
            let raw_object_ty = typed_place_ty(&typed_object, ctx, span)?;
            // Reach through a reference at the root, matching field/index paths.
            let object_ty = peel_type_references(&raw_object_ty).clone();
            match object_ty {
                Type::Tuple(elems) => {
                    if *index >= elems.len() {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0003,
                            format!(
                                "tuple index {index} out of bounds (tuple has {} elements)",
                                elems.len()
                            ),
                            span,
                        ));
                    }
                    Ok(TypedPlace::Tuple {
                        object: Box::new(typed_object),
                        index: *index,
                        span: span.clone(),
                    })
                }
                _ => Err(MetelError::type_error(
                    TypeErrorCode::T0002,
                    "cannot infer tuple type for assignment; add a type annotation",
                    span,
                )),
            }
        }
        Expr::Index {
            object,
            index,
            span,
        } => {
            let typed_idx = construct_expr(index, Some(&Type::U64), ctx)?;
            if typed_idx.ty() != &Type::U64 {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0001,
                    format!(
                        "array index must be u64, got {}; use `expr as u64`",
                        typed_idx.ty()
                    ),
                    span,
                ));
            }
            Ok(TypedPlace::Index {
                object: Box::new(expr_to_typed_place(object, ctx)?),
                index: Box::new(typed_idx),
                span: span.clone(),
            })
        }
        Expr::UnaryOp(UnaryOp::Deref, inner, span) => Ok(TypedPlace::Deref {
            object: Box::new(construct_expr(inner, None, ctx)?),
            span: span.clone(),
        }),
        _ => Err(MetelError::internal(
            "invalid sub-expression in assignment target",
        )),
    }
}

fn typed_place_ty(
    place: &TypedPlace,
    ctx: &mut ConstructCtx<'_>,
    span: &Span,
) -> Result<Type, MetelError> {
    match place {
        TypedPlace::Ident(name, ident_span) => ctx.lookup(name).cloned().ok_or_else(|| {
            MetelError::type_error(
                TypeErrorCode::T0003,
                format!("use of undeclared variable `{name}`"),
                ident_span,
            )
        }),
        TypedPlace::Deref { object, .. } => match object.ty() {
            Type::MutReference(inner) => Ok((**inner).clone()),
            t => Err(MetelError::type_error(
                TypeErrorCode::T0002,
                format!("cannot write through `{t}`; `&var T` required"),
                span,
            )),
        },
        TypedPlace::Field {
            object,
            field,
            span: field_span,
        } => {
            // Reach through a reference at any step of the path, not just the root:
            // `s.t.0 = v` for `s: &var S` resolves `s.t` through this arm.
            let object_ty = peel_type_references(&typed_place_ty(object, ctx, field_span)?).clone();
            typed_place_field_ty(&object_ty, field, field_span, ctx)
        }
        TypedPlace::Tuple {
            object,
            index,
            span,
        } => {
            let object_ty = peel_type_references(&typed_place_ty(object, ctx, span)?).clone();
            match object_ty {
                Type::Tuple(elems) => elems.get(*index).cloned().ok_or_else(|| {
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
                    "cannot infer tuple type for assignment; add a type annotation",
                    span,
                )),
            }
        }
        TypedPlace::Index { object, .. } => {
            let object_ty = peel_type_references(&typed_place_ty(object, ctx, span)?).clone();
            match object_ty {
                Type::SizedArray(elem, _) => Ok((*elem).clone()),
                // Not T0002 (#633): the element type is already known here --
                // `values: i64[]` is fully annotated -- and no annotation
                // could fix this. `T[]` views are unconditionally immutable
                // through an index (RFC-0126), independent of whether the
                // binding itself is `var`; that's a different failure shape
                // than T0006's three forms (all of which name a `let`
                // binding that a `var` would fix).
                Type::Array(_) => Err(MetelError::type_error(
                    TypeErrorCode::T0023,
                    "cannot assign through `T[]`: array views are immutable; use `[T; N]` or `List<T>`",
                    span,
                )),
                _ => Err(MetelError::type_error(
                    TypeErrorCode::T0002,
                    "cannot infer array type for assignment; add a type annotation",
                    span,
                )),
            }
        }
    }
}

fn typed_place_field_ty(
    object_ty: &Type,
    field: &str,
    field_span: &Span,
    ctx: &mut ConstructCtx<'_>,
) -> Result<Type, MetelError> {
    match object_ty {
        Type::Record(fields) => fields
            .iter()
            .find(|(name, _)| name == field)
            .map(|(_, ty)| ty.clone())
            .ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!("no field `{field}` on record"),
                    field_span,
                )
            }),
        Type::Named(struct_name, type_args) => {
            if let Some(type_params) = ctx
                .registry
                .raw_struct_type_params()
                .get(struct_name.as_str())
            {
                let raw_fields = ctx
                    .registry
                    .raw_struct_env()
                    .get(struct_name.as_str())
                    .ok_or_else(|| {
                        MetelError::type_error(
                            TypeErrorCode::T0003,
                            format!("unknown type `{struct_name}`"),
                            field_span,
                        )
                    })?;
                let raw_ty = raw_fields
                    .iter()
                    .find(|entry| entry.name == field)
                    .map(|entry| entry.ty.clone())
                    .ok_or_else(|| {
                        MetelError::type_error(
                            TypeErrorCode::T0003,
                            format!("no field `{field}` on `{struct_name}`"),
                            field_span,
                        )
                    })?;
                let mut remap = Substitution::new();
                for (&tp, arg) in type_params.iter().zip(type_args.iter()) {
                    remap.bind(tp, type_to_infer(arg));
                }
                infer_type_to_type(&remap.apply(&raw_ty), field_span)
            } else {
                let fields = ctx.get_struct_fields(struct_name).ok_or_else(|| {
                    MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!("unknown type `{struct_name}`"),
                        field_span,
                    )
                })?;
                let field_entry =
                    fields
                        .iter()
                        .find(|entry| entry.0 == field)
                        .ok_or_else(|| {
                            MetelError::type_error(
                                TypeErrorCode::T0003,
                                format!("no field `{field}` on `{struct_name}`"),
                                field_span,
                            )
                        })?;
                Ok(field_entry.1.clone())
            }
        }
        _ => Err(MetelError::type_error(
            TypeErrorCode::T0002,
            "cannot infer struct type for field assignment; add a type annotation",
            field_span,
        )),
    }
}
