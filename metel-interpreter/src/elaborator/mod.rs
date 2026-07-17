/// The elaboration pass: walks the typed AST and resolves `MethodDispatch` for every
/// `TypedExpr::MethodCall`. After elaboration the caller holds an `ElaboratedModuleGraph`,
/// a proof that this pass has run.
///
/// For the tree-walk interpreter, elaboration pre-resolves whether each method call goes
/// through an aspect (and which one) or is a direct inherent call, so the evaluator can
/// skip the runtime aspect-registry lookup for statically-known sites.
use std::collections::HashMap;

use crate::ast::TypeExpr;
use crate::error::{MetelError, TypeErrorCode};
use crate::name_resolver::ResolvedNames;
use crate::symbols::SymbolId;
use crate::typed_ast::{
    FunBody, MethodDispatch, TypedBlock, TypedDecl, TypedExpr, TypedForInit, TypedImplBlock,
    TypedMatchArm, TypedMatchExpr, TypedModuleGraph, TypedPlace, TypedStmt,
};
use crate::types::Type;

/// Proof that the elaboration pass has run over a `TypedModuleGraph`.
///
/// ## Environment responsibilities after elaboration
///
/// | Artifact | Owner | Responsibility |
/// |---|---|---|
/// | `TypeDefinitionRegistry` | `TypedModuleGraph::type_registry` | Type/aspect/method definitions; the elaboration-facing `aspect_declaring_module` lookup |
/// | `ResolvedNames::symbols` | Caller-supplied to `elaborate` | Stable `SymbolId` intern table; elaboration reads but does not write it |
/// | `MethodDispatch` per call site | `TypedExpr::MethodCall::dispatch` | Resolved during elaboration; evaluator reads, does not re-derive |
/// | `TypedImplBlock::aspect_id` | `TypedImplBlock` | Set during typechecker construction pass (Pass 2) using the same symbol table |
///
/// After `elaborate` returns, `MethodDispatch::Dynamic` sites are those whose receiver type
/// had no aspect-method registration in the registry (e.g. calls on `fn` or tuple types).
/// All others are `Inherent` or `Aspect { aspect_id }`.
pub struct ElaboratedModuleGraph(pub TypedModuleGraph);

/// Run elaboration over `graph` and return an `ElaboratedModuleGraph`.
///
/// Each `MethodCall::dispatch` field starts as `Dynamic`; this pass upgrades it to
/// `Aspect { aspect_id }` or `Inherent` where the target can be statically determined.
///
/// # Errors
/// Returns an error if two different aspects provide the same method name for the
/// same type, making dispatch ambiguous (T0013).
pub fn elaborate(
    mut graph: TypedModuleGraph,
    names: &ResolvedNames,
) -> Result<ElaboratedModuleGraph, MetelError> {
    let aspect_method_map = build_aspect_method_map(&graph, names)?;

    for module in &mut graph.modules {
        for decl in &mut module.decls {
            elaborate_decl(decl, &aspect_method_map);
        }
    }

    Ok(ElaboratedModuleGraph(graph))
}

// ── Dispatch map ─────────────────────────────────────────────────────────────

/// Maps `(concrete_type_name, method_name)` → `SymbolId` of the aspect that owns
/// that method for that type. Keying by receiver type avoids false matches when two
/// unrelated aspects from different modules both declare a method with the same name.
///
/// If two distinct aspects define the same method name on the same receiver type,
/// elaboration rejects the program with T0013 rather than silently picking one.
fn build_aspect_method_map(
    graph: &TypedModuleGraph,
    names: &ResolvedNames,
) -> Result<HashMap<(String, String), AspectDispatchOwner>, MetelError> {
    let mut map: HashMap<(String, String), AspectDispatchOwner> = HashMap::new();
    let registry = &graph.type_registry;

    for module in &graph.modules {
        for decl in &module.decls {
            if let TypedDecl::Impl(block) = decl {
                let Some(aspect_name) = &block.aspect_name else {
                    continue;
                };
                let Some(type_name) = type_expr_outer_name(&block.target_type) else {
                    continue;
                };
                // Resolve the aspect's SymbolId via its declaring module.
                let Some(declaring_module) = registry.aspect_declaring_module(aspect_name) else {
                    continue;
                };
                let Some(&id) = names
                    .symbols
                    .get(&(declaring_module.clone(), aspect_name.clone()))
                else {
                    continue;
                };
                let is_generic = !block.generics.is_empty();
                for method in &block.methods {
                    let key = (type_name.clone(), method.name.clone());
                    let owner = AspectDispatchOwner {
                        aspect_id: id,
                        aspect_name: aspect_name.clone(),
                        is_generic,
                    };
                    if let Some(existing_owner) = map.get(&key) {
                        if existing_owner.aspect_id != id {
                            // A conditional/generic impl on either side of this pair
                            // was already vetted by `coherence::check`, which runs
                            // earlier in the pipeline and -- unlike this map, a plain
                            // "same (type, method) seen twice" check with no bound
                            // awareness -- actually knows whether the two impls'
                            // bounds can overlap (issue #272). Any such pair that
                            // reached construction/elaboration at all is therefore
                            // already proven non-overlapping; only a pair of
                            // concrete, unconditional impls (which have no bounds to
                            // ever be disjoint on, so always genuinely conflict) is
                            // still an error here.
                            if !is_generic && !existing_owner.is_generic {
                                return Err(MetelError::type_error(
                                    TypeErrorCode::T0013,
                                    format!(
                                        "ambiguous aspect method `{}` on type `{}`: both `{}` and `{}` provide this method; use distinct method names or remove one impl",
                                        method.name,
                                        type_name,
                                        aspect_name,
                                        existing_owner.aspect_name,
                                    ),
                                    &method.span,
                                ));
                            }
                        }
                    } else {
                        map.insert(key, owner);
                    }
                }
            }
        }
    }

    Ok(map)
}

/// Extract the outermost named type from a `TypeExpr` — the part used as the registry key.
/// `List<i32>` → `"List"`, `Foo` → `"Foo"`, everything else → `None`.
fn type_expr_outer_name(te: &TypeExpr) -> Option<String> {
    match te {
        TypeExpr::Named(name, _) => Some(name.clone()),
        _ => None,
    }
}

/// Map a resolved `Type` to the string used in the runtime registry.
/// Mirrors `runtime_type_name` in the evaluator.  Returns `None` for types
/// (arrays, tuples, fn pointers) that don't have a named registry entry.
fn receiver_type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Named(name, _) => Some(name.clone()),
        Type::Boolean => Some("boolean".to_string()),
        Type::Str => Some("String".to_string()),
        Type::Char => Some("Char".to_string()),
        Type::I8 => Some("i8".to_string()),
        Type::I16 => Some("i16".to_string()),
        Type::I32 => Some("i32".to_string()),
        Type::I64 => Some("i64".to_string()),
        Type::U8 => Some("u8".to_string()),
        Type::U16 => Some("u16".to_string()),
        Type::U32 => Some("u32".to_string()),
        Type::U64 => Some("u64".to_string()),
        Type::F32 => Some("f32".to_string()),
        Type::F64 => Some("f64".to_string()),
        // References: dispatch through the referent type (deref_value unwraps them at runtime).
        Type::Reference(inner) | Type::MutReference(inner) => receiver_type_name(inner),
        _ => None,
    }
}

// ── Recursive elaboration ─────────────────────────────────────────────────────

#[derive(Clone)]
struct AspectDispatchOwner {
    aspect_id: SymbolId,
    aspect_name: String,
    /// Whether the impl that registered this owner has its own generics
    /// (RFC-0036 conditional impl). See the comment at the conflict check
    /// below for why this changes whether a same-method-name collision is an
    /// error here.
    is_generic: bool,
}

type DispatchMap = HashMap<(String, String), AspectDispatchOwner>;

fn elaborate_decl(decl: &mut TypedDecl, map: &DispatchMap) {
    match decl {
        TypedDecl::Fun(f) => elaborate_fun_body(&mut f.body, map),
        TypedDecl::Let(l) => elaborate_expr(&mut l.value, map),
        TypedDecl::Mut(m) => elaborate_expr(&mut m.value, map),
        TypedDecl::Impl(block) => elaborate_impl_block(block, map),
        // Struct / Enum / Aspect carry no executable bodies.
        TypedDecl::Struct(_) | TypedDecl::Enum(_) | TypedDecl::Aspect(_) => {}
        TypedDecl::Stmt(stmt) => elaborate_stmt(stmt, map),
    }
}

fn elaborate_fun_body(body: &mut FunBody, map: &DispatchMap) {
    if let FunBody::Typed(block) = body {
        elaborate_block(block, map);
    }
    // FunBody::Generic bodies are re-evaluated at call sites; skip here.
}

fn elaborate_impl_block(block: &mut TypedImplBlock, map: &DispatchMap) {
    for method in &mut block.methods {
        elaborate_fun_body(&mut method.body, map);
    }
}

fn elaborate_block(block: &mut TypedBlock, map: &DispatchMap) {
    for decl in &mut block.stmts {
        elaborate_decl(decl, map);
    }
    if let Some(tail) = &mut block.tail {
        elaborate_expr(tail, map);
    }
}

fn elaborate_stmt(stmt: &mut TypedStmt, map: &DispatchMap) {
    match stmt {
        TypedStmt::Expr(e) => elaborate_expr(e, map),
        TypedStmt::While(w) => {
            elaborate_expr(&mut w.condition, map);
            elaborate_block(&mut w.body, map);
        }
        TypedStmt::For(f) => {
            if let Some(init) = &mut f.init {
                match init {
                    TypedForInit::Let(l) => elaborate_expr(&mut l.value, map),
                    TypedForInit::Mut(m) => elaborate_expr(&mut m.value, map),
                    TypedForInit::Expr(e) => elaborate_expr(e, map),
                }
            }
            if let Some(cond) = &mut f.condition {
                elaborate_expr(cond, map);
            }
            if let Some(step) = &mut f.step {
                elaborate_expr(step, map);
            }
            elaborate_block(&mut f.body, map);
        }
        TypedStmt::ForIn(fi) => {
            elaborate_expr(&mut fi.iterable, map);
            elaborate_block(&mut fi.body, map);
        }
    }
}

fn elaborate_place(place: &mut TypedPlace, map: &DispatchMap) {
    match place {
        TypedPlace::Ident(..) => {}
        TypedPlace::Deref { object, .. } => elaborate_expr(object, map),
        TypedPlace::Field { object, .. } => elaborate_place(object, map),
        TypedPlace::Index { object, index, .. } => {
            elaborate_place(object, map);
            elaborate_expr(index, map);
        }
    }
}

fn elaborate_expr(expr: &mut TypedExpr, map: &DispatchMap) {
    match expr {
        TypedExpr::MethodCall {
            method,
            dispatch,
            receiver,
            args,
            ..
        } => {
            if *dispatch == MethodDispatch::Dynamic {
                let recv_type = receiver_type_name(receiver.ty());
                *dispatch = resolve_dispatch(recv_type.as_deref(), method, map);
            }
            elaborate_expr(receiver, map);
            for arg in args.iter_mut() {
                elaborate_expr(arg, map);
            }
        }
        TypedExpr::Call { callee, args, .. } => {
            elaborate_expr(callee, map);
            for arg in args.iter_mut() {
                elaborate_expr(arg, map);
            }
        }
        TypedExpr::BinOp(lhs, _, rhs, ..) => {
            elaborate_expr(lhs, map);
            elaborate_expr(rhs, map);
        }
        TypedExpr::UnaryOp(_, operand, ..) => elaborate_expr(operand, map),
        TypedExpr::Tuple(elems, ..) | TypedExpr::Array(elems, ..) => {
            for e in elems.iter_mut() {
                elaborate_expr(e, map);
            }
        }
        TypedExpr::RepeatArray(elem, ..) => elaborate_expr(elem, map),
        TypedExpr::Assign { target, value, .. } => {
            elaborate_place(target, map);
            elaborate_expr(value, map);
        }
        TypedExpr::FieldAccess { object, .. } | TypedExpr::TupleAccess { object, .. } => {
            elaborate_expr(object, map);
        }
        TypedExpr::Index { object, index, .. } => {
            elaborate_expr(object, map);
            elaborate_expr(index, map);
        }
        TypedExpr::Cast { expr: inner, .. } | TypedExpr::SingletonCoerce { inner, .. } => {
            elaborate_expr(inner, map);
        }
        TypedExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            elaborate_expr(condition, map);
            elaborate_block(then_branch, map);
            if let Some(b) = else_branch {
                elaborate_block(b, map);
            }
        }
        TypedExpr::Loop { body, .. } | TypedExpr::Closure { body, .. } => {
            elaborate_block(body, map);
        }
        TypedExpr::Match(m) => elaborate_match(m, map),
        TypedExpr::StructLiteral { fields, .. } => {
            for (_, e) in fields.iter_mut() {
                elaborate_expr(e, map);
            }
        }
        TypedExpr::Return(r) => {
            if let Some(v) = &mut r.value {
                elaborate_expr(v, map);
            }
        }
        TypedExpr::Break(b) => {
            if let Some(v) = &mut b.value {
                elaborate_expr(v, map);
            }
        }
        TypedExpr::GenericClosure { .. }
        | TypedExpr::Continue(_)
        | TypedExpr::Literal(..)
        | TypedExpr::Ident(..)
        | TypedExpr::Path(..) => {}
    }
}

fn elaborate_match(m: &mut TypedMatchExpr, map: &DispatchMap) {
    elaborate_expr(&mut m.scrutinee, map);
    for arm in &mut m.arms {
        elaborate_match_arm(arm, map);
    }
}

fn elaborate_match_arm(arm: &mut TypedMatchArm, map: &DispatchMap) {
    if let Some(guard) = &mut arm.guard {
        elaborate_expr(guard, map);
    }
    elaborate_block(&mut arm.body, map);
}

/// Resolve dispatch for a single call site.
/// `recv_type` is `None` when the receiver has no nameable type (array, tuple, fn);
/// those calls are always `Inherent` since aspects only apply to named types.
fn resolve_dispatch(recv_type: Option<&str>, method: &str, map: &DispatchMap) -> MethodDispatch {
    let Some(type_name) = recv_type else {
        return MethodDispatch::Inherent;
    };
    match map.get(&(type_name.to_string(), method.to_string())) {
        Some(owner) => MethodDispatch::Aspect {
            aspect_id: owner.aspect_id,
        },
        None => MethodDispatch::Inherent,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::SYM_ASPECT_DISPLAY;

    fn display_owner() -> AspectDispatchOwner {
        AspectDispatchOwner {
            aspect_id: SYM_ASPECT_DISPLAY,
            aspect_name: "Display".to_string(),
            is_generic: false,
        }
    }

    #[test]
    fn resolve_dispatch_aspect_returns_aspect_variant() {
        let mut map = HashMap::new();
        map.insert(
            ("Foo".to_string(), "to_string".to_string()),
            display_owner(),
        );
        assert_eq!(
            resolve_dispatch(Some("Foo"), "to_string", &map),
            MethodDispatch::Aspect {
                aspect_id: SYM_ASPECT_DISPLAY
            }
        );
    }

    #[test]
    fn resolve_dispatch_wrong_type_returns_inherent() {
        let mut map = HashMap::new();
        map.insert(
            ("Foo".to_string(), "to_string".to_string()),
            display_owner(),
        );
        // Same method name but different receiver type → Inherent, not an aspect call.
        assert_eq!(
            resolve_dispatch(Some("Bar"), "to_string", &map),
            MethodDispatch::Inherent
        );
    }

    #[test]
    fn resolve_dispatch_no_type_returns_inherent() {
        let mut map = HashMap::new();
        map.insert(
            ("Foo".to_string(), "to_string".to_string()),
            display_owner(),
        );
        assert_eq!(
            resolve_dispatch(None, "to_string", &map),
            MethodDispatch::Inherent
        );
    }

    #[test]
    fn resolve_dispatch_unknown_method_returns_inherent() {
        let map = HashMap::new();
        assert_eq!(
            resolve_dispatch(Some("Foo"), "len", &map),
            MethodDispatch::Inherent
        );
    }

    #[test]
    fn resolve_dispatch_non_aspect_method_returns_inherent() {
        let mut map = HashMap::new();
        map.insert(
            ("Foo".to_string(), "to_string".to_string()),
            display_owner(),
        );
        assert_eq!(
            resolve_dispatch(Some("Foo"), "push", &map),
            MethodDispatch::Inherent
        );
    }
}
