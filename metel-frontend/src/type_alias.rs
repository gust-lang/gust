//! RFC-0160: transparent type aliases.
//!
//! `type Name<G> := T;` at module scope introduces a name that is **erased to its
//! right-hand side before any further reasoning** — name resolution, coherence, and
//! the typechecker never see an alias. This pass runs right after `module_loader`:
//! it collects each module's aliases, fully expands their targets against one
//! another (rejecting cycles), rewrites every remaining `TypeExpr` in the module,
//! and drops the alias declarations.
//!
//! v1 scope: **module-level, single-module**. A cross-module aliased name is left
//! as an ordinary `Named` reference and fails later as an unknown type;
//! function/block-local aliases (RFC-0160 OQ1) and aliases inside a closure
//! literal's parameter annotation are follow-on slices. All tracked on
//! metel-core#921.

use std::collections::{HashMap, HashSet};

use crate::ast::Span;
use crate::ast::{Block, Decl, Program, TypeExpr};
use crate::error::{MetelError, TypeErrorCode};
use crate::module_loader::ModuleGraph;

struct AliasDef {
    params: Vec<String>,
    target: TypeExpr,
    span: Span,
}

/// Expand every module-level type alias in the graph, in place.
///
/// # Errors
/// `T0003` for a recursive alias or a duplicate alias name; `T0004` for a
/// generic-argument arity mismatch on an alias use.
pub fn expand(graph: &mut ModuleGraph) -> Result<(), MetelError> {
    for module in &mut graph.modules {
        expand_program(&mut module.program)?;
    }
    Ok(())
}

fn expand_program(program: &mut Program) -> Result<(), MetelError> {
    // 1. Collect this module's aliases.
    let mut raw: HashMap<String, AliasDef> = HashMap::new();
    for decl in &program.decls {
        if let Decl::TypeAlias(ta) = decl {
            let def = AliasDef {
                params: ta.generics.iter().map(|g| g.name.clone()).collect(),
                target: ta.target.clone(),
                span: ta.span.clone(),
            };
            if raw.insert(ta.name.clone(), def).is_some() {
                return Err(err_t0003(
                    format!(
                        "type alias `{}` is declared more than once in this module",
                        ta.name
                    ),
                    &ta.span,
                ));
            }
        }
    }
    if raw.is_empty() {
        return Ok(());
    }

    // 2. Fully expand each alias target against the others (cycle-detected).
    let mut resolved: HashMap<String, AliasDef> = HashMap::new();
    for name in raw.keys().cloned().collect::<Vec<_>>() {
        let mut chain = HashSet::new();
        let target = resolve_alias(&name, &raw, &mut chain)?;
        let d = &raw[&name];
        resolved.insert(
            name.clone(),
            AliasDef {
                params: d.params.clone(),
                target,
                span: d.span.clone(),
            },
        );
    }

    // 3. Rewrite every non-alias declaration's type expressions.
    for decl in &mut program.decls {
        if !matches!(decl, Decl::TypeAlias(_)) {
            walk_decl_types(decl, &mut |te| substitute(te, &resolved))?;
        }
    }

    // 4. Drop the alias declarations — nothing downstream expects them.
    program.decls.retain(|d| !matches!(d, Decl::TypeAlias(_)));
    Ok(())
}

/// The alias `name`'s target with every *other* alias it references expanded.
fn resolve_alias(
    name: &str,
    raw: &HashMap<String, AliasDef>,
    chain: &mut HashSet<String>,
) -> Result<TypeExpr, MetelError> {
    let def = &raw[name];
    if !chain.insert(name.to_string()) {
        return Err(err_t0003(
            format!(
                "recursive type alias `{name}` — a transparent alias must expand to a finite type; \
                 use a `struct` or `enum` for a genuinely recursive shape"
            ),
            &def.span,
        ));
    }
    let local: HashSet<&str> = def.params.iter().map(String::as_str).collect();
    let mut out = def.target.clone();
    expand_refs(&mut out, raw, &local, chain)?;
    chain.remove(name);
    Ok(out)
}

/// Replace alias uses inside `te`. `local` names are the enclosing alias's own
/// generic parameters — left as-is (param references, not alias uses).
fn expand_refs(
    te: &mut TypeExpr,
    raw: &HashMap<String, AliasDef>,
    local: &HashSet<&str>,
    chain: &mut HashSet<String>,
) -> Result<(), MetelError> {
    for child in children_mut(te) {
        expand_refs(child, raw, local, chain)?;
    }
    if let TypeExpr::Named(name, args) = te {
        if !local.contains(name.as_str()) {
            if let Some(inner) = raw.get(name) {
                let body = resolve_alias(name, raw, chain)?;
                check_arity(name, inner.params.len(), args.len(), &inner.span)?;
                let subst: HashMap<&str, &TypeExpr> = inner
                    .params
                    .iter()
                    .map(String::as_str)
                    .zip(args.iter())
                    .collect();
                *te = subst_params(&body, &subst);
            }
        }
    }
    Ok(())
}

/// Rewrite an ordinary declaration's type expressions: alias use -> expanded
/// target with the alias's generic parameters bound to the use's args.
fn substitute(te: &mut TypeExpr, resolved: &HashMap<String, AliasDef>) -> Result<(), MetelError> {
    for child in children_mut(te) {
        substitute(child, resolved)?;
    }
    if let TypeExpr::Named(name, args) = te {
        if let Some(def) = resolved.get(name) {
            check_arity(name, def.params.len(), args.len(), &def.span)?;
            let subst: HashMap<&str, &TypeExpr> = def
                .params
                .iter()
                .map(String::as_str)
                .zip(args.iter())
                .collect();
            *te = subst_params(&def.target, &subst);
        }
    }
    Ok(())
}

/// A copy of `body` with every bare `Named(param, [])` replaced by its bound type.
fn subst_params(body: &TypeExpr, subst: &HashMap<&str, &TypeExpr>) -> TypeExpr {
    let mut out = body.clone();
    subst_params_in_place(&mut out, subst);
    out
}

fn subst_params_in_place(te: &mut TypeExpr, subst: &HashMap<&str, &TypeExpr>) {
    if let TypeExpr::Named(name, args) = te {
        if args.is_empty() {
            if let Some(replacement) = subst.get(name.as_str()) {
                *te = (*replacement).clone();
                return;
            }
        }
    }
    for child in children_mut(te) {
        subst_params_in_place(child, subst);
    }
}

/// Mutable references to the directly-nested `TypeExpr` children of `te`
/// (including a `Named`'s type arguments).
fn children_mut(te: &mut TypeExpr) -> Vec<&mut TypeExpr> {
    match te {
        TypeExpr::Named(_, args) | TypeExpr::Tuple(args) => args.iter_mut().collect(),
        TypeExpr::Record(fields) => fields.iter_mut().map(|(_, t)| t).collect(),
        TypeExpr::Array(inner)
        | TypeExpr::SizedArray(inner, _)
        | TypeExpr::Reference(inner)
        | TypeExpr::MutReference(inner)
        | TypeExpr::ImplAspect { bound: inner, .. }
        | TypeExpr::DynAspect { bound: inner, .. }
        | TypeExpr::Projection { base: inner, .. } => vec![inner.as_mut()],
        TypeExpr::Fun {
            params,
            return_type,
            ..
        } => {
            let mut v: Vec<&mut TypeExpr> = params.iter_mut().collect();
            if let Some(rt) = return_type {
                v.push(rt.as_mut());
            }
            v
        }
        TypeExpr::Unit | TypeExpr::RecordProjection { .. } => vec![],
    }
}

fn check_arity(name: &str, want: usize, got: usize, span: &Span) -> Result<(), MetelError> {
    if want == got {
        Ok(())
    } else {
        Err(MetelError::type_error(
            TypeErrorCode::T0004,
            format!("type alias `{name}` takes {want} type argument(s), but {got} were supplied"),
            span,
        ))
    }
}

fn err_t0003(msg: String, span: &Span) -> MetelError {
    MetelError::type_error(TypeErrorCode::T0003, msg, span)
}

/// Apply `f` to every `TypeExpr` reachable from a declaration (signatures, fields,
/// annotations, and `let`/`type` in nested blocks).
fn walk_decl_types(
    decl: &mut Decl,
    f: &mut dyn FnMut(&mut TypeExpr) -> Result<(), MetelError>,
) -> Result<(), MetelError> {
    match decl {
        Decl::Fun(fd) => {
            for p in &mut fd.params {
                if let Some(t) = &mut p.type_ann {
                    f(t)?;
                }
            }
            if let Some(rt) = &mut fd.return_type {
                f(rt)?;
            }
            walk_block(&mut fd.body, f)?;
        }
        Decl::Struct(sd) => {
            for field in &mut sd.fields {
                f(&mut field.type_ann)?;
            }
        }
        Decl::Enum(ed) => {
            for v in &mut ed.variants {
                for field in &mut v.fields {
                    f(&mut field.type_ann)?;
                }
            }
        }
        Decl::Impl(ib) => {
            f(&mut ib.target_type)?;
            for a in &mut ib.aspect_type_args {
                f(a)?;
            }
            for atd in &mut ib.assoc_type_defs {
                f(&mut atd.ty)?;
            }
            for m in &mut ib.methods {
                for p in &mut m.params {
                    if let Some(t) = &mut p.type_ann {
                        f(t)?;
                    }
                }
                if let Some(rt) = &mut m.return_type {
                    f(rt)?;
                }
                walk_block(&mut m.body, f)?;
            }
        }
        Decl::Aspect(ad) => {
            for m in &mut ad.methods {
                for p in &mut m.params {
                    if let Some(t) = &mut p.type_ann {
                        f(t)?;
                    }
                }
                if let Some(rt) = &mut m.return_type {
                    f(rt)?;
                }
            }
        }
        Decl::Let(ld) => {
            if let Some(t) = &mut ld.type_ann {
                f(t)?;
            }
        }
        Decl::Mut(md) => {
            if let Some(t) = &mut md.type_ann {
                f(t)?;
            }
        }
        Decl::Stmt(_) | Decl::TypeAlias(_) => {}
    }
    Ok(())
}

fn walk_block(
    block: &mut Block,
    f: &mut dyn FnMut(&mut TypeExpr) -> Result<(), MetelError>,
) -> Result<(), MetelError> {
    for decl in &mut block.stmts {
        walk_decl_types(decl, f)?;
    }
    Ok(())
}
