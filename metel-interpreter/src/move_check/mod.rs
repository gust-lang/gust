pub mod place;

use std::collections::{HashMap, HashSet};

use crate::ast::{GenericParam, Pattern, Polarity, ReceiverKind, Span};
use crate::error::{MetelError, TypeErrorCode};
use crate::typed_ast::{
    FunBody, MethodDispatch, TypedBlock, TypedDecl, TypedExpr, TypedForInit, TypedModule,
    TypedModuleGraph, TypedPlace, TypedStmt,
};
use crate::typeinference::{
    type_to_infer, AspectAssumptions, GenericBound, InferType, Substitution, TypeCtx,
    TypeDefinitionRegistry, TypeScheme, TypeVar, TypeVarGenerator,
};
use crate::types::Type;

use self::place::{from_expr as place_from_expr, from_typed_place, Place, Projection};

#[derive(Debug, Clone)]
pub struct MoveViolation {
    pub binding: String,
    pub use_place: Place,
    pub moved_place: Place,
    pub kind: MoveViolationKind,
    pub moved_by_value_receiver: bool,
    /// Coarse shape of the value that moved, for triage: which of these
    /// violations would disappear if a given type became `Copy`. Notably
    /// `T[]`, whose ownership is RFC-0124's open question.
    pub moved_type: String,
    pub use_span: Span,
    pub moved_span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveViolationKind {
    UseAfterMove,
    PartialMoveUsedAsWhole,
    PartialMoveOfDropType,
    ArrayElementMove,
    BorrowedArrayElementMove,
    MovedMutReferenceWithoutReborrow,
}

/// A coarse bucket for `ty`, enough to separate the sequence types from
/// everything else without exploding into one label per user struct.
#[must_use]
fn type_bucket(ty: &Type) -> String {
    match ty {
        Type::Array(_) => "T[]".to_string(),
        Type::SizedArray(_, _) => "[T; N]".to_string(),
        Type::Tuple(_) => "tuple".to_string(),
        Type::Record(_) => "record".to_string(),
        Type::Str => "String".to_string(),
        Type::Reference(_) => "&T".to_string(),
        Type::MutReference(_) => "&var T".to_string(),
        Type::Fun(_, _) => "fun".to_string(),
        Type::Named(name, _) => format!("named:{}", name.rsplit("::").next().unwrap_or(name)),
        other => format!("other:{other}"),
    }
}

#[derive(Debug, Clone, Default)]
pub struct MoveCheckReport {
    pub violations: Vec<MoveViolation>,
    pub skipped_generic_bodies_user: usize,
    pub skipped_generic_bodies_embedded_std: usize,
    pub unchecked_generic_bodies: Vec<UncheckedGenericBody>,
}

#[derive(Debug, Clone)]
pub struct UncheckedGenericBody {
    pub span: Span,
    pub reason: String,
}

impl MoveCheckReport {
    #[must_use]
    pub fn violation_count(&self) -> usize {
        self.violations.len()
    }

    #[must_use]
    pub fn skipped_generic_bodies_total(&self) -> usize {
        self.skipped_generic_bodies_user + self.skipped_generic_bodies_embedded_std
    }
}

#[must_use]
pub fn collect_graph_violations(graph: &TypedModuleGraph) -> MoveCheckReport {
    let mut checker = Checker::new(&graph.type_registry);
    for module in &graph.modules {
        checker.check_module(module);
    }
    checker.report
}

/// Run the move checker, convert the first violation into a user-facing type error,
/// and return diagnostics for bodies that could not be checked.
///
/// # Errors
/// Returns `T0019` when the graph contains a move-checking violation.
pub fn check_graph(graph: &TypedModuleGraph) -> Result<Vec<String>, MetelError> {
    let report = collect_graph_violations(graph);
    if let Some(violation) = report
        .violations
        .into_iter()
        .find(|violation| !is_embedded_std_span(&violation.use_span))
    {
        let span = violation.use_span.clone();
        return Err(MetelError::type_error(
            TypeErrorCode::T0019,
            violation_message(&violation),
            &span,
        ));
    }
    Ok(report
        .unchecked_generic_bodies
        .into_iter()
        .map(|unchecked| {
            format!(
                "move checking could not analyze generic body at {}:{}:{}: {}",
                unchecked.span.filename,
                unchecked.span.line,
                unchecked.span.col,
                unchecked.reason
            )
        })
        .collect())
}

#[derive(Debug, Clone)]
struct MoveRecord {
    place: Place,
    moved_span: Span,
    cause: MoveCause,
    moved_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoveCause {
    Other,
    ByValueReceiver,
}

#[derive(Debug, Clone, Default)]
struct FlowState {
    moved: HashMap<String, Vec<MoveRecord>>,
    binding_types: HashMap<String, Type>,
    /// Loop bindings sourced from a `T[]` borrowed view. They may be read or
    /// borrowed, but a non-Copy value cannot be moved out through the binding.
    borrowed_array_bindings: HashSet<String>,
    scopes: Vec<Vec<String>>,
}

impl FlowState {
    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        if let Some(bindings) = self.scopes.pop() {
            for binding in bindings {
                self.moved.remove(&binding);
                self.binding_types.remove(&binding);
                self.borrowed_array_bindings.remove(&binding);
            }
        }
    }

    fn bind(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(name.to_string());
        }
        self.moved.remove(name);
        self.binding_types.remove(name);
        self.borrowed_array_bindings.remove(name);
    }

    fn bind_typed(&mut self, name: &str, ty: &Type) {
        self.bind(name);
        self.binding_types.insert(name.to_string(), ty.clone());
    }

    fn bind_borrowed_array_element(&mut self, name: &str, ty: &Type) {
        self.bind_typed(name, ty);
        self.borrowed_array_bindings.insert(name.to_string());
    }

    fn is_borrowed_array_element(&self, place: &Place) -> bool {
        self.borrowed_array_bindings.contains(place.root())
    }

    fn binding_type(&self, name: &str) -> Option<&Type> {
        self.binding_types.get(name)
    }

    fn record_move(&mut self, place: Place, moved_span: Span, cause: MoveCause, moved_type: String) {
        let root = place.root().to_string();
        let records = self.moved.entry(root).or_default();
        if place.projections().is_empty() {
            records.clear();
            records.push(MoveRecord { place, moved_span, cause, moved_type });
            return;
        }
        records.retain(|record| !place.is_prefix_of(&record.place));
        records.push(MoveRecord { place, moved_span, cause, moved_type });
    }

    fn moved_record_for_descendant_use(&self, place: &Place) -> Option<&MoveRecord> {
        self.moved
            .get(place.root())?
            .iter()
            .find(|record| record.place.is_prefix_of(place))
    }

    fn moved_record_for_whole_use(&self, place: &Place) -> Option<&MoveRecord> {
        self.moved.get(place.root())?.iter().find(|record| {
            record.place.is_prefix_of(place) || place.is_prefix_of(&record.place)
        })
    }

    fn union_from(&mut self, other: &Self) {
        for (root, incoming) in &other.moved {
            let records = self.moved.entry(root.clone()).or_default();
            for record in incoming {
                if records
                    .iter()
                    .any(|existing| existing.place == record.place && existing.moved_span == record.moved_span)
                {
                    continue;
                }
                if record.place.projections().is_empty() {
                    records.clear();
                    records.push(record.clone());
                    continue;
                }
                if records.iter().any(|existing| existing.place.projections().is_empty()) {
                    continue;
                }
                records.push(record.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
struct GenericMoveEnv {
    placeholders: HashMap<String, TypeVar>,
    assumptions: AspectAssumptions,
    symbolic_aspects: HashMap<String, HashSet<String>>,
    arg_types: Vec<Type>,
}

struct Checker<'a> {
    registry: &'a TypeDefinitionRegistry,
    report: MoveCheckReport,
    type_ctx: Option<TypeCtx>,
    generic_envs: Vec<GenericMoveEnv>,
}

impl<'a> Checker<'a> {
    fn new(registry: &'a TypeDefinitionRegistry) -> Self {
        Self {
            registry,
            report: MoveCheckReport::default(),
            type_ctx: None,
            generic_envs: Vec::new(),
        }
    }

    fn check_module(&mut self, module: &TypedModule) {
        let mut state = FlowState::default();
        self.type_ctx = Some(TypeCtx {
            scheme_env: module.scheme_env.clone(),
            registry: self.registry.clone(),
        });
        state.push_scope();
        for decl in &module.decls {
            self.check_decl(decl, &module.module_path, &mut state);
        }
        state.pop_scope();
    }

    fn check_decl(&mut self, decl: &TypedDecl, current_module: &[String], state: &mut FlowState) {
        match decl {
            TypedDecl::Let(let_decl) => {
                self.consume_expr(&let_decl.value, current_module, state);
                state.bind_typed(&let_decl.name, let_decl.value.ty());
            }
            TypedDecl::Mut(mut_decl) => {
                self.consume_expr(&mut_decl.value, current_module, state);
                state.bind_typed(&mut_decl.name, mut_decl.value.ty());
            }
            TypedDecl::Fun(fun) => {
                state.bind(&fun.name);
                match &fun.body {
                    FunBody::Typed(body) => {
                        let mut fn_state = FlowState::default();
                        fn_state.push_scope();
                        for param in &fun.params {
                            fn_state.bind(&param.name);
                        }
                        self.check_block(body, current_module, &mut fn_state);
                        fn_state.pop_scope();
                    }
                    FunBody::Generic(body) => {
                        self.check_generic_body(
                            &fun.name,
                            &fun.generics,
                            &fun.params,
                            body,
                            &fun.span,
                            current_module,
                        );
                    }
                    FunBody::Native(_) => {}
                }
            }
            TypedDecl::Stmt(stmt) => self.check_stmt(stmt, current_module, state),
            TypedDecl::Impl(ib) => {
                for method in &ib.methods {
                    match &method.body {
                        FunBody::Typed(body) => {
                            let mut fn_state = FlowState::default();
                            fn_state.push_scope();
                            for param in &method.params {
                                fn_state.bind(&param.name);
                            }
                            self.check_block(body, current_module, &mut fn_state);
                            fn_state.pop_scope();
                        }
                        FunBody::Generic(body) => {
                            self.check_generic_method_body(ib, method, body, current_module);
                        }
                        FunBody::Native(_) => {}
                    }
                }
            }
            TypedDecl::Struct(_) | TypedDecl::Enum(_) | TypedDecl::Aspect(_) => {}
        }
    }

    fn check_generic_body(
        &mut self,
        name: &str,
        generics: &[GenericParam],
        params: &[crate::ast::Param],
        body: &crate::ast::Block,
        span: &Span,
        current_module: &[String],
    ) {
        if let Some((typed_body, generic_env)) =
            self.construct_generic_body_for_move(name, generics, params, body, span)
        {
            let mut fn_state = FlowState::default();
            fn_state.push_scope();
            for (param, ty) in params.iter().zip(&generic_env.arg_types) {
                fn_state.bind_typed(&param.name, ty);
            }
            self.generic_envs.push(generic_env);
            self.check_block(&typed_body, current_module, &mut fn_state);
            fn_state.pop_scope();
            self.generic_envs.pop();
        }
    }

    fn construct_generic_body_for_move(
        &mut self,
        name: &str,
        generics: &[GenericParam],
        params: &[crate::ast::Param],
        body: &crate::ast::Block,
        span: &Span,
    ) -> Option<(TypedBlock, GenericMoveEnv)> {
        let Some(type_ctx) = self.type_ctx.as_ref() else {
            self.record_skipped_generic_body(span, "type context was unavailable");
            return None;
        };
        let Some(raw_scheme) = type_ctx.scheme_env.get(name) else {
            self.record_skipped_generic_body(span, "function type scheme was unavailable");
            return None;
        };
        let scheme = scheme_with_source_generics(raw_scheme, generics);
        let Some((arg_types, generic_env)) = Self::generic_sample_args(&scheme, &type_ctx.registry)
        else {
            self.record_skipped_generic_body(
                span,
                "function type scheme could not be converted to symbolic arguments",
            );
            return None;
        };
        let symbolic_type_ctx = type_ctx_with_symbolic_aspect_methods(type_ctx, &generic_env);
        match crate::typechecker::construct_generic_body(
            &scheme,
            params,
            &arg_types,
            body,
            span,
            &symbolic_type_ctx,
        ) {
            Ok(typed_body) => Some((typed_body, generic_env)),
            Err(error) => {
                let reason = symbolic_method_ambiguity_reason(
                    &error,
                    &generic_env,
                    &type_ctx.registry,
                )
                .unwrap_or_else(|| error.to_string());
                self.record_skipped_generic_body(span, reason);
                None
            }
        }
    }

    fn check_generic_method_body(
        &mut self,
        impl_block: &crate::typed_ast::TypedImplBlock,
        method: &crate::typed_ast::TypedFunDecl,
        body: &crate::ast::Block,
        current_module: &[String],
    ) {
        if self.type_ctx.is_none() {
            self.record_skipped_generic_body(&method.span, "type context was unavailable");
            return;
        }
        let raw_scheme = self
            .registry
            .generic_method_scheme_for_decl(&method.span)
            .cloned()
            .or_else(|| {
                crate::typechecker::symbolic_impl_method_scheme(
                    self.registry,
                    &impl_block.generics,
                    &method.generics,
                    &impl_block.target_type,
                    impl_block.aspect_name.as_deref(),
                    &method.params,
                    method.return_type.as_ref(),
                )
            });
        let Some(raw_scheme) = raw_scheme else {
            self.record_skipped_generic_body(&method.span, "method type scheme was unavailable");
            return;
        };
        let mut source_generics = impl_block.generics.clone();
        source_generics.extend_from_slice(&method.generics);
        let scheme = scheme_with_source_generics(&raw_scheme, &source_generics);
        let Some(type_ctx) = self.type_ctx.as_ref() else {
            self.record_skipped_generic_body(&method.span, "type context was unavailable");
            return;
        };
        let Some((arg_types, generic_env)) = Self::generic_sample_args(&scheme, &type_ctx.registry)
        else {
            self.record_skipped_generic_body(
                &method.span,
                "method type scheme could not be converted to symbolic arguments",
            );
            return;
        };
        let symbolic_type_ctx = type_ctx_with_symbolic_aspect_methods(type_ctx, &generic_env);
        match crate::typechecker::construct_generic_body(
            &scheme,
            &method.params,
            &arg_types,
            body,
            &method.span,
            &symbolic_type_ctx,
        ) {
            Ok(typed_body) => {
                let mut fn_state = FlowState::default();
                fn_state.push_scope();
                for (param, ty) in method.params.iter().zip(&generic_env.arg_types) {
                    fn_state.bind_typed(&param.name, ty);
                }
                self.generic_envs.push(generic_env);
                self.check_block(&typed_body, current_module, &mut fn_state);
                fn_state.pop_scope();
                self.generic_envs.pop();
            }
            Err(error) => {
                let reason = symbolic_method_ambiguity_reason(
                    &error,
                    &generic_env,
                    &type_ctx.registry,
                )
                .unwrap_or_else(|| error.to_string());
                self.record_skipped_generic_body(&method.span, reason);
            }
        }
    }

    fn generic_sample_args(
        scheme: &TypeScheme,
        registry: &TypeDefinitionRegistry,
    ) -> Option<(Vec<Type>, GenericMoveEnv)> {
        let mut subst = Substitution::new();
        let mut generic_env = GenericMoveEnv::default();
        let mut named_samples = HashMap::new();
        for (index, var) in scheme.quantified_vars.iter().enumerate() {
            let placeholder = generic_placeholder_name(*var);
            let sample = Type::Named(placeholder.clone(), Vec::new());
            if let Some(name) = scheme.param_names.get(index) {
                named_samples.insert(name.clone(), type_to_infer(&sample));
            }
            generic_env.placeholders.insert(placeholder, *var);
            if let Some(bounds) = scheme.bounds.get(index) {
                let assumed: HashSet<String> = bounds
                    .iter()
                    .filter_map(GenericBound::aspect_name)
                    .map(ToOwned::to_owned)
                    .collect();
                if !assumed.is_empty() {
                    generic_env.assumptions.insert(*var, assumed);
                }
            }
            subst.bind(*var, type_to_infer(&sample));
        }
        generic_env.symbolic_aspects = symbolic_aspect_assumptions(
            registry,
            &generic_env.placeholders,
            &generic_env.assumptions,
        );
        let InferType::Fun(params, _) = &scheme.ty else {
            return None;
        };
        let arg_types = params
            .iter()
            .map(|param| {
                let substituted = subst.apply(param);
                infer_to_type(&substitute_named_generics(
                    &substituted,
                    &named_samples,
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        generic_env.arg_types.clone_from(&arg_types);
        Some((arg_types, generic_env))
    }

    fn check_stmt(&mut self, stmt: &TypedStmt, current_module: &[String], state: &mut FlowState) {
        match stmt {
            TypedStmt::Expr(expr) => self.observe_expr(expr, current_module, state),
            TypedStmt::While(while_stmt) => {
                self.observe_expr(&while_stmt.condition, current_module, state);
                let mut body_state = state.clone();
                self.check_block(&while_stmt.body, current_module, &mut body_state);
                state.union_from(&body_state);
            }
            TypedStmt::For(for_stmt) => {
                state.push_scope();
                if let Some(init) = &for_stmt.init {
                    match init {
                        TypedForInit::Let(let_decl) => {
                            self.consume_expr(&let_decl.value, current_module, state);
                            state.bind_typed(&let_decl.name, let_decl.value.ty());
                        }
                        TypedForInit::Mut(mut_decl) => {
                            self.consume_expr(&mut_decl.value, current_module, state);
                            state.bind_typed(&mut_decl.name, mut_decl.value.ty());
                        }
                        TypedForInit::Expr(expr) => self.observe_expr(expr, current_module, state),
                    }
                }
                if let Some(condition) = &for_stmt.condition {
                    self.observe_expr(condition, current_module, state);
                }
                let mut body_state = state.clone();
                self.check_block(&for_stmt.body, current_module, &mut body_state);
                if let Some(step) = &for_stmt.step {
                    self.observe_expr(step, current_module, &mut body_state);
                }
                state.union_from(&body_state);
                state.pop_scope();
            }
            TypedStmt::ForIn(for_in) => {
                self.observe_expr(&for_in.iterable, current_module, state);
                let mut body_state = state.clone();
                body_state.push_scope();
                let iterable_ty = peel_type_references(for_in.iterable.ty());
                match iterable_ty {
                    Type::Array(element_ty) => {
                        body_state.bind_borrowed_array_element(&for_in.binding, element_ty);
                    }
                    Type::SizedArray(element_ty, _) => {
                        body_state.bind_typed(&for_in.binding, element_ty);
                    }
                    _ => body_state.bind(&for_in.binding),
                }
                self.check_block(&for_in.body, current_module, &mut body_state);
                body_state.pop_scope();
                state.union_from(&body_state);
            }
        }
    }

    fn check_block(&mut self, block: &TypedBlock, current_module: &[String], state: &mut FlowState) {
        state.push_scope();
        for decl in &block.stmts {
            self.check_decl(decl, current_module, state);
        }
        if let Some(tail) = &block.tail {
            self.consume_expr(tail, current_module, state);
        }
        state.pop_scope();
    }

    fn observe_expr(&mut self, expr: &TypedExpr, current_module: &[String], state: &mut FlowState) {
        if let Some(place) = place_from_expr(expr) {
            self.record_whole_use_if_moved(&place, expr.span(), state);
        }
        match expr {
            TypedExpr::Literal(..)
            | TypedExpr::Ident(..)
            | TypedExpr::Path(..)
            | TypedExpr::Continue(_) => {}
            TypedExpr::Tuple(items, ..) | TypedExpr::Array(items, ..) => {
                for item in items {
                    self.observe_expr(item, current_module, state);
                }
            }
            TypedExpr::RecordLiteral { fields, .. } | TypedExpr::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.consume_expr_with_cause(value, current_module, state, MoveCause::Other);
                }
            }
            TypedExpr::RepeatArray(value, ..) => self.consume_expr(value, current_module, state),
            TypedExpr::BinOp(left, _, right, ..) => {
                self.observe_expr(left, current_module, state);
                self.observe_expr(right, current_module, state);
            }
            TypedExpr::UnaryOp(_, inner, ..) => self.observe_expr(inner, current_module, state),
            // `init`'s value is moved into the fresh cell the temporary reference
            // wraps, the same as RepeatArray's element or a struct field's value —
            // it is not itself a place being read, so it's consumed, not observed.
            TypedExpr::RefTemp { init, .. } => self.consume_expr(init, current_module, state),
            TypedExpr::Assign { target, value, .. } => {
                self.observe_typed_place(target, current_module, state);
                self.consume_expr(value, current_module, state);
                Self::reinitialize_assigned_place(target, state);
            }
            TypedExpr::Call { callee, args, .. } => {
                self.observe_call_expr(callee, args, current_module, state);
            }
            TypedExpr::MethodCall {
                receiver,
                method,
                args,
                dispatch,
                ..
            } => {
                self.observe_method_call_expr(receiver, method, args, dispatch, current_module, state);
            }
            TypedExpr::FieldAccess { object, .. } | TypedExpr::TupleAccess { object, .. } => {
                self.observe_projection_base_expr(object, current_module, state);
            }
            TypedExpr::Index { object, index, .. } => {
                self.observe_projection_base_expr(object, current_module, state);
                self.observe_expr(index, current_module, state);
            }
            TypedExpr::Cast { expr, .. } | TypedExpr::SingletonCoerce { inner: expr, .. } => {
                self.observe_expr(expr, current_module, state);
            }
            TypedExpr::Match(m) => self.observe_match_expr(m, current_module, state),
            TypedExpr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => self.observe_if_expr(condition, then_branch, else_branch.as_ref(), current_module, state),
            TypedExpr::Loop { body, .. } => self.observe_loop_expr(body, current_module, state),
            TypedExpr::Closure {
                params,
                body,
                span,
                ..
            } => self.observe_closure_expr(params, body, span, current_module, state),
            TypedExpr::GenericClosure {
                name,
                params,
                body,
                span,
                ..
            } => {
                if let Some(name) = name {
                    self.observe_generic_closure_expr(name, params, body, span, current_module, state);
                } else {
                    self.record_skipped_generic_body(
                        span,
                        "anonymous generic closure has no type scheme lookup key",
                    );
                }
            }
            TypedExpr::Return(ret) => {
                if let Some(value) = &ret.value {
                    self.consume_expr_with_cause(value, current_module, state, MoveCause::Other);
                }
            }
            TypedExpr::Break(brk) => {
                if let Some(value) = &brk.value {
                    self.consume_expr(value, current_module, state);
                }
            }
        }
    }

    fn observe_call_expr(
        &mut self,
        callee: &TypedExpr,
        args: &[TypedExpr],
        current_module: &[String],
        state: &mut FlowState,
    ) {
        self.observe_expr(callee, current_module, state);
        self.observe_call_args(args, function_param_types(callee.ty()), current_module, state);
    }

    fn observe_method_call_expr(
        &mut self,
        receiver: &TypedExpr,
        method: &str,
        args: &[TypedExpr],
        dispatch: &MethodDispatch,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        self.consume_method_receiver(receiver, method, dispatch, current_module, state);
        let param_types = self.method_param_types(receiver.ty(), method, current_module, dispatch);
        self.observe_call_args(args, param_types.as_deref(), current_module, state);
    }

    fn observe_call_args(
        &mut self,
        args: &[TypedExpr],
        param_types: Option<&[Type]>,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        for (index, arg) in args.iter().enumerate() {
            let reborrow = param_types
                .and_then(|params| params.get(index))
                .is_some_and(|param_ty| is_reborrow(arg, param_ty));
            if reborrow {
                self.observe_expr(arg, current_module, state);
            } else {
                self.consume_expr(arg, current_module, state);
            }
        }
    }

    fn observe_match_expr(
        &mut self,
        m: &crate::typed_ast::TypedMatchExpr,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        self.observe_expr(&m.scrutinee, current_module, state);
        let mut joined = state.clone();
        joined.moved.clear();
        for arm in &m.arms {
            let mut arm_state = state.clone();
            arm_state.push_scope();
            self.apply_pattern_moves(&arm.pattern, &m.scrutinee, current_module, &mut arm_state);
            if let Some(guard) = &arm.guard {
                self.observe_expr(guard, current_module, &mut arm_state);
            }
            self.check_block(&arm.body, current_module, &mut arm_state);
            arm_state.pop_scope();
            joined.union_from(&arm_state);
        }
        state.union_from(&joined);
    }

    fn observe_if_expr(
        &mut self,
        condition: &TypedExpr,
        then_branch: &TypedBlock,
        else_branch: Option<&TypedBlock>,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        self.observe_expr(condition, current_module, state);
        let mut then_state = state.clone();
        self.check_block(then_branch, current_module, &mut then_state);
        let mut joined = state.clone();
        joined.union_from(&then_state);
        if let Some(else_branch) = else_branch {
            let mut else_state = state.clone();
            self.check_block(else_branch, current_module, &mut else_state);
            joined.union_from(&else_state);
        }
        state.union_from(&joined);
    }

    fn observe_loop_expr(
        &mut self,
        body: &TypedBlock,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        let mut body_state = state.clone();
        self.check_block(body, current_module, &mut body_state);
        state.union_from(&body_state);
    }

    fn observe_closure_expr(
        &mut self,
        params: &[crate::ast::Param],
        body: &TypedBlock,
        span: &Span,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        self.capture_closure(body, params.iter().map(|param| param.name.as_str()), span, state);
        let mut closure_state = FlowState::default();
        closure_state.push_scope();
        for captured in collect_free_roots_from_typed_block(body, &HashSet::new()) {
            closure_state.bind(&captured.name);
        }
        for param in params {
            closure_state.bind(&param.name);
        }
        self.check_block(body, current_module, &mut closure_state);
        closure_state.pop_scope();
    }

    fn observe_generic_closure_expr(
        &mut self,
        name: &str,
        params: &[crate::ast::Param],
        body: &crate::ast::Block,
        span: &Span,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        if let Some((typed_body, generic_env)) =
            self.construct_generic_body_for_move(name, &[], params, body, span)
        {
            self.capture_closure(&typed_body, params.iter().map(|param| param.name.as_str()), span, state);
            let mut closure_state = FlowState::default();
            closure_state.push_scope();
            for captured in collect_free_roots_from_typed_block(&typed_body, &HashSet::new()) {
                closure_state.bind(&captured.name);
            }
            for (param, ty) in params.iter().zip(&generic_env.arg_types) {
                closure_state.bind_typed(&param.name, ty);
            }
            self.generic_envs.push(generic_env);
            self.check_block(&typed_body, current_module, &mut closure_state);
            closure_state.pop_scope();
            self.generic_envs.pop();
        }
    }

    fn consume_expr(&mut self, expr: &TypedExpr, current_module: &[String], state: &mut FlowState) {
        self.consume_expr_with_cause(expr, current_module, state, MoveCause::Other);
    }

    fn consume_expr_with_cause(
        &mut self,
        expr: &TypedExpr,
        current_module: &[String],
        state: &mut FlowState,
        cause: MoveCause,
    ) {
        if let Some(place) = place_from_expr(expr) {
            let root_ty = state
                .binding_type(place.root())
                .cloned()
                .or_else(|| root_place_ty_from_expr(expr).cloned())
                .unwrap_or_else(|| expr.ty().clone());
            match expr {
                TypedExpr::Index { index, .. } => {
                    self.consume_place(&place, &root_ty, expr.span(), current_module, state, cause);
                    self.observe_expr(index, current_module, state);
                }
                TypedExpr::FieldAccess { object, .. } | TypedExpr::TupleAccess { object, .. } => {
                    self.consume_place(&place, &root_ty, expr.span(), current_module, state, cause);
                    self.observe_projection_base_expr(object, current_module, state);
                }
                _ => {
                    self.consume_place(&place, &root_ty, expr.span(), current_module, state, cause);
                }
            }
            return;
        }
        // A tuple or array literal takes ownership of its elements, so building
        // one *consumes* them. `observe_expr`'s arm only reads them, which let a
        // banned move slip past every guard `consume_place` applies —
        // `(h.name, 1)` out of a `Drop` type, `(xs[0], 1)` out of an array —
        // and also lost the move itself, so the element stayed usable
        // afterwards. Record and struct literals already consumed their fields;
        // these two were the outliers.
        if let TypedExpr::Tuple(items, ..) | TypedExpr::Array(items, ..) = expr {
            for item in items {
                self.consume_expr_with_cause(item, current_module, state, cause);
            }
            return;
        }
        self.observe_expr(expr, current_module, state);
    }

    fn observe_typed_place(
        &mut self,
        typed_place: &TypedPlace,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        if let Some(place) = from_typed_place(typed_place) {
            let span = typed_place_span(typed_place);
            self.record_whole_use_if_moved(&place, span, state);
        }
        match typed_place {
            TypedPlace::Ident(_, _) => {}
            TypedPlace::Deref { object, .. } => self.observe_expr(object, current_module, state),
            TypedPlace::Field { object, .. } | TypedPlace::Tuple { object, .. } => {
                self.observe_projection_base_typed_place(object, current_module, state);
            }
            TypedPlace::Index { object, index, .. } => {
                self.observe_projection_base_typed_place(object, current_module, state);
                self.observe_expr(index, current_module, state);
            }
        }
    }

    fn observe_projection_base_expr(
        &mut self,
        expr: &TypedExpr,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        if let Some(place) = place_from_expr(expr) {
            if self.record_descendant_use_if_moved(&place, expr.span(), state) {
                return;
            }
            match expr {
                TypedExpr::FieldAccess { object, .. } | TypedExpr::TupleAccess { object, .. } => {
                    self.observe_projection_base_expr(object, current_module, state);
                }
                TypedExpr::Index { object, index, .. } => {
                    self.observe_projection_base_expr(object, current_module, state);
                    self.observe_expr(index, current_module, state);
                }
                _ => {}
            }
            return;
        }
        self.observe_expr(expr, current_module, state);
    }

    fn observe_projection_base_typed_place(
        &mut self,
        typed_place: &TypedPlace,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        if let Some(place) = from_typed_place(typed_place) {
            if self.record_descendant_use_if_moved(&place, typed_place_span(typed_place), state) {
                return;
            }
        }
        match typed_place {
            TypedPlace::Ident(_, _) => {}
            TypedPlace::Deref { object, .. } => self.observe_expr(object, current_module, state),
            TypedPlace::Field { object, .. } | TypedPlace::Tuple { object, .. } => {
                self.observe_projection_base_typed_place(object, current_module, state);
            }
            TypedPlace::Index { object, index, .. } => {
                self.observe_projection_base_typed_place(object, current_module, state);
                self.observe_expr(index, current_module, state);
            }
        }
    }

    fn consume_method_receiver(
        &mut self,
        receiver: &TypedExpr,
        method: &str,
        dispatch: &MethodDispatch,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        let receiver_kind = self.method_receiver_kind(receiver.ty(), method, current_module, dispatch);
        match receiver_kind {
            Some(ReceiverKind::Value) => {
                self.consume_expr_with_cause(receiver, current_module, state, MoveCause::ByValueReceiver);
            }
            Some(ReceiverKind::Ref | ReceiverKind::RefMut) | None => {
                self.observe_expr(receiver, current_module, state);
            }
        }
    }

    fn apply_pattern_moves(
        &mut self,
        pattern: &Pattern,
        scrutinee: &TypedExpr,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        if let Some(place) = place_from_expr(scrutinee) {
            // The *root's* type, not the scrutinee's. `illegal_move_kind` walks
            // the projection chain from the root looking for a `Drop` ancestor,
            // so handing it `h.name`'s type for the place `h.name` starts the
            // walk one level too deep and never sees `Handle`. The array-element
            // rule is purely syntactic on projections and so survived that,
            // which is why only the `Drop` half was reachable through a pattern.
            let root_ty = root_place_ty_from_expr(scrutinee).unwrap_or_else(|| scrutinee.ty());
            self.apply_pattern_place_move(
                pattern,
                &place,
                root_ty,
                scrutinee.span(),
                current_module,
                state,
            );
        } else {
            Self::observe_pattern_bindings(pattern, state);
        }
    }

    fn apply_pattern_place_move(
        &mut self,
        pattern: &Pattern,
        place: &Place,
        root_ty: &Type,
        use_span: &Span,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        match pattern {
            Pattern::Wildcard(_) | Pattern::Literal(_, _) => {}
            Pattern::Binding(name, _) => {
                self.consume_place(
                    place,
                    root_ty,
                    use_span,
                    current_module,
                    state,
                    MoveCause::Other,
                );
                state.bind(name);
            }
            Pattern::Tuple(items, _) => {
                for (index, item) in items.iter().enumerate() {
                    let child = place.clone().with_projection(Projection::TupleIndex(index));
                    self.apply_pattern_place_move(item, &child, root_ty, use_span, current_module, state);
                }
            }
            Pattern::Record { fields, .. } => {
                for field in fields {
                    let child = place.clone().with_projection(Projection::Field(field.clone()));
                    self.consume_place(
                        &child,
                        root_ty,
                        use_span,
                        current_module,
                        state,
                        MoveCause::Other,
                    );
                    state.bind(field);
                }
            }
            Pattern::EnumVariant { fields, .. } => {
                if !fields.is_empty() {
                    self.consume_place(
                        place,
                        root_ty,
                        use_span,
                        current_module,
                        state,
                        MoveCause::Other,
                    );
                    for field in fields {
                        state.bind(field);
                    }
                }
            }
            Pattern::Array { elems, rest, .. } => {
                for item in elems {
                    let child = place.clone().with_projection(Projection::OpaqueIndex);
                    self.apply_pattern_place_move(item, &child, root_ty, use_span, current_module, state);
                }
                if let Some(rest) = rest {
                    state.bind(rest);
                }
            }
        }
    }

    fn observe_pattern_bindings(pattern: &Pattern, state: &mut FlowState) {
        match pattern {
            Pattern::Binding(name, _) => state.bind(name),
            Pattern::Tuple(items, _) => {
                for item in items {
                    Self::observe_pattern_bindings(item, state);
                }
            }
            Pattern::Record { fields, .. } | Pattern::EnumVariant { fields, .. } => {
                for field in fields {
                    state.bind(field);
                }
            }
            Pattern::Array { elems, rest, .. } => {
                for item in elems {
                    Self::observe_pattern_bindings(item, state);
                }
                if let Some(rest) = rest {
                    state.bind(rest);
                }
            }
            Pattern::Wildcard(_) | Pattern::Literal(_, _) => {}
        }
    }

    fn capture_closure<'names>(
        &mut self,
        body: &TypedBlock,
        params: impl Iterator<Item = &'names str>,
        span: &Span,
        state: &mut FlowState,
    ) {
        let mut locals: HashSet<String> = params.map(ToOwned::to_owned).collect();
        let captures = collect_free_roots_from_typed_block(body, &locals);
        for capture in captures {
            let CapturedRoot { name, ty } = capture;
            locals.insert(name.clone());
            let capture_place = Place::new(name.clone());
            let Some(capture_ty) = ty.or_else(|| state.binding_type(&name).cloned()) else {
                continue;
            };
            self.consume_place(
                &capture_place,
                &capture_ty,
                span,
                &[],
                state,
                MoveCause::Other,
            );
        }
    }

    fn consume_place(
        &mut self,
        place: &Place,
        root_ty: &Type,
        use_span: &Span,
        current_module: &[String],
        state: &mut FlowState,
        cause: MoveCause,
    ) {
        let place_ty = self
            .type_of_place(root_ty, place, current_module)
            .unwrap_or_else(|| root_ty.clone());
        if state.is_borrowed_array_element(place) && !self.is_copy(current_module, &place_ty) {
            self.report_illegal_move(
                place,
                use_span.clone(),
                type_bucket(&place_ty),
                MoveViolationKind::BorrowedArrayElementMove,
            );
            return;
        }
        if let Some(kind) = self.illegal_move_kind(place, root_ty, &place_ty, current_module) {
            self.report_illegal_move(place, use_span.clone(), type_bucket(&place_ty), kind);
            return;
        }
        self.check_place_use_before_move(place, use_span, state);
        self.record_move_if_needed(place, &place_ty, use_span, current_module, state, cause);
    }

    fn check_place_use_before_move(&mut self, place: &Place, use_span: &Span, state: &FlowState) {
        self.record_whole_use_if_moved(place, use_span, state);
    }

    fn record_move_if_needed(
        &self,
        place: &Place,
        ty: &Type,
        use_span: &Span,
        current_module: &[String],
        state: &mut FlowState,
        cause: MoveCause,
    ) {
        if self.is_copy(current_module, ty) {
            return;
        }
        state.record_move(place.clone(), use_span.clone(), cause, type_bucket(ty));
    }

    fn record_descendant_use_if_moved(
        &mut self,
        place: &Place,
        use_span: &Span,
        state: &FlowState,
    ) -> bool {
        if let Some(record) = state.moved_record_for_descendant_use(place) {
            self.report.violations.push(MoveViolation {
                binding: place.root().to_string(),
                use_place: place.clone(),
                moved_place: record.place.clone(),
                kind: Self::violation_kind(place, record),
                moved_by_value_receiver: record.cause == MoveCause::ByValueReceiver,
                moved_type: record.moved_type.clone(),
                use_span: use_span.clone(),
                moved_span: record.moved_span.clone(),
            });
            return true;
        }
        false
    }

    fn record_whole_use_if_moved(&mut self, place: &Place, use_span: &Span, state: &FlowState) {
        if let Some(record) = state.moved_record_for_whole_use(place) {
            self.report.violations.push(MoveViolation {
                binding: place.root().to_string(),
                use_place: place.clone(),
                moved_place: record.place.clone(),
                kind: Self::violation_kind(place, record),
                moved_by_value_receiver: record.cause == MoveCause::ByValueReceiver,
                moved_type: record.moved_type.clone(),
                use_span: use_span.clone(),
                moved_span: record.moved_span.clone(),
            });
        }
    }

    fn record_skipped_generic_body(&mut self, span: &Span, reason: impl Into<String>) {
        if is_embedded_std_span(span) {
            self.report.skipped_generic_bodies_embedded_std += 1;
        } else {
            self.report.skipped_generic_bodies_user += 1;
            self.report.unchecked_generic_bodies.push(UncheckedGenericBody {
                span: span.clone(),
                reason: reason.into(),
            });
        }
    }

    fn is_copy(&self, current_module: &[String], ty: &Type) -> bool {
        matches!(peel_type_references(ty), Type::Fun(_, _))
            || self.type_satisfies_aspect(current_module, ty, "Copy")
    }

    fn is_drop(&self, current_module: &[String], ty: &Type) -> bool {
        self.type_satisfies_aspect(current_module, ty, "Drop")
    }

    fn type_satisfies_aspect(&self, current_module: &[String], ty: &Type, aspect_name: &str) -> bool {
        let Some(generic_env) = self.generic_envs.last() else {
            return self
                .registry
                .type_satisfies_aspect(current_module, ty, aspect_name);
        };
        if let Type::Named(name, args) = peel_type_references(ty) {
            if args.is_empty()
                && generic_env
                    .symbolic_aspects
                    .get(name)
                    .is_some_and(|aspects| aspects.contains(aspect_name))
            {
                return true;
            }
        }
        let infer_ty = type_to_infer_under_generic_env(ty, &generic_env.placeholders);
        self.registry.infer_type_satisfies_aspect(
            current_module,
            &infer_ty,
            aspect_name,
            &generic_env.assumptions,
        )
    }

    fn illegal_move_kind(
        &self,
        place: &Place,
        root_ty: &Type,
        place_ty: &Type,
        current_module: &[String],
    ) -> Option<MoveViolationKind> {
        if self.is_copy(current_module, place_ty) {
            return None;
        }
        if place
            .projections()
            .iter()
            .any(|projection| matches!(projection, Projection::OpaqueIndex))
        {
            return Some(MoveViolationKind::ArrayElementMove);
        }

        let mut prefix_ty = root_ty.clone();
        for projection in place.projections() {
            if self.is_drop(current_module, &prefix_ty) {
                return Some(MoveViolationKind::PartialMoveOfDropType);
            }
            prefix_ty = self.project_type(&prefix_ty, projection, current_module)?;
        }
        None
    }

    fn reinitialize_assigned_place(typed_place: &TypedPlace, state: &mut FlowState) {
        if let TypedPlace::Ident(name, _) = typed_place {
            state.moved.remove(name);
        }
    }

    fn type_of_place(
        &self,
        root_ty: &Type,
        place: &Place,
        current_module: &[String],
    ) -> Option<Type> {
        let mut ty = root_ty.clone();
        for projection in place.projections() {
            ty = self.project_type(&ty, projection, current_module)?;
        }
        Some(ty)
    }

    fn project_type(
        &self,
        base_ty: &Type,
        projection: &Projection,
        current_module: &[String],
    ) -> Option<Type> {
        let peeled = peel_type_references(base_ty);
        match projection {
            Projection::TupleIndex(index) => match peeled {
                Type::Tuple(items) => items.get(*index).cloned(),
                _ => None,
            },
            Projection::OpaqueIndex => match peeled {
                Type::Array(item) | Type::SizedArray(item, _) => Some((**item).clone()),
                _ => None,
            },
            Projection::Field(field) => match peeled {
                Type::Record(fields) => fields
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, ty)| ty.clone()),
                Type::Named(name, args) => {
                    let (resolved_name, fields) =
                        self.registry.projection_struct_fields(current_module, name)?;
                    let field_entry = fields.iter().find(|entry| entry.name == *field)?;
                    let raw_ty = field_entry.ty.clone();
                    let infer_ty = if let Some(type_params) =
                        self.registry.struct_type_params_for(resolved_name)
                    {
                        let mut remap = Substitution::new();
                        for (&param, arg) in type_params.iter().zip(args.iter()) {
                            remap.bind(param, type_to_infer(arg));
                        }
                        remap.apply(&raw_ty)
                    } else {
                        raw_ty
                    };
                    infer_to_type(&infer_ty)
                }
                _ => None,
            },
        }
    }

    fn report_illegal_move(
        &mut self,
        place: &Place,
        use_span: Span,
        moved_type: String,
        kind: MoveViolationKind,
    ) {
        self.report.violations.push(MoveViolation {
            binding: place.root().to_string(),
            use_place: place.clone(),
            moved_place: place.clone(),
            kind,
            moved_by_value_receiver: false,
            moved_type,
            use_span,
            moved_span: dummy_span_from_place(place),
        });
    }

    fn violation_kind(place: &Place, record: &MoveRecord) -> MoveViolationKind {
        if record.moved_type == "&var T" && record.cause != MoveCause::ByValueReceiver {
            return MoveViolationKind::MovedMutReferenceWithoutReborrow;
        }
        if place.projections().is_empty() && !record.place.projections().is_empty() {
            return MoveViolationKind::PartialMoveUsedAsWhole;
        }
        MoveViolationKind::UseAfterMove
    }

    fn method_receiver_kind(
        &self,
        receiver_ty: &Type,
        method: &str,
        _current_module: &[String],
        _dispatch: &MethodDispatch,
    ) -> Option<ReceiverKind> {
        if let Some((_, method_def, _)) = self.symbolic_aspect_method(receiver_ty, method) {
            return method_def
                .params
                .first()
                .and_then(|param| param.receiver.clone());
        }
        match peel_type_references(receiver_ty) {
            Type::Array(_) => self.registry.array_method_receiver_kind(method).cloned(),
            Type::Named(name, _) => self.registry.method_receiver_kind(name, method).cloned(),
            other => primitive_type_name(other)
                .and_then(|name| self.registry.method_receiver_kind(&name, method).cloned()),
        }
    }

    fn method_param_types(
        &self,
        receiver_ty: &Type,
        method: &str,
        _current_module: &[String],
        _dispatch: &MethodDispatch,
    ) -> Option<Vec<Type>> {
        if let Some((aspect, method_def, placeholder)) =
            self.symbolic_aspect_method(receiver_ty, method)
        {
            return crate::typechecker::symbolic_aspect_method_type(
                self.registry,
                &aspect,
                &method_def,
                &placeholder,
            )
            .as_ref()
            .and_then(infer_method_arg_types);
        }
        match peel_type_references(receiver_ty) {
            Type::Array(_) => self
                .registry
                .array_method_type(method)
                .and_then(infer_method_arg_types),
            Type::Named(name, _) => self
                .registry
                .method_type(name, method)
                .and_then(infer_method_arg_types),
            other => primitive_type_name(other).and_then(|name| {
                self.registry
                    .method_type(&name, method)
                    .and_then(infer_method_arg_types)
            }),
        }
    }

    fn symbolic_aspect_method(
        &self,
        receiver_ty: &Type,
        method: &str,
    ) -> Option<(String, crate::ast::AspectMethod, String)> {
        let Type::Named(placeholder, args) = peel_type_references(receiver_ty) else {
            return None;
        };
        if !args.is_empty() {
            return None;
        }
        let generic_env = self.generic_envs.last()?;
        let aspects = generic_env.symbolic_aspects.get(placeholder)?;
        let mut matches = aspects.iter().filter_map(|aspect| {
            self.registry
                .aspect_method_defs(aspect)?
                .iter()
                .find(|method_def| method_def.name == method)
                .cloned()
                .map(|method_def| (aspect.clone(), method_def, placeholder.clone()))
        });
        let result = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(result)
    }
}

fn function_param_types(ty: &Type) -> Option<&[Type]> {
    match ty {
        Type::Fun(params, _) => Some(params),
        _ => None,
    }
}

fn is_embedded_std_span(span: &Span) -> bool {
    span.filename.starts_with("<embedded std::")
}

fn generic_placeholder_name(var: TypeVar) -> String {
    format!("__metel_move_check_generic_{}", var.0)
}

fn scheme_with_source_generics(
    scheme: &TypeScheme,
    generics: &[GenericParam],
) -> TypeScheme {
    let mut repaired = scheme.clone();
    let existing = repaired.quantified_vars.len();
    repaired.bounds.resize_with(existing, Vec::new);
    repaired.neg_bounds.resize_with(existing, Vec::new);
    repaired.record_kinds.resize(existing, false);
    repaired.assoc_projections.resize(existing, None);
    repaired.assoc_eq_constraints.resize_with(existing, Vec::new);
    repaired.opaque_returns.resize(existing, None);
    let mut replacements = HashMap::new();
    let mut gen = TypeVarGenerator::with_counter(4_000_000);
    for generic in generics {
        if repaired.param_names.contains(&generic.name) {
            continue;
        }
        let var = gen.fresh();
        replacements.insert(generic.name.clone(), InferType::Var(var));
        repaired.quantified_vars.push(var);
        repaired.param_names.push(generic.name.clone());
        repaired.bounds.push(
            generic
                .bounds
                .iter()
                .filter(|bound| bound.polarity == Polarity::Positive)
                .filter_map(GenericBound::from_ast)
                .collect(),
        );
        repaired.neg_bounds.push(
            generic
                .bounds
                .iter()
                .filter(|bound| bound.polarity == Polarity::Negative)
                .filter_map(GenericBound::from_ast)
                .collect(),
        );
        repaired.record_kinds.push(generic.is_record);
        repaired.assoc_projections.push(None);
        repaired.assoc_eq_constraints.push(Vec::new());
        repaired.opaque_returns.push(None);
    }
    repaired.ty = substitute_named_generics(&repaired.ty, &replacements);
    repaired
}

fn type_ctx_with_symbolic_aspect_methods(
    type_ctx: &TypeCtx,
    generic_env: &GenericMoveEnv,
) -> TypeCtx {
    let mut enriched = type_ctx.clone();
    let mut method_gen = TypeVarGenerator::with_counter(2_000_000);
    for (placeholder, aspects) in &generic_env.symbolic_aspects {
        enriched
            .registry
            .register_symbolic_named_aspects(placeholder.clone(), aspects.clone());
        let mut methods: HashMap<String, Vec<(String, crate::ast::AspectMethod)>> =
            HashMap::new();
        for aspect in aspects {
            let Some(method_defs) = enriched.registry.aspect_method_defs(aspect) else {
                continue;
            };
            for method in method_defs {
                methods
                    .entry(method.name.clone())
                    .or_default()
                    .push((aspect.clone(), method.clone()));
            }
        }
        for candidates in methods.into_values() {
            let [(aspect, method)] = candidates.as_slice() else {
                continue;
            };
            let Some(method_scheme) = crate::typechecker::symbolic_aspect_method_scheme(
                &enriched.registry,
                aspect,
                method,
                placeholder,
                &mut method_gen,
            ) else {
                continue;
            };
            enriched.registry.register_method_scheme(
                placeholder.clone(),
                method.name.clone(),
                method_scheme.clone(),
                Vec::new(),
            );
            enriched.registry.register_method_scheme_variant(
                placeholder.clone(),
                method.name.clone(),
                method_scheme,
                Vec::new(),
                Some(aspect.clone()),
                method.span.clone(),
            );
            if let Some(receiver) = method
                .params
                .first()
                .and_then(|param| param.receiver.clone())
            {
                enriched.registry.register_method_receiver(
                    placeholder.clone(),
                    method.name.clone(),
                    receiver,
                );
            }
        }
    }
    enriched
}

fn symbolic_aspect_assumptions(
    registry: &TypeDefinitionRegistry,
    placeholders: &HashMap<String, TypeVar>,
    assumptions: &AspectAssumptions,
) -> HashMap<String, HashSet<String>> {
    let mut symbolic_aspects: HashMap<String, HashSet<String>> = placeholders
        .iter()
        .filter_map(|(placeholder, var)| {
            assumptions
                .get(var)
                .cloned()
                .map(|aspects| (placeholder.clone(), aspects))
        })
        .collect();
    let mut pending: Vec<String> = symbolic_aspects.keys().cloned().collect();
    let mut next = 0;

    while let Some(symbolic_name) = pending.get(next).cloned() {
        next += 1;
        let aspects = symbolic_aspects
            .get(&symbolic_name)
            .cloned()
            .unwrap_or_default();
        for aspect in aspects {
            let Some(assoc_decls) = registry.aspect_assoc_type_decls(&aspect) else {
                continue;
            };
            for assoc_decl in assoc_decls {
                let assoc_aspects: HashSet<String> = assoc_decl
                    .bounds
                    .iter()
                    .filter(|bound| bound.polarity == Polarity::Positive)
                    .filter_map(GenericBound::from_ast)
                    .filter_map(|bound| bound.aspect_name().map(ToOwned::to_owned))
                    .collect();
                if assoc_aspects.is_empty() {
                    continue;
                }
                let projection = format!("{symbolic_name}::{}", assoc_decl.name);
                let entry = symbolic_aspects.entry(projection.clone()).or_default();
                let previous_len = entry.len();
                entry.extend(assoc_aspects);
                if entry.len() != previous_len {
                    pending.push(projection);
                }
            }
        }
    }

    symbolic_aspects
}

fn symbolic_method_ambiguity_reason(
    error: &MetelError,
    generic_env: &GenericMoveEnv,
    registry: &TypeDefinitionRegistry,
) -> Option<String> {
    let MetelError::Internal { message, .. } = error else {
        return None;
    };
    for (symbolic_name, aspects) in &generic_env.symbolic_aspects {
        let prefix = "no method `";
        let suffix = format!("` on `{symbolic_name}`");
        let Some(method) = message
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(&suffix))
        else {
            continue;
        };
        let mut candidates: Vec<String> = aspects
            .iter()
            .filter(|aspect| {
                registry
                    .aspect_method_defs(aspect)
                    .is_some_and(|methods| methods.iter().any(|candidate| candidate.name == method))
            })
            .cloned()
            .collect();
        if candidates.len() < 2 {
            continue;
        }
        candidates.sort();
        return Some(format!(
            "ambiguous aspect method `{method}` on symbolic type `{symbolic_name}`; candidates: {}",
            candidates.join(", ")
        ));
    }
    None
}

/// The declared parameter types of a method, excluding the receiver.
///
/// Returns `None` if any parameter cannot be resolved, rather than omitting it (#337).
/// The result is consumed positionally by `observe_call_args` (`params.get(index)` against
/// `args.iter().enumerate()`), so dropping one parameter would judge every later argument
/// against the wrong type — and that judgement is borrow-vs-move. A short list is worse
/// than no list: with `None`, `observe_call_args` recognises no reborrows and consumes
/// every argument, which can only over-report moves, never miss one. A shifted list can do
/// either.
fn infer_method_arg_types(fun_ty: &crate::typeinference::InferType) -> Option<Vec<Type>> {
    match fun_ty {
        crate::typeinference::InferType::Fun(params, _) => params
            .iter()
            .skip(1)
            .map(infer_to_type)
            .collect::<Option<Vec<_>>>(),
        _ => None,
    }
}

fn substitute_named_generics(
    ty: &InferType,
    named_samples: &HashMap<String, InferType>,
) -> InferType {
    match ty {
        InferType::Named(name, args) if args.is_empty() => named_samples
            .get(name)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        InferType::Named(name, args) => InferType::Named(
            name.clone(),
            args.iter()
                .map(|arg| substitute_named_generics(arg, named_samples))
                .collect(),
        ),
        InferType::Fun(params, ret) => InferType::Fun(
            params
                .iter()
                .map(|param| substitute_named_generics(param, named_samples))
                .collect(),
            Box::new(substitute_named_generics(ret, named_samples)),
        ),
        InferType::Tuple(items) => InferType::Tuple(
            items
                .iter()
                .map(|item| substitute_named_generics(item, named_samples))
                .collect(),
        ),
        InferType::Record(fields) => InferType::Record(
            fields
                .iter()
                .map(|(name, field_ty)| {
                    (
                        name.clone(),
                        substitute_named_generics(field_ty, named_samples),
                    )
                })
                .collect(),
        ),
        InferType::Array(item) => InferType::Array(Box::new(substitute_named_generics(
            item,
            named_samples,
        ))),
        InferType::SizedArray(item, len) => InferType::SizedArray(
            Box::new(substitute_named_generics(item, named_samples)),
            *len,
        ),
        InferType::Reference(inner) => InferType::Reference(Box::new(
            substitute_named_generics(inner, named_samples),
        )),
        InferType::MutReference(inner) => InferType::MutReference(Box::new(
            substitute_named_generics(inner, named_samples),
        )),
        InferType::Concrete(_) | InferType::Var(_) | InferType::Never => ty.clone(),
    }
}

fn type_to_infer_under_generic_env(ty: &Type, placeholders: &HashMap<String, TypeVar>) -> InferType {
    match ty {
        Type::Boolean
        | Type::Str
        | Type::Char
        | Type::Unit
        | Type::Never
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::F32
        | Type::F64 => type_to_infer(ty),
        Type::Tuple(items) => InferType::Tuple(
            items
                .iter()
                .map(|item| type_to_infer_under_generic_env(item, placeholders))
                .collect(),
        ),
        Type::Record(fields) => InferType::Record(
            fields
                .iter()
                .map(|(name, field_ty)| {
                    (
                        name.clone(),
                        type_to_infer_under_generic_env(field_ty, placeholders),
                    )
                })
                .collect(),
        ),
        Type::Array(inner) => InferType::Array(Box::new(type_to_infer_under_generic_env(
            inner,
            placeholders,
        ))),
        Type::SizedArray(inner, len) => InferType::SizedArray(
            Box::new(type_to_infer_under_generic_env(inner, placeholders)),
            *len,
        ),
        Type::Reference(inner) => InferType::Reference(Box::new(type_to_infer_under_generic_env(
            inner,
            placeholders,
        ))),
        Type::MutReference(inner) => InferType::MutReference(Box::new(type_to_infer_under_generic_env(
            inner,
            placeholders,
        ))),
        Type::Fun(params, ret) => InferType::Fun(
            params
                .iter()
                .map(|param| type_to_infer_under_generic_env(param, placeholders))
                .collect(),
            Box::new(type_to_infer_under_generic_env(ret, placeholders)),
        ),
        Type::Named(name, args) => {
            if args.is_empty() {
                if let Some(var) = placeholders.get(name) {
                    return InferType::Var(*var);
                }
            }
            InferType::Named(
                name.clone(),
                args.iter()
                    .map(|arg| type_to_infer_under_generic_env(arg, placeholders))
                    .collect(),
            )
        }
    }
}

/// Convert an `InferType` to a `Type`, or `None` if any part of it is still an unresolved
/// inference variable.
///
/// **All-or-nothing by construction (#337).** Every compound arm collects into
/// `Option<Vec<_>>` rather than filtering, so a failure anywhere inside propagates out as
/// `None`. Filtering would silently change arity — a tuple `(T, i64)` with an unresolved
/// `T` would become the 1-tuple `(i64)`, a record would lose a field, and a `fun`'s
/// parameter list would shift. That last one is not cosmetic: `observe_call_args` pairs a
/// parameter list with arguments *positionally*, and the only thing it decides from a
/// parameter type is borrow-vs-move, so a shifted list silently converts a reborrow into a
/// move or vice versa.
///
/// The two callers handle `None` differently, and neither is harmed by it:
///
/// - `generic_sample_args` propagates it, and the generic-body path records a skip with a
///   reason — this module's convention for "could not analyse".
/// - `infer_method_arg_types` propagates it to `observe_call_args`, which records *no*
///   diagnostic and simply proceeds without reborrow information, consuming each argument.
///   That is more conservative than a shifted list, not less.
fn infer_to_type(ty: &crate::typeinference::InferType) -> Option<Type> {
    use crate::typeinference::InferType;
    match ty {
        InferType::Concrete(inner) => Some(inner.clone()),
        InferType::Never => Some(Type::Never),
        InferType::Tuple(items) => Some(Type::Tuple(
            items
                .iter()
                .map(infer_to_type)
                .collect::<Option<Vec<_>>>()?,
        )),
        InferType::Record(fields) => Some(Type::Record(
            fields
                .iter()
                .map(|(name, ty)| infer_to_type(ty).map(|ty| (name.clone(), ty)))
                .collect::<Option<Vec<_>>>()?,
        )),
        InferType::Array(inner) => infer_to_type(inner).map(|inner| Type::Array(Box::new(inner))),
        InferType::SizedArray(inner, len) => {
            infer_to_type(inner).map(|inner| Type::SizedArray(Box::new(inner), *len))
        }
        InferType::Reference(inner) => {
            infer_to_type(inner).map(|inner| Type::Reference(Box::new(inner)))
        }
        InferType::MutReference(inner) => {
            infer_to_type(inner).map(|inner| Type::MutReference(Box::new(inner)))
        }
        InferType::Fun(params, ret) => Some(Type::Fun(
            params
                .iter()
                .map(infer_to_type)
                .collect::<Option<Vec<_>>>()?,
            Box::new(infer_to_type(ret)?),
        )),
        InferType::Named(name, args) => Some(Type::Named(
            name.clone(),
            args.iter()
                .map(infer_to_type)
                .collect::<Option<Vec<_>>>()?,
        )),
        InferType::Var(_) => None,
    }
}

fn is_reborrow(expr: &TypedExpr, param_ty: &Type) -> bool {
    matches!(expr.ty(), Type::MutReference(_)) && matches!(param_ty, Type::MutReference(_))
}

fn peel_type_references(ty: &Type) -> &Type {
    match ty {
        Type::Reference(inner) | Type::MutReference(inner) => peel_type_references(inner),
        _ => ty,
    }
}

fn primitive_type_name(ty: &Type) -> Option<String> {
    match ty {
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
        Type::Boolean => Some("boolean".to_string()),
        Type::Char => Some("Char".to_string()),
        Type::Str => Some("String".to_string()),
        _ => None,
    }
}

fn collect_free_roots_from_typed_block(
    block: &TypedBlock,
    initial_locals: &HashSet<String>,
) -> Vec<CapturedRoot> {
    let mut collector = FreeRootCollector {
        scope_stack: vec![initial_locals.clone()],
        captures: Vec::new(),
        seen: HashSet::new(),
    };
    collector.block(block);
    collector.captures
}

#[derive(Debug, Clone)]
struct CapturedRoot {
    name: String,
    ty: Option<Type>,
}

struct FreeRootCollector {
    scope_stack: Vec<HashSet<String>>,
    captures: Vec<CapturedRoot>,
    seen: HashSet<String>,
}

impl FreeRootCollector {
    fn block(&mut self, block: &TypedBlock) {
        self.scope_stack.push(HashSet::new());
        for decl in &block.stmts {
            self.decl(decl);
        }
        if let Some(tail) = &block.tail {
            self.expr(tail);
        }
        self.scope_stack.pop();
    }

    fn decl(&mut self, decl: &TypedDecl) {
        match decl {
            TypedDecl::Let(let_decl) => {
                self.expr(&let_decl.value);
                self.bind(&let_decl.name);
            }
            TypedDecl::Mut(mut_decl) => {
                self.expr(&mut_decl.value);
                self.bind(&mut_decl.name);
            }
            TypedDecl::Fun(fun) => {
                self.bind(&fun.name);
            }
            TypedDecl::Stmt(stmt) => self.stmt(stmt),
            TypedDecl::Impl(_) | TypedDecl::Struct(_) | TypedDecl::Enum(_) | TypedDecl::Aspect(_) => {}
        }
    }

    fn stmt(&mut self, stmt: &TypedStmt) {
        match stmt {
            TypedStmt::Expr(expr) => self.expr(expr),
            TypedStmt::While(while_stmt) => {
                self.expr(&while_stmt.condition);
                self.block(&while_stmt.body);
            }
            TypedStmt::For(for_stmt) => {
                self.scope_stack.push(HashSet::new());
                if let Some(init) = &for_stmt.init {
                    match init {
                        TypedForInit::Let(let_decl) => {
                            self.expr(&let_decl.value);
                            self.bind(&let_decl.name);
                        }
                        TypedForInit::Mut(mut_decl) => {
                            self.expr(&mut_decl.value);
                            self.bind(&mut_decl.name);
                        }
                        TypedForInit::Expr(expr) => self.expr(expr),
                    }
                }
                if let Some(condition) = &for_stmt.condition {
                    self.expr(condition);
                }
                if let Some(step) = &for_stmt.step {
                    self.expr(step);
                }
                self.block(&for_stmt.body);
                self.scope_stack.pop();
            }
            TypedStmt::ForIn(for_in) => {
                self.expr(&for_in.iterable);
                self.scope_stack.push(HashSet::new());
                self.bind(&for_in.binding);
                self.block(&for_in.body);
                self.scope_stack.pop();
            }
        }
    }

    fn expr(&mut self, expr: &TypedExpr) {
        match expr {
            TypedExpr::Ident(name, ty, _) => self.capture_if_free(name, ty),
            TypedExpr::Tuple(items, ..) | TypedExpr::Array(items, ..) => {
                for item in items {
                    self.expr(item);
                }
            }
            TypedExpr::RecordLiteral { fields, .. } | TypedExpr::StructLiteral { fields, .. } => {
                for (_, value) in fields {
                    self.expr(value);
                }
            }
            TypedExpr::RepeatArray(value, ..)
            | TypedExpr::UnaryOp(_, value, ..)
            | TypedExpr::Cast { expr: value, .. }
            | TypedExpr::SingletonCoerce { inner: value, .. }
            | TypedExpr::RefTemp { init: value, .. } => self.expr(value),
            TypedExpr::BinOp(left, _, right, ..) => {
                self.expr(left);
                self.expr(right);
            }
            TypedExpr::Assign { target, value, .. } => {
                self.place(target);
                self.expr(value);
            }
            TypedExpr::Call { callee, args, .. } => {
                self.expr(callee);
                for arg in args {
                    self.expr(arg);
                }
            }
            TypedExpr::MethodCall { receiver, args, .. } => {
                self.expr(receiver);
                for arg in args {
                    self.expr(arg);
                }
            }
            TypedExpr::FieldAccess { object, .. } | TypedExpr::TupleAccess { object, .. } => {
                self.expr(object);
            }
            TypedExpr::Index { object, index, .. } => {
                self.expr(object);
                self.expr(index);
            }
            TypedExpr::Match(m) => {
                self.expr(&m.scrutinee);
                for arm in &m.arms {
                    self.scope_stack.push(HashSet::new());
                    bind_pattern_names(&arm.pattern, self.scope_stack.last_mut().expect("scope exists"));
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.block(&arm.body);
                    self.scope_stack.pop();
                }
            }
            TypedExpr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                self.expr(condition);
                self.block(then_branch);
                if let Some(else_branch) = else_branch {
                    self.block(else_branch);
                }
            }
            TypedExpr::Loop { body, .. } => self.block(body),
            TypedExpr::Closure { params, body, .. } => {
                self.scope_stack.push(HashSet::new());
                for param in params {
                    self.bind(&param.name);
                }
                self.block(body);
                self.scope_stack.pop();
            }
            TypedExpr::GenericClosure { params, .. } => {
                self.scope_stack.push(HashSet::new());
                for param in params {
                    self.bind(&param.name);
                }
                self.scope_stack.pop();
            }
            TypedExpr::Return(ret) => {
                if let Some(value) = &ret.value {
                    self.expr(value);
                }
            }
            TypedExpr::Break(brk) => {
                if let Some(value) = &brk.value {
                    self.expr(value);
                }
            }
            TypedExpr::Literal(..) | TypedExpr::Path(..) | TypedExpr::Continue(_) => {}
        }
    }

    fn place(&mut self, place: &TypedPlace) {
        match place {
            TypedPlace::Ident(name, _) => self.capture_free_name(name),
            TypedPlace::Deref { object, .. } => self.expr(object),
            TypedPlace::Field { object, .. } | TypedPlace::Tuple { object, .. } => self.place(object),
            TypedPlace::Index { object, index, .. } => {
                self.place(object);
                self.expr(index);
            }
        }
    }

    fn bind(&mut self, name: &str) {
        self.scope_stack
            .last_mut()
            .expect("scope exists")
            .insert(name.to_string());
    }

    fn capture_if_free(&mut self, name: &str, ty: &Type) {
        if self
            .scope_stack
            .iter()
            .rev()
            .any(|scope| scope.contains(name))
        {
            return;
        }
        if self.seen.insert(name.to_string()) {
            self.captures.push(CapturedRoot {
                name: name.to_string(),
                ty: Some(ty.clone()),
            });
        }
    }

    fn capture_free_name(&mut self, name: &str) {
        if self
            .scope_stack
            .iter()
            .rev()
            .any(|scope| scope.contains(name))
        {
            return;
        }
        if self.seen.insert(name.to_string()) {
            self.captures.push(CapturedRoot {
                name: name.to_string(),
                ty: None,
            });
        }
    }
}

fn violation_message(violation: &MoveViolation) -> String {
    match violation.kind {
        MoveViolationKind::UseAfterMove => format!(
            "use of moved value `{}`: `{}` was moved at {}",
            violation.binding,
            format_place(&violation.moved_place),
            format_span(&violation.moved_span)
        ),
        MoveViolationKind::PartialMoveUsedAsWhole => format!(
            "use of partially moved value `{}`: field or element `{}` was moved at {}",
            violation.binding,
            format_place(&violation.moved_place),
            format_span(&violation.moved_span)
        ),
        // No trailing location for the outright-banned rules: the offending
        // expression *is* the diagnostic's own span, so citing it again only
        // repeats the location the reader is already looking at.
        MoveViolationKind::PartialMoveOfDropType => format!(
            "cannot partially move value `{}`: `{}` belongs to a `Drop` type",
            violation.binding,
            format_place(&violation.use_place),
        ),
        MoveViolationKind::ArrayElementMove => format!(
            "cannot move from `{}`: array element moves are not allowed",
            format_place(&violation.use_place),
        ),
        MoveViolationKind::BorrowedArrayElementMove => format!(
            "cannot move `{}`: it is borrowed from a `T[]` view",
            format_place(&violation.use_place),
        ),
        MoveViolationKind::MovedMutReferenceWithoutReborrow => format!(
            "use of moved `&var` binding `{}`: `{}` was moved by a non-reborrow use at {}",
            violation.binding,
            violation.binding,
            format_span(&violation.moved_span)
        ),
    }
}

fn format_place(place: &Place) -> String {
    let mut rendered = place.root().to_string();
    for projection in place.projections() {
        match projection {
            Projection::Field(field) => {
                rendered.push('.');
                rendered.push_str(field);
            }
            Projection::TupleIndex(index) => {
                rendered.push('.');
                rendered.push_str(&index.to_string());
            }
            Projection::OpaqueIndex => rendered.push_str("[_]"),
        }
    }
    rendered
}

fn format_span(span: &Span) -> String {
    format!("{}:{}:{}", span.filename, span.line, span.col)
}

fn bind_pattern_names(pattern: &Pattern, into: &mut HashSet<String>) {
    match pattern {
        Pattern::Binding(name, _) => {
            into.insert(name.clone());
        }
        Pattern::Tuple(items, _) => {
            for item in items {
                bind_pattern_names(item, into);
            }
        }
        Pattern::EnumVariant { fields, .. } | Pattern::Record { fields, .. } => {
            for field in fields {
                into.insert(field.clone());
            }
        }
        Pattern::Array { elems, rest, .. } => {
            for item in elems {
                bind_pattern_names(item, into);
            }
            if let Some(rest) = rest {
                into.insert(rest.clone());
            }
        }
        Pattern::Wildcard(_) | Pattern::Literal(_, _) => {}
    }
}

fn root_place_ty_from_expr(expr: &TypedExpr) -> Option<&Type> {
    match expr {
        TypedExpr::Ident(_, ty, _) => Some(ty),
        TypedExpr::FieldAccess { object, .. }
        | TypedExpr::TupleAccess { object, .. }
        | TypedExpr::Index { object, .. } => root_place_ty_from_expr(object),
        _ => None,
    }
}

fn dummy_span_from_place(place: &Place) -> Span {
    Span {
        start: 0,
        end: 0,
        filename: format!("<place:{}>", place.root()),
        line: 0,
        col: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{coherence, module_loader, name_resolver, path_normalizer, typechecker};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn move_violations_for_source(source: &str) -> Vec<MoveViolation> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "metel_move_check_{}_{n}.mtl",
            std::process::id()
        ));
        {
            let mut file = std::fs::File::create(&path).expect("create temp fixture");
            file.write_all(source.as_bytes())
                .expect("write temp fixture");
        }
        let violations = (|| {
            let graph = module_loader::load_root(&path).expect("load temp fixture");
            let names = name_resolver::resolve(&graph).expect("resolve temp fixture");
            let normalized = path_normalizer::normalize(graph, &names).expect("normalize temp fixture");
            coherence::check(&normalized, &names).expect("coherence temp fixture");
            let typed = typechecker::check_graph(&normalized, &names, &typechecker::CorePrelude::default())
                .expect("typecheck temp fixture");
            collect_graph_violations(&typed)
                .violations
                .into_iter()
                .filter(|violation| violation.use_span.filename == path.to_string_lossy())
                .collect()
        })();
        let _ = std::fs::remove_file(&path);
        violations
    }

    fn assert_has_violation(source: &str, binding: &str) -> Vec<MoveViolation> {
        let violations = move_violations_for_source(source);
        assert!(
            violations.iter().any(|violation| violation.binding == binding),
            "expected a move violation for `{binding}`, got {violations:#?}"
        );
        violations
    }

    fn assert_no_violations(source: &str) {
        let violations = move_violations_for_source(source);
        assert!(violations.is_empty(), "unexpected violations: {violations:#?}");
    }

    fn move_warnings_for_source(source: &str) -> Vec<String> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "metel_move_check_warning_{}_{n}.mtl",
            std::process::id()
        ));
        {
            let mut file = std::fs::File::create(&path).expect("create temp fixture");
            file.write_all(source.as_bytes())
                .expect("write temp fixture");
        }
        let warnings = (|| {
            let graph = module_loader::load_root(&path).expect("load temp fixture");
            let names = name_resolver::resolve(&graph).expect("resolve temp fixture");
            let normalized =
                path_normalizer::normalize(graph, &names).expect("normalize temp fixture");
            coherence::check(&normalized, &names).expect("coherence temp fixture");
            let typed =
                typechecker::check_graph(&normalized, &names, &typechecker::CorePrelude::default())
                    .expect("typecheck temp fixture");
            check_graph(&typed).expect("move-check temp fixture")
        })();
        let _ = std::fs::remove_file(&path);
        warnings
    }

    #[test]
    fn unchecked_generic_body_is_reported_to_compiler_callers() {
        let warnings = move_warnings_for_source(
            r#"
aspect FirstMarker {
    fun inspect(&self);
}

aspect SecondMarker {
    fun inspect(&self);
}

aspect Container {
    type Item: FirstMarker + SecondMarker;
    fun get(self) -> Item;
}

fun inspect<T: Container>(value: T) {
    let item = value.get();
    item.inspect();
}

fun main() { }
"#,
        );
        assert!(
            warnings
                .iter()
                .any(|warning| {
                    warning.contains("ambiguous aspect method `inspect`")
                        && warning.contains("FirstMarker, SecondMarker")
                }),
            "expected the reconstruction failure reason, got {warnings:#?}"
        );
    }

    #[test]
    fn bounded_generic_mut_receiver_and_argument_are_reborrowed() {
        let warnings = move_warnings_for_source(
            r#"
aspect Blend {
    fun blend(&var self, other: &var Self);
}

fun blend_twice<T: Blend>(value: T, other: T) {
    var value = value;
    var other = other;
    value.blend(&var other);
    value.blend(&var other);
}

fun main() { }
"#,
        );
        assert!(
            warnings.is_empty(),
            "bounded generic method body was not fully checked: {warnings:#?}"
        );
    }

    #[test]
    fn bounded_generic_method_generic_copy_bound_is_checked() {
        let warnings = move_warnings_for_source(
            r#"
aspect GenericSink {
    fun take<U: Copy>(&self, other: U);
}

fun take_copy<T: GenericSink, U: Copy>(value: T, other: U) -> U {
    value.take(other);
    other
}

fun main() { }
"#,
        );
        assert!(
            warnings.is_empty(),
            "bounded method-generic body was not fully checked: {warnings:#?}"
        );
    }

    #[test]
    fn unmet_method_generic_bound_is_reported_unchecked() {
        let warnings = move_warnings_for_source(
            r#"
aspect GenericSink {
    fun take<U: Copy>(&self, other: U);
}

fun take_unbounded<T: GenericSink, U>(value: T, other: U) {
    value.take(other);
}

fun main() { }
"#,
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("does not implement `Copy`")),
            "expected the reconstruction failure reason, got {warnings:#?}"
        );
    }

    #[test]
    fn assignment_move_then_use_is_reported() {
        assert_has_violation(
            r#"
fun main() {
    let a = "hello";
    let b = a;
    let c = a;
}
"#,
            "a",
        );
    }

    #[test]
    fn argument_move_then_use_is_reported() {
        assert_has_violation(
            r#"
fun take(s: String) { }

fun main() {
    let s = "hello";
    take(s);
    let again = s;
}
"#,
            "s",
        );
    }

    #[test]
    fn return_move_then_use_is_reported() {
        assert_has_violation(
            r#"
fun forward(s: String) -> String {
    return s;
}

fun main() {
    let s = "hello";
    let kept = forward(s);
    let again = s;
}
"#,
            "s",
        );
    }

    #[test]
    fn copy_type_can_be_used_twice() {
        assert_no_violations(
            r#"
fun main() {
    let n = 41;
    let a = n;
    let b = n;
}
"#,
        );
        assert_no_violations(
            r#"
fun main() {
    let n = 5;
    let f = () -> i64 { return n; };
    assert(n == 5);
}
"#,
        );
    }

    #[test]
    fn using_moved_field_again_is_reported() {
        let violations = move_violations_for_source(
            r#"
struct Pair {
    left: String,
    right: i64,
}

fun main() {
    let pair = Pair { left = "a", right = 1 };
    let moved: String = pair.left;
    let again: String = pair.left;
}
"#,
        );

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].binding, "pair");
    }

    #[test]
    fn sibling_field_stays_accessible_after_partial_move() {
        assert_no_violations(
            r#"
struct Pair {
    left: String,
    right: i64,
}

fun main() {
    let pair = Pair { left = "a", right = 1 };
    let moved: String = pair.left;
    let still_live: i64 = pair.right;
}
"#,
        );
    }

    #[test]
    fn whole_value_use_after_partial_move_is_reported() {
        let violations = assert_has_violation(
            r#"
struct Pair {
    left: String,
    right: i64,
}

fun take(pair: Pair) -> i64 {
    pair.right
}

fun main() {
    let pair = Pair { left = "a", right = 1 };
    let moved: String = pair.left;
    let value: i64 = take(pair);
}
"#,
            "pair",
        );
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn partial_move_of_drop_type_is_reported() {
        assert_has_violation(
            r#"
struct Handle {
    name: String,
    fd: i64,
}

extend Handle: Drop {
    fun drop(self) { }
}

fun main() {
    let handle = Handle { name = "x", fd = 1 };
    let name = handle.name;
}
"#,
            "handle",
        );
    }

    #[test]
    fn partial_move_of_drop_type_in_match_binding_is_reported() {
        assert_has_violation(
            r#"
struct Handle {
    name: String,
    fd: i64,
}

extend Handle: Drop {
    fun drop(self) { }
}

fun main() {
    let handle = Handle { name = "x", fd = 1 };
    let n = match handle.name {
        name => name.len(),
    };
}
"#,
            "handle",
        );
    }

    /// A record pattern moves at field granularity, like a struct field access:
    /// the moved field is gone, and the record may no longer be used as a whole.
    ///
    /// The `Drop` variant of this test is deliberately absent rather than
    /// overlooked. An anonymous record can never implement `Drop` (RFC-0116 §3,
    /// enforced in `coherence`: "anonymous records cannot implement `Drop`"), so
    /// a record pattern partially moving a `Drop` value is unrepresentable. An
    /// earlier revision tried to write it by destructuring a *nominal* struct
    /// with a record pattern, which the typechecker rejects outright — the test
    /// was failing on its own fixture, not on the checker.
    #[test]
    fn record_pattern_moves_at_field_granularity() {
        assert_has_violation(
            r#"
fun take(r: { n: i64, name: String }) -> i64 {
    return r.n;
}

fun main() {
    let r = { name = "x", n = 1 };
    let moved = match r {
        { name, n } => name,
    };
    let again = take(r);
}
"#,
            "r",
        );
    }

    #[test]
    fn tuple_pattern_partial_move_of_drop_prefix_is_reported() {
        assert_has_violation(
            r#"
struct Wrapper {
    pair: (String, i64),
}

extend Wrapper: Drop {
    fun drop(self) { }
}

fun main() {
    let wrapper = Wrapper { pair = ("x", 1) };
    let n = match wrapper.pair {
        (name, _) => name.len(),
    };
}
"#,
            "wrapper",
        );
    }

    #[test]
    fn enum_payload_pattern_partial_move_of_drop_prefix_is_reported() {
        assert_has_violation(
            r#"
enum MaybeText {
    Empty,
    Full { text: String },
}

struct Wrapper {
    payload: MaybeText,
}

extend Wrapper: Drop {
    fun drop(self) { }
}

fun main() {
    let wrapper = Wrapper {
        payload = MaybeText::Full { text = "x" },
    };
    let n = match wrapper.payload {
        MaybeText::Full { text } => text.len(),
        MaybeText::Empty => 0,
    };
}
"#,
            "wrapper",
        );
    }

    #[test]
    fn nested_direct_partial_move_of_drop_prefix_is_reported() {
        assert_has_violation(
            r#"
struct Wrapper {
    pair: (String, i64),
}

extend Wrapper: Drop {
    fun drop(self) { }
}

fun main() {
    let wrapper = Wrapper { pair = ("x", 1) };
    let name = wrapper.pair.0;
}
"#,
            "wrapper",
        );
    }

    #[test]
    fn tuple_element_partial_move_then_reuse_is_reported() {
        assert_has_violation(
            r#"
fun main() {
    let pair = ("x", 1);
    let left = pair.0;
    let again = pair.0;
}
"#,
            "pair",
        );
    }

    #[test]
    fn enum_payload_move_consumes_whole_value() {
        assert_has_violation(
            r#"
enum MaybeText {
    Empty,
    Full { text: String },
}

fun main() {
    let value = MaybeText::Full { text = "x" };
    let n = match value {
        MaybeText::Full { text } => text.len(),
        MaybeText::Empty => 0,
    };
    let again = value;
}
"#,
            "value",
        );
    }

    #[test]
    fn array_element_move_is_reported() {
        assert_has_violation(
            r#"
fun main() {
    let xs = ["x"];
    let first = xs[0];
}
"#,
            "xs",
        );
    }

    #[test]
    fn array_element_move_in_match_binding_is_reported() {
        assert_has_violation(
            r#"
fun main() {
    let xs = ["x"];
    let n = match xs[0] {
        s => s.len(),
    };
}
"#,
            "xs",
        );
    }

    #[test]
    fn array_pattern_binding_array_element_is_reported() {
        assert_has_violation(
            r#"
fun main() {
    let xs: [String; 1] = ["x"];
    let n = match xs {
        [s] => s.len(),
    };
}
"#,
            "xs",
        );
    }

    #[test]
    fn closure_capture_then_use_is_reported() {
        let violations = assert_has_violation(
            r#"
fun main() {
    let s = "hello";
    let f = () -> String { s };
    let again = s;
}
"#,
            "s",
        );
        assert_eq!(violations[0].moved_type, "String");
    }

    #[test]
    fn move_in_one_if_arm_persists_after_join() {
        assert_has_violation(
            r#"
fun main() {
    let s = "hello";
    if (true) {
        let moved = s;
    } else {
        let keep = 0;
    }
    let again = s;
}
"#,
            "s",
        );
    }

    #[test]
    fn move_in_loop_body_persists_after_loop() {
        assert_has_violation(
            r#"
fun main() {
    let s = "hello";
    loop {
        let moved = s;
        break;
    }
    let again = s;
}
"#,
            "s",
        );
    }

    #[test]
    fn mut_ref_argument_reborrows_cleanly() {
        assert_no_violations(
            r#"
struct Counter { value: i64 }

fun bump(r: &var Counter) { }

fun main() {
    var c = Counter { value = 0 };
    let r = &var c;
    bump(r);
    bump(r);
}
"#,
        );
    }

    #[test]
    fn plain_binding_of_mut_ref_then_use_is_reported() {
        assert_has_violation(
            r#"
struct Counter { value: i64 }

fun bump(r: &var Counter) { }

fun main() {
    var c = Counter { value = 0 };
    let r = &var c;
    let q = r;
    bump(r);
}
"#,
            "r",
        );
    }

    #[test]
    fn move_site_is_not_reported_as_its_own_use() {
        let violations = move_violations_for_source(
            r#"
struct Pair {
    left: String,
    right: i64,
}

fun main() {
    let pair = Pair { left = "a", right = 1 };
    let moved = pair.left;
}
"#,
        );
        assert!(
            violations.is_empty(),
            "the move site must not accuse itself: {violations:#?}"
        );
    }

    #[test]
    fn moving_projection_does_not_report_base_as_only_use() {
        let violations = move_violations_for_source(
            r#"
struct Pair {
    left: String,
    right: i64,
}

fun main() {
    let pair = Pair { left = "a", right = 1 };
    let moved = pair.left;
    let sibling = pair.right;
}
"#,
        );
        assert!(
            violations.is_empty(),
            "moving `pair.left` must not report `pair` as used-after-move: {violations:#?}"
        );
    }

    /// A tuple literal takes ownership of its elements. Regression for a false
    /// negative where they were only *observed*: the element stayed usable
    /// afterwards, and every rule `consume_place` enforces was skipped.
    #[test]
    fn tuple_literal_consumes_its_elements() {
        assert_has_violation(
            r#"
struct Owned {
    s: String,
}

fun main() {
    let a = Owned { s = "x" };
    let t = (a, 1);
    let n = a.s.len();
}
"#,
            "a",
        );
    }

    #[test]
    fn tuple_literal_cannot_partially_move_a_drop_type() {
        assert_has_violation(
            r#"
struct Handle {
    name: String,
    fd: i64,
}

extend Handle: Drop {
    fun drop(self) { }
}

fun main() {
    let h = Handle { name = "x", fd = 1 };
    let t = (h.name, 1);
}
"#,
            "h",
        );
    }

    #[test]
    fn array_literal_cannot_move_an_array_element() {
        assert_has_violation(
            r#"
fun main() {
    let xs = ["a"];
    let ys = [xs[0]];
}
"#,
            "xs",
        );
    }

    #[test]
    fn borrowed_array_for_in_cannot_move_a_noncopy_element() {
        assert_has_violation(
            r#"
fun first<T>(items: T[]) -> T {
    for (item in items) {
        return item;
    }
    panic("empty")
}

fun main() { }
"#,
            "item",
        );
    }

    #[test]
    fn borrowed_array_for_in_allows_copy_elements() {
        assert_no_violations(
            r#"
fun first<T: Copy>(items: T[]) -> T {
    for (item in items) {
        return item;
    }
    panic("empty")
}

fun main() {
    let values: i64[] = [1, 2, 3];
    assert(first(values) == 1);
}
"#,
        );
    }

    #[test]
    fn function_values_are_copy() {
        assert_no_violations(
            r#"
fun increment(value: i64) -> i64 { value + 1 }

fun apply(f: (i64) -> i64) -> i64 { f(1) }

fun main() {
    let f = increment;
    assert(apply(f) == 2);
    assert(apply(f) == 2);
}
"#,
        );
    }

    // --- #337: type conversion must preserve arity or fail outright ---------------------
    //
    // These exercise the conversion helpers directly. The misalignment they guard against
    // needs an unresolved `InferType::Var` to survive into a parameter list, which the
    // reconstruction path does not currently produce from source -- it abandons a body
    // wholesale instead. So there is no `.mtl` fixture that would fail without the fix;
    // asserting on the helpers is what actually pins the invariant.

    use crate::typeinference::{InferType, TypeVar};

    fn var() -> InferType {
        InferType::Var(TypeVar(0))
    }

    fn concrete() -> InferType {
        InferType::Concrete(Type::I64)
    }

    #[test]
    fn tuple_with_an_unresolved_element_converts_to_none_not_a_shorter_tuple() {
        let ty = InferType::Tuple(vec![var(), concrete()]);
        assert_eq!(infer_to_type(&ty), None);
    }

    #[test]
    fn record_with_an_unresolved_field_converts_to_none_not_a_smaller_record() {
        let ty = InferType::Record(vec![
            ("a".to_string(), var()),
            ("b".to_string(), concrete()),
        ]);
        assert_eq!(infer_to_type(&ty), None);
    }

    #[test]
    fn fun_with_an_unresolved_param_converts_to_none_not_a_shorter_signature() {
        let ty = InferType::Fun(vec![var(), concrete()], Box::new(concrete()));
        assert_eq!(infer_to_type(&ty), None);
    }

    #[test]
    fn named_with_an_unresolved_argument_converts_to_none_not_fewer_arguments() {
        let ty = InferType::Named("Holder".to_string(), vec![var(), concrete()]);
        assert_eq!(infer_to_type(&ty), None);
    }

    #[test]
    fn fully_resolved_compounds_still_convert_and_keep_their_arity() {
        let tuple = InferType::Tuple(vec![concrete(), concrete(), concrete()]);
        assert_eq!(
            infer_to_type(&tuple),
            Some(Type::Tuple(vec![Type::I64, Type::I64, Type::I64]))
        );

        let fun = InferType::Fun(vec![concrete(), concrete()], Box::new(concrete()));
        assert_eq!(
            infer_to_type(&fun),
            Some(Type::Fun(vec![Type::I64, Type::I64], Box::new(Type::I64)))
        );
    }

    #[test]
    fn method_arg_types_are_none_when_a_parameter_is_unresolved() {
        // Receiver plus three parameters, the middle one unresolved. A filtered list would
        // be `[i64, i64]`, and `observe_call_args` -- which indexes positionally -- would
        // then judge the third argument against the second parameter's type. That decides
        // borrow-vs-move, so the shift silently turns a reborrow into a move or back.
        let fun_ty = InferType::Fun(
            vec![concrete(), concrete(), var(), concrete()],
            Box::new(concrete()),
        );
        assert_eq!(infer_method_arg_types(&fun_ty), None);
    }

    #[test]
    fn method_arg_types_skip_the_receiver_and_keep_the_rest_in_order() {
        let fun_ty = InferType::Fun(
            vec![
                InferType::Concrete(Type::Boolean),
                InferType::Concrete(Type::I64),
                InferType::Concrete(Type::MutReference(Box::new(Type::I64))),
            ],
            Box::new(concrete()),
        );
        assert_eq!(
            infer_method_arg_types(&fun_ty),
            Some(vec![Type::I64, Type::MutReference(Box::new(Type::I64))])
        );
    }
}

fn typed_place_span(place: &TypedPlace) -> &Span {
    match place {
        TypedPlace::Ident(_, span)
        | TypedPlace::Deref { span, .. }
        | TypedPlace::Field { span, .. }
        | TypedPlace::Tuple { span, .. }
        | TypedPlace::Index { span, .. } => span,
    }
}
