use crate::ast::{Span, TypeExpr};
use crate::error::{MetelError, TypeErrorCode};
use crate::typeinference::{InferType, Substitution, TypeDefinitionRegistry, TypeVar};
use crate::types::Type;
use std::collections::HashMap;

/// Context for resolving associated-type projections (RFC-0082).
/// Passed through `type_expr_to_infer_in_context` so that `Projection` nodes
/// and bare-name sugar can be resolved against the registry.
pub(super) struct AssocResolveCtx<'a> {
    pub registry: &'a TypeDefinitionRegistry,
    pub current_module: &'a [String],
    /// Set when converting an ASPECT's own method signature (§1.2 bare-name sugar):
    /// the aspect currently being processed, so `Item` alone resolves as `Self::Item`.
    pub current_aspect: Option<&'a str>,
}

// Exhaustive match over every TypeExpr variant; splitting it up would scatter
// one coherent dispatch table across many small functions with no real gain
// in clarity.
#[allow(clippy::too_many_lines)]
fn type_expr_to_infer_in_context(
    te: &TypeExpr,
    generics: Option<&HashMap<String, TypeVar>>,
    self_ty_name: Option<&str>,
    assoc_ctx: Option<&AssocResolveCtx<'_>>,
) -> InferType {
    match te {
        TypeExpr::Named(name, args) => {
            // RFC-0082 §1.2 bare-name sugar: inside an aspect's method signature,
            // `Item` alone resolves as `Self::Item` when the aspect declares
            // an associated type named `Item`.
            if args.is_empty() {
                if let Some(ctx) = assoc_ctx {
                    if let Some(aspect) = ctx.current_aspect {
                        if let Some(decls) = ctx.registry.aspect_assoc_type_decls(aspect) {
                            if decls.iter().any(|d| d.name == *name) {
                                // Treat as Projection { base: Self, assoc_name: name }
                                let base = TypeExpr::Named("Self".to_string(), vec![]);
                                let proj = TypeExpr::Projection {
                                    base: Box::new(base),
                                    assoc_name: name.clone(),
                                    span: Span::new(0, 0, ""),
                                };
                                return type_expr_to_infer_in_context(
                                    &proj, generics, self_ty_name, assoc_ctx,
                                );
                            }
                        }
                    }
                }
                if let Some(generics) = generics {
                    if let Some(&tv) = generics.get(name.as_str()) {
                        return InferType::Var(tv);
                    }
                }
                if name == "Self" {
                    if let Some(self_ty_name) = self_ty_name {
                        return InferType::Named(self_ty_name.to_string(), vec![]);
                    }
                }
            }
            let arg_tys: Vec<_> = args
                .iter()
                .map(|a| type_expr_to_infer_in_context(a, generics, self_ty_name, assoc_ctx))
                .collect();
            match (name.as_str(), arg_tys.len()) {
                ("i64", 0) => InferType::int(),
                ("f64", 0) => InferType::float(),
                ("boolean", 0) => InferType::bool(),
                ("Char", 0) => InferType::Concrete(Type::Char),
                ("String", 0) => InferType::str(),
                ("Never", 0) => InferType::never(),
                ("i8", 0) => InferType::Concrete(Type::I8),
                ("i16", 0) => InferType::Concrete(Type::I16),
                ("i32", 0) => InferType::Concrete(Type::I32),
                ("u8", 0) => InferType::Concrete(Type::U8),
                ("u16", 0) => InferType::Concrete(Type::U16),
                ("u32", 0) => InferType::Concrete(Type::U32),
                ("u64", 0) => InferType::Concrete(Type::U64),
                ("f32", 0) => InferType::Concrete(Type::F32),
                ("Array", 1) => InferType::Array(Box::new(arg_tys.into_iter().next().unwrap())),
                _ => InferType::Named(name.clone(), arg_tys),
            }
        }
        TypeExpr::Unit => InferType::unit(),
        TypeExpr::Tuple(ts) => InferType::Tuple(
            ts.iter()
                .map(|t| type_expr_to_infer_in_context(t, generics, self_ty_name, assoc_ctx))
                .collect(),
        ),
        TypeExpr::Array(t) => InferType::Array(Box::new(type_expr_to_infer_in_context(
            t,
            generics,
            self_ty_name,
            assoc_ctx,
        ))),
        TypeExpr::SizedArray(t, n) => InferType::SizedArray(
            Box::new(type_expr_to_infer_in_context(t, generics, self_ty_name, assoc_ctx)),
            *n,
        ),
        TypeExpr::Reference(t) => InferType::Reference(Box::new(type_expr_to_infer_in_context(
            t,
            generics,
            self_ty_name,
            assoc_ctx,
        ))),
        TypeExpr::MutReference(t) => {
            InferType::MutReference(Box::new(type_expr_to_infer_in_context(
                t,
                generics,
                self_ty_name,
                assoc_ctx,
            )))
        }
        TypeExpr::Fun(ps, ret) => InferType::Fun(
            ps.iter()
                .map(|p| type_expr_to_infer_in_context(p, generics, self_ty_name, assoc_ctx))
                .collect(),
            Box::new(ret.as_deref().map_or(InferType::unit(), |r| {
                type_expr_to_infer_in_context(r, generics, self_ty_name, assoc_ctx)
            })),
        ),
        TypeExpr::ImplAspect { bound, .. } => {
            type_expr_to_infer_in_context(bound, generics, self_ty_name, assoc_ctx)
        }
        // RFC-0082 §3: `T::AssocType` projection.
        // Concrete case: base resolves to a known type (not a generic param).
        // Look up the aspect that declares this assoc name and resolve to the
        // concrete binding from the impl.
        TypeExpr::Projection {
            base,
            assoc_name,
            ..
        } => {
            // Resolve the base type.
            let base_ty = type_expr_to_infer_in_context(
                base.as_ref(),
                generics,
                self_ty_name,
                assoc_ctx,
            );
            // If the base is a TypeVar (generic param), the concrete case
            // doesn't apply — fall back to a Named placeholder (abstract case
            // is handled by the caller in inference.rs with &mut InferContext).
            if matches!(&base_ty, InferType::Var(_)) {
                let base_name = match base.as_ref() {
                    TypeExpr::Named(n, _) => n.clone(),
                    _ => String::new(),
                };
                return InferType::Named(format!("{base_name}::{assoc_name}"), vec![]);
            }
            // Extract the base type's name for registry lookup.
            let base_name = match &base_ty {
                InferType::Named(n, _) | InferType::Concrete(Type::Named(n, _)) => Some(n.as_str()),
                _ => None,
            };
            if let (Some(ctx), Some(bn)) = (assoc_ctx, base_name) {
                // If current_aspect is known (we're inside an aspect method or an
                // impl block's own conversion), resolve directly against it.
                if let Some(aspect) = ctx.current_aspect {
                    if let Some(ty) = ctx.registry.impl_assoc_type(
                        ctx.current_module,
                        bn,
                        aspect,
                        assoc_name,
                    ) {
                        return type_to_infer(ty);
                    }
                }
                // No known aspect and a concrete (non-generic, non-Self) base: a
                // projection like `SomeConcreteType::AssocName` used outside any
                // impl/aspect context. Not exercised by any RFC-0082 example (every
                // real case is either `T::AssocType` on a generic param -- handled
                // by the abstract-case special-casing in inference.rs before this
                // function is ever called -- or bare `Self::AssocType` sugar inside
                // an aspect/impl, where current_aspect is always Some). Falls
                // through to the defensive placeholder below rather than guessing
                // which aspect is meant.
            }
            // Fallback: return a Named placeholder (defensive — §2's completeness
            // check is the real guard).
            let base_name_str = match &base_ty {
                InferType::Named(n, _) | InferType::Concrete(Type::Named(n, _)) => n.clone(),
                _ => String::new(),
            };
            InferType::Named(format!("{base_name_str}::{assoc_name}"), vec![])
        }
    }
}

/// Like `type_expr_to_infer` but substitutes known generic parameter names with their
/// corresponding `InferType::Var`s.  Call this when inferring a generic function body
/// where `generics` maps each parameter name (e.g. `"T"`) to its fresh `TypeVar`.
pub(super) fn type_expr_to_infer_with_generics(
    te: &TypeExpr,
    generics: &HashMap<String, TypeVar>,
) -> InferType {
    type_expr_to_infer_in_context(te, Some(generics), None, None)
}

pub(super) fn type_expr_to_infer_with_generics_and_self(
    te: &TypeExpr,
    generics: &HashMap<String, TypeVar>,
    self_ty_name: &str,
) -> InferType {
    type_expr_to_infer_in_context(te, Some(generics), Some(self_ty_name), None)
}

/// Convert a source-level `TypeExpr` to an `InferType` for use during inference.
pub(super) fn type_expr_to_infer(te: &TypeExpr) -> InferType {
    type_expr_to_infer_in_context(te, None, None, None)
}

pub(super) fn type_expr_to_infer_with_self(te: &TypeExpr, self_ty_name: &str) -> InferType {
    type_expr_to_infer_in_context(te, None, Some(self_ty_name), None)
}

/// Convert a source-level `TypeExpr` to an `InferType` with associated-type
/// resolution context. Used when converting type annotations inside aspect
/// method signatures (§1.2 bare-name sugar) or concrete projection positions.
pub(super) fn type_expr_to_infer_with_assoc_ctx(
    te: &TypeExpr,
    generics: &HashMap<String, TypeVar>,
    self_ty_name: Option<&str>,
    assoc_ctx: &AssocResolveCtx<'_>,
) -> InferType {
    type_expr_to_infer_in_context(te, Some(generics), self_ty_name, Some(assoc_ctx))
}

/// Convert a fully-solved `InferType` to a concrete `Type`.
/// Returns E0002 if any type variable is still unresolved.
pub(super) fn infer_type_to_type(ty: &InferType, span: &Span) -> Result<Type, MetelError> {
    match ty {
        InferType::Concrete(t) => Ok(t.clone()),
        InferType::Never => Ok(Type::Never),
        InferType::Var(_) => Err(MetelError::type_error(
            TypeErrorCode::T0002,
            "cannot infer type; add a type annotation",
            span,
        )),
        InferType::Fun(params, ret) => {
            let p: Result<Vec<_>, _> = params.iter().map(|p| infer_type_to_type(p, span)).collect();
            Ok(Type::Fun(p?, Box::new(infer_type_to_type(ret, span)?)))
        }
        InferType::Tuple(ts) => {
            let t: Result<Vec<_>, _> = ts.iter().map(|t| infer_type_to_type(t, span)).collect();
            Ok(Type::Tuple(t?))
        }
        InferType::Array(t) => Ok(Type::Array(Box::new(infer_type_to_type(t, span)?))),
        InferType::SizedArray(t, n) => {
            Ok(Type::SizedArray(Box::new(infer_type_to_type(t, span)?), *n))
        }
        InferType::Reference(t) => Ok(Type::Reference(Box::new(infer_type_to_type(t, span)?))),
        InferType::MutReference(t) => Ok(Type::MutReference(Box::new(infer_type_to_type(t, span)?))),
        InferType::Named(name, args) => {
            let a: Result<Vec<_>, _> = args.iter().map(|a| infer_type_to_type(a, span)).collect();
            let args = a?;
            Ok(Type::Named(name.clone(), args))
        }
    }
}

pub(super) fn resolved_to_type(
    ty: &InferType,
    subst: &Substitution,
    span: &Span,
) -> Result<Type, MetelError> {
    infer_type_to_type(&subst.apply(ty), span)
}

pub(super) fn type_to_infer(ty: &Type) -> InferType {
    match ty {
        Type::Never => InferType::Never,
        Type::Array(t) => InferType::Array(Box::new(type_to_infer(t))),
        Type::SizedArray(t, n) => InferType::SizedArray(Box::new(type_to_infer(t)), *n),
        Type::Tuple(ts) => InferType::Tuple(ts.iter().map(type_to_infer).collect()),
        Type::Reference(t) => InferType::Reference(Box::new(type_to_infer(t))),
        Type::MutReference(t) => InferType::MutReference(Box::new(type_to_infer(t))),
        Type::Fun(ps, ret) => InferType::Fun(
            ps.iter().map(type_to_infer).collect(),
            Box::new(type_to_infer(ret)),
        ),
        Type::Named(n, args) => {
            InferType::Named(n.clone(), args.iter().map(type_to_infer).collect())
        }
        other => InferType::Concrete(other.clone()),
    }
}
