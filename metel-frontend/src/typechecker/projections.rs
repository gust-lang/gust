//! Eager validation of names used in annotation positions.
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
//! after the registry is complete and before inference. In addition to record projections,
//! it rejects named types and aspects that cannot resolve in their declaring scope.
//!
//! - the projected type does not exist,
//! - it exists but is not a struct (only structs have rows to project),
//! - it is a struct but has no such field,
//! - a named type or aspect does not exist.
//!
//! ## Coverage, and why under-coverage is the safe direction
//!
//! It walks every type-bearing annotation position, including bounds, generic closure
//! signatures, casts, ascriptions, and explicit call type arguments. The verdict reuses
//! registry lookups rather than re-deriving visibility rules, so imports and qualified
//! paths agree with the rest of the typechecker.

use std::collections::{HashMap, HashSet};

use super::inference::primitive_type_from_name;
use crate::ast::{
    AspectDecl, AspectMethod, AssignTarget, Block, Bound, BoundHead, Decl, Expr, ForInit, FunDecl,
    GenericParam, ImplBlock, Param, Program, Span, Stmt, TypeExpr, WhereClause,
};
use crate::error::{MetelError, TypeErrorCode};
use crate::typeinference::{TypeDefinitionRegistry, VisibleTypeKind};

/// Check every type and aspect name reachable in an annotation position.
///
/// # Errors
/// Returns `T0003` for an unknown type/aspect or an invalid record projection.
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
        cx.decl(decl, Some(index), &HashSet::new(), false, &[])?;
    }
    Ok(())
}

struct Cx<'a> {
    registry: &'a TypeDefinitionRegistry,
    current_module: &'a [String],
    struct_order: HashMap<&'a str, usize>,
}

impl Cx<'_> {
    fn decl(
        &self,
        decl: &Decl,
        at: Option<usize>,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        match decl {
            Decl::Fun(fun) => self.fun(fun, generics, self_allowed, local_types)?,
            Decl::Let(d) => {
                if let Some(t) = &d.type_ann {
                    self.ty(t, &d.span, generics, self_allowed, local_types)?;
                }
                self.expr(&d.value, generics, self_allowed, local_types)?;
            }
            Decl::Mut(d) => {
                if let Some(t) = &d.type_ann {
                    self.ty(t, &d.span, generics, self_allowed, local_types)?;
                }
                self.expr(&d.value, generics, self_allowed, local_types)?;
            }
            Decl::Struct(sd) => {
                let scope = Self::with_generics(generics, &sd.generics);
                self.bounds(
                    &sd.generics,
                    sd.where_clause.as_ref(),
                    &sd.span,
                    &scope,
                    local_types,
                )?;
                for f in &sd.fields {
                    // `at` marks this struct's own position, so a field projecting a
                    // struct declared later can be reported precisely.
                    self.ty_at(&f.type_ann, &f.span, at, &scope, false, false, local_types)?;
                }
            }
            Decl::Enum(ed) => {
                let scope = Self::with_generics(generics, &ed.generics);
                self.bounds(
                    &ed.generics,
                    ed.where_clause.as_ref(),
                    &ed.span,
                    &scope,
                    local_types,
                )?;
                for v in &ed.variants {
                    for f in &v.fields {
                        self.ty(&f.type_ann, &f.span, &scope, false, local_types)?;
                    }
                }
            }
            Decl::Impl(ib) => {
                self.impl_block(ib, generics, local_types)?;
            }
            Decl::Aspect(ad) => self.aspect(ad, generics, local_types)?,
            Decl::Stmt(stmt) => self.stmt(stmt, generics, self_allowed, local_types)?,
        }
        Ok(())
    }

    fn fun(
        &self,
        fun: &FunDecl,
        inherited_generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        let generics = Self::with_generics(inherited_generics, &fun.generics);
        self.bounds(
            &fun.generics,
            fun.where_clause.as_ref(),
            &fun.span,
            &generics,
            local_types,
        )?;
        for p in &fun.params {
            self.param(p, &generics, self_allowed, local_types)?;
        }
        if let Some(t) = &fun.return_type {
            self.ty_return(t, &fun.span, &generics, self_allowed, local_types)?;
        }
        self.block(&fun.body, &generics, self_allowed, local_types)
    }

    fn block(
        &self,
        block: &Block,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        let mut types = HashSet::new();
        for decl in &block.stmts {
            match decl {
                Decl::Struct(sd) => {
                    types.insert(sd.name.clone());
                }
                Decl::Enum(ed) => {
                    types.insert(ed.name.clone());
                }
                _ => {}
            }
        }
        let mut nested_local_types = local_types.to_vec();
        nested_local_types.push(types);
        for d in &block.stmts {
            self.decl(d, None, generics, self_allowed, &nested_local_types)?;
        }
        if let Some(tail) = &block.tail {
            self.expr(tail, generics, self_allowed, &nested_local_types)?;
        }
        Ok(())
    }

    fn stmt(
        &self,
        stmt: &Stmt,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        match stmt {
            Stmt::While(w) => {
                self.expr(&w.condition, generics, self_allowed, local_types)?;
                self.block(&w.body, generics, self_allowed, local_types)
            }
            Stmt::For(f) => {
                if let Some(init) = &f.init {
                    match init {
                        ForInit::Let(d) => self.decl(
                            &Decl::Let(d.clone()),
                            None,
                            generics,
                            self_allowed,
                            local_types,
                        )?,
                        ForInit::Mut(d) => self.decl(
                            &Decl::Mut(d.clone()),
                            None,
                            generics,
                            self_allowed,
                            local_types,
                        )?,
                        ForInit::Expr(e) => self.expr(e, generics, self_allowed, local_types)?,
                    }
                }
                if let Some(condition) = &f.condition {
                    self.expr(condition, generics, self_allowed, local_types)?;
                }
                if let Some(step) = &f.step {
                    self.expr(step, generics, self_allowed, local_types)?;
                }
                self.block(&f.body, generics, self_allowed, local_types)
            }
            Stmt::ForIn(f) => {
                self.expr(&f.iterable, generics, self_allowed, local_types)?;
                self.block(&f.body, generics, self_allowed, local_types)
            }
            Stmt::Expr(e) => self.expr(e, generics, self_allowed, local_types),
        }
    }

    fn ty(
        &self,
        te: &TypeExpr,
        span: &Span,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        self.ty_at(te, span, None, generics, self_allowed, false, local_types)
    }

    fn ty_return(
        &self,
        te: &TypeExpr,
        span: &Span,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        self.ty_at(te, span, None, generics, self_allowed, true, local_types)
    }

    /// Recurse through a type expression, checking every projection inside it — so a
    /// projection nested in `{ inner: Handle.{ fd } }` or `Handle.{ fd }[]` is checked too.
    ///
    /// `field_of` is `Some(index)` only when this type is a struct field's annotation; see
    /// the forward-reference note in `check`.
    #[allow(clippy::too_many_arguments)]
    fn ty_at(
        &self,
        te: &TypeExpr,
        span: &Span,
        field_of: Option<usize>,
        generics: &HashSet<String>,
        self_allowed: bool,
        impl_aspect_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        match te {
            TypeExpr::RecordProjection { path, fields, span } => {
                self.projection(path, fields, span, field_of)
            }
            TypeExpr::Named(name, args) => {
                if !self.is_type_name(name, generics, self_allowed, local_types) {
                    return Err(Self::unknown_type(name, span));
                }
                for a in args {
                    self.ty_at(
                        a,
                        span,
                        field_of,
                        generics,
                        self_allowed,
                        impl_aspect_allowed,
                        local_types,
                    )?;
                }
                Ok(())
            }
            TypeExpr::Tuple(items) => {
                for t in items {
                    self.ty_at(
                        t,
                        span,
                        field_of,
                        generics,
                        self_allowed,
                        impl_aspect_allowed,
                        local_types,
                    )?;
                }
                Ok(())
            }
            TypeExpr::Record(fields) => {
                for (_, t) in fields {
                    self.ty_at(
                        t,
                        span,
                        field_of,
                        generics,
                        self_allowed,
                        impl_aspect_allowed,
                        local_types,
                    )?;
                }
                Ok(())
            }
            TypeExpr::Array(inner)
            | TypeExpr::SizedArray(inner, _)
            | TypeExpr::Reference(inner)
            | TypeExpr::MutReference(inner) => self.ty_at(
                inner,
                span,
                field_of,
                generics,
                self_allowed,
                impl_aspect_allowed,
                local_types,
            ),
            TypeExpr::Fun(params, ret) => {
                for p in params {
                    self.ty_at(
                        p,
                        span,
                        field_of,
                        generics,
                        self_allowed,
                        impl_aspect_allowed,
                        local_types,
                    )?;
                }
                if let Some(r) = ret {
                    self.ty_at(
                        r,
                        span,
                        field_of,
                        generics,
                        self_allowed,
                        impl_aspect_allowed,
                        local_types,
                    )?;
                }
                Ok(())
            }
            TypeExpr::ImplAspect { bound, .. } => {
                if !impl_aspect_allowed {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0022,
                        "`impl Aspect` is only allowed in parameter or return position".to_owned(),
                        span,
                    ));
                }
                self.aspect_type(bound, span, generics, self_allowed, local_types)
            }
            TypeExpr::Projection { base, .. } => self.ty_at(
                base,
                span,
                field_of,
                generics,
                self_allowed,
                impl_aspect_allowed,
                local_types,
            ),
            TypeExpr::Unit => Ok(()),
        }
    }

    fn impl_block(
        &self,
        ib: &ImplBlock,
        inherited_generics: &HashSet<String>,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        let mut generics = Self::with_generics(inherited_generics, &ib.generics);
        generics.extend(self.inherited_impl_generics(&ib.target_type));
        self.bounds(
            &ib.generics,
            ib.where_clause.as_ref(),
            &ib.span,
            &generics,
            local_types,
        )?;
        // `Self` becomes meaningful only inside an extend block, where it names this
        // concrete target. It cannot name the target itself: no enclosing type exists
        // at that point.
        self.ty(&ib.target_type, &ib.span, &generics, false, local_types)?;
        if let Some(aspect) = &ib.aspect_name {
            if !self.registry.is_visible_aspect(self.current_module, aspect) {
                return Err(Self::unknown_aspect(aspect, &ib.span));
            }
        }
        for arg in &ib.aspect_type_args {
            self.ty(arg, &ib.span, &generics, true, local_types)?;
        }
        for assoc in &ib.assoc_type_defs {
            self.ty(&assoc.ty, &assoc.span, &generics, true, local_types)?;
        }
        for method in &ib.methods {
            self.fun(method, &generics, true, local_types)?;
        }
        Ok(())
    }

    fn aspect(
        &self,
        aspect: &AspectDecl,
        inherited_generics: &HashSet<String>,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        let mut generics = inherited_generics.clone();
        generics.extend(aspect.generics.iter().cloned());
        for assoc in &aspect.assoc_types {
            for bound in &assoc.bounds {
                self.bound(bound, &assoc.span, &generics, true, local_types)?;
            }
        }
        // RFC-0082 §1.2: inside an aspect's own method signatures, a bare
        // associated-type name is sugar for `Self::Name`. Treat those names as
        // in-scope for validation just as the converter does; method-level generic
        // parameters are added by `aspect_method` below.
        let mut method_scope = generics.clone();
        method_scope.extend(aspect.assoc_types.iter().map(|assoc| assoc.name.clone()));
        for method in &aspect.methods {
            self.aspect_method(method, &method_scope, local_types)?;
        }
        Ok(())
    }

    fn aspect_method(
        &self,
        method: &AspectMethod,
        inherited_generics: &HashSet<String>,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        let generics = Self::with_generics(inherited_generics, &method.generics);
        for param in &method.params {
            self.param(param, &generics, true, local_types)?;
        }
        if let Some(ret) = &method.return_type {
            self.ty_return(ret, &method.span, &generics, true, local_types)?;
        }
        if let Some(body) = &method.default_body {
            self.block(body, &generics, true, local_types)?;
        }
        Ok(())
    }

    fn param(
        &self,
        param: &Param,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        if let Some(ty) = &param.type_ann {
            self.ty(ty, &param.span, generics, self_allowed, local_types)?;
        }
        Ok(())
    }

    fn bounds(
        &self,
        generic_params: &[GenericParam],
        where_clause: Option<&WhereClause>,
        span: &Span,
        generics: &HashSet<String>,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        for param in generic_params {
            for bound in &param.bounds {
                self.bound(bound, span, generics, false, local_types)?;
            }
        }
        if let Some(where_clause) = where_clause {
            for constraint in &where_clause.constraints {
                for bound in &constraint.bounds {
                    self.bound(bound, span, generics, false, local_types)?;
                }
            }
        }
        Ok(())
    }

    fn bound(
        &self,
        bound: &Bound,
        fallback_span: &Span,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        match &bound.head {
            BoundHead::Aspect(aspect) => {
                self.aspect_type(aspect, &bound.span, generics, self_allowed, local_types)?;
            }
            BoundHead::Row(row) => {
                for field in &row.fields {
                    if let Some(ty) = &field.ty {
                        self.ty(ty, fallback_span, generics, self_allowed, local_types)?;
                    }
                }
            }
        }
        for (_, ty) in &bound.assoc_bindings {
            self.ty(ty, &bound.span, generics, self_allowed, local_types)?;
        }
        Ok(())
    }

    fn aspect_type(
        &self,
        aspect: &TypeExpr,
        span: &Span,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        let TypeExpr::Named(name, args) = aspect else {
            return Err(MetelError::type_error(
                TypeErrorCode::T0003,
                "aspect bounds must name an aspect",
                span,
            ));
        };
        if !self.registry.is_visible_aspect(self.current_module, name) {
            return Err(Self::unknown_aspect(name, span));
        }
        for arg in args {
            self.ty(arg, span, generics, self_allowed, local_types)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn expr(
        &self,
        expr: &Expr,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        match expr {
            Expr::Literal(_, _)
            | Expr::Ident(_, _)
            | Expr::Path(_, _)
            | Expr::ResolvedPath { .. }
            | Expr::RecordProjection { .. }
            | Expr::Continue(_) => Ok(()),
            Expr::Tuple(items, _) | Expr::Array(items, _) => {
                for item in items {
                    self.expr(item, generics, self_allowed, local_types)?;
                }
                Ok(())
            }
            Expr::RecordLiteral { fields, .. } | Expr::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.expr(value, generics, self_allowed, local_types)?;
                }
                Ok(())
            }
            Expr::RepeatArray(value, _, _)
            | Expr::UnaryOp(_, value, _)
            | Expr::FieldAccess { object: value, .. }
            | Expr::TupleAccess { object: value, .. }
            | Expr::PropagateError { expr: value, .. } => {
                self.expr(value, generics, self_allowed, local_types)
            }
            Expr::BinOp(left, _, right, _)
            | Expr::Index {
                object: left,
                index: right,
                ..
            } => {
                self.expr(left, generics, self_allowed, local_types)?;
                self.expr(right, generics, self_allowed, local_types)
            }
            Expr::Assign { target, value, .. } => {
                self.assign_target(target, generics, self_allowed, local_types)?;
                self.expr(value, generics, self_allowed, local_types)
            }
            Expr::Call {
                callee,
                type_args,
                args,
                span,
            } => {
                self.expr(callee, generics, self_allowed, local_types)?;
                for ty in type_args {
                    self.ty(ty, span, generics, self_allowed, local_types)?;
                }
                for arg in args {
                    self.expr(arg, generics, self_allowed, local_types)?;
                }
                Ok(())
            }
            Expr::MethodCall {
                receiver,
                type_args,
                args,
                span,
                ..
            } => {
                self.expr(receiver, generics, self_allowed, local_types)?;
                for ty in type_args {
                    self.ty(ty, span, generics, self_allowed, local_types)?;
                }
                for arg in args {
                    self.expr(arg, generics, self_allowed, local_types)?;
                }
                Ok(())
            }
            Expr::Cast {
                expr,
                target_type,
                span,
            } => {
                self.expr(expr, generics, self_allowed, local_types)?;
                self.ty(target_type, span, generics, self_allowed, local_types)
            }
            Expr::Ascribe { expr, ann, span } => {
                self.expr(expr, generics, self_allowed, local_types)?;
                self.ty(ann, span, generics, self_allowed, local_types)
            }
            Expr::Match(m) => {
                self.expr(&m.scrutinee, generics, self_allowed, local_types)?;
                for arm in &m.arms {
                    if let Some(guard) = &arm.guard {
                        self.expr(guard, generics, self_allowed, local_types)?;
                    }
                    self.block(&arm.body, generics, self_allowed, local_types)?;
                }
                Ok(())
            }
            Expr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                self.expr(condition, generics, self_allowed, local_types)?;
                self.block(then_branch, generics, self_allowed, local_types)?;
                if let Some(else_branch) = else_branch {
                    self.block(else_branch, generics, self_allowed, local_types)?;
                }
                Ok(())
            }
            Expr::Loop { body, .. } => self.block(body, generics, self_allowed, local_types),
            Expr::Closure {
                params,
                return_type,
                body,
                span,
            } => {
                for param in params {
                    self.param(param, generics, self_allowed, local_types)?;
                }
                if let Some(ret) = return_type {
                    self.ty_return(ret, span, generics, self_allowed, local_types)?;
                }
                self.block(body, generics, self_allowed, local_types)
            }
            Expr::Return(ret) => {
                if let Some(value) = &ret.value {
                    self.expr(value, generics, self_allowed, local_types)?;
                }
                Ok(())
            }
            Expr::Break(ret) => {
                if let Some(value) = &ret.value {
                    self.expr(value, generics, self_allowed, local_types)?;
                }
                Ok(())
            }
        }
    }

    fn assign_target(
        &self,
        target: &AssignTarget,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> Result<(), MetelError> {
        match target {
            AssignTarget::Ident(_, _) => Ok(()),
            AssignTarget::FieldAccess { object, .. }
            | AssignTarget::TupleAccess { object, .. }
            | AssignTarget::Deref { object, .. } => {
                self.expr(object, generics, self_allowed, local_types)
            }
            AssignTarget::Index { object, index, .. } => {
                self.expr(object, generics, self_allowed, local_types)?;
                self.expr(index, generics, self_allowed, local_types)
            }
        }
    }

    fn with_generics(inherited: &HashSet<String>, params: &[GenericParam]) -> HashSet<String> {
        let mut result = inherited.clone();
        result.extend(params.iter().map(|param| param.name.clone()));
        result
    }

    /// Inherent impl blocks on generic nominal types use the declaration's type
    /// parameter names without repeating an `impl<T>` binder (`extend Perhaps<T>`).
    /// Treat only an argument that repeats its declared parameter name as that inherited
    /// binder; `extend Perhaps<i64>` must not accidentally make a free `T` valid.
    fn inherited_impl_generics(&self, target: &TypeExpr) -> HashSet<String> {
        let TypeExpr::Named(name, args) = target else {
            return HashSet::new();
        };
        let Some(declared) = self.registry.struct_generic_names_for(name) else {
            return HashSet::new();
        };
        declared
            .iter()
            .zip(args)
            .filter_map(|(declared_name, arg)| match arg {
                TypeExpr::Named(actual_name, nested)
                    if nested.is_empty() && actual_name == declared_name =>
                {
                    Some(actual_name.clone())
                }
                _ => None,
            })
            .collect()
    }

    fn is_type_name(
        &self,
        name: &str,
        generics: &HashSet<String>,
        self_allowed: bool,
        local_types: &[HashSet<String>],
    ) -> bool {
        (name == "Self" && self_allowed)
            || generics.contains(name)
            || local_types.iter().rev().any(|scope| scope.contains(name))
            || primitive_type_from_name(name).is_some()
            // These are the two non-registry named forms accepted directly by the
            // TypeExpr -> InferType conversion.
            || matches!(name, "Array" | "Never")
            || self
                .registry
                .visible_type_kind(self.current_module, name)
                .is_some()
    }

    fn unknown_type(name: &str, span: &Span) -> MetelError {
        MetelError::type_error(TypeErrorCode::T0003, format!("unknown type `{name}`"), span)
    }

    fn unknown_aspect(name: &str, span: &Span) -> MetelError {
        MetelError::type_error(
            TypeErrorCode::T0003,
            format!("unknown aspect `{name}`"),
            span,
        )
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
