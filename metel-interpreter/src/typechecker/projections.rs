//! Validation of record projection types (`Handle.{ fd }`, RFC-0116 §4).
//!
//! ## Why this is a separate pass
//!
//! Resolving `Handle.{ fd }` to a row needs the struct's field types, so it happens in
//! `conversions::resolve_record_projection_type` — which is **infallible**, because every
//! `TypeExpr` → `InferType` conversion path is. That means it cannot report *why* a
//! projection failed; it can only return a stand-in that fails later, somewhere else,
//! with a message about the wrong thing.
//!
//! This pass exists so the diagnosis happens where an error *can* be returned. It runs
//! after the registry is complete and before inference, and reports the three real
//! mistakes directly, at the projection's own span:
//!
//! - the projected type does not exist,
//! - it exists but is not a struct (only structs have rows to project),
//! - it is a struct but has no such field.
//!
//! ## Coverage, and why under-coverage is the safe direction
//!
//! It walks *annotation* positions — function and aspect-method signatures, `let`/`var`
//! annotations (including inside bodies), struct and enum field types, and `extend`
//! targets — not arbitrary nested expressions. A projection in a position this pass does
//! not reach still fails during inference via the stand-in, just with a blunter message.
//! Missing a position therefore costs message quality, never soundness; reporting a *valid*
//! projection as broken would be the real bug, so the verdict here reuses the same registry
//! lookups the resolver itself uses rather than re-deriving them.

use std::collections::HashMap;

use crate::ast::{Block, Decl, FunDecl, Program, Span, Stmt, TypeExpr};
use crate::error::{MetelError, TypeErrorCode};
use crate::typeinference::{TypeDefinitionRegistry, VisibleTypeKind};

/// Check every record projection reachable in an annotation position.
///
/// # Errors
/// Returns `T0003` for a projection whose target type is unknown, is not a struct, or
/// lacks one of the projected fields.
pub(super) fn check(
    program: &Program,
    registry: &TypeDefinitionRegistry,
    current_module: &[String],
) -> Result<(), MetelError> {
    // Declaration order of top-level structs, used only for the forward-reference case
    // below. Struct *field* types are converted while the registry is still being built,
    // so a field projecting a struct declared later resolves against an incomplete
    // registry and silently stores a stand-in. This pass runs against the *complete*
    // registry and would otherwise call that fine, leaving the mistake to surface much
    // later as a bare `cannot unify … with B.{ x }`.
    let struct_order: HashMap<&str, usize> = program
        .decls
        .iter()
        .enumerate()
        .filter_map(|(i, d)| match d {
            Decl::Struct(sd) => Some((sd.name.as_str(), i)),
            _ => None,
        })
        .collect();

    let cx = Cx {
        registry,
        current_module,
        struct_order,
    };
    for (index, decl) in program.decls.iter().enumerate() {
        cx.decl(decl, Some(index))?;
    }
    Ok(())
}

struct Cx<'a> {
    registry: &'a TypeDefinitionRegistry,
    current_module: &'a [String],
    struct_order: HashMap<&'a str, usize>,
}

impl Cx<'_> {
    fn decl(&self, decl: &Decl, at: Option<usize>) -> Result<(), MetelError> {
        match decl {
            Decl::Fun(fun) => self.fun(fun)?,
            Decl::Let(d) => {
                if let Some(t) = &d.type_ann {
                    self.ty(t)?;
                }
            }
            Decl::Mut(d) => {
                if let Some(t) = &d.type_ann {
                    self.ty(t)?;
                }
            }
            Decl::Struct(sd) => {
                for f in &sd.fields {
                    // `at` marks this struct's own position, so a field projecting a
                    // struct declared later can be reported precisely.
                    self.ty_at(&f.type_ann, at)?;
                }
            }
            Decl::Enum(ed) => {
                for v in &ed.variants {
                    for f in &v.fields {
                        self.ty(&f.type_ann)?;
                    }
                }
            }
            Decl::Impl(ib) => {
                self.ty(&ib.target_type)?;
                for m in &ib.methods {
                    self.fun(m)?;
                }
            }
            Decl::Aspect(ad) => {
                for m in &ad.methods {
                    for p in &m.params {
                        if let Some(t) = &p.type_ann {
                            self.ty(t)?;
                        }
                    }
                    if let Some(t) = &m.return_type {
                        self.ty(t)?;
                    }
                }
            }
            Decl::Stmt(stmt) => self.stmt(stmt)?,
        }
        Ok(())
    }

    fn fun(&self, fun: &FunDecl) -> Result<(), MetelError> {
        for p in &fun.params {
            if let Some(t) = &p.type_ann {
                self.ty(t)?;
            }
        }
        if let Some(t) = &fun.return_type {
            self.ty(t)?;
        }
        self.block(&fun.body)
    }

    fn block(&self, block: &Block) -> Result<(), MetelError> {
        for d in &block.stmts {
            self.decl(d, None)?;
        }
        Ok(())
    }

    fn stmt(&self, stmt: &Stmt) -> Result<(), MetelError> {
        match stmt {
            Stmt::While(w) => self.block(&w.body),
            Stmt::For(f) => self.block(&f.body),
            Stmt::ForIn(f) => self.block(&f.body),
            Stmt::Expr(_) => Ok(()),
        }
    }

    fn ty(&self, te: &TypeExpr) -> Result<(), MetelError> {
        self.ty_at(te, None)
    }

    /// Recurse through a type expression, checking every projection inside it — so a
    /// projection nested in `{ inner: Handle.{ fd } }` or `Handle.{ fd }[]` is checked too.
    ///
    /// `field_of` is `Some(index)` only when this type is a struct field's annotation; see
    /// the forward-reference note in `check`.
    fn ty_at(&self, te: &TypeExpr, field_of: Option<usize>) -> Result<(), MetelError> {
        match te {
            TypeExpr::RecordProjection { path, fields, span } => {
                self.projection(path, fields, span, field_of)
            }
            TypeExpr::Named(_, args) => {
                for a in args {
                    self.ty_at(a, field_of)?;
                }
                Ok(())
            }
            TypeExpr::Tuple(items) => {
                for t in items {
                    self.ty_at(t, field_of)?;
                }
                Ok(())
            }
            TypeExpr::Record(fields) => {
                for (_, t) in fields {
                    self.ty_at(t, field_of)?;
                }
                Ok(())
            }
            TypeExpr::Array(inner)
            | TypeExpr::SizedArray(inner, _)
            | TypeExpr::Reference(inner)
            | TypeExpr::MutReference(inner) => self.ty_at(inner, field_of),
            TypeExpr::Fun(params, ret) => {
                for p in params {
                    self.ty_at(p, field_of)?;
                }
                if let Some(r) = ret {
                    self.ty_at(r, field_of)?;
                }
                Ok(())
            }
            TypeExpr::ImplAspect { bound, .. } => self.ty_at(bound, field_of),
            TypeExpr::Projection { base, .. } => self.ty_at(base, field_of),
            TypeExpr::Unit => Ok(()),
        }
    }

    fn projection(
        &self,
        path: &[String],
        fields: &[String],
        span: &Span,
        field_of: Option<usize>,
    ) -> Result<(), MetelError> {
        let target = path.join("::");
        let spelling = format!("{target}.{{ {} }}", fields.join(", "));

        // Forward reference from a struct field: the registry is complete *here*, so the
        // lookups below would succeed, but the field's stored type was converted before
        // the target existed and is already a stand-in. Report it now rather than let it
        // reappear as an opaque `cannot unify … with B.{ x }` at the first use.
        if let (Some(site), Some(&target_at)) = (field_of, self.struct_order.get(target.as_str())) {
            if target_at > site {
                return Err(MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!(
                        "invalid record projection `{spelling}`: `{target}` is declared later in this module; a struct field cannot project a struct that is not yet declared — move `{target}` above it"
                    ),
                    span,
                ));
            }
        }

        let Some((struct_name, raw_fields)) = self
            .registry
            .projection_struct_fields(self.current_module, &target)
        else {
            // Not a projectable struct. Distinguish "wrong kind of type" from "no such
            // type" — the fix differs, and the blunt version of this message was the
            // reason this pass exists.
            let reason = match self.registry.visible_type_kind(self.current_module, &target) {
                Some(VisibleTypeKind::Enum) => format!(
                    "`{target}` is an enum; only structs have a row to project (an enum is a sum, not a product)"
                ),
                // A struct the registry knows but cannot project here: either declared
                // later than this use, or not visible from this module.
                Some(VisibleTypeKind::Struct) => format!(
                    "`{target}` is not available at this point — a projection cannot refer to a struct declared later in the same module"
                ),
                None => format!("unknown type `{target}`"),
            };
            return Err(MetelError::type_error(
                TypeErrorCode::T0003,
                format!("invalid record projection `{spelling}`: {reason}"),
                span,
            ));
        };

        for field_name in fields {
            if !raw_fields.iter().any(|e| e.name == *field_name) {
                let mut known: Vec<&str> = raw_fields.iter().map(|e| e.name.as_str()).collect();
                known.sort_unstable();
                return Err(MetelError::type_error(
                    TypeErrorCode::T0003,
                    format!(
                        "invalid record projection `{spelling}`: struct `{struct_name}` has no field `{field_name}` (it has: {})",
                        known.join(", ")
                    ),
                    span,
                ));
            }
        }
        Ok(())
    }
}
