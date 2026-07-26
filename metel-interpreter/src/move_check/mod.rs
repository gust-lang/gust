pub mod place;

use std::collections::{HashMap, HashSet};

use crate::ast::{Pattern, ReceiverKind, Span};
use crate::typed_ast::{
    FunBody, MethodDispatch, TypedBlock, TypedDecl, TypedExpr, TypedForInit, TypedModule,
    TypedModuleGraph, TypedPlace, TypedStmt,
};
use crate::typeinference::TypeDefinitionRegistry;
use crate::types::Type;

use self::place::{from_expr as place_from_expr, from_typed_place, Place, Projection};

#[derive(Debug, Clone)]
pub struct MoveViolation {
    pub binding: String,
    pub use_place: Place,
    pub moved_place: Place,
    pub moved_by_value_receiver: bool,
    pub use_span: Span,
    pub moved_span: Span,
}

#[derive(Debug, Clone, Default)]
pub struct MoveCheckReport {
    pub violations: Vec<MoveViolation>,
    pub skipped_generic_bodies_user: usize,
    pub skipped_generic_bodies_embedded_std: usize,
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

#[derive(Debug, Clone)]
struct MoveRecord {
    place: Place,
    moved_span: Span,
    cause: MoveCause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoveCause {
    Other,
    ByValueReceiver,
}

#[derive(Debug, Clone, Default)]
struct FlowState {
    moved: HashMap<String, Vec<MoveRecord>>,
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
            }
        }
    }

    fn bind(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(name.to_string());
        }
        self.moved.remove(name);
    }

    fn record_move(&mut self, place: Place, moved_span: Span, cause: MoveCause) {
        let root = place.root().to_string();
        let records = self.moved.entry(root).or_default();
        if place.projections().is_empty() {
            records.clear();
            records.push(MoveRecord { place, moved_span, cause });
            return;
        }
        records.retain(|record| !place.is_prefix_of(&record.place));
        records.push(MoveRecord { place, moved_span, cause });
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

struct Checker<'a> {
    registry: &'a TypeDefinitionRegistry,
    report: MoveCheckReport,
}

impl<'a> Checker<'a> {
    fn new(registry: &'a TypeDefinitionRegistry) -> Self {
        Self {
            registry,
            report: MoveCheckReport::default(),
        }
    }

    fn check_module(&mut self, module: &TypedModule) {
        let mut state = FlowState::default();
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
                state.bind(&let_decl.name);
            }
            TypedDecl::Mut(mut_decl) => {
                self.consume_expr(&mut_decl.value, current_module, state);
                state.bind(&mut_decl.name);
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
                    FunBody::Generic(_) => {
                        self.record_skipped_generic_body(&fun.span);
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
                        FunBody::Generic(_) => {
                            self.record_skipped_generic_body(&method.span);
                        }
                        FunBody::Native(_) => {}
                    }
                }
            }
            TypedDecl::Struct(_) | TypedDecl::Enum(_) | TypedDecl::Aspect(_) => {}
        }
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
                            state.bind(&let_decl.name);
                        }
                        TypedForInit::Mut(mut_decl) => {
                            self.consume_expr(&mut_decl.value, current_module, state);
                            state.bind(&mut_decl.name);
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
                body_state.bind(&for_in.binding);
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
            TypedExpr::Literal(..) | TypedExpr::Ident(..) | TypedExpr::Path(..) => {}
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
            TypedExpr::Assign { target, value, .. } => {
                self.observe_typed_place(target, current_module, state);
                self.consume_expr(value, current_module, state);
            }
            TypedExpr::Call { callee, args, .. } => {
                self.observe_expr(callee, current_module, state);
                let param_types = function_param_types(callee.ty());
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
            TypedExpr::MethodCall {
                receiver,
                method,
                args,
                dispatch,
                ..
            } => {
                self.consume_method_receiver(receiver, method, dispatch, current_module, state);
                let param_types =
                    self.method_param_types(receiver.ty(), method, current_module, dispatch);
                for (index, arg) in args.iter().enumerate() {
                    let reborrow = param_types
                        .as_ref()
                        .and_then(|params| params.get(index))
                        .is_some_and(|param_ty| is_reborrow(arg, param_ty));
                    if reborrow {
                        self.observe_expr(arg, current_module, state);
                    } else {
                        self.consume_expr(arg, current_module, state);
                    }
                }
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
            TypedExpr::Match(m) => {
                self.observe_expr(&m.scrutinee, current_module, state);
                let mut joined = state.clone();
                joined.moved.clear();
                for arm in &m.arms {
                    let mut arm_state = state.clone();
                    arm_state.push_scope();
                    self.apply_pattern_moves(
                        &arm.pattern,
                        &m.scrutinee,
                        current_module,
                        &mut arm_state,
                    );
                    if let Some(guard) = &arm.guard {
                        self.observe_expr(guard, current_module, &mut arm_state);
                    }
                    self.check_block(&arm.body, current_module, &mut arm_state);
                    arm_state.pop_scope();
                    joined.union_from(&arm_state);
                }
                state.union_from(&joined);
            }
            TypedExpr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
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
            TypedExpr::Loop { body, .. } => {
                let mut body_state = state.clone();
                self.check_block(body, current_module, &mut body_state);
                state.union_from(&body_state);
            }
            TypedExpr::Closure {
                params,
                body,
                span,
                ..
            } => {
                self.capture_closure(body, params.iter().map(|param| param.name.as_str()), span, state);
                let mut closure_state = FlowState::default();
                closure_state.push_scope();
                for captured in collect_free_roots_from_typed_block(body, &HashSet::new()) {
                    closure_state.bind(&captured);
                }
                for param in params {
                    closure_state.bind(&param.name);
                }
                self.check_block(body, current_module, &mut closure_state);
                closure_state.pop_scope();
            }
            TypedExpr::GenericClosure { params, span, .. } => {
                for param in params {
                    let _ = param;
                }
                self.record_skipped_generic_body(span);
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
            TypedExpr::Continue(_) => {}
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
            match expr {
                TypedExpr::Index { index, .. } => {
                    if !self.is_copy(current_module, expr.ty()) {
                        self.report_illegal_move(place.clone(), expr.span().clone());
                        self.observe_expr(index, current_module, state);
                        return;
                    }
                    self.check_place_use_before_move(&place, expr.span(), state);
                    self.observe_expr(index, current_module, state);
                    self.record_move_if_needed(place, expr.ty(), expr.span(), current_module, state, cause);
                }
                TypedExpr::FieldAccess { object, .. } | TypedExpr::TupleAccess { object, .. } => {
                    if self.is_drop(current_module, object.ty()) && !self.is_copy(current_module, expr.ty()) {
                        self.report_illegal_move(place.clone(), expr.span().clone());
                        self.observe_projection_base_expr(object, current_module, state);
                        return;
                    }
                    self.check_place_use_before_move(&place, expr.span(), state);
                    self.observe_projection_base_expr(object, current_module, state);
                    self.record_move_if_needed(place, expr.ty(), expr.span(), current_module, state, cause);
                }
                TypedExpr::Ident(..) => {
                    self.consume_place(place, expr.ty(), expr.span(), current_module, state, cause);
                }
                _ => {
                    self.consume_place(place, expr.ty(), expr.span(), current_module, state, cause);
                }
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
                self.consume_expr_with_cause(receiver, current_module, state, MoveCause::ByValueReceiver)
            }
            Some(ReceiverKind::Ref) | Some(ReceiverKind::RefMut) | None => {
                self.observe_expr(receiver, current_module, state)
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
        match pattern {
            Pattern::Wildcard(_) | Pattern::Literal(_, _) => {}
            Pattern::Binding(name, span) => {
                if let Some(place) = place_from_expr(scrutinee) {
                    self.consume_place(
                        place,
                        scrutinee.ty(),
                        scrutinee.span(),
                        current_module,
                        state,
                        MoveCause::Other,
                    );
                }
                state.bind(name);
                let _ = span;
            }
            Pattern::Tuple(items, _) => {
                for (index, item) in items.iter().enumerate() {
                    if let Some(base) = place_from_expr(scrutinee) {
                        let field_place = base.clone().with_projection(Projection::TupleIndex(index));
                        self.apply_pattern_place_move(item, &field_place, scrutinee.ty(), current_module, state);
                    }
                }
            }
            Pattern::Record { fields, .. } => {
                for field in fields {
                    if let Some(base) = place_from_expr(scrutinee) {
                        let field_place = base.clone().with_projection(Projection::Field(field.clone()));
                        self.consume_place(
                            field_place,
                            scrutinee.ty(),
                            scrutinee.span(),
                            current_module,
                            state,
                            MoveCause::Other,
                        );
                    }
                    state.bind(field);
                }
            }
            Pattern::EnumVariant { fields, .. } => {
                if !fields.is_empty() {
                    if let Some(place) = place_from_expr(scrutinee) {
                        self.consume_place(
                            place,
                            scrutinee.ty(),
                            scrutinee.span(),
                            current_module,
                            state,
                            MoveCause::Other,
                        );
                        for field in fields {
                            state.bind(field);
                        }
                    }
                }
            }
            Pattern::Array { elems, rest, .. } => {
                for item in elems {
                    self.observe_pattern_bindings(item, state);
                }
                if let Some(rest) = rest {
                    state.bind(rest);
                }
            }
        }
    }

    fn apply_pattern_place_move(
        &mut self,
        pattern: &Pattern,
        place: &Place,
        parent_ty: &Type,
        current_module: &[String],
        state: &mut FlowState,
    ) {
        match pattern {
            Pattern::Wildcard(_) | Pattern::Literal(_, _) => {}
            Pattern::Binding(name, _) => {
                self.consume_place(
                    place.clone(),
                    parent_ty,
                    &dummy_span_from_place(place),
                    current_module,
                    state,
                    MoveCause::Other,
                );
                state.bind(name);
            }
            Pattern::Tuple(items, _) => {
                for (index, item) in items.iter().enumerate() {
                    let child = place.clone().with_projection(Projection::TupleIndex(index));
                    self.apply_pattern_place_move(item, &child, parent_ty, current_module, state);
                }
            }
            Pattern::Record { fields, .. } => {
                for field in fields {
                    let child = place.clone().with_projection(Projection::Field(field.clone()));
                    self.consume_place(
                        child,
                        parent_ty,
                        &dummy_span_from_place(place),
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
                        place.clone(),
                        parent_ty,
                        &dummy_span_from_place(place),
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
                    self.observe_pattern_bindings(item, state);
                }
                if let Some(rest) = rest {
                    state.bind(rest);
                }
            }
        }
    }

    fn observe_pattern_bindings(&mut self, pattern: &Pattern, state: &mut FlowState) {
        match pattern {
            Pattern::Binding(name, _) => state.bind(name),
            Pattern::Tuple(items, _) => {
                for item in items {
                    self.observe_pattern_bindings(item, state);
                }
            }
            Pattern::Record { fields, .. } => {
                for field in fields {
                    state.bind(field);
                }
            }
            Pattern::EnumVariant { fields, .. } => {
                for field in fields {
                    state.bind(field);
                }
            }
            Pattern::Array { elems, rest, .. } => {
                for item in elems {
                    self.observe_pattern_bindings(item, state);
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
            locals.insert(capture.clone());
            let capture_place = Place::new(capture.clone());
            self.consume_place(
                capture_place,
                &Type::Named(String::new(), vec![]),
                span,
                &[],
                state,
                MoveCause::Other,
            );
        }
    }

    fn consume_place(
        &mut self,
        place: Place,
        ty: &Type,
        use_span: &Span,
        current_module: &[String],
        state: &mut FlowState,
        cause: MoveCause,
    ) {
        self.check_place_use_before_move(&place, use_span, state);
        self.record_move_if_needed(place, ty, use_span, current_module, state, cause);
    }

    fn check_place_use_before_move(&mut self, place: &Place, use_span: &Span, state: &FlowState) {
        self.record_whole_use_if_moved(place, use_span, state);
    }

    fn record_move_if_needed(
        &self,
        place: Place,
        ty: &Type,
        use_span: &Span,
        current_module: &[String],
        state: &mut FlowState,
        cause: MoveCause,
    ) {
        if self.is_copy(current_module, ty) {
            return;
        }
        state.record_move(place, use_span.clone(), cause);
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
                moved_by_value_receiver: record.cause == MoveCause::ByValueReceiver,
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
                moved_by_value_receiver: record.cause == MoveCause::ByValueReceiver,
                use_span: use_span.clone(),
                moved_span: record.moved_span.clone(),
            });
        }
    }

    fn record_skipped_generic_body(&mut self, span: &Span) {
        if span.filename.starts_with("<embedded std::") {
            self.report.skipped_generic_bodies_embedded_std += 1;
        } else {
            self.report.skipped_generic_bodies_user += 1;
        }
    }

    fn is_copy(&self, current_module: &[String], ty: &Type) -> bool {
        self.registry.type_satisfies_aspect(current_module, ty, "Copy")
    }

    fn is_drop(&self, current_module: &[String], ty: &Type) -> bool {
        self.registry.type_satisfies_aspect(current_module, ty, "Drop")
    }

    fn report_illegal_move(&mut self, place: Place, use_span: Span) {
        self.report.violations.push(MoveViolation {
            binding: place.root().to_string(),
            use_place: place.clone(),
            moved_place: place.clone(),
            moved_by_value_receiver: false,
            use_span,
            moved_span: dummy_span_from_place(&place),
        });
    }

    fn method_receiver_kind(
        &self,
        receiver_ty: &Type,
        method: &str,
        _current_module: &[String],
        _dispatch: &MethodDispatch,
    ) -> Option<ReceiverKind> {
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
        match peel_type_references(receiver_ty) {
            Type::Array(_) => self
                .registry
                .array_method_type(method)
                .and_then(infer_fun_param_types),
            Type::Named(name, _) => self.registry.method_type(name, method).and_then(infer_fun_param_types),
            other => primitive_type_name(other)
                .and_then(|name| self.registry.method_type(&name, method).and_then(infer_fun_param_types)),
        }
    }
}

fn function_param_types(ty: &Type) -> Option<&[Type]> {
    match ty {
        Type::Fun(params, _) => Some(params),
        _ => None,
    }
}

fn infer_fun_param_types(fun_ty: &crate::typeinference::InferType) -> Option<Vec<Type>> {
    match fun_ty {
        crate::typeinference::InferType::Fun(params, _) => Some(
            params
                .iter()
                .filter_map(infer_to_type_lossy)
                .collect(),
        ),
        _ => None,
    }
}

fn infer_to_type_lossy(ty: &crate::typeinference::InferType) -> Option<Type> {
    use crate::typeinference::InferType;
    match ty {
        InferType::Concrete(inner) => Some(inner.clone()),
        InferType::Never => Some(Type::Never),
        InferType::Tuple(items) => Some(Type::Tuple(
            items.iter().filter_map(infer_to_type_lossy).collect(),
        )),
        InferType::Record(fields) => Some(Type::Record(
            fields
                .iter()
                .filter_map(|(name, ty)| infer_to_type_lossy(ty).map(|ty| (name.clone(), ty)))
                .collect(),
        )),
        InferType::Array(inner) => infer_to_type_lossy(inner).map(|inner| Type::Array(Box::new(inner))),
        InferType::SizedArray(inner, len) => {
            infer_to_type_lossy(inner).map(|inner| Type::SizedArray(Box::new(inner), *len))
        }
        InferType::Reference(inner) => {
            infer_to_type_lossy(inner).map(|inner| Type::Reference(Box::new(inner)))
        }
        InferType::MutReference(inner) => {
            infer_to_type_lossy(inner).map(|inner| Type::MutReference(Box::new(inner)))
        }
        InferType::Fun(params, ret) => Some(Type::Fun(
            params.iter().filter_map(infer_to_type_lossy).collect(),
            Box::new(infer_to_type_lossy(ret)?),
        )),
        InferType::Named(name, args) => Some(Type::Named(
            name.clone(),
            args.iter().filter_map(infer_to_type_lossy).collect(),
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
) -> Vec<String> {
    let mut collector = FreeRootCollector {
        scope_stack: vec![initial_locals.clone()],
        captures: Vec::new(),
        seen: HashSet::new(),
    };
    collector.block(block);
    collector.captures
}

struct FreeRootCollector {
    scope_stack: Vec<HashSet<String>>,
    captures: Vec<String>,
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
            TypedExpr::Ident(name, _, _) => self.capture_if_free(name),
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
            | TypedExpr::SingletonCoerce { inner: value, .. } => self.expr(value),
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
            TypedPlace::Ident(name, _) => self.capture_if_free(name),
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

    fn capture_if_free(&mut self, name: &str) {
        if self
            .scope_stack
            .iter()
            .rev()
            .any(|scope| scope.contains(name))
        {
            return;
        }
        if self.seen.insert(name.to_string()) {
            self.captures.push(name.to_string());
        }
    }
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
    fn closure_capture_then_use_is_reported() {
        assert_has_violation(
            r#"
fun main() {
    let s = "hello";
    let f = () -> String { s };
    let again = s;
}
"#,
            "s",
        );
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
