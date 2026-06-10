//! Free-function overloading by exact argument-type match (METEL-180).
//!
//! A function *name* is "overloaded" when a module declares more than one free
//! `fun` with that name. Overloaded functions must be non-generic and have fully
//! annotated parameters so each definition has a distinct, concrete signature.
//!
//! Resolution is **exact-match only**: at a call site the argument types must
//! equal a candidate's parameter types exactly. Implicit numeric `From`
//! coercions do **not** participate in overload selection (decided in sprint 22).
//!
//! Implementation strategy: each overloaded definition is given a unique mangled
//! runtime name (`print$i32`, `print$i64`). Construction rewrites both the
//! declaration name and every call site to the mangled name, so the rest of the
//! pipeline — the elaborator, the runtime environment, and call dispatch — needs
//! no overload-specific logic: mangled names are simply distinct names.
//!
//! NOTE (intermediate design): name mangling is a v1 mechanism. The intended end
//! state is `SymbolId`-based dispatch for all calls. When that lands (folded into
//! METEL-181, where builtins also gain `SymbolId`s), the *selection* logic here
//! ([`build_overload_table`], [`select`]) is retained but repointed to yield a
//! `SymbolId`/`CalleeId`, and the string [`mangle`] machinery plus the
//! construction-time name rewriting are deleted.

use std::collections::HashMap;

use crate::ast::{Decl, Expr, FunDecl, Span};
use crate::error::{MetelError, TypeErrorCode};
use crate::types::Type;
use crate::typeinference::{OverloadEntry, OverloadTable};

use super::conversions::{infer_type_to_type, type_expr_to_infer};

/// Build the overload table for a module's top-level declarations.
///
/// Errors if an overloaded function is generic, has an unannotated parameter, or
/// collides with another overload on identical parameter types.
pub(super) fn build_overload_table(decls: &[Decl]) -> Result<OverloadTable, MetelError> {
    let mut groups: HashMap<&str, Vec<&FunDecl>> = HashMap::new();
    for decl in decls {
        if let Decl::Fun(f) = decl {
            groups.entry(f.name.as_str()).or_default().push(f);
        }
    }

    let mut table = OverloadTable::new();
    for (name, funs) in groups {
        if funs.len() < 2 {
            continue;
        }
        let mut entries: Vec<OverloadEntry> = Vec::new();
        for f in funs {
            if !f.generics.is_empty() {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0002,
                    format!("overloaded function `{name}` cannot be generic"),
                    &f.span,
                ));
            }
            let params = fun_param_types(f)?;
            let ret = match &f.return_type {
                Some(te) => infer_type_to_type(&type_expr_to_infer(te), &f.span)?,
                None => Type::Unit,
            };
            if entries.iter().any(|e| e.params == params) {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0011,
                    format!(
                        "duplicate definition of `{name}` with identical parameter types; \
                         overloads must differ in their parameter types"
                    ),
                    &f.span,
                ));
            }
            let mangled = mangle(name, &params);
            entries.push(OverloadEntry {
                params,
                ret,
                mangled,
            });
        }
        table.insert(name.to_string(), entries);
    }
    Ok(table)
}

/// Concrete parameter types of a function declaration. Every parameter must be
/// annotated (overloaded functions require this).
fn fun_param_types(fun: &FunDecl) -> Result<Vec<Type>, MetelError> {
    fun.params
        .iter()
        .map(|p| {
            let ann = p.type_ann.as_ref().ok_or_else(|| {
                MetelError::type_error(
                    TypeErrorCode::T0002,
                    format!(
                        "overloaded function `{}` requires a type annotation on every parameter",
                        fun.name
                    ),
                    &p.span,
                )
            })?;
            infer_type_to_type(&type_expr_to_infer(ann), &p.span)
        })
        .collect()
}

/// The mangled runtime name for an overloaded definition, or `None` if `fun`'s
/// name is not overloaded. Matches the declaration against the table by its
/// concrete parameter signature.
pub(super) fn mangled_for_decl(table: &OverloadTable, fun: &FunDecl) -> Option<String> {
    let entries = table.get(fun.name.as_str())?;
    mangled_for_entries(entries, fun)
}

/// Same as [`mangled_for_decl`] but reads the overload set from an
/// [`InferContext`] (used during the inference pass).
pub(super) fn mangled_for_decl_in_ctx(
    ctx: &crate::typeinference::InferContext,
    fun: &FunDecl,
) -> Option<String> {
    let entries = ctx.overload_candidates(fun.name.as_str())?;
    mangled_for_entries(entries, fun)
}

fn mangled_for_entries(entries: &[OverloadEntry], fun: &FunDecl) -> Option<String> {
    let params = fun_param_types(fun).ok()?;
    entries
        .iter()
        .find(|e| e.params == params)
        .map(|e| e.mangled.clone())
}

/// Select the overload candidate whose parameters exactly match `arg_types`.
/// Returns `None` when no candidate matches (no implicit coercion is applied).
pub(super) fn select<'a>(
    entries: &'a [OverloadEntry],
    arg_types: &[Type],
) -> Option<&'a OverloadEntry> {
    entries
        .iter()
        .find(|e| e.params.len() == arg_types.len() && e.params == arg_types)
}

/// If `callee` is a bare name reference (`Ident` or a normalized `ResolvedPath`),
/// return that name. Overloading applies only to such direct named calls.
pub(super) fn callee_name(callee: &Expr) -> Option<&str> {
    match callee {
        Expr::Ident(name, _) => Some(name.as_str()),
        Expr::ResolvedPath { resolved, .. } => Some(resolved.as_str()),
        _ => None,
    }
}

/// Build a stable, collision-free runtime name for an overloaded definition.
pub(super) fn mangle(name: &str, params: &[Type]) -> String {
    let mut s = String::from(name);
    for p in params {
        s.push('$');
        s.push_str(&type_mangle(p));
    }
    s
}

fn type_mangle(ty: &Type) -> String {
    match ty {
        Type::Named(n, args) if args.is_empty() => n.clone(),
        Type::Named(n, args) => {
            let inner = args.iter().map(type_mangle).collect::<Vec<_>>().join(".");
            format!("{n}_{inner}_")
        }
        Type::Array(inner) => format!("arr.{}", type_mangle(inner)),
        Type::SizedArray(inner, len) => format!("arr.{}.{len}", type_mangle(inner)),
        Type::Pointer(inner) => format!("ptr.{}", type_mangle(inner)),
        Type::MutPointer(inner) => format!("mptr.{}", type_mangle(inner)),
        Type::Tuple(items) => {
            let inner = items.iter().map(type_mangle).collect::<Vec<_>>().join(".");
            format!("tup.{inner}.")
        }
        // Primitives and Unit/Never have simple Display forms with no separators.
        other => format!("{other}"),
    }
}

/// Error for a call to an overloaded name where no candidate matches the argument
/// types exactly. Lists the available signatures.
pub(super) fn no_match_error(name: &str, arg_types: &[Type], entries: &[OverloadEntry], span: &Span) -> MetelError {
    let got = arg_types
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let candidates = entries
        .iter()
        .map(|e| {
            format!(
                "({})",
                e.params.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    MetelError::type_error(
        TypeErrorCode::T0003,
        format!(
            "no overload of `{name}` matches argument types ({got}); \
             available: {candidates} (overload resolution is exact, with no implicit coercion)"
        ),
        span,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeinference::OverloadEntry;

    fn entry(params: Vec<Type>, ret: Type) -> OverloadEntry {
        let mangled = mangle("f", &params);
        OverloadEntry {
            params,
            ret,
            mangled,
        }
    }

    #[test]
    fn mangle_is_distinct_per_signature() {
        assert_eq!(mangle("print", &[Type::I32]), "print$i32");
        assert_eq!(mangle("print", &[Type::I64]), "print$i64");
        assert_ne!(
            mangle("f", &[Type::I32, Type::Str]),
            mangle("f", &[Type::Str, Type::I32]),
            "argument order must affect the mangled name"
        );
        assert_eq!(
            mangle("f", &[Type::Named("Foo".into(), vec![])]),
            "f$Foo"
        );
    }

    #[test]
    fn select_requires_exact_match() {
        let entries = vec![
            entry(vec![Type::I32], Type::Unit),
            entry(vec![Type::I64], Type::Unit),
        ];
        // Exact match picks the right candidate.
        assert_eq!(
            select(&entries, &[Type::I32]).unwrap().mangled,
            "f$i32"
        );
        assert_eq!(
            select(&entries, &[Type::I64]).unwrap().mangled,
            "f$i64"
        );
        // No coercion: a type with no exact candidate does not match.
        assert!(select(&entries, &[Type::I16]).is_none());
        // Arity mismatch does not match.
        assert!(select(&entries, &[Type::I32, Type::I32]).is_none());
        assert!(select(&entries, &[]).is_none());
    }

    #[test]
    fn select_distinguishes_by_arity() {
        let entries = vec![
            entry(vec![Type::I64], Type::I64),
            entry(vec![Type::I64, Type::I64], Type::I64),
        ];
        assert_eq!(select(&entries, &[Type::I64]).unwrap().params.len(), 1);
        assert_eq!(
            select(&entries, &[Type::I64, Type::I64])
                .unwrap()
                .params
                .len(),
            2
        );
    }
}
