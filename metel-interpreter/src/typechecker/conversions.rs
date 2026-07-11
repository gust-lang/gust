use crate::ast::{Span, TypeExpr};
use crate::error::{MetelError, TypeErrorCode};
use crate::typeinference::{InferType, Substitution, TypeVar};
use crate::types::Type;
use std::collections::HashMap;

fn type_expr_to_infer_in_context(
    te: &TypeExpr,
    generics: Option<&HashMap<String, TypeVar>>,
    self_ty_name: Option<&str>,
) -> InferType {
    match te {
        TypeExpr::Named(name, args) => {
            if args.is_empty() {
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
                .map(|a| type_expr_to_infer_in_context(a, generics, self_ty_name))
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
                .map(|t| type_expr_to_infer_in_context(t, generics, self_ty_name))
                .collect(),
        ),
        TypeExpr::Array(t) => InferType::Array(Box::new(type_expr_to_infer_in_context(
            t,
            generics,
            self_ty_name,
        ))),
        TypeExpr::SizedArray(t, n) => InferType::SizedArray(
            Box::new(type_expr_to_infer_in_context(t, generics, self_ty_name)),
            *n,
        ),
        TypeExpr::Reference(t) => InferType::Reference(Box::new(type_expr_to_infer_in_context(
            t,
            generics,
            self_ty_name,
        ))),
        TypeExpr::MutReference(t) => InferType::MutReference(Box::new(type_expr_to_infer_in_context(
            t,
            generics,
            self_ty_name,
        ))),
        TypeExpr::Fun(ps, ret) => InferType::Fun(
            ps.iter()
                .map(|p| type_expr_to_infer_in_context(p, generics, self_ty_name))
                .collect(),
            Box::new(
                ret.as_deref()
                    .map(|r| type_expr_to_infer_in_context(r, generics, self_ty_name))
                    .unwrap_or(InferType::unit()),
            ),
        ),
        // ImplAspect is removed by the lowering pass before inference runs.
        // If it reaches here, treat as the bound type (best-effort fallback).
        TypeExpr::ImplAspect { bound, .. } => {
            type_expr_to_infer_in_context(bound, generics, self_ty_name)
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
    type_expr_to_infer_in_context(te, Some(generics), None)
}

pub(super) fn type_expr_to_infer_with_generics_and_self(
    te: &TypeExpr,
    generics: &HashMap<String, TypeVar>,
    self_ty_name: &str,
) -> InferType {
    type_expr_to_infer_in_context(te, Some(generics), Some(self_ty_name))
}

/// Convert a source-level `TypeExpr` to an `InferType` for use during inference.
pub(super) fn type_expr_to_infer(te: &TypeExpr) -> InferType {
    type_expr_to_infer_in_context(te, None, None)
}

pub(super) fn type_expr_to_infer_with_self(te: &TypeExpr, self_ty_name: &str) -> InferType {
    type_expr_to_infer_in_context(te, None, Some(self_ty_name))
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
